//! The app behind the window: `fiotransfer_core::App` driven by an egui frame.
//!
//! `crate::app::FioApp` draws; this runs. It owns the tokio runtime, hands the
//! app its [`Host`] (`crate::platform::DesktopHost`), drains the requests the
//! five screens pushed (`model::Request`), calls the app and puts the answers
//! back into [`UiState`]. Every call here is the same call the webview front end
//! made through `window.go.main.App[...]` — `tests/contract/app.js` is the
//! script it follows, and the wording of every toast is that script's.
//!
//! ## Who owns which field
//!
//! The app owns `logged_in`, `user`, `settings`, `snapshot`, `history`,
//! `watches`, `space`, `space_accounts`, `space_walking`, `destination`,
//! `version`, `open_at_login`, `log_dir`, `home` and `update`: they are read
//! back from it on every tick or event. The screen owns
//! `view`, `link`, `passphrase`, `new_folder`, `download_dir`, `expanded`,
//! `files`, `toasts` and `requests`: nothing here overwrites them from app state,
//! because a keystroke is not something the engine knows about. (`files` is the
//! one the shell fills — the app's file list for the expanded job is not state a
//! screen can ask for any other way — and only while a job is expanded.)
//!
//! ## Threads
//!
//! `pump` runs on the draw thread and only makes cheap, synchronous calls, with
//! the runtime entered so the app's own `tokio::spawn`s work. Anything that
//! awaits goes on the runtime (`spawn_task`); anything that can block — a panel,
//! the clipboard, the CSV saver — goes on the runtime's blocking pool
//! (`spawn_panel`), so a panel that is open for ten minutes costs no frame.
//! Answers come back over one `std::sync::mpsc` channel of [`Update`]s, which is
//! also what the `Host` uses for `emit`, `show_window` and `quit`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use fiotransfer_core::app::{App, State, UpdateStatus};
use fiotransfer_core::history;
use fiotransfer_core::host::Host;
use fiotransfer_core::portal::agent::PortalStatus;
use fiotransfer_core::xfer::{DownloadOptions, LinkInfo};

use crate::model::{
    fmt_bytes, in_block_order, Crumb, HistoryRow, LinkInfo as UiLinkInfo, PortalView, Request,
    ToastKind, UiState, UpdateState, View, WatchRow,
};
use crate::platform;

// ---------- Timing ----------

/// How often the app is read while something is transferring (10 fps of state).
pub const ACTIVE_TICK: Duration = Duration::from_millis(100);
/// ... and while nothing is: four reads a second, which is all an idle window
/// needs to stay honest about a watch folder picking something up.
pub const IDLE_TICK: Duration = Duration::from_millis(250);
/// The expanded job's file list, at the rate the webview asked for it.
const FILES_EVERY: Duration = Duration::from_millis(900);
/// How long this machine's UTC offset is trusted before it is asked again.
const ZONE_EVERY: Duration = Duration::from_secs(30 * 60);
/// `History(query, kind, 500)`, the limit the webview asked for.
const HISTORY_LIMIT: i64 = 500;

/// What the app, a background call, or the window tells the shell.
#[derive(Debug)]
pub enum Update {
    /// An event the app emitted: `state-changed`, `update`, `service`,
    /// `files-dropped` (and `menu` if a menu bar is ever wired up again).
    Event(String, Value),
    /// The app wants its window in front of the user.
    ShowWindow,
    /// The app is finished (an update installed, or the user asked): close.
    Quit,
    /// The answer to the question asked before quitting: `true` when it was
    /// `Quit`.
    QuitAnswer(bool),
    /// A background call's answer, applied by `pump`.
    Done(Done),
}

/// What a background call hands back.
#[derive(Debug)]
pub enum Done {
    /// Something in the app changed: read it again.
    Changed,
    /// A message for the user, in the app's own words.
    Toast { text: String, kind: ToastKind },
    /// `get_state`'s answer (`Login`, `Logout`, `SaveSettings`,
    /// `SetOpenAtLogin`).
    State(Box<State>),
    /// `InspectLink`'s answer, with the sequence it was asked under: only the
    /// newest link's answer may land.
    Link { seq: u64, info: Box<LinkInfo> },
    /// A `Login` that failed, with its sequence: a sign-in the user restarted
    /// must not report over the one they are waiting for.
    LoginFailed { seq: u64, error: String },
    /// `CheckForUpdate`'s answer.
    Update(Box<UpdateStatus>),
    /// A folder chosen for downloads; `None` when the panel was cancelled.
    DownloadDir(Option<String>),
    /// A folder chosen for the watch editor's local-folder field; `None` when the
    /// panel was cancelled.
    WatchFolder(Option<String>),
    /// `App::portal_sign_in`'s answer: the new status, or the sentence to show
    /// under the field. The password is not part of either.
    SignedIn(Result<Box<PortalStatus>, String>),
    /// `App::portal_sign_in_totp`'s answer: the new status, or Portal's sentence
    /// about the code. The code is not part of either.
    SignedInTotp(Result<Box<PortalStatus>, String>),
    /// The clipboard's text, for the composer's link field (`Paste`).
    Clipboard(String),
}

/// The app, its runtime and the channel the two talk over.
pub struct Shell {
    app: Arc<App>,
    /// `None` once the window has closed and the runtime has been stopped.
    rt: Option<tokio::runtime::Runtime>,
    host: Arc<dyn Host>,
    tx: Sender<Update>,
    rx: Receiver<Update>,
    /// One panel at a time: a second click while a panel is open is dropped.
    panel_open: Arc<AtomicBool>,
    /// The app has not been read yet: the first `pump` reads everything, the way
    /// the webview's `init()` called `GetState` before it drew anything.
    first: bool,
    /// When the app was last read.
    last_tick: Instant,
    /// Whether anything was transferring at the last read (100 ms or 250 ms).
    active: bool,
    /// The History screen's last filter, so a `state-changed` can re-run it.
    hist_query: String,
    hist_kind: String,
    /// Bumped for every `InspectLink`, so a slow answer cannot land over a newer
    /// one (the webview's `S.linkSeq`).
    link_seq: u64,
    /// The same for `Login`, whose first attempt is cancelled by the second.
    login_seq: u64,
    /// The expanded job whose files are in `UiState::files`, and when they were
    /// read.
    files_for: Option<String>,
    files_at: Option<Instant>,
    /// A Space walk has been started (or one was already running) since this
    /// window opened: the first visit starts the first walk, and only the first
    /// (`openSpace` in the webview).
    space_asked: bool,
    /// This machine's offset from UTC, and when it was asked.
    zone: i64,
    zone_at: Instant,
    /// The window should come to the front (`Host::show_window`).
    focus: bool,
    /// The window should close (`Host::quit`, or an answered question).
    close: bool,
    /// The close has been agreed: do not ask again.
    quit_ok: bool,
    /// A question is on screen; a second close request must not open another.
    asking: bool,
    /// `shutdown` has run.
    stopped: bool,
}

impl Shell {
    /// Builds the tokio runtime, hands the app its [`Host`], and starts the app's
    /// own background work (`App::start`: history, the transfer manager, the
    /// watch folders, the update loop, Finder's services).
    ///
    /// `state` seeds what the shell keeps between frames — the download folder a
    /// panel opens at — so the first frame and the first panel are right even
    /// before the first read from the app lands.
    pub fn start(state: UiState) -> Shell {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fiotransfer")
            .build()
            .expect("a tokio runtime for the app");
        let (tx, rx) = std::sync::mpsc::channel();
        let host: Arc<dyn Host> = Arc::new(platform::DesktopHost::new(tx.clone()));

        // The app's own settings, from disk: `state` is what a screen is showing,
        // and on a real launch it is the empty state.
        let app = App::new(fiotransfer_core::config::load());
        app.set_host(host.clone());

        let shell = Shell {
            app: app.clone(),
            rt: Some(rt),
            host,
            tx,
            rx,
            panel_open: Arc::new(AtomicBool::new(false)),
            first: true,
            last_tick: Instant::now(),
            active: false,
            hist_query: String::new(),
            hist_kind: String::new(),
            link_seq: 0,
            login_seq: 0,
            files_for: None,
            files_at: None,
            space_asked: false,
            zone: platform::local_offset_secs(),
            zone_at: Instant::now(),
            focus: false,
            close: false,
            quit_ok: false,
            asking: false,
            stopped: false,
        };
        // `state` decides nothing else: the first `pump` reads the real one.
        let _ = &state;

        // The app's startup owns history, the manager and the watcher; it must run
        // inside the runtime, and it must not hold the frame.
        let me = app.clone();
        if let Some(rt) = &shell.rt {
            rt.spawn(async move { me.start().await });
        }
        shell
    }

    // ---------- the frame ----------

    /// Once per frame, before drawing: drain the requests the screens pushed,
    /// apply what came back, and read the app on the tick.
    pub fn pump(&mut self, ui: &mut UiState) {
        if self.rt.is_none() {
            // The window is closing; nothing to drive.
            ui.requests.clear();
            return;
        }
        // The app's own work goes through `tokio::spawn`; entering the runtime
        // here is what lets a synchronous call on the draw thread make one. The
        // handle is taken first so the guard borrows it, not `self`.
        let handle = self.rt.as_ref().map(|rt| rt.handle().clone());
        let _entered = handle.as_ref().map(|handle| handle.enter());

        let requests = std::mem::take(&mut ui.requests);
        for request in requests {
            self.handle(request, ui);
        }
        while let Ok(update) = self.rx.try_recv() {
            self.apply(update, ui);
        }
        if self.first {
            // Everything, once, before the first frame is drawn: who is signed
            // in, the settings, the version, the queue and the watch folders. The
            // webview's `init()` did the same with `GetState` and `Snapshot`.
            self.first = false;
            self.read_all(ui);
        }
        let interval = if self.active { ACTIVE_TICK } else { IDLE_TICK };
        if self.last_tick.elapsed() >= interval {
            self.tick(ui);
        }
        self.drive_space(ui);
    }

    /// How long the window may wait before the next frame: the tick's interval,
    /// so a transfer's progress moves and an idle window costs four frames a
    /// second.
    pub fn repaint_in(&self) -> Duration {
        if self.active { ACTIVE_TICK } else { IDLE_TICK }
    }

    /// `true` once, when the app asked for its window in front of the user.
    pub fn take_focus(&mut self) -> bool {
        std::mem::take(&mut self.focus)
    }

    /// `true` once, when the window should close.
    pub fn take_close(&mut self) -> bool {
        std::mem::take(&mut self.close)
    }

    /// The window is being closed. Answers whether the close may go ahead now:
    /// when transfers are running it asks first, and closes on the next frame if
    /// the answer is `Quit` (`Update::Quit`).
    pub fn on_close_request(&mut self) -> bool {
        self.may_quit()
    }

    /// On window close: the app's `shutdown()` (watch folders, the manager, the
    /// history store) and then the runtime.
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.app.shutdown();
        if let Some(rt) = self.rt.take() {
            // Blocks only for the app's own 300 ms cancel-all, then stops the
            // workers; a panel still open on the blocking pool is left to the OS.
            rt.shutdown_timeout(Duration::from_millis(500));
        }
    }

    // ---------- what a screen asked for ----------

    fn handle(&mut self, request: Request, ui: &mut UiState) {
        match request {
            // ---- panels ----
            Request::PickFiles => {
                self.panel(|app, tx| {
                    let paths = app.pick_files();
                    if !paths.is_empty() {
                        if let Err(err) = app.start_upload(paths) {
                            toast(&tx, err, ToastKind::Err);
                        }
                    }
                    let _ = tx.send(Update::Done(Done::Changed));
                });
            }
            Request::PickFolder => {
                self.panel(|app, tx| {
                    if let Some(folder) = app.pick_folder() {
                        if let Err(err) = app.start_upload(vec![folder]) {
                            toast(&tx, err, ToastKind::Err);
                        }
                    }
                    let _ = tx.send(Update::Done(Done::Changed));
                });
            }
            // The watch editor's "Choose…": a folder panel, and the answer goes
            // into the dialog's local-folder field (`Done::WatchFolder`).
            Request::PickWatchFolder => {
                self.panel(|app, tx| {
                    let folder = app.pick_watch_folder();
                    let _ = tx.send(Update::Done(Done::WatchFolder(folder)));
                });
            }
            Request::PickDownloadDir => {
                let current = if ui.download_dir.is_empty() {
                    ui.settings.download_dir.clone()
                } else {
                    ui.download_dir.clone()
                };
                self.panel(move |app, tx| {
                    let folder = app.pick_download_dir(&current);
                    let _ = tx.send(Update::Done(Done::DownloadDir(folder)));
                });
            }

            // ---- transfers ----
            Request::StartUpload(paths) => match self.app.start_upload(paths) {
                Ok(_) => self.read_running(ui),
                Err(err) => ui.push_toast(err, ToastKind::Err),
            },
            Request::InspectLink { link, passphrase } => self.inspect(link, passphrase),
            Request::StartDownload { link, passphrase, dest_dir, subfolder } => {
                let options = DownloadOptions { link, passphrase, dest_dir, subfolder };
                match self.app.start_download(options) {
                    Ok(_) => {
                        // `startDownload` in the webview empties the composer and
                        // drops the preview; a stale `InspectLink` answer must not
                        // paint over the empty field either.
                        ui.link.clear();
                        ui.passphrase.clear();
                        ui.link_info = None;
                        self.link_seq += 1;
                        self.read_running(ui);
                    }
                    Err(err) => ui.push_toast(err, ToastKind::Err),
                }
            }
            Request::Cancel(id) => {
                self.app.cancel(&id);
                self.read_running(ui);
            }
            Request::ClearFinished => {
                self.app.clear_finished();
                self.read_running(ui);
            }
            Request::Retry(id) => {
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    match app.retry(&id, "").await {
                        // An empty id is a watch folder taking the files back.
                        Ok(new_id) if new_id.is_empty() => {
                            toast(&tx, "Its watch folder will retry these files", ToastKind::Ok);
                            let _ = tx.send(Update::Done(Done::Changed));
                        }
                        Ok(_) => {
                            let _ = tx.send(Update::Done(Done::Changed));
                        }
                        Err(err) => toast(&tx, err, ToastKind::Err),
                    }
                });
            }
            Request::ShareLink(id) => {
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    match app.share_link(&id).await {
                        Ok(link) => {
                            app.clipboard_set_text(&link);
                            toast(&tx, "Share link copied", ToastKind::Ok);
                        }
                        Err(err) => toast(&tx, err, ToastKind::Err),
                    }
                });
            }

            // ---- Finder and the browser ----
            Request::Reveal(path) => {
                if let Err(err) = self.app.reveal(&path) {
                    ui.push_toast(err, ToastKind::Err);
                }
            }
            Request::OpenUrl(url) => self.app.open_url(&url),
            Request::OpenLogs => {
                if let Err(err) = self.app.open_logs() {
                    ui.push_toast(err, ToastKind::Err);
                }
            }

            // ---- history ----
            Request::History { query, kind } => {
                self.hist_query = query;
                self.hist_kind = kind;
                self.read_history(ui);
            }
            Request::ExportCsv => {
                let home = ui.home.clone();
                self.panel(move |app, tx| match app.export_history() {
                    Ok(path) if !path.is_empty() => {
                        toast(&tx, format!("Exported to {}", short_path(&home, &path)), ToastKind::Ok)
                    }
                    Ok(_) => {}
                    Err(err) => toast(&tx, err, ToastKind::Err),
                });
            }

            // ---- the clipboard ----
            Request::CopyText(text) => self.app.clipboard_set_text(&text),
            Request::Paste => {
                // `ClipboardText`, trimmed, then `InspectLink` — the webview's
                // `paste-btn` handler.
                self.spawn_blocking(|app, tx| {
                    let text = app.clipboard_text().trim().to_string();
                    let _ = tx.send(Update::Done(Done::Clipboard(text)));
                });
            }

            // ---- Portal ----
            // Every one of these writes through `App::set_portal`, the single
            // call that stores the URL, the local root and the enabled flag (and
            // the token, when one is given: it goes to the keychain, never to
            // settings.json).
            Request::SetPortalUrl(url) => {
                ui.settings.portal_url = url;
                self.portal_write(ui, "");
            }
            Request::SetPortalRoot(root) => {
                ui.settings.portal_root = root;
                self.portal_write(ui, "");
            }
            Request::SetPortalEnabled(on) => {
                ui.settings.portal_enabled = on;
                self.portal_write(ui, "");
            }
            Request::SetPortalToken(token) => {
                // Stored: the field empties, so the token is on screen nowhere
                // at all. Refused: the field keeps it, so it can be retried.
                if !token.is_empty() && self.portal_write(ui, token.expose()) {
                    ui.portal.token_draft.clear();
                }
            }
            Request::ForgetPortalToken => self.forget_portal_token(ui),
            Request::SignInPortal { email, password } => {
                // The address signed in to is the one on screen: a URL typed in
                // this same frame is saved first, as Reconcile does, so a
                // sign-in can never be aimed at the Portal that was there
                // before. The password is this call's argument and nothing
                // else: it is not stored, not logged, and the draft is emptied
                // when the answer lands. `signing_in` disables the button
                // meanwhile.
                if !self.portal_write(ui, "") {
                    return;
                }
                ui.portal.signing_in = true;
                ui.portal.signin_error = None;
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    let answer = app.portal_sign_in(&email, password.expose()).await.map(Box::new);
                    let _ = tx.send(Update::Done(Done::SignedIn(answer)));
                });
            }
            Request::SignInPortalTotp { code } => {
                // The code finishes the same sign-in the password started: the
                // pending step lives in the app, and the address is the one it
                // was started for. The code is this call's argument and nothing
                // else — never stored, never drawn, and emptied below whichever
                // way the answer went.
                ui.portal.signing_in = true;
                ui.portal.code_error = None;
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    let answer = app.portal_sign_in_totp(code.expose()).await.map(Box::new);
                    let _ = tx.send(Update::Done(Done::SignedInTotp(answer)));
                });
            }
            Request::SignOutPortal => self.sign_out_portal(ui),
            Request::PortalReconcile => {
                // Whatever the screen holds is what Portal is asked with: a URL
                // typed in this same frame is saved before the call, not lost to
                // the one-request-a-frame rule.
                if !self.portal_write(ui, "") {
                    return;
                }
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    app.portal_reconcile().await;
                    let _ = tx.send(Update::Done(Done::Changed));
                });
            }

            // ---- settings, sign-in, updates ----
            Request::SaveSettings => self.save_settings(ui),
            Request::SetOpenAtLogin(on) => match self.app.set_open_at_login(on) {
                Ok(state) => self.apply_state(ui, state),
                Err(err) => {
                    // The checkbox follows the app, not the click: put it back.
                    ui.push_toast(err, ToastKind::Err);
                    self.read_state(ui);
                }
            },
            Request::Login => {
                self.login_seq += 1;
                let seq = self.login_seq;
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    match app.login().await {
                        Ok(state) => {
                            let _ = tx.send(Update::Done(Done::State(Box::new(state))));
                        }
                        Err(error) => {
                            let _ = tx.send(Update::Done(Done::LoginFailed { seq, error }));
                        }
                    }
                });
            }
            Request::Logout => {
                let state = self.app.logout();
                self.apply_state(ui, state);
            }
            Request::CheckUpdate => {
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    let status = app.check_for_update().await;
                    let _ = tx.send(Update::Done(Done::Update(Box::new(status))));
                });
            }
            Request::InstallUpdate => {
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    if let Err(err) = app.install_update().await {
                        toast(&tx, err, ToastKind::Err);
                    }
                });
            }

            // ---- watch folders ----
            Request::Watches => self.read_watches(ui),
            // The editor's Save, in create mode: every value is the dialog's, and
            // nothing is read from the settings behind it. `add_watch` walks the
            // folder for its baseline, so it runs off the draw thread.
            Request::AddWatch { local, acct, folder, label, dir, on_change, include_existing } => {
                self.spawn_blocking(move |app, tx| {
                    match app.add_watch(
                        &local,
                        &acct,
                        &folder,
                        &label,
                        &dir,
                        &on_change,
                        include_existing,
                    ) {
                        // No toast on success: the list the shell reads next
                        // shows the new folder, which is the answer.
                        Ok(()) => {
                            let _ = tx.send(Update::Done(Done::Changed));
                        }
                        Err(err) => toast(&tx, err, ToastKind::Err),
                    }
                });
            }
            // The editor's Save, in edit mode. A refusal — a path that is not a
            // folder, a folder another watch already holds — is the app's own
            // sentence, and the dialog has already closed by the time it lands.
            Request::UpdateWatch { id, local, acct, folder, label, dir, on_change, include_existing } => {
                self.spawn_blocking(move |app, tx| {
                    match app.update_watch(
                        &id,
                        &local,
                        &acct,
                        &folder,
                        &label,
                        &dir,
                        &on_change,
                        include_existing,
                    ) {
                        Ok(()) => {
                            let _ = tx.send(Update::Done(Done::Changed));
                        }
                        Err(err) => toast(&tx, err, ToastKind::Err),
                    }
                });
            }
            Request::SetWatchEnabled { id, on } => {
                self.app.set_watch_enabled(&id, on);
                self.read_watches(ui);
            }
            Request::RemoveWatch(id) => {
                self.app.remove_watch(&id);
                self.read_watches(ui);
            }
            Request::RequeueCanceled(id) => {
                self.app.requeue_canceled(&id);
                self.read_watches(ui);
            }

            // ---- space ----
            Request::RefreshSpace => self.refresh_space(),
            Request::DeleteProject { account_id, project_id, name } => {
                let (app, tx) = (self.app.clone(), self.tx.clone());
                self.spawn_task(async move {
                    // The webview's `deleteSpaceProject`: one call, its own two
                    // toasts, and the row gone from the chart once it answered.
                    match app.delete_project(&account_id, &project_id).await {
                        Ok(()) => {
                            toast(&tx, format!("Deleted {name} on Frame.io"), ToastKind::Ok);
                            let _ = tx.send(Update::Done(Done::Changed));
                        }
                        Err(err) => toast(&tx, err, ToastKind::Err),
                    }
                });
            }

            // ---- the window ----
            Request::Refresh => self.read_all(ui),
            Request::Quit => {
                if self.may_quit() {
                    self.quit_ok = true;
                    self.close = true;
                }
            }
        }
    }

    // ---------- what came back ----------

    fn apply(&mut self, update: Update, ui: &mut UiState) {
        match update {
            Update::Event(event, payload) => self.on_event(&event, payload, ui),
            Update::ShowWindow => self.focus = true,
            Update::Quit => {
                self.quit_ok = true;
                self.close = true;
            }
            Update::QuitAnswer(true) => {
                self.asking = false;
                self.quit_ok = true;
                self.close = true;
            }
            Update::QuitAnswer(false) => self.asking = false,
            Update::Done(done) => self.on_done(done, ui),
        }
    }

    fn on_event(&mut self, event: &str, payload: Value, ui: &mut UiState) {
        match event {
            // `GetState`, `UpdateStatus`, and the History rows when that screen is
            // the one showing (its filter is the screen's, so it is re-run as it
            // was last asked for).
            "state-changed" => self.read_all(ui),
            "update" => ui.update = UpdateState::from_core(&self.app.update_status()),
            // Finder's right-click menu: "Upload with FioTransfer", "Create Watch
            // Folder in FioTransfer".
            "service" => self.on_service(ui),
            // A Portal reconcile finished: the status line's numbers are new, and
            // its watches may have been added, edited or removed.
            "portal" => {
                self.read_portal(ui);
                self.read_watches(ui);
            }
            // Files dropped on the window (the webview's `files-dropped`).
            "files-dropped" => {
                let paths: Vec<String> = serde_json::from_value(payload).unwrap_or_default();
                if !paths.is_empty() {
                    self.start_upload(ui, paths);
                }
            }
            _ => {}
        }
    }

    fn on_done(&mut self, done: Done, ui: &mut UiState) {
        match done {
            Done::Changed => self.read_all(ui),
            Done::Toast { text, kind } => ui.push_toast(text, kind),
            Done::State(state) => self.apply_state(ui, *state),
            Done::Link { seq, info } => {
                if seq != self.link_seq {
                    // A newer link was typed while this one was resolving.
                    return;
                }
                ui.link_info = Some(UiLinkInfo::from_core(*info));
                // A link that needs a passphrase says so in the preview, not in a
                // toast: the prompt is the message.
                if let Some(info) = &ui.link_info {
                    if !info.error.is_empty() && !info.needs_passphrase {
                        let error = info.error.clone();
                        ui.push_toast(error, ToastKind::Err);
                    }
                }
            }
            Done::LoginFailed { seq, error } => {
                if seq == self.login_seq {
                    ui.push_toast(error, ToastKind::Err);
                }
            }
            Done::Update(status) => ui.update = UpdateState::from_core(&status),
            Done::DownloadDir(Some(dir)) => {
                ui.download_dir = dir.clone();
                ui.settings.download_dir = dir;
                self.save_settings(ui);
            }
            Done::DownloadDir(None) => {}
            // The editor's "Choose…" answer lands in the field it was asked for.
            // A dialog that closed while the panel was up keeps nothing, and a
            // cancelled panel changes nothing.
            Done::WatchFolder(Some(dir)) => {
                if let Some(editor) = &mut ui.watch_editor {
                    editor.local = dir;
                }
            }
            Done::WatchFolder(None) => {}
            Done::SignedIn(result) => {
                ui.portal.signing_in = false;
                // The password goes either way: a refused one is not worth
                // keeping, and there is nothing left to press with it.
                ui.portal.password_draft.clear();
                match result {
                    Ok(status) => {
                        ui.portal.signin_error = None;
                        ui.portal.code_error = None;
                        ui.portal.code_draft.clear();
                        ui.portal.email_draft = status.email.clone();
                        ui.portal.status = PortalView::from_core(&status);
                        ui.portal.status.now_ms = now_ms();
                        // `portal_email` was saved by the sign-in, and so was
                        // "a credential is stored": read both back from the app.
                        self.read_state(ui);
                    }
                    Err(err) => {
                        // The email stays, so the sentence under the field can
                        // be read next to the address it is about.
                        ui.portal.signin_error = Some(err);
                        // A sign-in that stopped for a code is not a refusal:
                        // the app now says so, and the card draws the row that
                        // finishes it.
                        self.read_portal(ui);
                    }
                }
            }
            Done::SignedInTotp(result) => {
                ui.portal.signing_in = false;
                // The code goes either way, and so does the row: a refused code
                // is not worth keeping and the next six digits replace it.
                ui.portal.code_draft.clear();
                match result {
                    Ok(status) => {
                        ui.portal.signin_error = None;
                        ui.portal.code_error = None;
                        ui.portal.email_draft = status.email.clone();
                        ui.portal.status = PortalView::from_core(&status);
                        ui.portal.status.now_ms = now_ms();
                        self.read_state(ui);
                    }
                    Err(err) => {
                        // Portal's sentence, under the code field it is about.
                        ui.portal.code_error = Some(err);
                        self.read_portal(ui);
                    }
                }
            }
            Done::Clipboard(text) => {
                if text.is_empty() {
                    return;
                }
                // `paste-btn` fills the field and inspects it at once.
                ui.link_info = None;
                ui.link = text;
                let link = ui.link.trim().to_string();
                let passphrase = ui.passphrase.clone();
                self.inspect(link, passphrase);
            }
        }
    }

    // ---------- reading the app ----------

    /// The tick: the two things that move on their own, and the expanded job's
    /// files.
    fn tick(&mut self, ui: &mut UiState) {
        self.last_tick = Instant::now();
        self.read_running(ui);
        // The Portal card is the one screen whose content can change with nothing in
        // this window doing anything: the app refreshes the stored credential as it
        // starts, and the poller reconciles in the background. Without a read here a
        // card that was drawn before the first pump finished would keep saying
        // "Not signed in yet" until the user touched something. Only while it shows,
        // so a closed Settings screen costs nothing.
        if ui.view == View::Settings {
            self.read_portal(ui);
        }
    }

    /// Everything the app owns: the state, the queue, the watch folders, and the
    /// history rows when that screen is showing.
    fn read_all(&mut self, ui: &mut UiState) {
        self.read_state(ui);
        self.read_running(ui);
        if ui.view == View::History {
            self.read_history(ui);
        }
    }

    /// `GetState` and `UpdateStatus`: who is signed in, the settings, the version,
    /// the log folder, the destination and the update card.
    fn read_state(&mut self, ui: &mut UiState) {
        let state = self.app.get_state();
        self.apply_state(ui, state);
        ui.update = UpdateState::from_core(&self.app.update_status());
        self.read_portal(ui);
    }

    /// The Portal status line: the last reconcile's report, whether a token is
    /// stored and where the local root is. All of it is the app's own cache, so
    /// this rides with every state read rather than on a channel of its own; and
    /// the moment it was taken comes with it, so "2 min ago" is the caller's
    /// clock and not the frame's.
    fn read_portal(&mut self, ui: &mut UiState) {
        let mut view = PortalView::from_core(&self.app.portal_status());
        view.now_ms = now_ms();
        ui.portal.status = view;
    }

    fn apply_state(&mut self, ui: &mut UiState, state: State) {
        ui.logged_in = state.logged_in;
        ui.user = state.user;
        ui.settings = state.settings;
        ui.version = state.version;
        ui.open_at_login = state.open_at_login;
        ui.log_dir = state.log_dir;
        ui.home = state.home;
        // `renderState` reads the destination and the crumbs out of the settings.
        ui.destination = ui.settings.last_folder_label.clone();
        ui.dest_path = ui
            .settings
            .last_folder_path
            .iter()
            .map(|crumb| Crumb { kind: crumb.kind.clone(), name: crumb.name.clone() })
            .collect();
    }

    /// `Snapshot` and `Watches`: the queue, the watch folders and the files of the
    /// job whose row is open.
    fn read_running(&mut self, ui: &mut UiState) {
        ui.snapshot = self.app.snapshot();
        self.active = ui.snapshot.jobs.iter().any(|job| !job.terminal());
        self.read_watches(ui);
        self.read_files(ui);
        self.read_space(ui);
    }

    /// `Space` and `SpaceWalking`: what the last reading found and whether a
    /// walk is running. Both are cheap reads of the app's own cache, so this
    /// runs on the tick with everything else — a walk that ends is picked up on
    /// the next frame rather than by a channel of its own.
    fn read_space(&mut self, ui: &mut UiState) {
        ui.apply_space(self.app.space().into());
        ui.space_walking = self.app.space_walking();
    }

    /// The watch folders, as the screen draws them: one row per watch, each
    /// carrying the duplex and lane its record's `origin` names, and the rows
    /// put in the order the screen draws them — a duplex's two lanes adjacent,
    /// inbound first, and the blocks in label order. The screen groups by
    /// `duplex_id` itself, so the order here is what a straight walk of
    /// `UiState::watches` sees, not something it has to trust.
    fn read_watches(&mut self, ui: &mut UiState) {
        let rows: Vec<WatchRow> = self.app.watches().iter().map(WatchRow::from_core).collect();
        ui.watches = in_block_order(rows);
    }

    /// The expanded job's files. `UiState::files` belongs to the screen, so this
    /// only ever fills it for the job the user opened — and empties it only when
    /// this shell was the one that filled it.
    fn read_files(&mut self, ui: &mut UiState) {
        let Some(id) = ui.expanded.clone() else {
            if self.files_for.take().is_some() {
                ui.files.clear();
                self.files_at = None;
            }
            return;
        };
        let switched = self.files_for.as_deref() != Some(id.as_str());
        let stale = self.files_at.map(|at| at.elapsed() >= FILES_EVERY).unwrap_or(true);
        if !switched && !stale {
            return;
        }
        ui.files = self.app.job_files(&id);
        self.files_for = Some(id);
        self.files_at = Some(Instant::now());
    }

    /// `History(query, kind, 500)`, as rows: the same wording the History screen
    /// and the webview's `fmtDate` used.
    fn read_history(&mut self, ui: &mut UiState) {
        let previous = std::mem::take(&mut ui.history);
        match self.app.history(&self.hist_query, &self.hist_kind, HISTORY_LIMIT) {
            Ok(rows) => {
                let running: HashSet<&str> = ui
                    .snapshot
                    .jobs
                    .iter()
                    .filter(|job| !job.terminal())
                    .map(|job| job.id.as_str())
                    .collect();
                let zone = self.offset();
                let now = now_ms();
                ui.history = rows
                    .iter()
                    .map(|row| history_row(row, &running, zone, now))
                    .collect();
            }
            Err(err) => {
                // Keep what was on screen: a database that answered a moment ago
                // is better than an empty table.
                ui.history = previous;
                ui.push_toast(err, ToastKind::Err);
            }
        }
    }

    /// This machine's UTC offset, asked again every half hour (a laptop that
    /// crossed a time zone, or a daylight-saving change).
    fn offset(&mut self) -> i64 {
        if self.zone_at.elapsed() >= ZONE_EVERY {
            self.zone = platform::local_offset_secs();
            self.zone_at = Instant::now();
        }
        self.zone
    }

    // ---------- space ----------

    /// `RefreshSpace`: the walk runs on the runtime, and the tick picks the
    /// figures up when it ends — `space_walking` says it is running until then,
    /// so the screen says "Walking Frame.io…" instead of implying the numbers
    /// are current.
    fn refresh_space(&mut self) {
        self.space_asked = true;
        let (app, tx) = (self.app.clone(), self.tx.clone());
        self.spawn_task(async move {
            app.refresh_space().await;
            let _ = tx.send(Update::Done(Done::Changed));
        });
    }

    /// The webview's `openSpace`: the first time the Space screen is shown, walk
    /// Frame.io rather than making the user press Refresh to see anything.
    ///
    /// A walk already running (one started before this window opened) is adopted
    /// instead of doubled, and nothing is started while signed out — the screen
    /// offers Settings then, and asks again once the user is signed in.
    fn drive_space(&mut self, ui: &mut UiState) {
        if ui.view != View::Space || self.space_asked {
            return;
        }
        if ui.space_walking {
            self.space_asked = true;
            return;
        }
        if !ui.space.is_empty() || !ui.logged_in {
            return;
        }
        self.refresh_space();
    }

    // ---------- the app's own asks ----------

    /// `SaveSettings`: the whole settings struct the screen is holding, answered
    /// with the state as saved (normalised: a bad port or stream count comes back
    /// corrected).
    fn save_settings(&mut self, ui: &mut UiState) {
        match self.app.save_settings(ui.settings.clone()) {
            Ok(state) => self.apply_state(ui, state),
            Err(err) => ui.push_toast(err, ToastKind::Err),
        }
    }

    /// The Portal settings, written through the one call that owns them.
    ///
    /// `token` is empty for a URL, folder or toggle change: `App::set_portal`
    /// keeps the token already in the keychain when it is given none, which is
    /// also why an untouched (masked) field cannot wipe it. Answers whether the
    /// app stored them; a refusal is a toast here, and the screen goes back to
    /// the values that are really saved.
    fn portal_write(&mut self, ui: &mut UiState, token: &str) -> bool {
        let s = ui.settings.clone();
        let saved = self.app.set_portal(&s.portal_url, token, &s.portal_root, s.portal_enabled);
        let ok = match &saved {
            Ok(_) => true,
            Err(err) => {
                ui.push_toast(err.clone(), ToastKind::Err);
                false
            }
        };
        // Whatever happened, the screen follows the app: the normalised URL, the
        // root it kept, or the values it refused to replace.
        self.read_state(ui);
        ok
    }

    /// "Forget": the token goes, and the status line's `signed_in` with it.
    fn forget_portal_token(&mut self, ui: &mut UiState) {
        self.app.forget_portal_token();
        ui.portal.token_draft.clear();
        self.read_state(ui);
    }

    /// "Sign out": the session cookie goes, and with it the admin the status
    /// named. The fields go back to the last email used, with no password in
    /// them; a stored bearer token is a different credential and stays.
    fn sign_out_portal(&mut self, ui: &mut UiState) {
        self.app.portal_sign_out();
        ui.portal.email_draft.clear();
        ui.portal.password_draft.clear();
        ui.portal.signin_error = None;
        ui.portal.signing_in = false;
        self.read_state(ui);
    }

    fn start_upload(&mut self, ui: &mut UiState, paths: Vec<String>) {
        match self.app.start_upload(paths) {
            Ok(_) => self.read_running(ui),
            Err(err) => ui.push_toast(err, ToastKind::Err),
        }
    }

    /// `InspectLink`, answered into `UiState::link_info` if it is still the link
    /// in the field when it arrives.
    fn inspect(&mut self, link: String, passphrase: String) {
        self.link_seq += 1;
        let seq = self.link_seq;
        let (app, tx) = (self.app.clone(), self.tx.clone());
        self.spawn_task(async move {
            let info = app.inspect_link(&link, &passphrase).await;
            let _ = tx.send(Update::Done(Done::Link { seq, info: Box::new(info) }));
        });
    }

    /// Finder's requests, taken once: uploads with the remembered destination,
    /// watch folders as far as a destination can be chosen without the picker.
    fn on_service(&mut self, ui: &mut UiState) {
        let requests = self.app.take_service_requests();
        if requests.is_empty() {
            return;
        }
        self.focus = true;
        for request in requests {
            match request.kind.as_str() {
                "upload" => self.start_upload(ui, request.paths),
                "watch" => {
                    // A watch needs a Frame.io folder, and the destination picker
                    // is the Watch screen's; nothing here may invent one.
                    if let Some(hist) = self.app.hist() {
                        hist.log(&format!(
                            "SERVICE  watch request for {}: choose a destination on the Watch Folders screen",
                            request.paths.join(", ")
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    /// Closing the window with transfers running asks first. `true` when the app
    /// may close now.
    fn may_quit(&mut self) -> bool {
        if self.quit_ok {
            return true;
        }
        let running = self.app.snapshot().jobs.iter().any(|job| !job.terminal());
        if !running {
            self.quit_ok = true;
            return true;
        }
        if self.asking {
            // One question at a time; the answer closes the window.
            return false;
        }
        self.asking = true;
        let host = self.host.clone();
        self.spawn_blocking(move |_app, tx| {
            let quit = host.question(
                "Transfers in progress",
                "Quitting stops running transfers. You can retry them from History.",
                "Quit",
                "Cancel",
            );
            let _ = tx.send(Update::QuitAnswer(quit));
        });
        false
    }

    // ---------- the runtime ----------

    /// Runs a future on the runtime, answering over the channel.
    fn spawn_task<F>(&mut self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if let Some(rt) = &self.rt {
            rt.spawn(future);
        }
    }

    /// Runs something that can block — a panel, the clipboard — off the draw
    /// thread, with the runtime entered so the app can start its own work from
    /// it. A blocking call may sit on a worker for as long as its own panel is
    /// open; the app's own engine runs on the runtime's other threads.
    fn spawn_blocking<F>(&mut self, work: F)
    where
        F: FnOnce(Arc<App>, Sender<Update>) + Send + 'static,
    {
        let Some(rt) = &self.rt else { return };
        let handle = rt.handle().clone();
        let app = self.app.clone();
        let tx = self.tx.clone();
        rt.spawn_blocking(move || {
            let _entered = handle.enter();
            work(app, tx);
        });
    }

    /// One panel at a time: a click that arrives while a panel is open is
    /// dropped rather than queued behind it.
    fn panel<F>(&mut self, work: F)
    where
        F: FnOnce(Arc<App>, Sender<Update>) + Send + 'static,
    {
        if self.panel_open.swap(true, Ordering::SeqCst) {
            return;
        }
        let gate = Gate(self.panel_open.clone());
        self.spawn_blocking(move |app, tx| {
            let _gate = gate;
            work(app, tx);
        });
    }
}

/// Holds a panel's turn: dropping it — normally, or because the work panicked —
/// lets the next panel open.
struct Gate(Arc<AtomicBool>);

impl Drop for Gate {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A message for the user, from a thread that cannot touch the window.
fn toast(tx: &Sender<Update>, text: impl Into<String>, kind: ToastKind) {
    let _ = tx.send(Update::Done(Done::Toast { text: text.into(), kind }));
}

// ---------- rows ----------

/// One History row, from a stored transfer. The wording is the webview's
/// `loadHistory`: `files_done/files_total`, `fmtBytes(total_bytes)` and a date;
/// a transfer that is still running right now shows as running, however it was
/// last saved.
fn history_row(transfer: &history::Transfer, running: &HashSet<&str>, zone: i64, now: i64) -> HistoryRow {
    HistoryRow {
        title: transfer.title.clone(),
        kind: transfer.kind.clone(),
        status: if running.contains(transfer.id.as_str()) {
            "running".to_string()
        } else {
            transfer.status.clone()
        },
        when: fmt_when(transfer.created_at, zone, now),
        files: format!("{}/{}", transfer.files_done, transfer.files_total),
        size: fmt_bytes(transfer.total_bytes),
    }
}

/// A path the way `shortPath` shortens it: `~/Movies`, not
/// `/Users/dana/Movies`.
pub fn short_path(home: &str, path: &str) -> String {
    if home.is_empty() || path.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{}", rest),
        _ => path.to_string(),
    }
}

/// Unix milliseconds, as everything in the app counts time.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A timestamp the way the History screen words one: `Today, 09:41`,
/// `Yesterday, 18:02`, `27 Oct, 11:15`, and `27 Oct 2025, 11:15` once the year is
/// not this one. `zone` is seconds east of Greenwich, `now` is unix
/// milliseconds.
fn fmt_when(ms: i64, zone: i64, now: i64) -> String {
    if ms <= 0 {
        return "—".to_string();
    }
    let local = ms.div_euclid(1000) + zone;
    let today = now.div_euclid(1000) + zone;
    let (hour, minute) = clock(local);
    let time = format!("{:02}:{:02}", hour, minute);
    let days = local.div_euclid(86_400);
    let today_days = today.div_euclid(86_400);
    match today_days - days {
        0 => format!("Today, {}", time),
        1 => format!("Yesterday, {}", time),
        _ => {
            let (year, month, day) = civil_from_days(days);
            let name = MONTHS[(month as usize - 1).min(11)];
            if year == civil_from_days(today_days).0 {
                format!("{} {}, {}", day, name, time)
            } else {
                format!("{} {} {}, {}", day, name, year, time)
            }
        }
    }
}

/// Local seconds as `(hour, minute)`.
fn clock(secs: i64) -> (i64, i64) {
    let of_day = secs.rem_euclid(86_400);
    (of_day / 3600, of_day % 3600 / 60)
}

/// Days since the epoch as `(year, month, day)` — Howard Hinnant's
/// `civil_from_days`, the same arithmetic the app's log file names use.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month_number = if mp < 10 { mp + 3 } else { mp - 9 };
    let month = month_number as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(fields: (&str, &str, &str, i64, i64, i64)) -> history::Transfer {
        let (id, kind, status, created_at, files_done, total_bytes) = fields;
        history::Transfer {
            id: id.to_string(),
            kind: kind.to_string(),
            title: format!("{id}.mov"),
            status: status.to_string(),
            created_at,
            files_done,
            files_total: 3,
            total_bytes,
            ..history::Transfer::default()
        }
    }

    /// 2026-03-04 09:41 in UTC.
    const MARCH: i64 = 1_772_617_260_000;

    #[test]
    fn a_history_row_is_worded_the_way_the_screen_words_it() {
        let running: HashSet<&str> = ["run-1"].into_iter().collect();
        let row = history_row(&transfer(("run-1", "download", "done", MARCH, 2, 4_100_000)), &running, 0, MARCH);
        assert_eq!(row.title, "run-1.mov");
        assert_eq!(row.kind, "download");
        assert_eq!(row.status, "running", "a live transfer shows as running");
        assert_eq!(row.files, "2/3");
        assert_eq!(row.size, "4.10 MB");
        assert_eq!(row.when, "Today, 09:41");

        let idle = history_row(&transfer(("t-2", "upload", "failed", MARCH, 0, 820_000_000)), &running, 0, MARCH);
        assert_eq!(idle.status, "failed", "the stored status when nothing is running");
        assert_eq!(idle.when, "Today, 09:41");
    }

    #[test]
    fn a_date_is_today_yesterday_or_a_day_with_its_year() {
        // 2026-03-04 09:41 UTC, and the morning after.
        assert_eq!(fmt_when(MARCH, 0, MARCH), "Today, 09:41");
        let next_day = MARCH + 86_400_000;
        assert_eq!(fmt_when(MARCH, 0, next_day), "Yesterday, 09:41");
        let three_days = MARCH + 3 * 86_400_000;
        assert_eq!(fmt_when(MARCH, 0, three_days), "4 Mar, 09:41");
        let next_year = MARCH + 400 * 86_400_000;
        assert_eq!(fmt_when(MARCH, 0, next_year), "4 Mar 2026, 09:41");
        assert_eq!(fmt_when(0, 0, MARCH), "—", "a transfer that never started");
    }

    #[test]
    fn a_local_offset_moves_both_the_clock_and_the_day() {
        // An hour east, the same instant is an hour later on the clock.
        assert_eq!(fmt_when(MARCH, 3_600, MARCH), "Today, 10:41");
        // 2026-03-03 23:30 UTC, seen from 2026-03-04 12:00 UTC: yesterday in
        // UTC, but 00:30 on the 4th an hour east.
        let late = 1_772_580_600_000;
        let now = 1_772_625_600_000;
        assert_eq!(fmt_when(late, 0, now), "Yesterday, 23:30");
        assert_eq!(fmt_when(late, 3_600, now), "Today, 00:30");
    }

    #[test]
    fn a_short_path_is_the_home_relative_one() {
        assert_eq!(short_path("/Users/dana", "/Users/dana/Movies/Fio"), "~/Movies/Fio");
        assert_eq!(short_path("/Users/dana", "/Users/dana"), "~");
        assert_eq!(short_path("/Users/dana", "/Users/danax/Movies"), "/Users/danax/Movies");
        assert_eq!(short_path("/Users/dana", "/Volumes/NAS/Renders"), "/Volumes/NAS/Renders");
        assert_eq!(short_path("", "/Users/dana/Movies"), "/Users/dana/Movies");
        assert_eq!(short_path("/Users/dana", ""), "");
    }

    #[test]
    fn the_ticks_are_the_ones_the_brief_names() {
        assert_eq!(ACTIVE_TICK, Duration::from_millis(100));
        assert_eq!(IDLE_TICK, Duration::from_millis(250));
        assert_eq!(HISTORY_LIMIT, 500);
    }

    #[test]
    fn a_watch_the_editor_saves_is_spelled_the_way_core_validates_it() {
        // The editor's own words, checked against the two validators `add_watch`
        // and `update_watch` run: a typo here would be a dialog that never saves.
        use crate::model::{
            DIRECTION_DOWNLOAD, DIRECTION_MIRROR, DIRECTION_UPLOAD, ON_CHANGE_KEEP,
            ON_CHANGE_OVERWRITE,
        };
        use fiotransfer_core::history::watch::{valid_direction, valid_on_change};
        for dir in [DIRECTION_UPLOAD, DIRECTION_DOWNLOAD, DIRECTION_MIRROR] {
            assert!(valid_direction(dir), "{dir}");
        }
        for rule in [ON_CHANGE_OVERWRITE, ON_CHANGE_KEEP] {
            assert!(valid_on_change(rule), "{rule}");
        }
    }

    #[test]
    fn a_toast_reaches_the_window_through_the_channel() {
        let (tx, rx) = std::sync::mpsc::channel();
        toast(&tx, "Share link copied", ToastKind::Ok);
        // Bounded, like every wait in this crate: `recv()` on a channel whose
        // sender is still alive waits for ever.
        match rx.recv_timeout(Duration::from_secs(5)).expect("a toast") {
            Update::Done(Done::Toast { text, kind }) => {
                assert_eq!(text, "Share link copied");
                assert_eq!(kind, ToastKind::Ok);
            }
            other => panic!("not a toast: {other:?}"),
        }
    }
}
