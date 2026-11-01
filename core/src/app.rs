
//! The app: every method the front end calls, with the same names, arguments,
//! results and error strings as the Go `App` in app.go.
//!
//! This is the Go `App` moved across, not rewritten. The JSON bridge that
//! answered `window.go.main.App[method]` is not here: it lives in the root
//! package (`src/app.rs`), which is the only thing that still has to answer a
//! webview. Here every one of those calls is a plain Rust method, so the app
//! (`egui-ui`), the CLI and the bench all drive the same logic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::api::{self, Client};
use crate::auth::AuthManager;
use crate::config::{self, Crumb, Settings};
use crate::host::Host;
use crate::history;
use crate::portal;
use crate::services::ServiceRequest;
use crate::share;
use crate::update::{self, Release};
use crate::xfer::{self, manager as xfer_manager, DownloadOptions, LinkInfo, Snapshot, Status, UploadOptions};

/// One level of a Frame.io path: account, workspace, project or folder.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub kind: String, // account | workspace | project | folder
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_id: String,
    /// Search results: the folder's project.
    #[serde(rename = "where", default, skip_serializing_if = "String::is_empty")]
    pub where_: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct State {
    pub version: String,
    pub logged_in: bool,
    pub user: Option<api::User>,
    pub settings: Settings,
    pub redirect_uri: String,
    pub log_dir: String,
    pub log_file: String,
    pub home: String,
    pub open_at_login: bool,
}

impl State {
    // Go's State marshals Settings inline; `settings` above does that.
    fn _unused(&self) {}
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub url: String,
    pub state: String, // idle | checking | downloading | restarting
    pub progress: f64,
    pub error: String,
    pub checked_at: i64,
    /// Why this copy can't update itself.
    pub blocker: String,
}

struct UpdaterState {
    st: UpdateStatus,
    rel: Option<Release>,
    app: Option<String>,
    last_emit: Option<Instant>,
}

pub struct App {
    pub settings: Arc<Mutex<Settings>>,
    pub me_self: Mutex<std::sync::Weak<App>>,
    pub auth: Arc<AuthManager>,
    pub api: Arc<Client>,
    pub hist: Mutex<Option<Arc<history::Store>>>,
    pub mgr: Mutex<Option<Arc<xfer::Manager>>>,
    pub wat: Mutex<Option<Arc<xfer::watch::Watcher>>>,
    /// The platform, once a front end has handed one over.
    host: Mutex<Option<Arc<dyn Host>>>,

    me: Mutex<Option<api::User>>,
    me_fetching: AtomicBool,
    me_tried: Mutex<Option<Instant>>,

    login_lock: tokio::sync::Mutex<()>,
    login_cancel: Mutex<Option<CancellationToken>>,

    names: Mutex<HashMap<String, String>>,
    projects_listed: Mutex<Option<Instant>>,

    open_at_login: AtomicBool,
    started: Mutex<Option<Instant>>,

    /// Last Frame.io storage reading, so the Space view can be opened without
    /// asking Frame.io again.
    space: Mutex<Option<(Instant, SpacePayload)>>,
    space_walking: AtomicBool,
    /// The last Portal reconcile, for the status line.
    portal_last: Mutex<Option<portal::agent::Report>>,
    portal_running: AtomicBool,
    /// Whether a Portal credential is stored. Cached because the status line is
    /// drawn every frame and reading the Keychain is neither free nor silent.
    /// It is refreshed when a credential is stored and at every reconcile.
    portal_signed_in: AtomicBool,
    /// The signed-in admin's email, cached with the flag above for the same
    /// reason: the row that shows "Signed in as …" is drawn every frame.
    portal_email: Mutex<String>,
    /// A sign-in whose password step Portal accepted and whose one-time code is
    /// still owed: the client holds Portal's *pending* session, so the code
    /// finishes that same sign-in. Not a stored credential — it is dropped when
    /// the sign-in completes, when it is signed out of, and when a new password
    /// sign-in replaces it.
    portal_pending: Mutex<Option<portal::PortalClient>>,
    upd: Mutex<UpdaterState>,
    updating: AtomicBool,
}

impl App {
    pub fn new(settings: Settings) -> Arc<App> {
        let settings_cell = Arc::new(Mutex::new(settings));
        let auth = {
            let c = settings_cell.clone();
            AuthManager::new(move || c.lock().unwrap().client_id.clone())
        };
        let api = Arc::new(Client::new(auth.clone()));
        Self::assemble(settings_cell, auth, api)
    }

    /// The same `App`, but every API call goes through `api`. A test hands in a
    /// real `Client` whose `base_url` is a local stub, so the destructive paths
    /// can be driven without the network. Test seam only; nothing calls this.
    #[doc(hidden)]
    pub fn with_api(settings: Settings, api: Arc<Client>) -> Arc<App> {
        let auth = api.auth.clone();
        Self::assemble(Arc::new(Mutex::new(settings)), auth, api)
    }

    fn assemble(
        settings_cell: Arc<Mutex<Settings>>,
        auth: Arc<AuthManager>,
        api: Arc<Client>,
    ) -> Arc<App> {
        let app = Arc::new(App {
            settings: settings_cell,
            me_self: Mutex::new(std::sync::Weak::new()),
            auth,
            api,
            hist: Mutex::new(None),
            mgr: Mutex::new(None),
            wat: Mutex::new(None),
            host: Mutex::new(None),
            me: Mutex::new(None),
            me_fetching: AtomicBool::new(false),
            me_tried: Mutex::new(None),
            login_lock: tokio::sync::Mutex::new(()),
            login_cancel: Mutex::new(None),
            names: Mutex::new(HashMap::new()),
            projects_listed: Mutex::new(None),
            open_at_login: AtomicBool::new(false),
            started: Mutex::new(None),
            space: Mutex::new(None),
            space_walking: AtomicBool::new(false),
            portal_last: Mutex::new(None),
            portal_running: AtomicBool::new(false),
            portal_signed_in: AtomicBool::new(false),
            portal_email: Mutex::new(String::new()),
            portal_pending: Mutex::new(None),
            upd: Mutex::new(UpdaterState {
                st: UpdateStatus { current: crate::VERSION.to_string(), state: "idle".into(), ..Default::default() },
                rel: None,
                app: None,
                last_emit: None,
            }),
            updating: AtomicBool::new(false),
        });
        *app.me_self.lock().unwrap() = Arc::downgrade(&app);
        let _ = GLOBAL.set(Mutex::new(Arc::downgrade(&app)));
        app
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn mgr(&self) -> Option<Arc<xfer::Manager>> {
        self.mgr.lock().unwrap().clone()
    }

    pub fn hist(&self) -> Option<Arc<history::Store>> {
        self.hist.lock().unwrap().clone()
    }

    /// The running watcher, if the app has started one.
    pub fn wat(&self) -> Option<Arc<xfer::watch::Watcher>> {
        self.wat.lock().unwrap().clone()
    }

    /// Hands this app its platform. Called once, before `start`; without a host
    /// every platform call takes the trait's default (nothing chosen, nothing
    /// shown), which is what the CLI and the tests run on.
    pub fn set_host(&self, host: Arc<dyn Host>) {
        *self.host.lock().unwrap() = Some(host);
    }

    /// The platform, if a front end has handed one over.
    pub fn host(&self) -> Option<Arc<dyn Host>> {
        self.host.lock().unwrap().clone()
    }

    fn emit(&self, event: &str, payload: Value) {
        if let Some(h) = self.host.lock().unwrap().as_ref() {
            h.emit(event, payload);
        }
    }

    fn state_changed(&self) {
        self.emit("state-changed", json!(null));
    }

    pub fn show_window(&self) {
        if let Some(h) = self.host.lock().unwrap().as_ref() {
            h.show_window();
        }
    }

    /// Asks the front end to close (after an update installed itself).
    pub fn quit(&self) {
        if let Some(h) = self.host.lock().unwrap().as_ref() {
            h.quit();
        }
    }

    /// Asks a question with two answers; without a host, no (`cancel`), which is
    /// what the AppKit panes answered before the event loop ran.
    fn question(&self, title: &str, message: &str, ok: &str, cancel: &str) -> bool {
        match self.host() {
            Some(h) => h.question(title, message, ok, cancel),
            None => false,
        }
    }

    fn api_cx(&self) -> CancellationToken {
        CancellationToken::new()
    }

    // ---------- Startup ----------

    /// Opens history, starts the manager and the watcher, and begins the loops
    /// that talk to updates and Finder services.
    pub async fn start(self: &Arc<Self>) {
        let h = match history::Store::open(&config::dir(), &config::log_dir()) {
            Ok(h) => Arc::new(h),
            Err(e) => {
                if let Some(h) = self.host() {
                    h.error("FioTransfer", &format!("Could not open transfer history: {}", e));
                }
                std::process::exit(1);
            }
        };
        // Only the app does this: the CLI may run while the app is transferring.
        h.mark_interrupted();
        *self.hist.lock().unwrap() = Some(h.clone());

        let settings = self.settings.lock().unwrap().clone();
        let cell = {
            let a = Arc::clone(self);
            move || a.settings()
        };
        let _ = settings;
        let mgr = xfer_manager::new_manager(self.api.clone(), h.clone(), Box::new(cell));
        {
            let a = Arc::clone(self);
            *mgr.on_update.lock().unwrap() = Some(Arc::new(move |s: Snapshot| {
                a.on_snapshot(s);
            }));
        }
        *self.mgr.lock().unwrap() = Some(mgr.clone());
        let wat = xfer::watch::Watcher::new(mgr.clone());
        *self.wat.lock().unwrap() = Some(wat);
        {
            let me = Arc::clone(self);
            *share::VIEWER.lock().unwrap() = Some(Arc::new(move || me.viewer()));
        }
        {
            let mgr2 = mgr.clone();
            tokio::spawn(async move { crate::xfer::upload::clean_up(mgr2).await });
        }
        *self.started.lock().unwrap() = Some(Instant::now());
        self.open_at_login.store(crate::login::enabled(), Ordering::Relaxed);
        // Requests that came in while the app was starting stay queued for the
        // window to take (`take_service_requests`); `start` tells it they're there.
        crate::services::start(Arc::clone(self) as Arc<dyn crate::services::ServiceHost>);
        self.init_updates();
        // Portal's poller: idle until Portal is switched on in the settings.
        self.refresh_portal_credential();
        portal::agent::spawn(Arc::clone(self));
    }

    fn viewer(&self) -> Option<(String, String)> {
        let me = self.cached_user();
        me.map(|u| (u.name, u.email))
    }

    /// Posts a notification for the jobs that ran since the last one.
    fn on_snapshot(&self, s: Snapshot) {
        static BUSY: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);
        let mut guard = BUSY.lock().unwrap();
        let busy = guard.get_or_insert_with(HashMap::new);
        let mut active = false;
        for j in &s.jobs {
            if matches!(j.status, Status::Running | Status::Resolving | Status::Queued) {
                busy.insert(j.id.clone(), true);
                active = true;
            }
        }
        if !active && !busy.is_empty() {
            self.notify_finished(&s, busy);
            busy.clear();
        }
    }

    fn notify_finished(&self, s: &Snapshot, ran: &HashMap<String, bool>) {
        let (mut ok, mut failed) = (0, 0);
        for j in &s.jobs {
            if !ran.contains_key(&j.id) {
                continue;
            }
            match j.status {
                Status::Failed | Status::Partial => failed += 1,
                Status::Done if !j.watch => ok += 1,
                _ => {}
            }
        }
        if ok + failed == 0 {
            return;
        }
        let msg = if failed > 0 {
            format!("{} transfer(s) with errors", failed)
        } else {
            "Transfers complete".to_string()
        };
        crate::notify::notify("FioTransfer", &msg, Some("Glass"));
    }

    // ---------- Updates ----------

    fn init_updates(self: &Arc<Self>) {
        {
            let mut g = self.upd.lock().unwrap();
            g.st.current = crate::VERSION.to_string();
            g.st.state = "idle".into();
        }
        if !update::released(crate::VERSION) {
            self.upd.lock().unwrap().st.blocker = "development build".into();
            return;
        }
        let exe = std::env::current_exe().unwrap_or_default();
        let app = update::bundle_path(&exe);
        match app {
            Ok(app) => {
                self.upd.lock().unwrap().app = Some(app.to_string_lossy().into_owned());
                update::cleanup(&app);
                if let Err(e) = update::can_replace(&app) {
                    self.upd.lock().unwrap().st.blocker = e;
                }
            }
            Err(e) => self.upd.lock().unwrap().st.blocker = e,
        }
        let me = Arc::clone(self);
        tokio::spawn(async move { me.update_loop().await });
    }

    async fn update_loop(self: Arc<Self>) {
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            if !self.settings().no_update_check {
                let _ = self.check_update().await;
            }
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
        }
    }

    pub fn update_status(&self) -> UpdateStatus {
        self.upd.lock().unwrap().st.clone()
    }

    pub async fn check_for_update(&self) -> UpdateStatus {
        if !update::released(crate::VERSION) {
            return self.update_status();
        }
        self.check_update().await
    }

    async fn check_update(&self) -> UpdateStatus {
        {
            let mut g = self.upd.lock().unwrap();
            if g.st.state != "idle" {
                return g.st.clone();
            }
            g.st.state = "checking".into();
            g.st.error.clear();
        }
        self.emit_update(true);
        let cx = CancellationToken::new();
        let src = update::Source::new(&format!("FioTransfer/{}", crate::VERSION));
        let res = tokio::time::timeout(Duration::from_secs(30), src.latest(&cx)).await;
        // The card says one sentence either way; the reason goes to the log.
        let (rel, err): (Option<update::Release>, Option<String>) = match res {
            Ok(Ok(r)) => (r, None),
            Ok(Err(e)) => (None, Some(e)),
            Err(_) => (None, Some("the check took too long".to_string())),
        };
        let st = {
            let mut g = self.upd.lock().unwrap();
            g.st.state = "idle".into();
            g.st.checked_at = crate::xfer::now_ms();
            if err.is_some() {
                g.st.error = "Couldn't check for updates".into();
            }
            if let Some(r) = rel {
                g.st.latest = r.version.clone();
                g.st.url = r.url.clone();
                g.st.available = update::newer(&r.version, crate::VERSION);
                g.rel = Some(r);
            }
            g.st.clone()
        };
        if let Some(why) = err {
            if let Some(h) = self.hist() {
                h.log(&format!("UPDATE   check failed: {}", why));
            }
        }
        self.emit_update(true);
        if st.available {
            self.notify_update(&st.latest);
        }
        st
    }

    fn emit_update(&self, force: bool) {
        let st = {
            let mut g = self.upd.lock().unwrap();
            if !force && g.last_emit.map(|t| t.elapsed() < Duration::from_millis(200)).unwrap_or(false) {
                return;
            }
            g.last_emit = Some(Instant::now());
            g.st.clone()
        };
        self.emit("update", serde_json::to_value(&st).unwrap_or(Value::Null));
    }

    fn notify_update(&self, v: &str) {
        if self.settings().update_notified == v {
            return;
        }
        if let Ok(saved) = config::update(|s| s.update_notified = v.to_string()) {
            *self.settings.lock().unwrap() = saved;
        }
        crate::notify::notify("FioTransfer", &format!("Version {} is available", v), None);
    }

    pub async fn install_update(self: &Arc<Self>) -> Result<(), String> {
        let (rel, app, st) = {
            let g = self.upd.lock().unwrap();
            (g.rel.clone(), g.app.clone(), g.st.clone())
        };
        if !st.blocker.is_empty() {
            return Err(st.blocker);
        }
        if st.state == "downloading" || st.state == "restarting" {
            return Ok(());
        }
        let Some(rel) = rel else { return Err("no update available".into()) };
        if !update::newer(&rel.version, crate::VERSION) {
            return Err("no update available".into());
        }
        if self.mgr().map(|m| m.active()).unwrap_or(false) {
            let go = self.question(
                "Transfers in progress",
                "Updating restarts FioTransfer and stops running transfers. You can retry them from History.",
                "Update",
                "Cancel",
            );
            if !go {
                return Ok(());
            }
        }
        {
            let mut g = self.upd.lock().unwrap();
            if g.st.state != "idle" {
                return Ok(());
            }
            g.st.state = "downloading".into();
            g.st.progress = 0.0;
            g.st.error.clear();
        }
        self.emit_update(true);
        let app = app.unwrap_or_default();
        if let Some(h) = self.hist() {
            h.log(&format!("UPDATE   installing {} over {} at {}", rel.version, crate::VERSION, app));
        }
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let fail = |msg: String| {
                let mut g = me.upd.lock().unwrap();
                g.st.state = "idle".into();
                g.st.error = msg.clone();
                drop(g);
                me.emit_update(true);
                if let Some(h) = me.hist() {
                    h.log(&format!("UPDATE   {}", msg));
                }
            };
            let src = update::Source::new(&format!("FioTransfer/{}", crate::VERSION));
            let cx = CancellationToken::new();
            let progress = {
                let me = me.clone();
                Arc::new(move |p: f64| {
                    me.upd.lock().unwrap().st.progress = p;
                    me.emit_update(false);
                })
            };
            let old = match src.install(&cx, &rel, std::path::Path::new(&app), progress).await {
                Ok(o) => o,
                Err(e) => {
                    fail(format!("Update failed: {}", e));
                    return;
                }
            };
            if let Err(e) = update::relaunch(std::process::id(), std::path::Path::new(&app), &old) {
                fail(format!("Installed. Quit and reopen FioTransfer to finish. ({})", e));
                return;
            }
            me.upd.lock().unwrap().st.state = "restarting".into();
            me.emit_update(true);
            me.updating.store(true, Ordering::Relaxed);
            me.quit();
        });
        Ok(())
    }

    // ---------- State ----------

    pub fn get_state(&self) -> State {
        let s = self.settings();
        let st = State {
            version: crate::VERSION.to_string(),
            logged_in: self.auth.logged_in(),
            settings: s.clone(),
            redirect_uri: crate::auth::redirect_uri(s.redirect_port.max(1) as u16),
            log_dir: config::log_dir().to_string_lossy().into_owned(),
            log_file: self.hist().map(|h| h.log_path()).unwrap_or_default(),
            home: config::home_dir().to_string_lossy().into_owned(),
            open_at_login: crate::login::enabled(),
            user: None,
        };
        let mut st = st;
        if st.logged_in {
            st.user = self.cached_user();
        }
        st
    }

    /// The signed-in user without waiting on the network. If it isn't known
    /// yet it is fetched in the background, and the UI is told to refresh when
    /// the answer is in; a dead session is cleared then.
    fn cached_user(&self) -> Option<api::User> {
        let known = self.me.lock().unwrap().clone();
        let fetch = {
            let mut tried = self.me_tried.lock().unwrap();
            let stale = tried.map(|t| t.elapsed() > Duration::from_secs(30)).unwrap_or(true);
            if known.is_none() && !self.me_fetching.load(Ordering::Relaxed) && stale {
                self.me_fetching.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        };
        if fetch {
            let me = self.handle();
            tokio::spawn(async move {
                me.fetch_user().await;
                me.emit("state-changed", json!(null));
            });
        }
        known
    }

    /// A handle to this app for background work (the App lives in an `Arc`).
    pub fn handle(&self) -> Arc<App> {
        self.me_self.lock().unwrap().upgrade().expect("app dropped")
    }

    /// The running app, for the AppKit callbacks that have no other handle.
    pub fn global() -> Option<Arc<App>> {
        GLOBAL.get().and_then(|g| g.lock().unwrap().upgrade())
    }

    async fn fetch_user(&self) -> Option<api::User> {
        let cx = CancellationToken::new();
        let res = tokio::time::timeout(Duration::from_secs(15), self.api.me(&cx)).await;
        self.me_fetching.store(false, Ordering::Relaxed);
        *self.me_tried.lock().unwrap() = Some(Instant::now());
        match res {
            Ok(Ok(u)) => {
                *self.me.lock().unwrap() = Some(u.clone());
                Some(u)
            }
            _ => None,
        }
    }

    fn forget_user(&self) {
        *self.me.lock().unwrap() = None;
        *self.me_tried.lock().unwrap() = None;
    }

    pub fn save_settings(&self, s: Settings) -> Result<State, String> {
        let saved = config::update(|cur| {
            cur.client_id = s.client_id.trim().to_string();
            cur.redirect_port = s.redirect_port;
            cur.streams = s.streams;
            cur.segment_mb = s.segment_mb;
            cur.download_dir = s.download_dir.clone();
            cur.no_folder_logs = s.no_folder_logs;
            cur.no_update_check = s.no_update_check;
            // Portal: the non-secret settings only (the token is never here).
            cur.portal_url = s.portal_url.trim().trim_end_matches('/').to_string();
            cur.portal_root = s.portal_root.clone();
            cur.portal_enabled = s.portal_enabled;
            cur.portal_on_change = s.portal_on_change.clone();
            cur.portal_every_secs = s.portal_every_secs;
            cur.portal_email = s.portal_email.trim().to_string();
        })
        .map_err(|e| e.to_string())?;
        *self.settings.lock().unwrap() = saved.clone();
        if let Some(m) = self.mgr() {
            m.set_streams(saved.streams);
        }
        Ok(self.get_state())
    }

    /// Signs in through the browser. Clicking Sign in again restarts it.
    pub async fn login(self: &Arc<Self>) -> Result<State, String> {
        if let Some(cx) = self.login_cancel.lock().unwrap().take() {
            cx.cancel();
        }
        let cx = CancellationToken::new();
        *self.login_cancel.lock().unwrap() = Some(cx.clone());
        let _guard = self.login_lock.lock().await;
        if cx.is_cancelled() {
            return Err(crate::auth::ERR_LOGIN_CANCELED.to_string());
        }
        let s = self.settings();
        let open = |u: &str| {
            let _ = crate::osopen::open(u);
        };
        self.auth
            .login(&cx, s.redirect_port.max(1) as u16, open)
            .await
            .map_err(|e| e)?;
        self.forget_user();
        self.show_window();
        if self.fetch_user().await.is_none() {
            return Err("Frame.io rejected the Adobe sign-in (is the Frame.io API added to the Adobe project?)".into());
        }
        Ok(self.get_state())
    }

    pub fn logout(&self) -> State {
        self.auth.logout();
        self.forget_user();
        self.get_state()
    }

    // ---------- Frame.io browsing ----------

    fn remember(&self, ns: &[Node]) {
        let mut g = self.names.lock().unwrap();
        for n in ns {
            g.insert(n.id.clone(), n.name.clone());
        }
    }

    fn known_name(&self, id: &str) -> Option<String> {
        self.names.lock().unwrap().get(id).cloned()
    }

    pub async fn list_accounts(&self) -> Result<Vec<Node>, String> {
        let cx = self.api_cx();
        let accts = self.api.accounts(&cx).await.map_err(|e| e.to_string())?;
        let out: Vec<Node> = accts
            .iter()
            .map(|a| Node { id: a.id.clone(), name: a.display_name.clone(), kind: "account".into(), ..Default::default() })
            .collect();
        self.remember(&out);
        Ok(out)
    }

    pub async fn list_workspaces(&self, acct: &str) -> Result<Vec<Node>, String> {
        let cx = self.api_cx();
        let ws = self.api.workspaces(&cx, acct).await.map_err(|e| e.to_string())?;
        let mut out: Vec<Node> = ws
            .iter()
            .map(|w| Node { id: w.id.clone(), name: w.name.clone(), kind: "workspace".into(), ..Default::default() })
            .collect();
        out.sort_by_key(|n| n.name.to_lowercase());
        self.remember(&out);
        Ok(out)
    }

    pub async fn list_projects(&self, acct: &str, ws: &str) -> Result<Vec<Node>, String> {
        let cx = self.api_cx();
        let ps = self.api.projects(&cx, acct, ws).await.map_err(|e| e.to_string())?;
        let out: Vec<Node> = ps
            .iter()
            .map(|p| Node {
                id: p.id.clone(),
                name: p.name.clone(),
                kind: "project".into(),
                folder_id: p.root_folder_id.clone(),
                where_: String::new(),
            })
            .collect();
        self.remember(&out);
        Ok(out)
    }

    pub async fn list_folders(&self, acct: &str, folder: &str) -> Result<Vec<Node>, String> {
        let cx = self.api_cx();
        let kids = self.api.children(&cx, acct, folder).await.map_err(|e| e.to_string())?;
        Ok(kids
            .iter()
            .filter(|k| k.type_ == "folder")
            .map(|k| Node { id: k.id.clone(), name: k.name.clone(), kind: "folder".into(), folder_id: k.id.clone(), where_: String::new() })
            .collect())
    }

    pub async fn create_folder(&self, acct: &str, parent: &str, name: &str) -> Result<Node, String> {
        let cx = self.api_cx();
        let f = self.api.create_folder(&cx, acct, parent, name.trim()).await.map_err(|e| e.to_string())?;
        Ok(Node { id: f.id.clone(), name: f.name.clone(), kind: "folder".into(), folder_id: f.id, where_: String::new() })
    }

    /// The picker path down to a project or folder.
    pub async fn path_to(&self, acct: &str, kind: &str, id: &str) -> Result<Vec<Node>, String> {
        let cx = self.api_cx();
        let mut folders: Vec<Node> = Vec::new();
        let mut project = id.to_string();
        if kind == "folder" {
            let mut f = self.api.folder(&cx, acct, id).await.map_err(|e| gone(&e))?;
            while !f.parent_id.is_empty() {
                if folders.len() == 64 {
                    return Err("folder is nested too deep".into());
                }
                folders.insert(
                    0,
                    Node { id: f.id.clone(), name: f.name.clone(), kind: "folder".into(), folder_id: f.id.clone(), where_: String::new() },
                );
                let next = f.parent_id.clone();
                // As in the Go app: a failed step up the chain is an error, not a
                // shorter path (the UI would show a folder under the wrong parent).
                f = self.api.folder(&cx, acct, &next).await.map_err(|e| gone(&e))?;
            }
            project = f.project_id.clone();
        }
        let p = self.api.project(&cx, acct, &project).await.map_err(|e| gone(&e))?;
        self.remember(&[Node { id: p.id.clone(), name: p.name.clone(), kind: "project".into(), ..Default::default() }]);
        let acct_name = self.account_name(&cx, acct).await;
        let ws_name = self.workspace_name(&cx, acct, &p.workspace_id).await;
        let mut path = vec![
            Node { id: acct.to_string(), name: acct_name, kind: "account".into(), ..Default::default() },
            Node { id: p.workspace_id.clone(), name: ws_name, kind: "workspace".into(), ..Default::default() },
            Node { id: p.id.clone(), name: p.name.clone(), kind: "project".into(), folder_id: p.root_folder_id.clone(), where_: String::new() },
        ];
        path.extend(folders);
        Ok(path)
    }

    async fn account_name(&self, cx: &CancellationToken, acct: &str) -> String {
        if let Some(n) = self.known_name(acct) {
            return n;
        }
        if let Ok(accts) = self.api.accounts(cx).await {
            let ns: Vec<Node> = accts
                .iter()
                .map(|a| Node { id: a.id.clone(), name: a.display_name.clone(), kind: "account".into(), ..Default::default() })
                .collect();
            self.remember(&ns);
        }
        self.known_name(acct).unwrap_or_else(|| "Account".into())
    }

    async fn workspace_name(&self, cx: &CancellationToken, acct: &str, ws: &str) -> String {
        if let Some(n) = self.known_name(ws) {
            return n;
        }
        if let Ok(list) = self.api.workspaces(cx, acct).await {
            let ns: Vec<Node> = list
                .iter()
                .map(|w| Node { id: w.id.clone(), name: w.name.clone(), kind: "workspace".into(), ..Default::default() })
                .collect();
            self.remember(&ns);
        }
        self.known_name(ws).unwrap_or_else(|| "Workspace".into())
    }

    /// Finds projects and folders by name anywhere in an account.
    pub async fn search(&self, acct: &str, query: &str) -> Result<Vec<Node>, String> {
        let query = query.trim().to_string();
        if acct.is_empty() || query.chars().count() < 2 {
            return Ok(Vec::new());
        }
        let cx = self.api_cx();
        let hits = self.api.search(&cx, acct, &query, 50).await.map_err(|e| e.to_string())?;
        let mut out: Vec<Node> = Vec::new();
        let mut projects: Vec<String> = Vec::new();
        for h in &hits {
            if !h.name_match() {
                continue;
            }
            let r = &h.result;
            match h.type_.as_str() {
                "project_result" => {
                    self.remember(&[Node { id: r.id.clone(), name: r.name.clone(), kind: "project".into(), ..Default::default() }]);
                    out.push(Node {
                        id: r.id.clone(),
                        name: r.name.clone(),
                        kind: "project".into(),
                        folder_id: r.root_folder_id.clone(),
                        where_: String::new(),
                    });
                    projects.push(String::new());
                }
                "folder_result" => {
                    if r.parent_id.is_empty() {
                        continue;
                    }
                    out.push(Node { id: r.id.clone(), name: r.name.clone(), kind: "folder".into(), folder_id: r.id.clone(), where_: String::new() });
                    projects.push(r.project_id.clone());
                }
                _ => {}
            }
        }
        let names = self.project_names(&cx, acct, &projects).await;
        for (i, p) in projects.iter().enumerate() {
            out[i].where_ = names.get(p).cloned().unwrap_or_default();
        }
        Ok(out)
    }

    async fn project_names(&self, cx: &CancellationToken, acct: &str, ids: &[String]) -> HashMap<String, String> {
        let lookup = |me: &App| -> (HashMap<String, String>, bool) {
            let mut out = HashMap::new();
            let mut missing = false;
            let g = me.names.lock().unwrap();
            for id in ids {
                match g.get(id) {
                    Some(n) => {
                        out.insert(id.clone(), n.clone());
                    }
                    None => missing = true,
                }
            }
            let mut listed = me.projects_listed.lock().unwrap();
            if missing {
                if listed.map(|t| t.elapsed() < Duration::from_secs(60)).unwrap_or(false) {
                    missing = false;
                } else {
                    *listed = Some(Instant::now());
                }
            }
            (out, missing)
        };
        let (names, missing) = lookup(self);
        if !missing {
            return names;
        }
        let Ok(ws) = self.api.workspaces(cx, acct).await else { return names };
        for w in ws {
            self.remember(&[Node { id: w.id.clone(), name: w.name.clone(), kind: "workspace".into(), ..Default::default() }]);
            if let Ok(ps) = self.api.projects(cx, acct, &w.id).await {
                let ns: Vec<Node> = ps
                    .iter()
                    .map(|p| Node { id: p.id.clone(), name: p.name.clone(), kind: "project".into(), ..Default::default() })
                    .collect();
                self.remember(&ns);
            }
        }
        lookup(self).0
    }

    pub fn set_upload_destination(&self, acct: &str, folder: &str, label: &str, path: Vec<Crumb>) -> Result<State, String> {
        let saved = config::update(|s| {
            s.last_account_id = acct.to_string();
            s.last_folder_id = folder.to_string();
            s.last_folder_label = label.to_string();
            s.last_folder_path = path.clone();
        })
        .map_err(|e| e.to_string())?;
        *self.settings.lock().unwrap() = saved;
        Ok(self.get_state())
    }

    // ---------- Dialogs ----------

    pub fn pick_files(&self) -> Vec<String> {
        match self.host() {
            Some(h) => h.open_files("Choose files to upload"),
            None => Vec::new(),
        }
    }

    pub fn pick_folder(&self) -> Option<String> {
        self.host().and_then(|h| h.open_dir("Choose a folder to upload", "", false))
    }

    pub fn pick_watch_folder(&self) -> Option<String> {
        self.host().and_then(|h| h.open_dir("Choose a folder to watch", "", true))
    }

    pub fn pick_download_dir(&self, current: &str) -> Option<String> {
        let cur = if current.is_empty() { self.settings().download_dir } else { current.to_string() };
        self.host().and_then(|h| h.open_dir("Download to…", &cur, true))
    }

    // ---------- Transfers ----------

    pub fn start_upload(&self, paths: Vec<String>) -> Result<String, String> {
        let s = self.settings();
        let Some(m) = self.mgr() else { return Err("transfers aren't running".into()) };
        let j = m.start_upload(UploadOptions {
            paths,
            account_id: s.last_account_id,
            folder_id: s.last_folder_id,
            dest_label: s.last_folder_label,
        })?;
        Ok(j.id.clone())
    }

    pub async fn inspect_link(&self, link: &str, passphrase: &str) -> LinkInfo {
        let Some(m) = self.mgr() else { return LinkInfo::default() };
        let cx = CancellationToken::new();
        let _ = &cx;
        match tokio::time::timeout(Duration::from_secs(90), m.inspect_link(&cx, link, passphrase)).await {
            Ok(li) => li,
            Err(_) => LinkInfo { error: "timed out resolving the link".into(), ..Default::default() },
        }
    }

    pub fn start_download(&self, o: DownloadOptions) -> Result<String, String> {
        let Some(m) = self.mgr() else { return Err("transfers aren't running".into()) };
        let mut o = o;
        if o.dest_dir.is_empty() {
            o.dest_dir = self.settings().download_dir;
        }
        let j = m.start_download(o)?;
        Ok(j.id.clone())
    }

    /// A public Frame.io share link to what a transfer moved, making the share
    /// the first time it's asked for.
    pub async fn share_link(&self, id: &str) -> Result<String, String> {
        let Some(m) = self.mgr() else { return Err("transfers aren't running".into()) };
        let cx = self.api_cx();
        m.share_link(&cx, id).await
    }

    pub fn cancel(&self, id: &str) {
        if let Some(m) = self.mgr() {
            m.cancel(id);
        }
    }

    pub fn clear_finished(&self) {
        if let Some(m) = self.mgr() {
            m.clear_finished();
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.mgr().map(|m| m.snapshot()).unwrap_or_default()
    }

    pub fn job_files(&self, id: &str) -> Vec<xfer::FileView> {
        self.mgr().map(|m| m.files(id)).unwrap_or_default()
    }

    pub async fn retry(self: &Arc<Self>, id: &str, passphrase: &str) -> Result<String, String> {
        let Some(m) = self.mgr() else { return Err("transfers aren't running".into()) };
        let j = m.retry(id, passphrase).await?;
        Ok(j.map(|j| j.id.clone()).unwrap_or_default())
    }

    // ---------- History ----------

    pub fn history(&self, query: &str, kind: &str, limit: i64) -> Result<Vec<history::Transfer>, String> {
        let Some(h) = self.hist() else { return Ok(Vec::new()) };
        h.list(query, kind, limit)
    }

    pub fn history_files(&self, id: &str) -> Result<Vec<history::File>, String> {
        let Some(h) = self.hist() else { return Ok(Vec::new()) };
        h.files(id)
    }

    pub fn export_history(&self) -> Result<String, String> {
        let name = format!("FioTransfer history {}.csv", today());
        let Some(mut p) = self.host().and_then(|h| h.save_file("Export transfer history", &name)) else {
            return Ok(String::new());
        };
        if !p.to_lowercase().ends_with(".csv") {
            p.push_str(".csv");
        }
        let Some(h) = self.hist() else { return Err("history isn't open".into()) };
        h.export_csv(std::path::Path::new(&p))?;
        Ok(p)
    }

    // ---------- Finder / browser ----------

    pub fn reveal(&self, path: &str) -> Result<(), String> {
        let mut p = std::path::PathBuf::from(path);
        loop {
            if p.exists() {
                let _ = crate::osopen::reveal(&p.to_string_lossy());
                return Ok(());
            }
            match p.parent() {
                Some(parent) if parent != std::path::Path::new("/") => p = parent.to_path_buf(),
                _ => break,
            }
        }
        Err(format!("{} no longer exists", path))
    }

    pub fn open_url(&self, url: &str) {
        let _ = crate::osopen::open(url);
    }

    pub fn open_logs(&self) -> Result<(), String> {
        crate::osopen::open(&config::log_dir().to_string_lossy()).map_err(|e| e.to_string())
    }

    pub fn clipboard_text(&self) -> String {
        match self.host() {
            Some(h) => h.clipboard_text(),
            None => String::new(),
        }
    }

    pub fn clipboard_set_text(&self, text: &str) {
        if let Some(h) = self.host() {
            h.clipboard_set_text(text);
        }
    }

    // ---------- Watch folders ----------

    pub fn watches(&self) -> Vec<xfer::watch::WatchView> {
        self.wat.lock().unwrap().as_ref().map(|w| w.views()).unwrap_or_default()
    }

    pub fn add_watch(&self, local: &str, acct: &str, folder: &str, label: &str, dir: &str, on_change: &str, include_existing: bool) -> Result<(), String> {
        let Some(w) = self.wat.lock().unwrap().clone() else { return Err("watch folders aren't running".into()) };
        w.add(local, acct, folder, label, dir, on_change, include_existing)
    }

    pub fn set_watch_enabled(&self, id: &str, on: bool) {
        if let Some(w) = self.wat.lock().unwrap().as_ref() {
            w.set_enabled(id, on);
        }
    }

    pub fn remove_watch(&self, id: &str) {
        if let Some(w) = self.wat.lock().unwrap().as_ref() {
            w.remove(id);
        }
    }

    pub fn set_open_at_login(&self, on: bool) -> Result<State, String> {
        let err = crate::login::set(on);
        self.open_at_login.store(crate::login::enabled(), Ordering::Relaxed);
        err?;
        Ok(self.get_state())
    }

    pub fn update_watch(&self, id: &str, local: &str, acct: &str, folder: &str, label: &str, dir: &str, on_change: &str, include_existing: bool) -> Result<(), String> {
        let Some(w) = self.wat.lock().unwrap().clone() else { return Err("watch folders aren't running".into()) };
        w.update(id, local, acct, folder, label, dir, on_change, include_existing)
    }

    pub fn requeue_canceled(&self, id: &str) {
        if let Some(w) = self.wat.lock().unwrap().as_ref() {
            w.requeue_canceled(id);
        }
    }

    pub fn take_service_requests(&self) -> Vec<ServiceRequest> {
        crate::services::take()
    }

    // ---------- Portal ----------

    /// Re-reads the stored credential: the `signed_in` flag and the admin email
    /// the status line shows. The Keychain is read here and nowhere a frame is
    /// drawn — at start, when a credential is stored or forgotten, and at every
    /// reconcile.
    fn refresh_portal_credential(&self) {
        let session = portal::session();
        self.portal_signed_in
            .store(portal::token().is_some() || session.is_some(), Ordering::Relaxed);
        *self.portal_email.lock().unwrap() = session.map(|s| s.email).unwrap_or_default();
    }

    /// Whether Portal is on, which credential is stored, and how the last
    /// reconcile went. No network and no Keychain read: the numbers are the last
    /// run's, and the credential is the cached one.
    ///
    /// `needs_code` is the exception that is not cached: it is whether a
    /// password step is waiting for its one-time code. Nothing about that
    /// half-finished sign-in is a credential, so it never shows as signed in.
    pub fn portal_status(&self) -> portal::agent::PortalStatus {
        let s = self.settings();
        let last = self.portal_last.lock().unwrap().clone().unwrap_or_default();
        portal::agent::PortalStatus {
            enabled: s.portal_enabled,
            signed_in: self.portal_signed_in.load(Ordering::Relaxed),
            needs_code: self.portal_pending.lock().unwrap().is_some(),
            email: self.portal_email.lock().unwrap().clone(),
            url: s.portal_url.clone(),
            root: s.portal_root_path().to_string_lossy().into_owned(),
            running: self.portal_running.load(Ordering::Relaxed),
            last_at: last.at,
            links_seen: last.links_seen,
            ensured: last.ensured,
            removed: last.removed,
            skipped: last.skipped.len(),
            error: last.error,
        }
    }

    /// Reconciles the watch folders with Portal now. One at a time: a call while
    /// one is running answers so and changes nothing.
    pub async fn portal_reconcile(self: &Arc<Self>) -> portal::agent::Report {
        if self.portal_running.swap(true, Ordering::SeqCst) {
            return portal::agent::Report {
                at: xfer::now_ms(),
                error: "a Portal reconcile is already running".to_string(),
                ..Default::default()
            };
        }
        let rep = portal::agent::reconcile(self).await;
        self.refresh_portal_credential();
        *self.portal_last.lock().unwrap() = Some(rep.clone());
        self.portal_running.store(false, Ordering::SeqCst);
        self.emit("portal", serde_json::to_value(&rep).unwrap_or(Value::Null));
        if rep.ensured + rep.removed > 0 {
            // The watch list the front end is drawing is out of date.
            self.state_changed();
        }
        rep
    }

    /// Stores Portal's address, its bearer token and the local root, and turns
    /// the poller on or off. An empty token keeps the one already stored: the
    /// field is masked in the UI, so an untouched field comes back empty.
    pub fn set_portal(&self, url: &str, token: &str, root: &str, enabled: bool) -> Result<portal::agent::PortalStatus, String> {
        let url = url.trim().trim_end_matches('/').to_string();
        if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("the Portal URL has to start with http:// or https://".to_string());
        }
        let tok = token.trim();
        if !tok.is_empty() {
            portal::set_token(tok)?;
            self.refresh_portal_credential();
        }
        let root = root.trim().to_string();
        let saved = config::update(|s| {
            s.portal_url = url.clone();
            if !root.is_empty() {
                s.portal_root = root.clone();
            }
            s.portal_enabled = enabled;
        })
        .map_err(|e| e.to_string())?;
        *self.settings.lock().unwrap() = saved;
        Ok(self.portal_status())
    }

    /// Forgets the stored bearer token, and the cached "a token is stored" flag
    /// with it, so the Settings screen's status line says so at once instead of
    /// after the next reconcile.
    pub fn forget_portal_token(&self) -> portal::agent::PortalStatus {
        portal::clear_token();
        self.refresh_portal_credential();
        self.portal_status()
    }

    /// Signs in to Portal with its own login (`POST /api/auth/login`), and keeps
    /// the session cookie Portal answers with — never the password, which is not
    /// written anywhere: it is this call's argument and nothing else.
    ///
    /// `portal_email` is saved so the field is prefilled next time; the admin's
    /// name is taken from `GET /api/auth/me` in preference to what was typed, so
    /// "Signed in as …" is Portal's own answer.
    pub async fn portal_sign_in(self: &Arc<Self>, email: &str, password: &str) -> Result<portal::agent::PortalStatus, String> {
        let s = self.settings();
        let url = s.portal_url.trim().to_string();
        if url.is_empty() {
            return Err("add the Portal address first".to_string());
        }
        let email = email.trim().to_string();
        if email.is_empty() {
            return Err("enter the admin email address".to_string());
        }
        if password.is_empty() {
            return Err("enter the password".to_string());
        }
        // A new password step replaces any half-finished one: Portal's pending
        // session is bound to the cookie this call is about to make.
        *self.portal_pending.lock().unwrap() = None;
        let client = match portal::PortalClient::signin(&url, &email, password).await {
            Ok(portal::SignIn::Done(c)) => c,
            // The password was right and the account has a second factor. The
            // client holds Portal's pending session, which finishes the sign-in
            // on the same cookie; it is kept here, in memory, and nothing is
            // stored until the code lands.
            Ok(portal::SignIn::Totp(c)) => {
                *self.portal_pending.lock().unwrap() = Some(c);
                self.emit("portal", serde_json::to_value(self.portal_status()).unwrap_or(Value::Null));
                return Err(portal::PortalErr::TotpRequired.to_string());
            }
            Err(e) => return Err(e.to_string()),
        };
        // Portal's own name for the account; the typed address is an acceptable
        // fallback when the read fails, and nothing is stored unless a cookie
        // came back.
        let email = match client.me().await {
            Ok(who) => who.email,
            Err(_) => email,
        };
        let session = client.session().ok_or_else(|| "the sign-in produced no session".to_string())?.named(&email);
        portal::save_session(&session)?;
        // Only the email changes. The file is not adopted wholesale: a
        // settings.json this app has not written holds defaults, and adopting it
        // would lose the address the user just signed in to.
        let saved = config::update(|st| st.portal_email = email.clone()).map_err(|e| e.to_string())?;
        self.settings.lock().unwrap().portal_email = saved.portal_email;
        self.refresh_portal_credential();
        self.emit("portal", serde_json::to_value(self.portal_status()).unwrap_or(Value::Null));
        Ok(self.portal_status())
    }

    /// Finishes a sign-in Portal asked a one-time code for: the same pending
    /// session, the six digits, and then the session Portal issues is kept
    /// exactly as a password sign-in keeps it.
    ///
    /// The code is this call's argument and nothing else: it is not stored, not
    /// written anywhere and not part of any answer. A refused code changes
    /// nothing — the pending step stays, so the next six digits can be tried —
    /// and this returns Portal's own sentence, which says whether the code was
    /// wrong or the step is over.
    pub async fn portal_sign_in_totp(self: &Arc<Self>, code: &str) -> Result<portal::agent::PortalStatus, String> {
        let Some(mut client) = self.portal_pending.lock().unwrap().clone() else {
            return Err(portal::PortalErr::NoPendingSignIn.to_string());
        };
        match client.login_totp(code).await {
            Ok(()) => {
                *self.portal_pending.lock().unwrap() = None;
                // Portal's own name for the account, which the code step read
                // from `/api/auth/me`; the typed address is the fallback and is
                // empty here, because `login_totp` keeps it.
                let email = client.session().map(|s| s.email.clone()).unwrap_or_default();
                let saved = config::update(|st| st.portal_email = email.clone()).map_err(|e| e.to_string())?;
                self.settings.lock().unwrap().portal_email = saved.portal_email;
                self.refresh_portal_credential();
                self.emit("portal", serde_json::to_value(self.portal_status()).unwrap_or(Value::Null));
                Ok(self.portal_status())
            }
            // A refusal stores nothing, but the pending step is kept: a wrong
            // code, or a Portal that could not be reached, is worth another
            // try on the same session. Only a finished sign-in, a sign-out or
            // a new password step drops it.
            Err(e) => {
                *self.portal_pending.lock().unwrap() = Some(client);
                Err(e.to_string())
            }
        }
    }

    /// Forgets the stored session (the cookie jar), and the cached "signed in"
    /// flag with it. A stored bearer token is a different credential under a
    /// different key, and stays. A sign-in still waiting for its one-time code
    /// goes too: signing out finishes with it.
    pub fn portal_sign_out(&self) -> portal::agent::PortalStatus {
        *self.portal_pending.lock().unwrap() = None;
        portal::clear_session();
        self.refresh_portal_credential();
        self.portal_status()
    }

    // ---------- Shutdown ----------

    pub fn shutdown(&self) {
        if let Some(w) = self.wat.lock().unwrap().take() {
            w.close();
        }
        if let Some(m) = self.mgr.lock().unwrap().take() {
            m.cancel_all();
            std::thread::sleep(Duration::from_millis(300));
        }
        if let Some(h) = self.hist.lock().unwrap().take() {
            h.close();
        }
    }
}

static GLOBAL: once_cell::sync::OnceCell<Mutex<std::sync::Weak<App>>> = once_cell::sync::OnceCell::new();

fn gone<E: std::fmt::Display>(e: &E) -> String {
    let text = e.to_string();
    if text.contains("404") {
        "no longer on Frame.io".to_string()
    } else {
        text
    }
}

fn today() -> String {
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let days = secs / 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------- Space: what Frame.io is holding, per project ----------

/// True when `id` can be pasted into exactly one path segment of a Frame.io URL
/// and address nothing else: non-empty, no separator, no percent escape (a
/// `%2e` is a dot by the time the route is parsed), no control character, and
/// neither of the two dot segments.
pub fn one_path_segment(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && !id.chars().any(|c| matches!(c, '/' | '\\' | '%') || c.is_control())
}

/// An id as a log line may carry it: control characters made visible, so a
/// newline in an id cannot forge a second line in the transfer log.
fn for_log(id: &str) -> String {
    id.chars().flat_map(|c| c.escape_debug().collect::<Vec<_>>()).collect()
}

/// One project's footprint on Frame.io.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SpaceProject {
    pub account_id: String,
    pub account_name: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub project_id: String,
    pub name: String,
    pub root_folder_id: String,
    pub bytes: i64,
    /// Files the walk counted. 0 means "not counted", which is what a row whose
    /// size Frame.io reported says, so the row shows a dash instead of a zero.
    pub files: i64,
    /// True when the figures came from walking the project's folders, false
    /// when Frame.io reported the size (`Project.storage`) and nothing was
    /// walked. The row says which: a walked figure and a reported one are not
    /// the same measurement, and only a walk counts files. A walk that fails
    /// part way is still `true`, with `error` saying why it is not whole.
    pub walked: bool,
    /// Set when the walk could not finish (permissions, a deleted folder). An
    /// errored row has no figure at all, whatever `walked` says.
    pub error: String,
}

/// One account's storage against its limit, straight from `Account`.
///
/// `limit` is `None` when Frame.io sends `storage_limit: null`, which means "no
/// limit" — the view prints that rather than a limit of zero. `used` is `None`
/// when the account reports no `storage_usage`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SpaceAccount {
    pub account_id: String,
    pub name: String,
    pub limit: Option<i64>,
    pub used: Option<i64>,
}

/// The Space payload: the rows to chart, and the per-account headline over
/// them. A struct rather than a bare list of projects, because "how much is
/// left" belongs to the account, not to any one project.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SpacePayload {
    pub projects: Vec<SpaceProject>,
    pub accounts: Vec<SpaceAccount>,
}

/// Orders a walk biggest-first. `sort_by` is stable, so two projects holding the
/// same number of bytes keep the order the walk found them in (account, then
/// workspace, then Frame.io's `name_asc` listing): the Space view's colour ramp
/// and its legend rows must not swap places between two identical walks.
pub fn order_space(rows: &mut [SpaceProject]) {
    rows.sort_by(|a, b| b.bytes.cmp(&a.bytes));
}

impl App {
    /// The last reading, if there is one. Cheap: the Space view calls this on
    /// every frame.
    pub fn space(&self) -> SpacePayload {
        self.space.lock().unwrap().as_ref().map(|(_, v)| v.clone()).unwrap_or_default()
    }

    pub fn space_walking(&self) -> bool {
        self.space_walking.load(Ordering::Relaxed)
    }

    /// Reads what Frame.io says each account and each project holds, and walks
    /// a project's folders only when Frame.io does not report its size. Reads
    /// only: nothing here deletes or downloads anything.
    ///
    /// The project list is one request per workspace and `Project.storage`
    /// comes with it, so a refresh that never falls back costs one request per
    /// account and per workspace rather than one per folder.
    pub async fn refresh_space(self: &Arc<Self>) {
        if self.space_walking.swap(true, Ordering::SeqCst) {
            return; // a refresh is already running
        }
        let cx = self.api_cx();
        let mut out: Vec<SpaceProject> = Vec::new();
        let mut quotas: Vec<SpaceAccount> = Vec::new();
        // An error here is the difference between "this account holds nothing"
        // and "Frame.io did not answer", so it is written down and the screen is
        // told the walk is over. Swallowing it — which this once did — left the
        // empty Space screen indistinguishable from an empty account.
        let accounts = match self.api.accounts(&cx).await {
            Ok(a) => a,
            Err(e) => {
                self.space_walking.store(false, Ordering::SeqCst);
                if let Some(h) = self.hist() {
                    h.log(&format!(
                        "SPACE    could not list the accounts, so the Space screen has nothing to show: {e}"
                    ));
                }
                self.state_changed();
                return;
            }
        };
        for acct in accounts {
            quotas.push(SpaceAccount {
                account_id: acct.id.clone(),
                name: acct.display_name.clone(),
                limit: acct.storage_limit,
                used: acct.storage_usage,
            });
            let workspaces = self.api.workspaces(&cx, &acct.id).await.unwrap_or_default();
            for ws in workspaces {
                let projects = self.api.projects(&cx, &acct.id, &ws.id).await.unwrap_or_default();
                for p in projects {
                    let mut row = SpaceProject {
                        account_id: acct.id.clone(),
                        account_name: acct.display_name.clone(),
                        workspace_id: ws.id.clone(),
                        workspace_name: ws.name.clone(),
                        project_id: p.id.clone(),
                        name: p.name.clone(),
                        root_folder_id: p.root_folder_id.clone(),
                        ..Default::default()
                    };
                    match p.storage {
                        // Frame.io reported it: take the number, and leave
                        // `files` at 0 ("not counted") rather than walk.
                        Some(bytes) => row.bytes = bytes,
                        // No figure from Frame.io: the walk is the fallback.
                        None => {
                            row.walked = true;
                            match self.walk_folder(&cx, &acct.id, &p.root_folder_id, 0).await {
                                Ok((bytes, files)) => {
                                    row.bytes = bytes;
                                    row.files = files;
                                }
                                Err(e) => row.error = e,
                            }
                        }
                    }
                    out.push(row);
                }
            }
        }
        order_space(&mut out);
        *self.space.lock().unwrap() = Some((Instant::now(), SpacePayload { projects: out, accounts: quotas }));
        self.space_walking.store(false, Ordering::SeqCst);
        self.state_changed();
    }

    /// Adds up one folder tree. Depth-capped so a cycle in Frame.io's data can
    /// not spin forever.
    async fn walk_folder(&self, cx: &CancellationToken, acct: &str, folder: &str, depth: usize) -> Result<(i64, i64), String> {
        if depth > 32 || folder.is_empty() {
            return Ok((0, 0));
        }
        let kids = self.api.children(cx, acct, folder).await.map_err(|e| e.to_string())?;
        let (mut bytes, mut files) = (0i64, 0i64);
        for k in kids {
            match k.type_.as_str() {
                "folder" => {
                    let (b, f) = Box::pin(self.walk_folder(cx, acct, &k.id, depth + 1)).await?;
                    bytes += b;
                    files += f;
                }
                "version_stack" => {
                    if let Some(head) = &k.head_version {
                        bytes += head.file_size;
                        files += 1;
                    }
                }
                _ => {
                    bytes += k.file_size;
                    files += 1;
                }
            }
        }
        Ok((bytes, files))
    }

    /// Deletes a project on Frame.io. This is the only destructive call the Space
    /// view makes, and it touches Frame.io alone: no local file is read, moved or
    /// removed, so a copy elsewhere (a `Live_Work` folder, a NAS, a watch folder)
    /// stays exactly as it is.
    pub async fn delete_project(&self, acct: &str, project: &str) -> Result<(), String> {
        // Both ids land in the URL as one path segment each, and both are
        // refused unless they are exactly that. Frame.io's ids are UUIDs, but
        // the id arrives through a bridge call and could be anything: `""`
        // leaves the collection route (`DELETE /v4/accounts/{a}/projects`),
        // `..` collapses to the account, `.` collapses to the collection,
        // `../files/x` reaches a *file* route, `/` and `\` add segments, and
        // `%2e` decodes back to a dot before the route is parsed — so encoding
        // is not a fix. Nothing is sent for an id like that, and the refusal is
        // logged rather than swallowed, with the id's control characters
        // escaped so it cannot become a second, forged log line.
        if !one_path_segment(acct) || !one_path_segment(project) {
            let why = format!(
                "refused to delete project {} on account {}: an id must be exactly one path segment",
                for_log(project),
                for_log(acct)
            );
            if let Some(h) = self.hist() {
                h.log(&format!(
                    "SPACE    {why} — nothing was sent, and nothing local was touched"
                ));
            }
            return Err(why);
        }
        let cx = self.api_cx();
        self.api.delete_project(&cx, acct, project).await.map_err(|e| e.to_string())?;
        if let Some(h) = self.hist() {
            h.log(&format!("SPACE    deleted project {} on Frame.io (nothing local was touched)", project));
        }
        if let Some((_, payload)) = self.space.lock().unwrap().as_mut() {
            // By (account, project): the same project id can exist in a second
            // account, and that account's row is a different project.
            payload
                .projects
                .retain(|p| !(p.account_id == acct && p.project_id == project));
        }
        self.state_changed();
        Ok(())
    }
}

// ---------- Finder services ----------

impl crate::services::ServiceHost for App {
    fn show_window(&self) {
        App::show_window(self);
    }

    fn emit_service(&self) {
        self.emit("service", serde_json::json!(null));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> SpacePayload {
        SpacePayload {
            projects: vec![
                // A row Frame.io sized, and a row the walk sized.
                SpaceProject {
                    project_id: "proj-reported".into(),
                    name: "Nightfall".into(),
                    bytes: 6_200_000_000,
                    files: 0,
                    walked: false,
                    ..Default::default()
                },
                SpaceProject {
                    project_id: "proj-walked".into(),
                    name: "Archive 2024".into(),
                    bytes: 2_100_000_000,
                    files: 212,
                    walked: true,
                    ..Default::default()
                },
            ],
            accounts: vec![
                SpaceAccount {
                    account_id: "acct-limited".into(),
                    name: "Farbhaus".into(),
                    limit: Some(2_000_000_000_000),
                    used: Some(12_400_000_000),
                },
                // `storage_limit: null`: an account with no limit.
                SpaceAccount {
                    account_id: "acct-unlimited".into(),
                    name: "Marketing".into(),
                    limit: None,
                    used: Some(1_050_000_000),
                },
            ],
        }
    }

    /// The payload the two front ends read: a project list and an account list,
    /// with the account's limit and usage as numbers or as null.
    #[test]
    fn the_space_payload_is_projects_and_accounts() {
        let v = serde_json::to_value(payload()).expect("payload serialises");
        let projects = v.get("projects").and_then(|p| p.as_array()).expect("projects is a list");
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0]["project_id"], "proj-reported");
        assert_eq!(projects[0]["bytes"], 6_200_000_000_i64);
        assert_eq!(projects[0]["walked"], false);
        assert_eq!(projects[1]["walked"], true);
        assert_eq!(projects[1]["files"], 212);

        let accounts = v.get("accounts").and_then(|a| a.as_array()).expect("accounts is a list");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0]["name"], "Farbhaus");
        assert_eq!(accounts[0]["limit"], 2_000_000_000_000_i64);
        assert_eq!(accounts[0]["used"], 12_400_000_000_i64);
        assert!(accounts[1]["limit"].is_null(), "no limit is null, not 0");
        assert_eq!(accounts[1]["used"], 1_050_000_000_i64);
    }

    /// An account Frame.io says nothing about still gets a row, with both
    /// figures null, so the view can say "not reported" rather than print 0 B.
    #[test]
    fn an_unreported_account_serialises_as_null() {
        let v = serde_json::to_value(SpaceAccount {
            account_id: "acct-quiet".into(),
            name: "Quiet".into(),
            limit: None,
            used: None,
        })
        .expect("account serialises");
        assert!(v["limit"].is_null());
        assert!(v["used"].is_null());
    }

    /// A payload that was never filled is empty in both lists, never a null the
    /// front ends would have to guard against.
    #[test]
    fn an_empty_payload_has_two_empty_lists() {
        let v = serde_json::to_value(SpacePayload::default()).expect("payload serialises");
        assert_eq!(v["projects"], serde_json::json!([]));
        assert_eq!(v["accounts"], serde_json::json!([]));
    }
}
