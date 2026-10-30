//! Property tests for the Space view's delete promise: deleting a project on
//! Frame.io NEVER touches anything local.
//!
//! The delete path is driven for real — `App::delete_project`, which is what the
//! bridge's `DeleteProject` arm calls — with a *real* `Client` whose `base_url`
//! points at a one-thread stub HTTP server. The API replies are simulated; every
//! line of production code under test (URL building, rate limiting, retries,
//! auth, the history log, the Space cache) is the real one. `App::with_api` is
//! the seam that supplies that client; it lives on the core's `App`
//! (`core/src/app.rs`) and is re-exported here unchanged.
//!
//! unix only: the sandbox is built with `std::fs::symlink`, which needs
//! `SeCreateSymbolicLinkPrivilege` on Windows (developer mode), and the model
//! records links as their own kind of entry. Nothing here is verified on
//! Windows yet; the CI job only packages there.
//!
//! Each case gets a fresh sandbox: a media tree of sentinel files (a fake
//! watch-folder layout, nested folders, a symlink) that is byte-snapshotted
//! before and after, an empty history log, and a Space cache seeded by walking
//! a stub account tree (`refresh_space`).
//!
//! The reference model reads the wire, not the app: whether a delete happened is
//! taken from the stub's request log (a 2xx DELETE), so the model cannot inherit
//! the implementation's own idea of success. The whole sequence runs with the
//! process's working directory inside the case's sandbox, so an id like `.` or
//! `..` reaches the sandbox and nothing else even when the code under test is
//! mutated to treat ids as paths.
//!
//! Run with:
//!   CARGO_TARGET_DIR=/tmp/target-delete cargo test --offline --test prop_delete
//!
//! `#![cfg(unix)]` below keeps the whole file out of a Windows build.
//!
//! Nothing here sets an environment variable: a test thread that did would race
//! with the other test function in this binary. `config::dir()` is therefore the
//! caller's HOME, so the mutation calibration in the report runs this file with
//! `HOME=<scratch>` in the *cargo* environment rather than patching it in.
//!
//! The three probes at the bottom began as `#[ignore]`d reproductions for
//! findings F1 (a project id that could change the delete route), F2 (cache
//! eviction ignoring the account) and F3 (the log line not sanitised). The fixes
//! landed with them, so they now run with the rest of the file: they are the
//! regression tests for those three fixes, and leaving them ignored would mean
//! nothing in CI guarded the code they exist to protect.

#![cfg(unix)] // the sandbox needs a symlink: see the header

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proptest::collection::vec as pvec;
use proptest::prelude::*;
use sha2::{Digest, Sha256};

use fiotransfer::api::Client;
use fiotransfer::app::App;
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history::Store;

const PHRASE: &str = "nothing local was touched";

/// Held while a case's sequence runs: `set_current_dir` is process-wide, and
/// `cargo test` runs the test functions in this binary on separate threads.
static CWD_LOCK: Mutex<()> = Mutex::new(());

// ---------- the sandbox ----------

/// The media tree every case seeds. Nothing here is ever the app's business, so
/// every byte of it must survive any sequence of deletes.
fn seed_media(root: &Path) {
    std::fs::create_dir_all(root.join("media/sub/sub2")).unwrap();
    std::fs::create_dir_all(root.join("media/watch/In")).unwrap();
    std::fs::create_dir_all(root.join("media/watch/Out")).unwrap();
    std::fs::create_dir_all(root.join("media/empty_dir")).unwrap();
    let files: [(&str, &[u8]); 7] = [
        ("media/keep.MOV", b"\x00\x01sentinel master\xff\xfe"),
        ("media/sub/deep.bin", b"deep sentinel bytes"),
        ("media/sub/sub2/leaf.txt", b"leaf\n"),
        ("media/watch/In/upload_me.mov", b"watch folder input"),
        ("media/watch/In/.DS_Store", b"finder junk"),
        ("media/watch/Out/done.mov", b"watch folder output"),
        ("media/watch/upload.fiopart", b"partial"),
    ];
    for (p, b) in files {
        std::fs::write(root.join(p), b).unwrap();
    }
    std::os::unix::fs::symlink("keep.MOV", root.join("media/link_to_keep")).unwrap();
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
enum Entry {
    Dir,
    File(u64, String),
    Link(String),
}

/// Every path under `root`, with its content hash: a rename, a removal, an
/// addition or a single flipped byte all show up as an inequality.
fn snap(root: &Path) -> BTreeMap<String, Entry> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Entry>) {
        let mut kids: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        kids.sort();
        for p in kids {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let md = std::fs::symlink_metadata(&p).unwrap();
            if md.file_type().is_symlink() {
                let t = std::fs::read_link(&p).unwrap();
                out.insert(rel, Entry::Link(t.to_string_lossy().into_owned()));
            } else if md.is_dir() {
                out.insert(rel, Entry::Dir);
                walk(root, &p, out);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                let mut h = Sha256::new();
                h.update(&bytes);
                let hex: String = h.finalize().iter().map(|b| format!("{:02x}", b)).collect();
                out.insert(rel, Entry::File(bytes.len() as u64, hex));
            }
        }
    }
    let mut out = BTreeMap::new();
    match std::fs::read_dir(root) {
        Ok(_) => walk(root, root, &mut out),
        // A case whose root was deleted still needs a comparable snapshot.
        Err(e) => {
            out.insert("<root>".to_string(), Entry::Link(format!("unreadable: {}", e)));
        }
    }
    out
}

// ---------- the stub Frame.io ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// DELETE is answered 200: Frame.io says the project is gone.
    Deletes,
    /// DELETE is answered 404: the project is not there, or not allowed.
    Refuses,
}

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    status: u16,
}

struct Stub {
    base: String,
    reqs: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    fn start(mode: Mode, collide: bool) -> Stub {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap();
        let reqs = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let r = reqs.clone();
        let s = stop.clone();
        let join = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((c, _)) => {
                        let _ = c.set_nonblocking(false);
                        if let Some(req) = answer(c, mode, collide) {
                            r.lock().unwrap().push(req);
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Stub { base: format!("http://{}", addr), reqs, stop, join: Some(join) }
    }

    fn log(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// One request in, one reply out, then close (with `Connection: close`, so
/// reqwest never reuses a connection and every request is one `Req`).
fn answer(mut c: TcpStream, mode: Mode, collide: bool) -> Option<Req> {
    c.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut r = BufReader::new(c.try_clone().ok()?);
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let method = line.split(' ').next().unwrap_or("").to_string();
    let path = line.split(' ').nth(1).unwrap_or("").to_string();
    let mut content_length = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        let _ = r.read_exact(&mut body);
    }

    // Frame.io reports its own limits; the stub reports generous ones so the
    // client's token bucket never makes a case wait.
    let limits = "x-ratelimit-limit: 100000\r\nx-ratelimit-window: 1000\r\n";
    let bare = path.split('?').next().unwrap_or("");
    let (status, body) = if method == "DELETE" {
        match mode {
            Mode::Deletes => (200u16, String::new()),
            Mode::Refuses => (404u16, r#"{"errors":[{"title":"not found"}]}"#.to_string()),
        }
    } else if bare == "/v4/accounts" {
        (
            200,
            r#"{"data":[{"id":"acct","display_name":"Account One"},{"id":"acct2","display_name":"Account Two"}],"links":{"next":null}}"#
                .to_string(),
        )
    } else if bare.starts_with("/v4/accounts/") && bare.ends_with("/workspaces") {
        (200, r#"{"data":[{"id":"ws","name":"Workspace"}],"links":{"next":null}}"#.to_string())
    } else if bare.starts_with("/v4/accounts/") && bare.ends_with("/projects") {
        let acct = bare.split('/').nth(3).unwrap_or("");
        let body = if acct == "acct" {
            r#"{"data":[{"id":"projA","name":"A","root_folder_id":"rootA"},{"id":"projB","name":"B","root_folder_id":"rootB"}],"links":{"next":null}}"#
        } else if acct == "acct2" && collide {
            // The same project id in a second account: finding F2.
            r#"{"data":[{"id":"projA","name":"A copy","root_folder_id":"rootA"}],"links":{"next":null}}"#
        } else if acct == "acct2" {
            r#"{"data":[{"id":"projC","name":"C","root_folder_id":"rootA"}],"links":{"next":null}}"#
        } else {
            r#"{"data":[],"links":{"next":null}}"#
        };
        (200, body.to_string())
    } else if bare.starts_with("/v4/accounts/") && bare.ends_with("/children") {
        (200, r#"{"data":[{"id":"f1","name":"one.mov","type":"file","file_size":7}],"links":{"next":null}}"#.to_string())
    } else {
        (404, r#"{"errors":[{"title":"no route"}]}"#.to_string())
    };
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
        status,
        if status == 200 { "OK" } else { "Not Found" },
        body.len(),
        limits
    );
    let _ = c.write_all(resp.as_bytes());
    let _ = c.write_all(body.as_bytes());
    let _ = c.flush();
    Some(Req { method, path, status })
}

// ---------- one case ----------

struct World {
    tmp: Option<tempfile::TempDir>,
    root: PathBuf,
    stub: Stub,
    app: Arc<App>,
    rt: tokio::runtime::Runtime,
}

impl World {
    /// `collide` seeds a second account that holds the same project id, which is
    /// what finding F2 needs.
    fn new(mode: Mode, collide: bool) -> World {
        let tmp = tempfile::TempDir::new().unwrap();
        // One level down: the case runs with its working directory inside
        // `sandbox`, so even the id `..` can only reach this case's own tmp dir.
        std::fs::create_dir_all(tmp.path().join("sandbox")).unwrap();
        let root = std::fs::canonicalize(tmp.path().join("sandbox")).unwrap();
        seed_media(&root);
        let data = root.join("Library/Application Support/FioTransfer");
        let logs = root.join("Library/Logs/FioTransfer");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&logs).unwrap();

        let stub = Stub::start(mode, collide);
        let mut c = Client::new(AuthManager::new_static("test-token"));
        c.base_url = stub.base.clone();
        let app = App::with_api(Settings::default(), Arc::new(c));
        *app.hist.lock().unwrap() = Some(Arc::new(Store::open(&data, &logs).expect("history store")));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        World { tmp: Some(tmp), root, stub, app, rt }
    }

    fn media(&self) -> PathBuf {
        self.root.join("media")
    }

    /// Everything outside `Library/`: the log and the store live there, and the
    /// log is *supposed* to grow. Nothing else may change at all.
    fn outside_library(&self) -> BTreeMap<String, Entry> {
        snap(&self.root).into_iter().filter(|(k, _)| !k.starts_with("Library")).collect()
    }

    /// Seeds the Space cache the way the view does: a real walk of the stub.
    fn refresh(&self) {
        self.rt.block_on(self.app.refresh_space());
    }

    fn cache(&self) -> BTreeSet<(String, String)> {
        self.app.space().projects.iter().map(|p| (p.account_id.clone(), p.project_id.clone())).collect()
    }

    fn log_lines(&self) -> Vec<String> {
        let p = self.app.hist().unwrap().log_path();
        match std::fs::read_to_string(&p) {
            Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).map(|l| l.to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn delete(&self, acct: &str, project: &str) -> Result<(), String> {
        self.rt.block_on(self.app.delete_project(acct, project))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // A mutated delete may have taken the sandbox itself; TempDir's own drop
        // copes, this only keeps the field alive for the whole case.
        let _ = &self.tmp;
    }
}

#[derive(Debug, Clone)]
struct Op {
    acct: String,
    project: String,
}

fn acct_strat() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => Just("acct".to_string()),
        1 => Just(String::new()),
        1 => Just("other".to_string()),
        1 => Just("acct/../../v4/me".to_string()),
    ]
}

/// Ids Frame.io could really hand over, plus the ids that must not be able to
/// reach the local disk: the sandbox paths themselves, the sandbox root, the
/// separators and the dot segments.
fn project_strat() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => Just("projA".to_string()),
        3 => Just("projB".to_string()),
        3 => Just(String::new()),
        1 => Just(".".to_string()),
        1 => Just("..".to_string()),
        1 => Just("../files/other".to_string()),
        1 => Just("../../v4/me".to_string()),
        1 => Just("projA/../projB".to_string()),
        1 => Just("a/b".to_string()),
        1 => Just("%2e%2e%2fprojB".to_string()),
        1 => Just("{SBX}/media/watch".to_string()),
        1 => Just("{SBX}/media".to_string()),
        1 => Just("{SBX}/media/keep.MOV".to_string()),
        1 => Just("{SBX}".to_string()),
        1 => Just("media/watch".to_string()),
        1 => Just(" projA ".to_string()),
        1 => Just("\u{1D11E}".to_string()),
        1 => "[a-z]{1,40}".prop_map(|s| s),
    ]
}

fn op_strat() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => (acct_strat(), project_strat()).prop_map(|(acct, project)| Op { acct, project }),
        // A delete in the account that really holds a row (the row is
        // `acct2 / projC`).
        2 => Just(Op { acct: "acct2".to_string(), project: "projC".to_string() }),
    ]
}

fn seq_strat() -> impl Strategy<Value = Vec<Op>> {
    pvec(op_strat(), 0..=8)
}

/// Materialises the placeholder id that stands for a real sandbox path.
fn ops(seq: Vec<Op>, root: &Path) -> Vec<Op> {
    let root = root.to_string_lossy().into_owned();
    seq.into_iter().map(|o| Op { acct: o.acct, project: o.project.replace("{SBX}", &root) }).collect()
}

/// Runs `f` with the process's working directory inside the case's sandbox, so
/// a relative id (`.`, `..`, `media/watch`) can only ever reach the sandbox.
fn in_sandbox<F: FnOnce() -> R, R>(root: &Path, f: F) -> R {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let here = std::env::current_dir().unwrap();
    std::env::set_current_dir(root).unwrap();
    let out = f();
    let _ = std::env::set_current_dir(&here);
    out
}

fn check_sequence(seq: Vec<Op>, mode: Mode) -> Result<(), TestCaseError> {
    let w = World::new(mode, false);
    let seq = ops(seq, &w.root);
    in_sandbox(&w.root, || run_sequence(&w, &seq))
}

/// The three promises, checked over one generated sequence.
fn run_sequence(w: &World, seq: &[Op]) -> Result<(), TestCaseError> {
    w.refresh();
    let seeded = w.cache();
    assert_eq!(
        seeded,
        BTreeSet::from([
            ("acct".to_string(), "projA".to_string()),
            ("acct".to_string(), "projB".to_string()),
            ("acct2".to_string(), "projC".to_string()),
        ]),
        "the stub's account tree seeds three Space rows"
    );

    let media_before = snap(&w.media());
    let outside_before = w.outside_library();

    // The model: which rows each accepted delete takes out of the cache, and
    // which log lines it must leave.
    let mut model_cache = seeded.clone();
    let mut model_lines: Vec<String> = Vec::new();
    let mut prev_cache = seeded.clone();

    for (i, op) in seq.iter().enumerate() {
        let n0 = w.stub.log().len();
        let res = w.delete(&op.acct, &op.project);
        let wire: Vec<Req> = w.stub.log()[n0..].to_vec();
        let deletes: Vec<&Req> = wire.iter().filter(|r| r.method == "DELETE").collect();

        prop_assert!(
            wire.len() <= 8 && deletes.len() <= 1,
            "op {} (account {:?}, project {:?}) sent {} requests: {:?}",
            i, op.acct, op.project, wire.len(), wire
        );
        // The oracle is the wire, not the return value.
        let accepted = deletes.iter().any(|r| (200..300).contains(&r.status));
        prop_assert_eq!(
            res.is_ok(),
            accepted,
            "op {} (account {:?}, project {:?}): app said {:?} but the wire said {:?}",
            i, op.acct, op.project, res, wire
        );

        if accepted {
            // The eviction is by (account, project): the same project id can
            // live in a second account, and that account's row is a different
            // project. (Before finding F2 it was by id alone, and this model
            // said so; the account-aware model is
            // `one_accounts_delete_keeps_the_other_accounts_row` below.)
            model_cache.retain(|(a, p)| !(a == &op.acct && p == &op.project));
            model_lines.push(op.project.clone());
        } else {
            // The failure path: nothing local, and the row is still there.
            prop_assert_eq!(
                w.cache(),
                prev_cache.clone(),
                "a failed delete changed the Space cache (account {:?}, project {:?})",
                op.acct,
                op.project
            );
        }
        prev_cache = w.cache();
    }

    // ---- promise 1: the media tree is byte-identical, with nothing added,
    // renamed or removed ----
    prop_assert_eq!(snap(&w.media()), media_before, "the media tree changed after {:?}", seq);
    prop_assert_eq!(
        w.outside_library(),
        outside_before,
        "something outside the log/store directories changed after {:?}",
        seq
    );

    // ---- promise 2: the Space cache follows the model ----
    prop_assert_eq!(
        w.cache(),
        model_cache,
        "Space cache after {:?} (wire-accepted: {:?})",
        seq,
        model_lines
    );

    // ---- promise 3: one log line per accepted delete, naming the project and
    // saying nothing local was touched ----
    //
    // A *refused* delete (an id that is not one path segment) logs its own
    // line, saying it was refused, so the log is filtered to the deletes the
    // wire accepted before it is compared with the model.
    let lines: Vec<String> = w
        .log_lines()
        .into_iter()
        .filter(|line| line.contains("deleted project"))
        .collect();
    prop_assert_eq!(
        lines.len(),
        model_lines.len(),
        "expected {} delete log lines, got {:?}",
        model_lines.len(),
        lines
    );
    for (line, project) in lines.iter().zip(model_lines.iter()) {
        prop_assert!(line.contains("deleted project"), "log line does not say what happened: {:?}", line);
        prop_assert!(
            line.contains(PHRASE),
            "log line does not say nothing local was touched: {:?}",
            line
        );
        if !project.is_empty() {
            prop_assert!(line.contains(project.as_str()), "log line does not name project {:?}: {:?}", project, line);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, max_shrink_iters: 6000, ..ProptestConfig::default() })]

    /// Frame.io says yes: the delete is recorded, the row goes, the disk is
    /// untouched.
    #[test]
    fn deleting_a_project_never_touches_a_local_file(seq in seq_strat()) {
        check_sequence(seq, Mode::Deletes)?;
    }

    /// Frame.io says no: the error reaches the caller, nothing local changes,
    /// nothing is logged, and the row stays in the cache.
    #[test]
    fn a_refused_delete_changes_nothing_and_keeps_the_row(seq in seq_strat()) {
        check_sequence(seq, Mode::Refuses)?;
    }
}

// ---------- findings, kept as reproductions ----------

/// Finding F2, now a regression test: the cache
/// eviction is by project id alone, so deleting a project in one account also
/// drops the row another account holds for the same id. Real Frame.io project
/// ids are UUIDs, so this is unreachable through the Space view as shipped; it
/// is a latent coupling, not a live data-loss bug.
#[test]
fn one_accounts_delete_keeps_the_other_accounts_row() {
    let w = World::new(Mode::Deletes, true);
    in_sandbox(&w.root, || {
        w.refresh();
        assert_eq!(
            w.cache(),
            BTreeSet::from([
                ("acct".to_string(), "projA".to_string()),
                ("acct".to_string(), "projB".to_string()),
                ("acct2".to_string(), "projA".to_string()),
            ])
        );
        // Delete projA in `acct` only. The row in `acct2` is a different project.
        w.delete("acct", "projA").expect("stub says the delete went through");
        let want: BTreeSet<(String, String)> = BTreeSet::from([
            ("acct".to_string(), "projB".to_string()),
            ("acct2".to_string(), "projA".to_string()),
        ]);
        assert_eq!(w.cache(), want, "projA in acct2 is still on Frame.io, so its row must stay");
    });
}

/// Finding F1, now a regression test:
/// `Client::delete_project` pastes the project id straight into the URL. Nothing
/// local is touched (the promise this suite is about holds), but the id can
/// change which Frame.io resource the DELETE reaches: `..` collapses to a
/// different route, a separator adds a path segment, an empty id leaves the
/// collection route.
#[test]
fn a_project_id_cannot_change_the_delete_route() {
    proptest!(ProptestConfig { cases: 32, ..ProptestConfig::default() }, |(id in project_strat())| {
        let w = World::new(Mode::Deletes, false);
        let id = id.replace("{SBX}", &w.root.to_string_lossy());
        let n0 = w.stub.log().len();
        let res = w.delete("acct", &id);
        let paths: Vec<String> = w.stub.log()[n0..]
            .iter()
            .filter(|r| r.method == "DELETE")
            .map(|r| r.path.clone())
            .collect();
        // A delete may be refused before it is sent (an id with no segment at
        // all); what it must never do is send a DELETE somewhere else.
        prop_assert!(paths.len() <= 1, "id {:?} sent {} DELETEs: {:?}", id, paths.len(), paths);
        if paths.is_empty() {
            prop_assert!(res.is_err(), "id {:?} reported success without a request", id);
            return Ok(());
        }
        let p = &paths[0];
        let tail = p
            .strip_prefix("/v4/accounts/acct/projects/")
            .unwrap_or_else(|| panic!("DELETE for id {:?} left the project route: {:?}", id, p));
        prop_assert!(!tail.is_empty(), "DELETE for id {:?} addressed the collection: {:?}", id, p);
        prop_assert!(
            !tail.contains('/') && !tail.contains(".."),
            "DELETE for id {:?} put a path segment into the route: {:?}",
            id,
            p
        );
    });
}

/// Finding F3, now a regression test: the log
/// line was built with the raw project id, so an id containing a newline wrote a
/// second, forged line into the transfer log (and an empty id left a line that
/// named nothing).
///
/// Amended for the fix, and the amendment is the fix: such an id is now refused
/// before anything is sent (finding F1's refusal, whose predicate also rejects
/// control characters), and the refusal is logged with the id's control
/// characters escaped. One attempt is therefore still exactly one line, and no
/// attempt can forge a second one. The delete is an *error* rather than a
/// success now — refusing it is the point.
#[test]
fn one_delete_writes_exactly_one_log_line() {
    let w = World::new(Mode::Deletes, false);
    let id = "p1\nSPACE    deleted project X on Frame.io (nothing local was touched)";
    let err = w.delete("acct", id).expect_err("an id carrying a newline is refused");
    assert!(err.contains("one path segment"), "the refusal says why: {err}");
    let lines = w.log_lines();
    assert_eq!(lines.len(), 1, "one attempt wrote {} log lines: {:#?}", lines.len(), lines);
    assert!(lines[0].contains(PHRASE));
    assert!(
        !lines[0].contains('\n'),
        "the id's newline was written raw, so it could forge a line: {:?}",
        lines[0]
    );
}

/// The regression test for finding F1 as fixed: an id that is not exactly one
/// path segment is refused, logged, and produces no request at all.
///
/// These are the ids the URL parser would have re-pointed at another Frame.io
/// resource — `""` and `"."` to the project collection, `".."` to the account,
/// `"../files/x"` to a *file* route — plus a separator, a `%2e` (which is a dot
/// by the time the route is parsed, so encoding is not a fix) and a control
/// character (finding F3).
#[test]
fn the_ids_that_are_not_one_path_segment_are_refused_before_any_request() {
    let w = World::new(Mode::Deletes, false);
    in_sandbox(&w.root, || {
        w.refresh();
        let before = w.cache();
        for id in [
            "",
            ".",
            "..",
            "../files/x",
            "projA/../projB",
            "a/b",
            "a\\b",
            "%2e%2e",
            "p1\nx",
        ] {
            let n0 = w.stub.log().len();
            let err = w
                .delete("acct", id)
                .expect_err(&format!("id {:?} must be refused", id));
            assert!(
                err.contains("one path segment"),
                "id {:?}: the refusal says why: {}",
                id,
                err
            );
            assert_eq!(w.stub.log().len(), n0, "id {:?} sent a request", id);
            assert_eq!(w.cache(), before, "id {:?} changed the Space cache", id);
        }
    });
}

