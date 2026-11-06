//! The heartbeat: what this machine tells Portal about itself, and when.
//!
//! These cases drive the real path — `App::portal_reconcile` ->
//! `portal::agent::reconcile` -> `portal::heartbeat::send` -> the same
//! `PortalClient` over HTTP to a stub Portal that records every request — and
//! read the payload back out of the wire, not out of the app's own structs
//! where they could agree with each other and both be wrong.
//!
//! What they hold:
//!
//!   H1  one successful reconcile is exactly one `POST /api/live-work/heartbeat`
//!       with the stored `client_id`, the crate's version, the machine's own
//!       `platform`, and one lane per Portal watch — the absolute path, the
//!       direction, the Frame.io folder and the project from the marker. A
//!       second reconcile sends another with the same id, and the user's own
//!       watch is never in the payload.
//!   H2  a heartbeat that fails — a 500, or a connection broken before the
//!       answer — changes nothing: the watch list is identical, the report
//!       carries no error, the status line's error stays empty, nothing reaches
//!       the transfer log, and a 500 is not retried.
//!   H3  nothing is sent without a credential or with Portal switched off: not
//!       one request leaves the machine.
//!
//! `HOME` and `FIO_NO_KEYCHAIN` are process-wide, so the cases in this binary
//! take turns, exactly as `tests/prop_portal.rs` does.
//!
//! Run with:
//!   CARGO_TARGET_DIR=<own dir> cargo test --offline --test heartbeat

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use fiotransfer::app::App;
use fiotransfer::config::Settings;
use fiotransfer::history::watch::Watch;
use fiotransfer::history::Store;
use fiotransfer::portal::{self, agent, heartbeat};
use fiotransfer::xfer::{self, manager as xfer_manager};

static ENV: Mutex<()> = Mutex::new(());

// ---------- the stub Portal ----------

#[derive(Debug, Clone)]
struct Hit {
    method: String,
    path: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

impl Hit {
    /// The body as the payload Portal's side would read, when this is a
    /// heartbeat.
    fn payload(&self) -> heartbeat::Heartbeat {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("the body is not a heartbeat ({}): {:?}", e, String::from_utf8_lossy(&self.body)))
    }
}

#[derive(Default)]
struct StubState {
    links: Mutex<String>,
    links_status: Mutex<u16>,
    /// What `POST /api/live-work/heartbeat` answers: 204 normally.
    hb_status: Mutex<u16>,
    /// Accept the connection, read the request, then close without answering.
    hb_hangup: Mutex<bool>,
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
        *state.hb_status.lock().unwrap() = 204;
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

    fn links(&self, rows: Vec<Value>) {
        *self.state.links.lock().unwrap() = json!({ "upload_links": rows }).to_string();
    }

    fn heartbeat_status(&self, code: u16) {
        *self.state.hb_status.lock().unwrap() = code;
    }

    fn heartbeat_hangup(&self, on: bool) {
        *self.state.hb_hangup.lock().unwrap() = on;
    }

    fn hits(&self) -> Vec<Hit> {
        self.state.hits.lock().unwrap().clone()
    }

    /// The heartbeats this run sent, in order.
    fn heartbeats(&self) -> Vec<Hit> {
        self.hits().into_iter().filter(|h| h.path == heartbeat::PATH).collect()
    }

    fn paths(&self) -> Vec<String> {
        self.hits().into_iter().map(|h| format!("{} {}", h.method, h.path)).collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Reads one whole request: the head, then `Content-Length` bytes of body. The
/// body is the point here — a heartbeat is its payload.
async fn read_request(c: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = c.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 1 << 20 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let want: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < head_end + want {
        let n = c.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = buf[head_end..(head_end + want).min(buf.len())].to_vec();
    Some((head, body))
}

async fn answer(mut c: TcpStream, st: Arc<StubState>) {
    let Some((head, body)) = read_request(&mut c).await else { return };
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("").to_string();
    let header = |name: &str| -> Option<String> {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case(name) {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
    };
    st.hits.lock().unwrap().push(Hit {
        method: method.clone(),
        path: path.clone(),
        authorization: header("authorization"),
        content_type: header("content-type"),
        body,
    });
    let bare = path.split('?').next().unwrap_or("");
    let (status, text) = match (method.as_str(), bare) {
        ("GET", "/api/upload-links") => (*st.links_status.lock().unwrap(), st.links.lock().unwrap().clone()),
        ("POST", heartbeat::PATH) => {
            if *st.hb_hangup.lock().unwrap() {
                return; // read the request, then close: a broken connection
            }
            let code = *st.hb_status.lock().unwrap();
            (code, if code == 204 { String::new() } else { r#"{"detail":"no"}"#.to_string() })
        }
        ("GET", "/api/sync-rules") => (200, r#"{"data":[]}"#.to_string()),
        _ => (404, r#"{"detail":"no route"}"#.to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        text.len(),
        text
    );
    let _ = c.write_all(resp.as_bytes()).await;
    let _ = c.flush().await;
}

// ---------- one case's world ----------

struct World {
    rt: tokio::runtime::Runtime,
    home: PathBuf,
    root: PathBuf,
    app: Arc<App>,
    hist: Arc<Store>,
    stub: Stub,
    _tmp: tempfile::TempDir,
    _env: MutexGuard<'static, ()>,
}

/// Portal's answers: a `sync` link (two one-way folders), a plain mirrored
/// link, and a link whose marker is not a folder name — that one is skipped by
/// the reconciler, so it must have no lane.
fn seeded_links() -> Vec<Value> {
    vec![
        json!({
            "id": "L1",
            "name": "YML Christmas",
            "target_folder_id": "F1",
            "target_folder_name": "Drive",
            "target_account_id": "A1",
            "turbosort": "20261028_YML-XMAS-Luc",
            "folder": {
                "id": "F1",
                "name": "Drive",
                "account_id": "A1",
                "sync": {
                    "mode": "add_only",
                    "drive": {"local": "1_DRIVE", "folder_id": "FD"},
                    "exports": {"local": "0_EXPORTS", "folder_id": "FX"}
                }
            }
        }),
        json!({
            "id": "L2",
            "name": "Rakete",
            "target_folder_id": "F2",
            "target_folder_name": "Rakete",
            "target_account_id": "A1",
            "turbosort": "Rakete"
        }),
        json!({
            "id": "L3",
            "name": "Broken",
            "target_folder_id": "F3",
            "target_account_id": "A1",
            "turbosort": "a/b"
        }),
    ]
}

impl World {
    fn new() -> World {
        let env = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("temp dir");
        let home = tmp.path().join("home");
        let data = home.join("Library/Application Support/FioTransfer");
        let logs = home.join("Library/Logs/FioTransfer");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&logs).unwrap();
        std::env::set_var("HOME", &home);
        std::env::set_var("FIO_NO_KEYCHAIN", "1");
        std::env::remove_var("FIO_TRACE"); // no debug line is what "silent" means

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let hist = Arc::new(Store::open(&data, &logs).expect("history store"));
        let root = tmp.path().join("media/Portal");
        let (stub, app, hist) = {
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
            // These cases are about the heartbeat, not about transferring: no
            // scan may run behind the assertions.
            watcher.close();
            *app.wat.lock().unwrap() = Some(watcher);
            *app.mgr.lock().unwrap() = Some(mgr);
            portal::set_token("test-token").expect("the portal token is stored");
            (stub, app, hist)
        };
        stub.links(seeded_links());
        World { rt, home, root, app, hist, stub, _tmp: tmp, _env: env }
    }

    /// A watch the user made: their own media folder, no `portal:` origin. It
    /// must never appear in a lane.
    fn seed_user_watch(&self) -> String {
        let _g = self.rt.enter();
        let dir = self._tmp.path().join("media/own-project");
        std::fs::create_dir_all(&dir).unwrap();
        let w = self.app.wat().expect("watcher");
        w.add(&dir.to_string_lossy(), "A1", "F9", "My own project", "mirror", "overwrite", false)
            .expect("a user's own watch");
        self.hist
            .watches()
            .expect("watches")
            .into_iter()
            .find(|r| r.local_path == dir.to_string_lossy())
            .expect("it is recorded")
            .id
    }

    fn reconcile(&self) -> agent::Report {
        self.rt.block_on(async { self.app.portal_reconcile().await })
    }

    fn watches(&self) -> Vec<Watch> {
        self.hist.watches().expect("watches")
    }

    /// The watches, as the fields a heartbeat must not change.
    fn watch_snapshot(&self) -> Vec<(String, String, String, String, String, String)> {
        self.watches()
            .into_iter()
            .map(|w| (w.id, w.local_path, w.folder_id, w.direction, w.on_change, w.origin))
            .collect()
    }

    /// Everything in the transfer log — the one thing a person reads.
    fn transfer_log(&self) -> String {
        std::fs::read_to_string(self.hist.log_path()).unwrap_or_default()
    }

    fn settings_file(&self) -> String {
        std::fs::read_to_string(self.home.join("Library/Application Support/FioTransfer/settings.json")).unwrap_or_default()
    }
}

/// A uuid v4 as a shape, checked here rather than with a uuid crate.
fn is_uuid_v4(s: &str) -> bool {
    s.len() == 36
        && s.split('-').map(|g| g.len()).collect::<Vec<_>>() == vec![8, 4, 4, 4, 12]
        && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        && &s[14..15] == "4"
        && ['8', '9', 'a', 'b'].contains(&s.chars().nth(19).unwrap())
}

// ---------- H1: what is sent ----------

#[test]
fn one_reconcile_sends_one_heartbeat_with_the_stored_client_id() {
    let w = World::new();
    w.seed_user_watch();
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    assert_eq!(rep.ensured, 3, "L1's two folders and L2's one: {:?}", rep.skipped);

    let sent = w.stub.heartbeats();
    assert_eq!(sent.len(), 1, "one reconcile, one heartbeat: {:?}", w.stub.paths());
    let hit = &sent[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.authorization.as_deref(), Some("Bearer test-token"), "the credential the client already uses");
    assert_eq!(hit.content_type.as_deref(), Some("application/json"));

    let p = hit.payload();
    assert!(is_uuid_v4(&p.client_id), "{} is not a uuid v4", p.client_id);
    assert_eq!(p.client_id, w.app.settings().portal_client_id, "the id in the payload is the stored one");
    assert!(w.settings_file().contains(&p.client_id), "the id is in settings.json, so a restart keeps it");
    assert_eq!(p.app_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(p.app_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(p.platform, format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH));
    assert!(!p.name.trim().is_empty(), "a client with no name is no use in a list");
    assert!(!p.name.contains('\n'), "{}", p.name);

    // One lane per Portal watch, and none for the user's own.
    let paths: Vec<&str> = p.lanes.iter().map(|l| l.path.as_str()).collect();
    let root = w.root.to_string_lossy().into_owned();
    let drive = format!("{}/20261028_YML-XMAS-Luc/1_DRIVE", root);
    let exports = format!("{}/20261028_YML-XMAS-Luc/0_EXPORTS", root);
    let rakete = format!("{}/Rakete", root);
    let mut want = vec![exports.as_str(), drive.as_str(), rakete.as_str()];
    want.sort();
    assert_eq!(paths, want, "sorted by path: {:?}", paths);
    assert!(p.lanes.iter().all(|l| !l.path.contains("own-project")), "the user's watch is not Portal's business: {:?}", paths);
    assert!(p.lanes.iter().all(|l| l.project != "a"), "the skipped link's marker is not a lane");

    let lane = |path: &str| p.lanes.iter().find(|l| l.path == path).cloned().expect(path);
    let d = lane(&drive);
    assert_eq!(d.link_id, "L1");
    assert_eq!(d.project, "20261028_YML-XMAS-Luc", "the project is the link's turbosort marker");
    assert_eq!(d.local, "1_DRIVE");
    assert_eq!(d.direction, "download");
    assert_eq!(d.folder_id, "FD", "the Frame.io folder that lane moves from");

    let x = lane(&exports);
    assert_eq!(x.link_id, "L1", "one link, two lanes; the #exports suffix is ours, not Portal's");
    assert_eq!(x.local, "0_EXPORTS");
    assert_eq!(x.direction, "upload");
    assert_eq!(x.folder_id, "FX");

    let r = lane(&rakete);
    assert_eq!(r.link_id, "L2");
    assert_eq!(r.project, "Rakete");
    assert_eq!(r.local, "Rakete");
    assert_eq!(r.direction, "mirror");
    assert_eq!(r.folder_id, "F2");

    // The lanes are the watches: same absolute paths, one per Portal watch, and
    // the watcher was not changed by describing it.
    let portal_watches: Vec<Watch> = w.watches().into_iter().filter(|r| agent::link_of(&r.origin).is_some()).collect();
    assert_eq!(portal_watches.len(), 3);
}

#[test]
fn a_second_reconcile_sends_another_with_the_same_client_id() {
    let w = World::new();
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let second = w.reconcile();
    assert!(second.error.is_empty(), "{}", second.error);
    assert_eq!(second.ensured, 0, "nothing changed the second time");

    let sent = w.stub.heartbeats();
    assert_eq!(sent.len(), 2, "the tick says this machine is alive even when nothing changed: {:?}", w.stub.paths());
    let (a, b) = (sent[0].payload(), sent[1].payload());
    assert_eq!(a.client_id, b.client_id, "one machine is one client, restart after restart");
    assert!(is_uuid_v4(&b.client_id), "{}", b.client_id);
    assert_eq!(a.lanes, b.lanes, "the same lanes, in the same order");
    assert_eq!(a.app_version, b.app_version);
}

/// The startup heartbeat is the same call the poller makes: one POST of the same
/// payload, and nothing else on the wire.
#[test]
fn the_startup_heartbeat_is_one_post_of_the_same_payload() {
    let w = World::new();
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let after_first = w.stub.hits().len();

    let attempt = w.rt.block_on(async { heartbeat::send(&w.app, "startup").await });
    assert!(attempt.sent, "{}", attempt.note);
    assert_eq!(attempt.lanes, 3);
    let hits = w.stub.hits();
    assert_eq!(hits.len(), after_first + 1, "one more request, nothing else: {:?}", w.stub.paths());
    assert_eq!(hits.last().map(|h| (h.method.as_str(), h.path.as_str())), Some(("POST", heartbeat::PATH)));
}

// ---------- H2: a heartbeat that fails changes nothing ----------

#[test]
fn a_500_from_the_heartbeat_route_changes_no_watch_and_is_not_retried() {
    let w = World::new();
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let before = w.watch_snapshot();
    let first_id = w.stub.heartbeats()[0].payload().client_id;
    let log_before = w.transfer_log();
    w.stub.heartbeat_status(500);

    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "a site being down is not the user's problem: {}", rep.error);
    assert_eq!(w.watch_snapshot(), before, "the watch list is exactly what it was");

    let sent = w.stub.heartbeats();
    assert_eq!(sent.len(), 2, "the second reconcile tried once: {:?}", w.stub.paths());
    assert_eq!(sent[1].payload().client_id, first_id, "the same client, even when Portal refuses the row");
    // A 5xx on a POST may have been acted on: it is reported, not repeated.
    let posts = w.stub.hits().iter().filter(|h| h.method == "POST").count();
    assert_eq!(posts, 2, "one POST per reconcile, no retry: {:?}", w.stub.paths());

    // And nothing a person reads says a word about it.
    assert_eq!(w.transfer_log(), log_before, "no line was added to the transfer log");
    assert!(!w.transfer_log().contains("heartbeat"));
    assert!(w.app.portal_status().error.is_empty(), "the status line carries the reconcile's own error");
    let report = serde_json::to_value(w.app.portal_status()).expect("a status");
    assert!(report.get("heartbeat").is_none(), "the status carries no heartbeat field");
}

#[test]
fn a_broken_connection_to_the_heartbeat_route_changes_no_watch() {
    let w = World::new();
    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "{}", rep.error);
    let before = w.watch_snapshot();
    let log_before = w.transfer_log();
    w.stub.heartbeat_hangup(true);

    let rep = w.reconcile();
    assert!(rep.error.is_empty(), "a connection that closes changes no reconcile: {}", rep.error);
    assert_eq!(w.watch_snapshot(), before);
    let posts = w.stub.hits().iter().filter(|h| h.method == "POST").count();
    assert_eq!(posts, 2, "the reconcile still read its links: {:?}", w.stub.paths());
    assert_eq!(w.transfer_log(), log_before, "and said nothing in the log");
}

// ---------- H3: when nothing is sent ----------

#[test]
fn no_heartbeat_without_a_credential() {
    let w = World::new();
    portal::clear_token();
    let rep = w.reconcile();
    assert!(rep.error.to_lowercase().contains("token"), "{}", rep.error);
    assert!(w.stub.hits().is_empty(), "not one request without a credential: {:?}", w.stub.paths());
    assert_eq!(w.app.settings().portal_client_id, "", "and no client id is minted");
}

#[test]
fn no_heartbeat_when_portal_is_switched_off() {
    let w = World::new();
    w.app.settings.lock().unwrap().portal_enabled = false;
    let attempt = w.rt.block_on(async { heartbeat::send(&w.app, "startup").await });
    assert!(!attempt.sent, "{}", attempt.note);
    assert_eq!(attempt.lanes, 0);
    assert!(w.stub.hits().is_empty(), "{:?}", w.stub.paths());
    assert_eq!(w.app.settings().portal_client_id, "", "a machine with Portal off has no client id");
}

#[test]
fn no_heartbeat_without_an_address() {
    let w = World::new();
    w.app.settings.lock().unwrap().portal_url.clear();
    let attempt = w.rt.block_on(async { heartbeat::send(&w.app, "startup").await });
    assert!(!attempt.sent, "{}", attempt.note);
    assert!(w.stub.hits().is_empty(), "{:?}", w.stub.paths());
}

/// A reconcile that fails changes no watch **and** sends no heartbeat: there is
/// nothing true to report about a run that could not read Portal.
#[test]
fn a_failed_reconcile_sends_no_heartbeat() {
    let w = World::new();
    *w.stub.state.links_status.lock().unwrap() = 401;
    let rep = w.reconcile();
    assert!(!rep.error.is_empty(), "a refused read is an error");
    assert!(w.stub.heartbeats().is_empty(), "nothing was sent: {:?}", w.stub.paths());
    assert!(w.watches().is_empty());
}
