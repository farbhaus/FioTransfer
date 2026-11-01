//! Property tests for the Portal reconciler: `portal::agent::reconcile` against
//! a stub Portal, over generated sets of upload links.
//!
//! What is exercised is the real path: `App::portal_reconcile` ->
//! `portal::agent::reconcile` -> `portal::PortalClient` over HTTP to a
//! one-connection stub server -> `Watcher::add` / `update` / `remove` ->
//! sqlite. Only the network is simulated. The bearer token is stored the way the
//! app stores it (a 0600 file under `HOME`, because `FIO_NO_KEYCHAIN=1`), and
//! `HOME` is the case's own temporary directory, so no real keychain, no real
//! settings and no real user watch folder is touched.
//!
//! Properties, all read back from the database rather than from the reconciler's
//! own report:
//!
//!   P1  one mirrored watch per valid link, at `root/<marker>`, one level under
//!       the root, labelled with the link's own name; a watch the user made is
//!       never edited or removed; nothing is created outside the root and no
//!       local file is ever removed or changed; a second run changes nothing.
//!   P2  after the links change (a link disappears, a marker is cleared, a link
//!       is switched off, a marker is renamed, a link is added), the watches
//!       are the new set exactly: removals only ever hit Portal-owned watches.
//!   P3  a marker that is not exactly one path segment (`""`, `.`, `..`, `a/b`,
//!       `a\b`, `a%2f`, a control character, 300 bytes) never reaches the disk:
//!       no folder is created for it, no watch, and the report says why.
//!
//! Deterministic cases at the bottom cover what a generator would rarely hit:
//! a null / unknown / enveloped JSON reply, Portal refusing (401) and failing
//! (500 with a retry), Portal unreachable, a watch the user made sitting on the
//! folder a link names, and `on_change = keep`.
//!
//! Run with:
//!   CARGO_TARGET_DIR=<own dir> cargo test --offline --test prop_portal

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use proptest::collection::vec as pvec;
use proptest::prelude::*;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use fiotransfer::app::App;
use fiotransfer::config::Settings;
use fiotransfer::history::watch::Watch;
use fiotransfer::history::Store;
use fiotransfer::portal::{self, agent};
use fiotransfer::xfer::{self, manager as xfer_manager};

/// `HOME` and `FIO_NO_KEYCHAIN` are process-wide, so the cases in this binary
/// take turns. (`cargo test` runs the test functions of one binary on separate
/// threads.)
static ENV: Mutex<()> = Mutex::new(());

// ---------- the stub Portal ----------

#[derive(Debug, Clone)]
struct Hit {
    path: String,
    authorization: Option<String>,
}

#[derive(Default)]
struct StubState {
    /// The body `GET /api/upload-links` answers with.
    links: Mutex<String>,
    links_status: Mutex<u16>,
    /// Answer this many calls with 500 first (retry), then the body.
    failures: Mutex<u32>,
    /// Accept the connection and say nothing: a broken link, not a refused one.
    hangup: Mutex<bool>,
    hits: Mutex<Vec<Hit>>,
}

struct Stub {
    base: String,
    state: Arc<StubState>,
    task: tokio::task::JoinHandle<()>,
}

impl Stub {
    /// A server on 127.0.0.1, one request per connection. Must be called with a
    /// runtime entered.
    fn start() -> Stub {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.set_nonblocking(true).expect("nonblocking");
        let addr = l.local_addr().expect("addr");
        let l = TcpListener::from_std(l).expect("tokio listener");
        let state = Arc::new(StubState::default());
        *state.links_status.lock().unwrap() = 200;
        let st = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((c, _)) = l.accept().await else { return };
                let st = st.clone();
                tokio::spawn(async move { answer(c, st).await });
            }
        });
        Stub { base: format!("http://{}", addr), state, task }
    }

    /// What Portal answers from now on.
    fn links(&self, rows: Vec<Value>) {
        *self.state.links.lock().unwrap() = json!({ "upload_links": rows }).to_string();
    }

    fn body(&self, raw: &str) {
        *self.state.links.lock().unwrap() = raw.to_string();
    }

    fn status(&self, code: u16) {
        *self.state.links_status.lock().unwrap() = code;
    }

    fn failures(&self, n: u32) {
        *self.state.failures.lock().unwrap() = n;
    }

    fn hangup(&self, on: bool) {
        *self.state.hangup.lock().unwrap() = on;
    }

    fn hits(&self) -> Vec<Hit> {
        self.state.hits.lock().unwrap().clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer(mut c: TcpStream, st: Arc<StubState>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match c.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 1 << 20 {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.lines();
    let path = lines.next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_string();
    let authorization = head.lines().find_map(|l| {
        let (name, value) = l.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("authorization") {
            Some(value.trim().to_string())
        } else {
            None
        }
    });
    st.hits.lock().unwrap().push(Hit { path: path.clone(), authorization });
    if *st.hangup.lock().unwrap() {
        return; // the connection closes with nothing written
    }
    let bare = path.split('?').next().unwrap_or("");
    let (status, body) = match bare {
        "/api/upload-links" => {
            let mut f = st.failures.lock().unwrap();
            if *f > 0 {
                *f -= 1;
                (500u16, r#"{"error":"busy"}"#.to_string())
            } else {
                let code = *st.links_status.lock().unwrap();
                (code, st.links.lock().unwrap().clone())
            }
        }
        "/api/sync-rules" => (200, r#"{"data":[]}"#.to_string()),
        _ => (404, r#"{"error":"no route"}"#.to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        body.len()
    );
    let _ = c.write_all(resp.as_bytes()).await;
    let _ = c.write_all(body.as_bytes()).await;
    let _ = c.flush().await;
}

// ---------- one case's world ----------

struct World {
    rt: tokio::runtime::Runtime,
    home: PathBuf,
    media: PathBuf,
    root: PathBuf,
    app: Arc<App>,
    hist: Arc<Store>,
    stub: Stub,
    _tmp: tempfile::TempDir,
    _env: MutexGuard<'static, ()>,
}

/// The media tree every case starts from: files the reconciler must never touch.
fn seed_media(media: &Path) {
    std::fs::create_dir_all(media.join("sub/deep")).unwrap();
    std::fs::create_dir_all(media.join("user_project")).unwrap();
    std::fs::create_dir_all(media.join("watch/Out")).unwrap();
    let files: [(&str, &[u8]); 6] = [
        ("keep.MOV", b"\x00\x01master\xff\xfe"),
        ("sub/deep/leaf.txt", b"leaf\n"),
        ("sub/notes.md", b"notes"),
        ("user_project/in.mov", b"user watch folder input"),
        ("user_project/.DS_Store", b"finder junk"),
        ("watch/Out/done.mov", b"done"),
    ];
    for (p, b) in files {
        std::fs::write(media.join(p), b).unwrap();
    }
}

impl World {
    fn new() -> World {
        let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("temp dir");
        let home = tmp.path().join("home");
        let media = tmp.path().join("media");
        let data = home.join("Library/Application Support/FioTransfer");
        let logs = home.join("Library/Logs/FioTransfer");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&logs).unwrap();
        seed_media(&media);
        std::env::set_var("HOME", &home);
        std::env::set_var("FIO_NO_KEYCHAIN", "1");

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let hist = Arc::new(Store::open(&data, &logs).expect("history store"));
        let root = media.join("Live_Work");
        let (stub, app) = {
            let _g = rt.enter();
            let stub = Stub::start();
            let mut s = Settings::default();
            s.portal_url = stub.base.clone();
            s.portal_root = root.to_string_lossy().into_owned();
            s.portal_enabled = true;
            let app = App::new(s);
            *app.hist.lock().unwrap() = Some(hist.clone());
            let cell = {
                let a = app.clone();
                move || a.settings()
            };
            let mgr = xfer_manager::new_manager(app.api.clone(), hist.clone(), Box::new(cell));
            let watcher = xfer::watch::Watcher::new(mgr.clone());
            // These cases are about the watch list, not about transferring: no
            // scan may run behind the assertions.
            watcher.close();
            *app.wat.lock().unwrap() = Some(watcher);
            *app.mgr.lock().unwrap() = Some(mgr);
            portal::set_token("test-token").expect("the portal token is stored");
            (stub, app)
        };
        World { rt, home, media, root, app, hist, stub, _tmp: tmp, _env: env }
    }

    /// A watch the user made, exactly where a user would put one: in their own
    /// media folder, outside the Portal root.
    fn seed_user_watch(&self) -> String {
        let _g = self.rt.enter();
        let dir = self.media.join("user_project");
        let w = self.app.wat().expect("watcher");
        w.add(&dir.to_string_lossy(), "A1", "F1", "My own project", "mirror", "overwrite", false)
            .expect("a user's own watch");
        self.hist.watches().expect("watches").into_iter().find(|r| r.local_path == dir.to_string_lossy()).expect("it is recorded").id
    }

    fn reconcile(&self) -> agent::Report {
        self.rt.block_on(async { self.app.portal_reconcile().await })
    }

    fn watches(&self) -> Vec<Watch> {
        self.hist.watches().expect("watches")
    }

    fn portal_watches(&self) -> Vec<Watch> {
        self.watches().into_iter().filter(|w| agent::link_of(&w.origin).is_some()).collect()
    }

    fn log(&self) -> Vec<Hit> {
        self.stub.hits()
    }

    fn root_str(&self) -> String {
        self.app.settings().portal_root_path().to_string_lossy().into_owned()
    }
}

/// Every file under `root`, with its bytes.
fn files_under(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if let Ok(b) = std::fs::read(&p) {
                out.insert(p.strip_prefix(base).unwrap_or(&p).to_string_lossy().into_owned(), b);
            }
        }
    }
    walk(root, root, &mut out);
    out
}

/// Every directory under `root`.
fn dirs_under(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.insert(p.strip_prefix(base).unwrap_or(&p).to_string_lossy().into_owned());
                walk(&p, base, out);
            }
        }
    }
    walk(root, root, &mut out);
    out
}

// ---------- the links a case starts from ----------

#[derive(Debug, Clone)]
struct LinkSpec {
    id: String,
    marker: String,
    name: Option<String>,
    acct: Option<String>,
    folder: Option<String>,
    disabled: bool,
}

impl LinkSpec {
    /// The row Portal sends, with a null where a value is missing and an unknown
    /// field the client must ignore.
    fn row(&self) -> Value {
        json!({
            "id": self.id,
            "name": match &self.name { Some(n) => json!(n), None => Value::Null },
            "target_folder_id": match &self.folder { Some(f) => json!(f), None => Value::Null },
            "target_folder_name": format!("Folder {}", self.id),
            "target_account_id": match &self.acct { Some(a) => json!(a), None => Value::Null },
            "folder": { "id": self.folder.clone().unwrap_or_default(), "name": "Frame.io folder", "account_id": self.acct.clone().unwrap_or_default() },
            "turbosort": self.marker,
            "disabled": self.disabled,
            "portal_added_this_quarter": { "unread": true },
        })
    }

    fn label(&self) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None => format!("Folder {}", self.id),
        }
    }

    fn trimmed_marker(&self) -> String {
        self.marker.trim().to_string()
    }
}

/// The same rule the code documents, written out again here so the expectations
/// are not read from the implementation.
fn marker_ok(m: &str) -> bool {
    !m.is_empty()
        && m != "."
        && m != ".."
        && m.len() <= 255
        && !m.chars().any(|c| c == '/' || c == '\\' || c == '%' || c.is_control())
}

/// Which marker each link should own, and whose watch it is: the model, by link
/// id order, first claim of a folder wins.
fn expected(specs: &[LinkSpec]) -> BTreeMap<String, String> {
    let mut by_id: BTreeMap<String, &LinkSpec> = BTreeMap::new();
    for s in specs {
        if s.id.is_empty() {
            continue;
        }
        by_id.entry(s.id.clone()).or_insert(s);
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for (id, s) in by_id {
        let m = s.trimmed_marker();
        if m.is_empty() || s.disabled || !marker_ok(&m) || s.acct.is_none() || s.folder.is_none() {
            continue;
        }
        if claimed.insert(m.clone()) {
            out.insert(m, id);
        }
    }
    out
}

fn any_id() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("L1".to_string()),
        Just("L2".to_string()),
        Just("L3".to_string()),
        Just("L1".to_string()),
        Just(String::new()),
        Just("../L4".to_string()),
        Just("L/5".to_string()),
        Just("L%2f6".to_string()),
        "[A-Za-z0-9]{1,4}",
    ]
}

fn any_marker() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("Rakete".to_string()),
        Just("Same".to_string()),
        Just("Same".to_string()),
        Just(String::new()),
        Just("..".to_string()),
        Just(".".to_string()),
        Just("a/b".to_string()),
        Just("a\\b".to_string()),
        Just("a%2f..".to_string()),
        Just("tab\u{9}here".to_string()),
        Just(" spaced ".to_string()),
        Just("x".repeat(300)),
        "[A-Za-z0-9 _.()-]{0,6}",
    ]
}

fn any_link() -> impl Strategy<Value = LinkSpec> {
    (any_id(), any_marker(), proptest::option::of("[A-Za-z ]{1,8}"), proptest::option::of(Just("A1".to_string())), proptest::option::of(Just("F1".to_string())), any::<bool>())
        .prop_map(|(id, marker, name, acct, folder, disabled)| LinkSpec { id, marker, name, acct, folder, disabled })
}

fn any_set() -> impl Strategy<Value = Vec<LinkSpec>> {
    pvec(any_link(), 0..6)
}

// ---------- the assertions ----------

/// P1: the watch list is exactly the model's, and nothing outside the root moved.
fn check_watches(w: &World, specs: &[LinkSpec], user_watch: &str, before: &BTreeMap<String, Vec<u8>>) {
    let want = expected(specs);
    let rows = w.watches();
    let portal_rows: Vec<&Watch> = rows.iter().filter(|r| agent::link_of(&r.origin).is_some()).collect();
    let root = w.root_str();

    assert_eq!(
        portal_rows.len(),
        want.len(),
        "one watch per valid link: want {:?}, got {:?}",
        want,
        portal_rows.iter().map(|r| (&r.origin, &r.local_path)).collect::<Vec<_>>()
    );
    for (marker, id) in &want {
        let origin = agent::origin_of(id);
        let w0 = portal_rows
            .iter()
            .find(|r| r.origin == origin)
            .unwrap_or_else(|| panic!("no watch for {} (marker {:?})", id, marker));
        assert_eq!(w0.local_path, format!("{}/{}", root, marker), "the local path is root/marker");
        assert!(Path::new(&w0.local_path).parent() == Some(Path::new(&root)), "exactly one level under the root");
        assert_eq!(w0.direction, "mirror", "Portal's folder is mirrored");
        let spec = specs.iter().find(|s| &s.id == id).expect("the link");
        assert_eq!(w0.account_id, spec.acct.clone().expect("a link with an account"));
        assert_eq!(w0.folder_id, spec.folder.clone().expect("a link with a folder"));
        assert_eq!(w0.on_change, w.app.settings().portal_change_rule());
        assert!(w0.enabled, "a fresh Portal watch is on");
        assert_eq!(w0.dest_label, spec.label(), "the label is the link's own name");
    }

    // The user's own watch is exactly as it was, and still there.
    let mine: Vec<&Watch> = rows.iter().filter(|r| r.origin.is_empty()).collect();
    assert_eq!(mine.len(), 1, "the user's watch is the only one without an origin");
    assert_eq!(mine[0].id, user_watch);
    assert_eq!(mine[0].dest_label, "My own project");
    assert!(mine[0].local_path.ends_with("user_project"));

    // Nothing local was removed or changed, and no file was created anywhere.
    let after = files_under(&w.media);
    for (p, bytes) in before {
        assert_eq!(after.get(p), Some(bytes), "{} was removed or changed", p);
    }
    for p in after.keys() {
        assert!(before.contains_key(p), "a file appeared at {}: Portal may only make folders", p);
    }
    // New folders are all under the root.
    let new_dirs: BTreeSet<String> = dirs_under(&w.media)
        .into_iter()
        .filter(|d| !Path::new(&w.media).join(d).exists() || !before.keys().any(|k| k.starts_with(&format!("{}/", d))))
        .collect();
    for d in new_dirs {
        let abs = Path::new(&w.media).join(&d);
        assert!(abs.starts_with(&w.root), "a folder appeared outside the root: {}", abs.display());
    }
}

/// Every report entry has a reason, and every link the model calls invalid is in
/// the report.
fn check_report(rep: &agent::Report, specs: &[LinkSpec]) {
    assert!(rep.error.is_empty(), "the reconcile failed: {}", rep.error);
    assert_eq!(rep.links_seen, specs.len());
    for s in &rep.skipped {
        assert!(!s.reason.is_empty(), "a skip without a reason: {:?}", s);
    }
    // One row per id: the reconciler reads the first row a link id and ignores
    // the repeats, so that is the one the report has to be checked against.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for spec in specs.iter().filter(|s| !s.id.is_empty() && seen.insert(s.id.as_str())) {
        let m = spec.trimmed_marker();
        let invalid_marker = !m.is_empty() && !marker_ok(&m);
        if invalid_marker {
            assert!(
                rep.skipped.iter().any(|s| s.link == spec.id),
                "the marker {:?} of {} was ignored without a report entry",
                m,
                spec.id
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// P1: watches match the links exactly, twice in a row, and nothing local
    /// outside the root is created, changed or removed.
    #[test]
    fn p1_one_mirrored_watch_per_valid_link_and_a_second_run_changes_nothing(specs in any_set()) {
        let w = World::new();
        let user_watch = w.seed_user_watch();
        let before = files_under(&w.media);
        w.stub.links(specs.iter().map(|s| s.row()).collect());

        let rep = w.reconcile();
        check_report(&rep, &specs);
        check_watches(&w, &specs, &user_watch, &before);

        // The watch list, as the database holds it.
        let snapshot: Vec<(String, String, String, String, String)> = w
            .watches()
            .into_iter()
            .map(|r| (r.id, r.origin, r.local_path, r.dest_label, r.direction))
            .collect();

        // Running it again changes nothing at all.
        let rep2 = w.reconcile();
        prop_assert!(rep2.error.is_empty(), "second run failed: {}", rep2.error);
        prop_assert_eq!(rep2.ensured, 0, "the second run made or edited a watch");
        prop_assert_eq!(rep2.removed, 0, "the second run removed a watch");
        let again: Vec<(String, String, String, String, String)> = w
            .watches()
            .into_iter()
            .map(|r| (r.id, r.origin, r.local_path, r.dest_label, r.direction))
            .collect();
        prop_assert_eq!(snapshot, again, "the second run changed the watch list");
        check_watches(&w, &specs, &user_watch, &before);
    }

    /// P2: when the links change, the watches follow exactly — removals only for
    /// Portal-owned ones.
    #[test]
    fn p2_removals_are_only_ever_portal_watches(
        specs in any_set(),
        drop_one in proptest::option::of(0usize..8),
        clear_one in proptest::option::of(0usize..8),
        rename_one in proptest::option::of((0usize..8, "[A-Za-z]{1,6}")),
        add_one in any::<bool>(),
    ) {
        let w = World::new();
        let user_watch = w.seed_user_watch();
        let before = files_under(&w.media);
        w.stub.links(specs.iter().map(|s| s.row()).collect());
        let rep = w.reconcile();
        prop_assert!(rep.error.is_empty(), "first run failed: {}", rep.error);
        let first = expected(&specs);

        // A link disappears, a marker is cleared, a link is switched off, a
        // marker is renamed, or a link is added — as Portal would.
        let mut next: Vec<LinkSpec> = specs.clone();
        if let Some(i) = drop_one {
            if i < next.len() {
                next.remove(i);
            }
        }
        if let Some(i) = clear_one {
            if i < next.len() {
                next[i].marker = String::new();
            }
        }
        // A rename and the new link use markers no generated link can produce
        // (longer than the generator's alphabet allows), so the change never
        // swaps two links' folders: that would need a removal the model cannot
        // predict without knowing the order the reconciler takes them in.
        if let Some((i, name)) = rename_one.clone() {
            if i < next.len() {
                next[i].marker = format!("portal-renamed-{}", name);
            }
        }
        if add_one {
            next.push(LinkSpec {
                id: "L9".to_string(),
                marker: "portal-added-folder".to_string(),
                name: Some("Added later".to_string()),
                acct: Some("A1".to_string()),
                folder: Some("F9".to_string()),
                disabled: false,
            });
        }
        let second = expected(&next);
        w.stub.links(next.iter().map(|s| s.row()).collect());
        let rep2 = w.reconcile();
        prop_assert!(rep2.error.is_empty(), "second run failed: {}", rep2.error);
        check_watches(&w, &next, &user_watch, &before);

        // What went: the links that no longer ask for a folder. What was made or
        // edited: the folders and links that are new. Both counted by link, not
        // by marker, so a link that renamed its folder is one edit, not one
        // removal and one addition.
        let origins = |m: &BTreeMap<String, String>| m.values().cloned().collect::<BTreeSet<String>>();
        let (before_ids, after_ids) = (origins(&first), origins(&second));
        let gone = before_ids.difference(&after_ids).count();
        prop_assert_eq!(rep2.removed, gone, "removed {} but the model says {}", rep2.removed, gone);
        let made = second.iter().filter(|(marker, id)| first.get(*marker) != Some(*id)).count();
        prop_assert_eq!(rep2.ensured, made, "ensured {} but the model says {}", rep2.ensured, made);
    }

    /// P3: an unsafe marker never reaches the disk.
    #[test]
    fn p3_an_unsafe_marker_creates_nothing(marker in any_marker()) {
        let w = World::new();
        let before = files_under(&w.media);
        let dirs_before = dirs_under(&w.media);
        let spec = LinkSpec {
            id: "L1".to_string(),
            marker: marker.clone(),
            name: Some("Projekt".to_string()),
            acct: Some("A1".to_string()),
            folder: Some("F1".to_string()),
            disabled: false,
        };
        w.stub.links(vec![spec.row()]);
        let rep = w.reconcile();
        prop_assert!(rep.error.is_empty(), "{}", rep.error);
        let trimmed = spec.trimmed_marker();

        // Nothing was made anywhere, whatever the marker was.
        let dirs_after = dirs_under(&w.media);
        for d in dirs_after.difference(&dirs_before) {
            prop_assert!(d.starts_with("Live_Work"), "a folder appeared at {} for marker {:?}", d, marker);
        }
        prop_assert_eq!(files_under(&w.media), before, "a file appeared for marker {:?}", marker);

        if marker_ok(&trimmed) {
            let want = format!("{}/{}", w.root_str(), trimmed);
            prop_assert!(
                w.portal_watches().iter().any(|r| r.local_path == want),
                "the safe marker {:?} made no watch",
                marker
            );
        } else {
            prop_assert!(w.portal_watches().is_empty(), "the unsafe marker {:?} made a watch", marker);
            if !trimmed.is_empty() {
                prop_assert!(
                    rep.skipped.iter().any(|s| s.link == "L1"),
                    "the unsafe marker {:?} was dropped silently",
                    marker
                );
            }
        }
    }
}

// ---------- a link that is two one-way folders ----------
//
// A link whose `folder` carries a `sync` object is not one mirror but two
// one-way folders: `1_DRIVE` comes down from Frame.io, `0_EXPORTS` goes up to
// it, and neither side ever propagates a deletion. Portal sends it on the row
// `GET /api/upload-links` already returns, so nothing about the stub changes.

/// A row with a `sync` object inside its `folder`, the way Portal sends it.
fn sync_row(id: &str, marker: &str, sync: Value) -> Value {
    json!({
        "id": id,
        "name": format!("Link {}", id),
        "target_folder_id": "F1",
        "target_folder_name": "Folder",
        "target_account_id": "A1",
        "folder": {
            "type": "frameio",
            "account_id": "A1",
            "folder_id": "F1",
            "path": [{"id": "p1", "name": "Projects"}, {"id": "F1", "name": "Folder"}],
            "turbosort": marker,
            "sync": sync,
        },
        "turbosort": marker,
    })
}

/// The marker folder every case here works under.
const MARKER: &str = "20261028_YML-XMAS-Luc";

fn row_of(w: &World, local: &str) -> Watch {
    w.portal_watches().into_iter().find(|r| r.local_path == local).unwrap_or_else(|| panic!("no watch at {}", local))
}

#[test]
fn a_sync_link_with_a_drive_folder_is_one_download_watch() {
    let w = World::new();
    let before = files_under(&w.media);
    let dirs_before = dirs_under(&w.media);
    w.stub.links(vec![sync_row("L1", MARKER, json!({"mode": "add_only", "drive": {"local": "1_DRIVE"}}))]);

    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    assert!(rep.skipped.is_empty(), "nothing to refuse: {:?}", rep.skipped);
    assert_eq!(rep.ensured, 1);

    let rows = w.portal_watches();
    assert_eq!(rows.len(), 1, "one mapping, one watch: {:?}", rows.iter().map(|r| &r.origin).collect::<Vec<_>>());
    let drive = &rows[0];
    assert_eq!(drive.origin, "portal:L1", "the drive watch keeps the plain origin");
    assert_eq!(drive.direction, "download", "the client's folder only ever comes down");
    assert_eq!(
        Path::new(&drive.local_path),
        w.root.join(MARKER).join("1_DRIVE"),
        "under the marker folder, in the mapped name"
    );
    assert_eq!(drive.account_id, "A1");
    assert_eq!(drive.folder_id, "F1", "no folder_id on the drive side: the link's own folder");
    assert_eq!(drive.on_change, w.app.settings().portal_change_rule());
    assert!(drive.enabled, "a fresh Portal watch is on, as any other");
    assert!(Path::new(&drive.local_path).is_dir(), "the mapped folder is created");

    // A `download` watch takes files from Frame.io and puts nothing there: a file
    // deleted locally can therefore never take anything off Frame.io.
    let as_watch = Watch { direction: drive.direction.clone(), ..Watch::default() };
    assert!(as_watch.downloads() && !as_watch.uploads(), "one-way down");

    // Nothing outside the root appeared, and no file was made.
    assert_eq!(files_under(&w.media), before, "the reconciler writes folders, never files");
    for d in dirs_under(&w.media).difference(&dirs_before) {
        assert!(d == "Live_Work" || d.starts_with("Live_Work/"), "a folder appeared outside the root: {}", d);
    }
}

#[test]
fn a_sync_link_with_drive_and_exports_is_two_watches_going_opposite_ways() {
    let w = World::new();
    w.stub.links(vec![sync_row(
        "L1",
        MARKER,
        json!({
            "mode": "add_only",
            "drive": {"local": "1_DRIVE", "folder_id": "FDRIVE"},
            "exports": {"local": "0_EXPORTS", "folder_id": "FEXPORTS"},
        }),
    )]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    assert_eq!(rep.ensured, 2);

    let drive = row_of(&w, &w.root.join(MARKER).join("1_DRIVE").to_string_lossy());
    let exports = row_of(&w, &w.root.join(MARKER).join("0_EXPORTS").to_string_lossy());
    assert_eq!(drive.origin, "portal:L1");
    assert_eq!(exports.origin, "portal:L1#exports", "the exports watch has an origin of its own");
    assert!(agent::link_of(&exports.origin).is_some(), "and it is still Portal's");
    assert_eq!(drive.direction, "download");
    assert_eq!(exports.direction, "upload", "what we make only ever goes up");
    assert_eq!(drive.folder_id, "FDRIVE");
    assert_eq!(exports.folder_id, "FEXPORTS");
    assert_ne!(drive.id, exports.id);
    assert_ne!(drive.dest_label, exports.dest_label, "two rows on screen are told apart");

    // The direction is what keeps deletions from propagating: an upload watch
    // never looks at Frame.io, and a download watch never writes to it.
    for r in [&drive, &exports] {
        let as_watch = Watch { direction: r.direction.clone(), ..Watch::default() };
        assert!(!(as_watch.uploads() && as_watch.downloads()), "{} is one-way", r.local_path);
    }
}

#[test]
fn a_second_reconcile_of_a_sync_link_changes_nothing() {
    let w = World::new();
    w.stub.links(vec![sync_row(
        "L1",
        MARKER,
        json!({"mode": "add_only", "drive": {"local": "1_DRIVE"}, "exports": {"local": "0_EXPORTS", "folder_id": "FEXPORTS"}}),
    )]);
    assert!(w.reconcile().error.is_empty());
    let snapshot: Vec<(String, String, String, String, String)> = w
        .watches()
        .into_iter()
        .map(|r| (r.id, r.origin, r.local_path, r.direction, r.folder_id))
        .collect();

    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    assert_eq!(rep.ensured, 0, "a second run makes or edits nothing");
    assert_eq!(rep.removed, 0, "and removes nothing");
    let again: Vec<(String, String, String, String, String)> = w
        .watches()
        .into_iter()
        .map(|r| (r.id, r.origin, r.local_path, r.direction, r.folder_id))
        .collect();
    assert_eq!(snapshot, again, "the watch list is unchanged, ids included");
}

#[test]
fn clearing_the_exports_folder_removes_only_that_watch_and_leaves_the_folder_on_disk() {
    let w = World::new();
    w.stub.links(vec![sync_row(
        "L1",
        MARKER,
        json!({"mode": "add_only", "drive": {"local": "1_DRIVE"}, "exports": {"local": "0_EXPORTS", "folder_id": "FEXPORTS"}}),
    )]);
    assert!(w.reconcile().error.is_empty());
    let drive = row_of(&w, &w.root.join(MARKER).join("1_DRIVE").to_string_lossy());
    let exports_path = w.root.join(MARKER).join("0_EXPORTS");
    let exports = row_of(&w, &exports_path.to_string_lossy());

    // Somebody has already exported into it. That file is the reason this test
    // exists: nothing may take it away.
    std::fs::write(exports_path.join("SHOW_EP01_v03.mov"), b"master").unwrap();

    // Portal clears the exports folder (or somebody forgets the mapping).
    w.stub.links(vec![sync_row("L1", MARKER, json!({"mode": "add_only", "drive": {"local": "1_DRIVE"}}))]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    assert_eq!(rep.removed, 1, "exactly the exports watch goes: {:?}", rep.skipped);
    assert_eq!(rep.ensured, 0, "the drive watch was already right");

    let rows = w.portal_watches();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, drive.id, "the drive watch is the same watch, not a new one");
    assert_eq!(rows[0].origin, "portal:L1");
    assert!(!w.watches().iter().any(|r| r.id == exports.id), "the exports watch is gone");
    assert!(exports_path.is_dir(), "the folder stays on disk: unwatched, not deleted");
    assert_eq!(
        std::fs::read(exports_path.join("SHOW_EP01_v03.mov")).unwrap(),
        b"master",
        "and so does what is in it"
    );
}

#[test]
fn an_invalid_local_refuses_the_mapping_and_creates_nothing() {
    let w = World::new();
    let before = dirs_under(&w.media);
    w.stub.links(vec![sync_row(
        "L1",
        MARKER,
        json!({"mode": "add_only", "drive": {"local": ".."}, "exports": {"local": "0_EXPORTS", "folder_id": "FEXPORTS"}}),
    )]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    assert!(rep.skipped.iter().any(|s| s.reason.contains("the download folder")), "{:?}", rep.skipped);
    // The exports side is still made: one bad mapping does not lose the other.
    assert_eq!(w.portal_watches().len(), 1);
    assert_eq!(w.portal_watches()[0].direction, "upload");
    // And nothing was created for the refused name: the marker folder holds the
    // exports folder and nothing else, and `..` made no folder anywhere.
    let made: BTreeSet<String> = dirs_under(&w.media).into_iter().filter(|d| !before.contains(d)).collect();
    let want: BTreeSet<String> = [
        "Live_Work".to_string(),
        format!("Live_Work/{}", MARKER),
        format!("Live_Work/{}/0_EXPORTS", MARKER),
    ]
    .into_iter()
    .collect();
    assert_eq!(made, want, "the refused name made no folder");
}

/// A link ticked "Sync to FioTransfer" and nothing else (`"sync": {}`, no
/// turbosort) is a folder named after the link with IN inside; a link that is
/// not ticked is left alone. Renaming IN in the settings moves the watch to the
/// new folder on the next run, and the old folder stays on disk.
#[test]
fn a_ticked_link_is_named_after_itself_and_the_users_names_apply() {
    let w = World::new();
    let mut ticked = sync_row("L1", "", json!({}));
    ticked["name"] = json!("Acme Spot 30s");
    w.stub.links(vec![ticked, sync_row("L2", "", json!(null))]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty() && rep.skipped.is_empty(), "{:?} {:?}", rep.error, rep.skipped);
    let ws = w.portal_watches();
    assert_eq!(ws.len(), 1, "only the ticked link: {:?}", ws);
    let project = w.root.join("Acme Spot 30s");
    assert_eq!(ws[0].local_path, project.join("IN").to_string_lossy().into_owned());
    assert_eq!(ws[0].direction, "download");
    assert_eq!(ws[0].folder_id, "F1", "the link's own folder");
    assert!(!ws[0].ignore_remote, "IN takes what the client sent before the folder existed");

    // The app's settings live on disk, as they do in the app.
    let root = w.root.to_string_lossy().into_owned();
    w.app.set_portal(&w.stub.base, "", &root, true).expect("settings saved");
    w.app.set_portal_folders("Footage", "Exports").expect("the names are saved");
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let ws = w.portal_watches();
    assert_eq!(ws.len(), 1);
    assert_eq!(ws[0].local_path, project.join("Footage").to_string_lossy().into_owned());
    assert!(project.join("IN").is_dir(), "the old folder stays");
    assert!(w.app.set_portal_folders("Same", "Same").is_err(), "IN and OUT differ");
    assert!(w.app.set_portal_folders("a/b", "OUT").is_err(), "one segment each");
}

/// Portal's `state`: a link that expires or is revoked stops syncing at the
/// next reconcile, and its folder stays on disk; one already expired gets none.
#[test]
fn an_expired_or_revoked_link_stops_and_leaves_its_folder() {
    let w = World::new();
    let ticked = |id: &str, state: &str| {
        let mut row = sync_row(id, "", json!({}));
        row["name"] = json!(format!("Project {id}"));
        row["state"] = json!(state);
        row
    };
    w.stub.links(vec![ticked("L1", "ok"), ticked("L2", "ok"), ticked("L3", "expired")]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let ws = w.portal_watches();
    assert_eq!(ws.len(), 2, "the expired link gets no folder: {:?}", ws);
    assert!(!w.root.join("Project L3").exists(), "nothing made for it");

    w.stub.links(vec![ticked("L1", "revoked"), ticked("L2", "ok"), ticked("L3", "expired")]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let ws = w.portal_watches();
    assert_eq!(ws.len(), 1, "{:?}", ws);
    assert_eq!(ws[0].local_path, w.root.join("Project L2/IN").to_string_lossy().into_owned());
    assert!(w.root.join("Project L1/IN").is_dir(), "the revoked link's folder stays");
}

/// What Portal does to an older link, as its dev described it: the stored
/// `turbosort`, `drive` and `exports` stay as sent; a new return folder sets
/// `folder_id` on the existing `exports` lane; removing it drops `exports`. No
/// folder on disk moves at any step.
#[test]
fn portals_edits_to_an_older_link_move_no_folder() {
    let w = World::new();
    let row = |exports: Option<&str>| {
        let mut sync = json!({"mode": "add_only", "drive": {"local": "1_DRIVE"}});
        if let Some(f) = exports {
            sync["exports"] = json!({"local": "0_EXPORTS", "folder_id": f});
        }
        sync_row("L1", MARKER, sync)
    };
    let base = w.root.join(MARKER);
    let paths = |w: &World| {
        let mut p: Vec<(String, String, String)> =
            w.portal_watches().into_iter().map(|x| (x.local_path, x.folder_id, x.id)).collect();
        p.sort();
        p
    };

    w.stub.links(vec![row(Some("FX1"))]);
    assert!(w.reconcile().error.is_empty());
    let first = paths(&w);
    assert_eq!(
        first.iter().map(|p| p.0.clone()).collect::<Vec<_>>(),
        vec![base.join("0_EXPORTS").to_string_lossy().into_owned(), base.join("1_DRIVE").to_string_lossy().into_owned()]
    );

    // A new return folder: the same OUT folder, the same watch, a new destination.
    w.stub.links(vec![row(Some("FX2"))]);
    assert!(w.reconcile().error.is_empty());
    let second = paths(&w);
    assert_eq!(second[0].0, first[0].0, "OUT did not move");
    assert_eq!(second[0].2, first[0].2, "the same watch");
    assert_eq!(second[0].1, "FX2");
    assert_eq!(second[1], first[1], "IN untouched");

    // The return folder removed: OUT is unwatched and stays on disk; IN untouched.
    w.stub.links(vec![row(None)]);
    assert!(w.reconcile().error.is_empty());
    let third = paths(&w);
    assert_eq!(third, vec![first[1].clone()]);
    assert!(base.join("0_EXPORTS").is_dir());
}

#[test]
fn both_locals_the_same_refuses_the_second_mapping() {
    let w = World::new();
    w.stub.links(vec![sync_row(
        "L1",
        MARKER,
        json!({"mode": "add_only", "drive": {"local": "IN_OUT"}, "exports": {"local": "IN_OUT", "folder_id": "FEXPORTS"}}),
    )]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    assert_eq!(w.portal_watches().len(), 1, "one folder cannot be both one-way");
    assert_eq!(w.portal_watches()[0].direction, "download", "the drive side is the one that was made");
    assert!(rep.skipped.iter().any(|s| s.reason.contains("is the download folder")), "{:?}", rep.skipped);
}

#[test]
fn an_unknown_sync_mode_is_read_as_add_only_and_said_so() {
    let w = World::new();
    w.stub.links(vec![sync_row(
        "L1",
        MARKER,
        json!({"mode": "two_way_merge", "drive": {"local": "1_DRIVE"}, "exports": {"local": "0_EXPORTS", "folder_id": "FEXPORTS"}}),
    )]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "an unknown mode is not a failure: {}", rep.error);
    assert_eq!(rep.ensured, 2, "it is read as add_only and acted on");
    assert_eq!(rep.notes.len(), 1, "{:?}", rep.notes);
    assert!(rep.notes[0].contains("two_way_merge") && rep.notes[0].contains("add_only"), "{}", rep.notes[0]);
    assert!(rep.skipped.is_empty(), "a note is not a refusal");
}

/// The deletion promise, asserted rather than written down as a comment.
///
/// `Client::delete_file`, `Client::delete_folder` and `Client::delete_project`
/// are the only ways this app can take a file, a folder or a project off
/// Frame.io, and nothing on the watch path calls any of them: no `sync` mapping may turn "gone locally" into "delete it
/// there", and no mapping may turn "gone on Frame.io" into "delete it here".
///
/// The check reads the sources of the code that decides what a watch does — the
/// engine's tick, scan and batch, and the reconciler that makes the watches —
/// and fails if either name appears there as a call. `core/src/xfer/upload.rs`
/// is deliberately not in the list: a confirmed re-upload deletes the copy it
/// replaces when the watch's OnChange rule is `overwrite`. That is a replacement
/// the upload itself asked for, not a deletion either side propagated, and it is
/// why the names are matched *as calls* — with the `(` attached — rather than as
/// words.
#[test]
fn the_api_delete_functions_are_not_reachable_from_the_watch_path() {
    const WATCH_PATH: [(&str, &str); 3] = [
        ("core/src/xfer/watch.rs", include_str!("../core/src/xfer/watch.rs")),
        ("core/src/portal/agent.rs", include_str!("../core/src/portal/agent.rs")),
        ("core/src/portal/mod.rs", include_str!("../core/src/portal/mod.rs")),
    ];
    for (name, src) in WATCH_PATH {
        let code = without_comments(src);
        for call in ["delete_file", "delete_folder", "delete_project"] {
            assert!(
                !code.contains(&format!("{}(", call)),
                "{} calls {}: a watch must never delete on the other side",
                name,
                call
            );
        }
    }
    // Not a vacuous pass: the same scan does find the call where it belongs.
    let upload = without_comments(include_str!("../core/src/xfer/upload.rs"));
    assert!(upload.contains("delete_file("), "the scan would have seen a call if there were one");
}

/// Rust source with its comments taken out, so a sentence that mentions a
/// deletion is not read as one. `://` is left alone: a URL is a string, not a
/// comment.
fn without_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_block = false;
    for line in src.lines() {
        let mut rest = line;
        loop {
            if in_block {
                match rest.find("*/") {
                    Some(i) => {
                        rest = &rest[i + 2..];
                        in_block = false;
                    }
                    None => {
                        rest = "";
                        break;
                    }
                }
            } else {
                match rest.find("/*") {
                    Some(i) => {
                        out.push_str(&rest[..i]);
                        rest = &rest[i + 2..];
                        in_block = true;
                    }
                    None => break,
                }
            }
        }
        if !in_block {
            let cut = rest.match_indices("//").find(|(i, _)| !rest[..*i].ends_with(':')).map(|(i, _)| i);
            out.push_str(match cut {
                Some(i) => &rest[..i],
                None => rest,
            });
        }
        out.push('\n');
    }
    out
}

#[test]
fn a_dry_run_of_a_sync_link_plans_both_folders_and_changes_nothing() {
    let w = World::new();
    let before = files_under(&w.media);
    let dirs_before = dirs_under(&w.media);
    w.stub.links(vec![sync_row(
        "L1",
        MARKER,
        json!({"mode": "add_only", "drive": {"local": "1_DRIVE"}, "exports": {"local": "0_EXPORTS", "folder_id": "FEXPORTS"}}),
    )]);
    let plan = w.rt.block_on(async { agent::preview(&w.app).await });
    assert!(plan.error.is_empty(), "{}", plan.error);
    assert_eq!(plan.ensured, 2, "two watches would be made");
    let want: BTreeSet<String> = ["1_DRIVE", "0_EXPORTS"]
        .iter()
        .map(|l| w.root.join(MARKER).join(l).to_string_lossy().into_owned())
        .collect();
    assert_eq!(plan.planned.into_iter().collect::<BTreeSet<_>>(), want, "both mapped folders, and no others");
    assert_eq!(plan.removed, 0);
    assert!(w.watches().is_empty(), "a dry run makes no watch");
    assert_eq!(files_under(&w.media), before, "and writes nothing");
    assert_eq!(dirs_under(&w.media), dirs_before, "not even a folder");
}

#[test]
fn a_link_without_sync_is_still_one_mirror() {
    // The brief's first rule, checked against the same stub as the sync cases:
    // no `sync` in the folder is today's behaviour, unchanged.
    let w = World::new();
    let mut row = sync_row("L1", MARKER, Value::Null);
    row["folder"].as_object_mut().unwrap().remove("sync");
    w.stub.links(vec![row]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let rows = w.portal_watches();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].direction, "mirror");
    assert_eq!(Path::new(&rows[0].local_path), w.root.join(MARKER), "one watch at root/marker");
    assert_eq!(rows[0].origin, "portal:L1");
}

// ---------- deterministic cases ----------

#[test]
fn a_tolerant_read_against_the_stub() {
    let w = World::new();
    // A bare array, a `null` where a string is expected, a folder as a string,
    // an unknown field — and the row is still read.
    w.stub.body(
        r#"[{"id":"L1","name":null,"target_folder_id":"F1","target_folder_name":null,
             "target_account_id":"A1","folder":"Reel One","turbosort":"Rakete",
             "something_new":true},
            {"id":"L2","name":null,"target_folder_id":"F2","target_folder_name":null,
             "target_account_id":null,"folder":null,"turbosort":" Other "}]"#,
    );
    let c = portal::PortalClient::new(&w.stub.base, "tok");
    let links = w.rt.block_on(async { c.upload_links().await }).expect("a tolerant read");
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].marker(), "Rakete");
    assert_eq!(links[0].label(), "Reel One", "the folder's name when the link has none");
    assert_eq!(links[1].marker(), "Other", "the marker is trimmed");
    assert_eq!(links[1].label(), "F2");
    assert_eq!(links[1].acct(), "", "a null account is an empty account, not a failed page");
    let first = w.log()[0].clone();
    assert_eq!(first.path, "/api/upload-links", "only the documented endpoint is called");
    assert_eq!(first.authorization.as_deref(), Some("Bearer tok"), "the bearer token authenticates the call");

    // A value of the wrong type is not silently dropped: the whole read fails,
    // so a link is never ignored without anyone hearing about it.
    w.stub.body(r#"[{"id":"L3","target_folder_id":"F3","target_account_id":"A1","turbosort":42}]"#);
    let err = w.rt.block_on(async { c.upload_links().await }).expect_err("a number is not a marker");
    assert!(err.to_string().contains("invalid type"), "{}", err);
}

#[test]
fn a_500_is_retried_and_a_401_changes_nothing() {
    let w = World::new();
    let before = files_under(&w.media);
    let spec = LinkSpec {
        id: "L1".into(),
        marker: "Rakete".into(),
        name: None,
        acct: Some("A1".into()),
        folder: Some("F1".into()),
        disabled: false,
    };
    w.stub.links(vec![spec.row()]);
    w.stub.failures(2);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "two 500s are retried, then the answer is read: {}", rep.error);
    assert_eq!(w.portal_watches().len(), 1);

    // Now Portal refuses the token: nothing changes, and it is said.
    w.stub.failures(0);
    w.stub.status(401);
    let snapshot = w.watches();
    let rep = w.reconcile();
    assert!(!rep.error.is_empty(), "a 401 is a failure");
    assert!(rep.error.contains("401"), "the status is in the message: {}", rep.error);
    assert_eq!(w.watches().len(), snapshot.len(), "a failed read changes no watch");
    assert_eq!(files_under(&w.media), before, "a failed read changes nothing local");
}

#[test]
fn a_broken_connection_changes_nothing() {
    let w = World::new();
    let spec = LinkSpec {
        id: "L1".into(),
        marker: "Rakete".into(),
        name: None,
        acct: Some("A1".into()),
        folder: Some("F1".into()),
        disabled: false,
    };
    w.stub.links(vec![spec.row()]);
    assert!(w.reconcile().error.is_empty());
    let before = w.watches();
    let files_before = files_under(&w.media);

    // Portal accepts the connection and then says nothing at all.
    w.stub.hangup(true);
    let rep = w.reconcile();
    assert!(!rep.error.is_empty(), "a reply that never comes is an error: {:?}", rep);
    assert_eq!(w.watches().len(), before.len(), "no watch was added or removed");
    assert_eq!(files_under(&w.media), files_before, "nothing local changed");
    assert_eq!(w.portal_watches().len(), 1, "the watch from the good run is still the only one");
}

/// A refused connection is retried six times with exponential backoff: about
/// 30 s, which is why it is not part of the default run (`cargo test -- --ignored`).
#[test]
#[ignore = "six attempts, ~30 s of backoff"]
fn portal_unreachable_changes_nothing() {
    let w = World::new();
    let before = w.watches();
    let files_before = files_under(&w.media);
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = dead.local_addr().unwrap();
    drop(dead);
    {
        let mut s = w.app.settings.lock().unwrap();
        s.portal_url = format!("http://{}", addr);
    }
    let rep = w.rt.block_on(async { w.app.portal_reconcile().await });
    assert!(!rep.error.is_empty(), "an unreachable Portal is an error: {:?}", rep);
    assert_eq!(w.watches().len(), before.len(), "no watch was added or removed");
    assert_eq!(files_under(&w.media), files_before, "nothing local changed");
}

#[test]
fn a_watch_the_user_made_on_the_same_folder_is_untouchable() {
    let w = World::new();
    // The user watches Live_Work/Reserved by hand; Portal then asks for it.
    let reserved = w.media.join("Live_Work/Reserved");
    std::fs::create_dir_all(&reserved).unwrap();
    {
        let _g = w.rt.enter();
        w.app
            .wat()
            .unwrap()
            .add(&reserved.to_string_lossy(), "A9", "F9", "Mine", "upload", "keep", false)
            .expect("the user's watch");
    }
    let mine = w.watches().into_iter().find(|r| r.local_path.ends_with("Reserved")).unwrap();
    assert_eq!(mine.origin, "", "a watch the user made has no origin");

    let spec = LinkSpec {
        id: "L1".into(),
        marker: "Reserved".into(),
        name: Some("Portal's idea of it".into()),
        acct: Some("A1".into()),
        folder: Some("F1".into()),
        disabled: false,
    };
    w.stub.links(vec![spec.row()]);
    let rep = w.reconcile();
    let still = w.watches().into_iter().find(|r| r.id == mine.id).expect("the user's watch is still there");
    assert_eq!(still.local_path, mine.local_path);
    assert_eq!(still.dest_label, "Mine", "not renamed");
    assert_eq!(still.direction, "upload", "not turned into a mirror");
    assert_eq!(still.on_change, "keep");
    assert_eq!(w.portal_watches().len(), 0, "Portal made no watch of its own");
    assert!(rep.skipped.iter().any(|s| s.reason.contains("a watch you made")), "the report says why: {:?}", rep.skipped);

    // When Portal stops asking, the user's watch stays.
    w.stub.links(vec![]);
    let rep = w.reconcile();
    assert_eq!(rep.removed, 0);
    assert!(w.watches().iter().any(|r| r.id == mine.id));
}

#[test]
fn the_change_rule_and_the_root_are_settings() {
    let w = World::new();
    let other_root = w.media.join("Elsewhere");
    {
        let mut s = w.app.settings.lock().unwrap();
        s.portal_on_change = "keep".to_string();
        s.portal_root = other_root.to_string_lossy().into_owned();
    }
    let spec = LinkSpec {
        id: "L1".into(),
        marker: "Rakete".into(),
        name: None,
        acct: Some("A1".into()),
        folder: Some("F1".into()),
        disabled: false,
    };
    w.stub.links(vec![spec.row()]);
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let row = &w.portal_watches()[0];
    assert_eq!(row.on_change, "keep");
    assert!(Path::new(&row.local_path).starts_with(&other_root), "the new root is used: {}", row.local_path);
    assert!(other_root.is_dir(), "the root is created when missing");
}

#[test]
fn set_portal_stores_the_settings_and_the_token_but_not_in_the_settings_file() {
    let w = World::new();
    let st = w
        .app
        .set_portal("https://portal.example.com/", "sekret", "~/Live_Work", true)
        .expect("the portal is set");
    assert_eq!(st.url, "https://portal.example.com");
    assert!(st.enabled);
    assert!(st.signed_in);
    assert_eq!(st.root, format!("{}/Live_Work", w.home.to_string_lossy()));

    let saved = std::fs::read_to_string(fiotransfer::config::path()).expect("settings.json");
    assert!(!saved.contains("sekret"), "the token is not in settings.json: {}", saved);
    assert!(saved.contains("portal.example.com"));
    assert_eq!(portal::token().as_deref(), Some("sekret"));

    // An empty token keeps the stored one; a bad URL is refused.
    let st = w.app.set_portal("https://portal.example.com", "", "", false).expect("still valid");
    assert_eq!(portal::token().as_deref(), Some("sekret"));
    assert!(!st.enabled);
    assert!(w.app.set_portal("portal.example.com", "", "", true).is_err(), "no scheme is an error");
    assert!(w.app.set_portal("file:///etc/passwd", "", "", true).is_err());
}

/// The Settings screen's URL field commits through `App::set_portal` with an
/// empty token (which keeps the stored one), so the URL alone can change without
/// the token ever reaching settings.json.
#[test]
fn saving_the_url_alone_never_writes_the_token_into_settings_json() {
    let w = World::new();
    w.app
        .set_portal("https://portal.example.com", "sekret", "~/Live_Work", true)
        .expect("the token is stored");
    let st = w
        .app
        .set_portal("https://portal.example.org", "", "", true)
        .expect("the address is saved");
    assert_eq!(st.url, "https://portal.example.org");
    assert!(st.signed_in, "the token is still stored");
    assert_eq!(portal::token().as_deref(), Some("sekret"));

    let saved = std::fs::read_to_string(fiotransfer::config::path()).expect("settings.json");
    assert!(!saved.contains("sekret"), "the token is not in settings.json: {saved}");
    assert!(saved.contains("portal.example.org"));
    // Nor in anything else the app keeps beside it.
    for entry in std::fs::read_dir(fiotransfer::config::dir()).expect("the data folder") {
        let path = entry.expect("an entry").path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(!text.contains("sekret"), "{} carries the token", path.display());
        }
    }
}

/// "Forget" in the Settings screen: the token goes, the status line's
/// `signed_in` goes with it, and no setting changes.
#[test]
fn forgetting_the_token_leaves_the_settings_alone() {
    let w = World::new();
    w.app
        .set_portal("https://portal.example.com", "sekret", "~/Live_Work", true)
        .expect("the portal is set");
    let before = std::fs::read_to_string(fiotransfer::config::path()).expect("settings.json");
    let st = w.app.forget_portal_token();
    assert!(!st.signed_in, "the flag the status line reads is cleared with the token");
    assert_eq!(portal::token(), None, "nothing is left to send with");
    let after = std::fs::read_to_string(fiotransfer::config::path()).expect("settings.json");
    assert_eq!(before, after, "forgetting a token is not a settings change");
    assert!(after.contains("portal.example.com"), "the address is still there");
}

#[test]
fn a_reconcile_without_a_token_says_so_and_changes_nothing() {
    let w = World::new();
    portal::clear_token();
    w.stub.links(vec![json!({"id":"L1","target_folder_id":"F1","target_account_id":"A1","turbosort":"Rakete"})]);
    let rep = w.reconcile();
    assert!(rep.error.contains("token"), "{}", rep.error);
    assert!(w.portal_watches().is_empty());
    assert!(w.log().is_empty(), "no call is made without a token");
}

#[test]
fn a_new_settings_file_without_the_portal_fields_still_loads() {
    // The fields added for Portal must not make an older settings.json fail to
    // parse: that would silently reset every other setting.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"client_id":"abc","redirect_port":8765,"streams":16,"segment_mb":64,
            "download_dir":"/tmp/d","no_folder_logs":false,"no_update_check":false,
            "update_notified":"","last_account_id":"","last_folder_id":"",
            "last_folder_label":"","last_folder_path":[]}"#,
    )
    .unwrap();
    let s: Settings = serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("an old file still loads");
    assert_eq!(s.client_id, "abc", "the old settings survive");
    assert_eq!(s.portal_url, "");
    assert!(!s.portal_enabled);
    assert_eq!(s.portal_on_change, "overwrite");
    assert_eq!(s.portal_every_secs, 30);
    assert!(s.portal_root.ends_with("Portal"));
    assert_eq!((s.portal_in_folder.as_str(), s.portal_out_folder.as_str()), ("IN", "OUT"));
}
