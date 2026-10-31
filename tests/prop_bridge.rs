//! Property tests for the JS bridge: `window.go.main.App[<method>](...)`.
//!
//! The contract is the list of calls the webview front end made
//! (`tests/contract/app.js`, kept from `frontend/dist/app.js`). Each one is
//! dispatched by `App::dispatch` (`src/app.rs`, the root shell over
//! `fiotransfer-core`); the caller gets either a value or a message, never "no
//! such method", never a panic and never a hang, whatever the arguments are. The argument vectors are drawn from the shapes a
//! JS caller can produce: `undefined` (which `ipc.postMessage` sends as `null`),
//! a number where a string is expected, an object where an array is expected, an
//! empty string, a huge number, a nested object.
//!
//! Isolation: `HOME` is a temporary directory for the whole case, so settings,
//! logs and the launch agent land there, and `FIO_NO_KEYCHAIN=1` keeps the real
//! login keychain (and the real Frame.io account) out of the test. The tests in
//! this file take one process-wide lock, so the `HOME` swap is never observed by
//! another test thread.
//!
//! Panels (`PickFiles`, `PickFolder`, `ExportHistory`, the clipboard) answer as
//! cancelled: the app has no host installed here, and every platform call takes
//! `fiotransfer_core::host::Host`'s default, so they are safe to call. `Login`, `Reveal`, `OpenURL` and `OpenLogs` are not called: they would
//! open a browser, a Finder window or a sign-in server for real. Their names are
//! still checked against the dispatch table.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use fiotransfer::app::App;
use fiotransfer::config::{Crumb, Settings};
use fiotransfer::app::BridgeHost;
use proptest::prelude::*;
use serde_json::{json, Map, Value};

// ---------- isolation ----------

static ENV: Mutex<()> = Mutex::new(());

struct Sandbox {
    _guard: MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    prev_home: Option<OsString>,
}

impl Sandbox {
    fn new() -> Sandbox {
        let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("temp dir");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        std::env::set_var("FIO_NO_KEYCHAIN", "1");
        Sandbox { _guard: guard, dir, prev_home }
    }

    fn home(&self) -> &Path {
        self.dir.path()
    }

    fn app(&self) -> Arc<App> {
        self.app_with(Settings::default())
    }

    fn app_with(&self, s: Settings) -> Arc<App> {
        App::new(s)
    }

    fn settings_file(&self) -> PathBuf {
        self.home().join("Library").join("Application Support").join("FioTransfer").join("settings.json")
    }

    /// What the real app finds on disk: `main` loads the file at startup, so a
    /// test that starts from anything but the defaults has to write it.
    fn seed_settings(&self, s: &Value) {
        let p = self.settings_file();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_vec_pretty(s).unwrap()).unwrap();
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.prev_home.take() {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        std::env::remove_var("FIO_NO_KEYCHAIN");
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

fn call(rt: &tokio::runtime::Runtime, app: &Arc<App>, method: &str, args: Vec<Value>) -> Result<Value, String> {
    rt.block_on(app.call(method.to_string(), args))
}

fn is_missing_method(e: &str) -> bool {
    e.starts_with("no such method")
}

// ---------- the contract, read from the frontend ----------

/// Every `call('Name', ...)` in the webview front end, frozen at
/// `tests/contract/app.js` (it used to be embedded from `frontend/dist`, which
/// the egui app no longer has or serves).
fn appjs_methods() -> BTreeSet<String> {
    let js = include_str!("contract/app.js").to_string();
    let mut out = BTreeSet::new();
    let mut rest = js.as_str();
    while let Some(i) = rest.find("call('") {
        let after = &rest[i + "call('".len()..];
        let end = after.find('\'').unwrap_or(0);
        let name = &after[..end];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.insert(name.to_string());
        }
        rest = &after[end.max(1)..];
    }
    out
}

/// The match arms of `App::dispatch`, read back from the crate's own source, so
/// the check does not need a second copy of the table.
fn dispatch_arms() -> BTreeSet<String> {
    let src = include_str!("../src/app.rs");
    let start = src.find("async fn dispatch").expect("App::dispatch");
    let mut out = BTreeSet::new();
    for line in src[start..].lines() {
        if line == "    }" {
            break;
        }
        let t = line.trim();
        let Some(rest) = t.strip_prefix('"') else { continue };
        let Some(q) = rest.find('"') else { continue };
        let name = &rest[..q];
        if name.is_empty() {
            continue;
        }
        if rest[q + 1..].trim_start().starts_with("=>") {
            out.insert(name.to_string());
        }
    }
    out
}

/// Methods that would open a browser, a Finder window or a sign-in server.
const NOT_CALLED: &[&str] = &["Login", "Reveal", "OpenURL", "OpenLogs"];

fn callable() -> &'static [&'static str] {
    static CALLABLE: OnceLock<Vec<&'static str>> = OnceLock::new();
    CALLABLE.get_or_init(|| {
        appjs_methods()
            .into_iter()
            .filter(|n| !NOT_CALLED.contains(&n.as_str()))
            .map(|n| Box::leak(n.into_boxed_str()) as &'static str)
            .collect()
    })
}

// ---------- argument generators ----------

fn arg_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        2 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        2 => any::<i64>().prop_map(|n| json!(n)),
        1 => Just(json!(9_007_199_254_740_993i64)),
        1 => Just(json!(i64::MIN)),
        2 => any::<f64>().prop_map(|f| json!(f)),
        2 => ".{0,24}".prop_map(Value::String),
        1 => Just(Value::String("".into())),
        1 => Just(Value::String("héllo /\u{0}… 日本".into())),
        1 => Just(json!({})),
        2 => Just(json!({"id": 1, "name": ["nested"], "kind": null})),
        1 => Just(json!([])),
        1 => Just(json!([1, "two", null, {"three": 3}])),
        1 => Just(json!({"nested": {"deeper": [1, {"x": null}]}})),
    ]
}

fn args_strategy() -> impl Strategy<Value = Vec<Value>> {
    prop::collection::vec(arg_value(), 0..=5)
}

// ---------- the method table ----------


/// Fields that lawfully differ between two identical calls: `checked_at` is a
/// timestamp, stamped when a release check actually runs (with a released
/// version it does). Everything else must match exactly.
fn settled(v: serde_json::Value) -> serde_json::Value {
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(t) = m.get_mut("checked_at") {
                    *t = serde_json::Value::from(0);
                }
                for (_, x) in m.iter_mut() {
                    walk(x);
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut v = v;
    walk(&mut v);
    v
}

#[test]
fn every_appjs_call_has_a_dispatch_arm() {
    let js = appjs_methods();
    let arms = dispatch_arms();
    assert!(js.len() >= 40, "app.js should make at least 40 bridge calls, found {}", js.len());
    let missing: Vec<&String> = js.iter().filter(|m| !arms.contains(*m)).collect();
    assert!(missing.is_empty(), "app.js calls methods dispatch does not define: {:?}", missing);
    // The bridge's own helpers (clipboard, quit, show) are the runtime's, not
    // app.js's: they must stay dispatchable too.
    for helper in ["__clipboard_set", "__quit", "__window_show"] {
        assert!(arms.contains(helper), "dispatch lost {}", helper);
    }
}

#[test]
fn every_appjs_call_answers_from_the_bridge() {
    let js = appjs_methods();
    let sandbox = Sandbox::new();
    let app = sandbox.app();
    let rt = rt();
    for name in js.iter().filter(|n| !NOT_CALLED.contains(&n.as_str())) {
        for args in [vec![], vec![Value::Null; 3]] {
            match call(&rt, &app, name, args.clone()) {
                Ok(_) => {}
                Err(e) => assert!(
                    !is_missing_method(&e),
                    "app.js calls {}, which is not dispatchable ({} args): {}",
                    name,
                    args.len(),
                    e
                ),
            }
        }
    }
}

// ---------- properties ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// Decoding is total: any argument list is answered, quickly, with a value
    /// or a message. No panic, no hang, no "no such method".
    #[test]
    fn dispatch_is_total(which in 0..callable().len(), args in args_strategy()) {
        let method = callable()[which];
        let sandbox = Sandbox::new();
        let app = sandbox.app();
        let rt = rt();
        let fut = app.call(method.to_string(), args.clone());
        // The timeout is built inside the runtime: `tokio::time::timeout` needs a
        // timer context even before it is polled.
        match rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), fut).await }) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                prop_assert!(!is_missing_method(&e), "{} -> {}", method, e);
                prop_assert!(!e.is_empty(), "{} answered with an empty message", method);
            }
            Err(_) => prop_assert!(false, "{} ({:?}) did not answer within 2s", method, args),
        }
    }

    /// A JS caller's `undefined` arrives as `null` (JSON.stringify turns holes in
    /// the argument array into nulls), so a trailing null must not change an
    /// answer. `s()`, `b()`, `n()` and `arg()` all decode absent as null.
    #[test]
    fn null_arguments_decode_like_missing_ones(
        which in 0..callable().len(),
        args in args_strategy(),
        pad in 1..=3usize,
    ) {
        let method = callable()[which];
        let sandbox = Sandbox::new();
        let rt = rt();
        let mut padded = args.clone();
        padded.extend(std::iter::repeat(Value::Null).take(pad));
        let a = call(&rt, &sandbox.app(), method, args.clone());
        let b = call(&rt, &sandbox.app(), method, padded.clone());
        prop_assert_eq!(a.map(settled), b.map(settled), "{} answered differently for {:?} and {:?}", method, args, padded);
    }

}

/// The same, position by position, for every method and every length: `k` nulls
/// and no arguments at all are the same call. This is what makes `undefined` safe
/// wherever it lands in the argument list.
#[test]
fn nulls_in_every_position_decode_like_missing_ones() {
    let sandbox = Sandbox::new();
    let rt = rt();
    for method in callable() {
        for k in 0..=6usize {
            let empty = call(&rt, &sandbox.app(), method, vec![]);
            let filled = call(&rt, &sandbox.app(), method, vec![Value::Null; k]);
            assert_eq!(empty.map(settled), filled.map(settled), "{} answered differently for no args and {} nulls", method, k);
        }
    }
}

/// Every method answers, and does not hang, for a fixed set of argument shapes:
/// no arguments, all nulls, an empty string where a number belongs, a number
/// where a string belongs, a nested object, a huge number, an array, and too many
/// arguments. Deterministic, so it covers every method on every run.
#[test]
fn every_method_answers_every_shaped_argument_list() {
    let shapes: Vec<Vec<Value>> = vec![
        vec![],
        vec![Value::Null],
        vec![Value::Null; 8],
        vec![json!("")],
        vec![json!(0)],
        vec![json!(-1)],
        vec![json!(9_007_199_254_740_993i64)],
        vec![json!(true), json!(false)],
        vec![json!({"a": 1})],
        vec![json!({"nested": {"deeper": [1, {"x": null}]}})],
        vec![json!([]), json!([1, 2])],
        vec![json!("\u{0}…日本"), json!("")],
    ];
    let sandbox = Sandbox::new();
    let rt = rt();
    for method in callable() {
        for args in &shapes {
            let fut = sandbox.app().call(method.to_string(), args.clone());
            match rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), fut).await }) {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => assert!(!is_missing_method(&e) && !e.is_empty(), "{} {:?} -> {:?}", method, args, e),
                Err(_) => panic!("{} {:?} did not answer within 2s", method, args),
            }
        }
    }
}

/// Methods that need no window, no manager, no history store and no network:
/// the panel helpers answer as cancelled, the rest read or write local state.
/// They must succeed whatever the arguments are. (`SetOpenAtLogin` is not here:
/// it writes a launch agent, which fails when the binary is not in an .app.
/// `Search` is not here: a non-empty account makes it ask Frame.io.)
const LOCAL_OK: &[&str] = &[
    "GetState", "SaveSettings", "Logout", "Snapshot", "UpdateStatus", "CheckForUpdate", "Cancel",
    "ClearFinished", "JobFiles", "History", "HistoryFiles", "Watches", "RemoveWatch",
    "RequeueCanceled", "SetWatchEnabled", "SetUploadDestination", "TakeServiceRequests",
    "ClipboardText", "PickFiles", "PickFolder", "PickWatchFolder", "PickDownloadDir",
    "ExportHistory", "__clipboard_set", "__quit", "__window_show",
];

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// A call that needs nothing external answers with a value, whatever it was
    /// handed: these are the calls the frontend makes on every render.
    #[test]
    fn local_methods_always_answer(args in args_strategy()) {
        let sandbox = Sandbox::new();
        let rt = rt();
        for method in LOCAL_OK {
            if let Err(e) = call(&rt, &sandbox.app(), method, args.clone()) {
                prop_assert!(false, "{} ({:?}) failed: {}", method, args, e);
            }
        }
    }
}

// ---------- shapes ----------

fn obj(v: &Value) -> &Map<String, Value> {
    v.as_object().unwrap_or_else(|| panic!("not an object: {}", v))
}

fn need<'a>(o: &'a Map<String, Value>, key: &str, what: &str) -> &'a Value {
    o.get(key).unwrap_or_else(|| panic!("{} has no {:?}: {}", what, key, Value::Object(o.clone())))
}

fn need_str<'a>(o: &'a Map<String, Value>, key: &str, what: &str) -> &'a str {
    let v = need(o, key, what);
    v.as_str().unwrap_or_else(|| panic!("{}.{} is not a string: {}", what, key, v))
}

fn need_int(o: &Map<String, Value>, key: &str, what: &str) -> i64 {
    let v = need(o, key, what);
    v.as_i64().unwrap_or_else(|| panic!("{}.{} is not an integer: {}", what, key, v))
}

fn need_num(o: &Map<String, Value>, key: &str, what: &str) -> f64 {
    let v = need(o, key, what);
    v.as_f64().unwrap_or_else(|| panic!("{}.{} is not a number: {}", what, key, v))
}

fn need_bool(o: &Map<String, Value>, key: &str, what: &str) -> bool {
    let v = need(o, key, what);
    v.as_bool().unwrap_or_else(|| panic!("{}.{} is not a bool: {}", what, key, v))
}

/// The `State` keys app.js reads (renderState, shortPath, renderUploadRow,
/// startUpload, saveSettings).
fn check_state(v: &Value) {
    let st = obj(v);
    need_str(st, "version", "State");
    need_bool(st, "logged_in", "State");
    need_str(st, "redirect_uri", "State");
    need_str(st, "log_dir", "State");
    need_str(st, "log_file", "State");
    need_str(st, "home", "State");
    need_bool(st, "open_at_login", "State");
    match need(st, "user", "State") {
        Value::Null => {}
        u => {
            need_str(obj(u), "name", "State.user");
            need_str(obj(u), "email", "State.user");
        }
    }
    let s = obj(need(st, "settings", "State"));
    for k in ["client_id", "download_dir", "last_account_id", "last_folder_id", "last_folder_label"] {
        need_str(s, k, "State.settings");
    }
    need_int(s, "streams", "State.settings");
    need_int(s, "segment_mb", "State.settings");
    need_int(s, "redirect_port", "State.settings");
    need_bool(s, "no_folder_logs", "State.settings");
    need_bool(s, "no_update_check", "State.settings");
    match s.get("last_folder_path") {
        None | Some(Value::Array(_)) => {}
        Some(other) => panic!("State.settings.last_folder_path is not an array: {}", other),
    }
    // app.js and auth both build `https://localhost:<port>/callback`.
    let port = need_int(s, "redirect_port", "State.settings").max(1);
    let uri = need_str(st, "redirect_uri", "State");
    assert_eq!(uri, format!("https://localhost:{}/callback", port), "redirect_uri does not carry the port");
}

fn check_job_view(j: &Map<String, Value>) {
    for k in ["id", "kind", "title", "source", "dest", "status", "phase", "error", "link"] {
        need_str(j, k, "JobView");
    }
    for k in ["total", "done", "files_total", "files_done", "files_failed"] {
        need_int(j, k, "JobView");
    }
    for k in ["speed", "eta", "elapsed", "avg_speed"] {
        need_num(j, k, "JobView");
    }
    let status = need_str(j, "status", "JobView");
    assert!(
        ["queued", "resolving", "running", "done", "partial", "failed", "canceled", "skipped"].contains(&status),
        "JobView.status {:?} is not one app.js styles",
        status
    );
}

fn check_snapshot(v: &Value) {
    let s = obj(v);
    let jobs = need(s, "jobs", "Snapshot").as_array().unwrap_or_else(|| panic!("Snapshot.jobs is not an array"));
    need_num(s, "speed", "Snapshot");
    need_int(s, "active_streams", "Snapshot");
    need_int(s, "stream_limit", "Snapshot");
    for j in jobs {
        check_job_view(obj(j));
    }
}

fn check_update(v: &Value) {
    let u = obj(v);
    for k in ["current", "latest", "url", "state", "error", "blocker"] {
        need_str(u, k, "UpdateStatus");
    }
    need_bool(u, "available", "UpdateStatus");
    let progress = need_num(u, "progress", "UpdateStatus");
    need_int(u, "checked_at", "UpdateStatus");
    assert!((0.0..=1.0).contains(&progress), "UpdateStatus.progress out of 0..1: {}", progress);
    let state = need_str(u, "state", "UpdateStatus");
    assert!(["idle", "checking", "downloading", "restarting"].contains(&state), "UpdateStatus.state {:?}", state);
    if need_bool(u, "available", "UpdateStatus") {
        assert!(!need_str(u, "latest", "UpdateStatus").is_empty(), "an available update has no version");
    }
}

fn check_link_info(v: &Value) {
    let li = obj(v);
    for k in ["share_id", "kind", "name", "error"] {
        need_str(li, k, "LinkInfo");
    }
    need_int(li, "files", "LinkInfo");
    need_int(li, "unavailable", "LinkInfo");
    need_int(li, "total_bytes", "LinkInfo");
    need_bool(li, "needs_passphrase", "LinkInfo");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// The documented JSON shapes of every method that answers offline, over a
    /// spread of reachable settings (`config::load` normalizes, so these are in
    /// range), and SaveSettings(GetState().settings) is a fixed point of them.
    #[test]
    fn answers_carry_the_documented_shapes(
        client_id in "[ -~]{0,40}",
        port in 1i32..=65535,
        streams in 1i32..=128,
        segment in 8i32..=512,
        folder in "[ -~]{0,20}",
    ) {
        let sandbox = Sandbox::new();
        let mut s = Settings::default();
        // SaveSettings trims the client id, so the file main() would have loaded
        // holds the trimmed form.
        s.client_id = client_id.trim().to_string();
        s.redirect_port = port;
        s.streams = streams;
        s.segment_mb = segment;
        s.last_folder_id = folder;
        // main() loads settings.json at startup; start the app from the same
        // file so the in-memory settings and the file agree.
        sandbox.seed_settings(&serde_json::to_value(&s).unwrap());
        let app = sandbox.app_with(s.clone());
        let rt = rt();

        let st = call(&rt, &app, "GetState", vec![]).expect("GetState");
        check_state(&st);
        prop_assert_eq!(need_str(obj(&st), "home", "State"), sandbox.home().to_string_lossy());

        let snap = call(&rt, &app, "Snapshot", vec![]).expect("Snapshot");
        check_snapshot(&snap);
        let up = call(&rt, &app, "UpdateStatus", vec![]).expect("UpdateStatus");
        check_update(&up);
        prop_assert_eq!(
            need_str(obj(&up), "current", "UpdateStatus"),
            need_str(obj(&st), "version", "State"),
            "the update panel's version and the about box's disagree"
        );

        let li = call(&rt, &app, "InspectLink", vec![json!("https://f.io/abc"), json!("")]).expect("InspectLink");
        check_link_info(&li);

        for (m, a) in [
            ("JobFiles", vec![json!("nope")]),
            ("HistoryFiles", vec![json!("nope")]),
            ("History", vec![Value::Null, Value::Null, Value::Null]),
            ("Watches", vec![]),
            ("TakeServiceRequests", vec![]),
            ("PickFiles", vec![]),
        ] {
            let v = call(&rt, &app, m, a).unwrap_or_else(|e| panic!("{}: {}", m, e));
            prop_assert!(v.is_array(), "{} did not answer with an array: {}", m, v);
        }

        // SaveSettings(what GetState showed) changes nothing.
        let shown = obj(&st)["settings"].clone();
        let again = call(&rt, &app, "SaveSettings", vec![shown]).expect("SaveSettings");
        check_state(&again);
        prop_assert_eq!(&again, &st, "SaveSettings(GetState().settings) changed the state");
    }
}

/// A redirect port the settings file should never hold but can (it is a number
/// in JSON): the URI keeps the port the listener would use.
#[test]
fn an_out_of_range_redirect_port_is_shown_as_the_listener_port() {
    let sandbox = Sandbox::new();
    for (port, shown) in [(0i32, 1u16), (-5, 1), (70000, 70000u32 as u16)] {
        let mut s = Settings::default();
        s.redirect_port = port;
        let app = sandbox.app_with(s);
        let st = call(&rt(), &app, "GetState", vec![]).expect("GetState");
        assert_eq!(
            need_str(obj(&st), "redirect_uri", "State"),
            format!("https://localhost:{}/callback", shown),
            "redirect_uri for redirect_port {}",
            port
        );
    }
}

// ---------- settings are persisted exactly ----------

/// The model of `Settings::normalize` (src/config.rs), written from the field
/// documentation: a non-positive port falls back to 8765, streams to 16, its
/// ceiling is 128; a segment size below 8 MiB falls back to 64, its ceiling is
/// 512; an empty download folder falls back to ~/Downloads.
fn model_normalize(m: &Map<String, Value>, home: &Path) -> Map<String, Value> {
    let mut e = m.clone();
    let port = e["redirect_port"].as_i64().unwrap_or(0);
    e.insert("redirect_port".into(), json!(if port <= 0 { 8765 } else { port }));
    let streams = e["streams"].as_i64().unwrap_or(0);
    e.insert("streams".into(), json!(if streams <= 0 { 16 } else { streams.min(128) }));
    let seg = e["segment_mb"].as_i64().unwrap_or(0);
    e.insert("segment_mb".into(), json!(if seg < 8 { 64 } else { seg.min(512) }));
    if e["download_dir"].as_str().unwrap_or("").is_empty() {
        e.insert("download_dir".into(), json!(home.join("Downloads").to_string_lossy()));
    }
    if e.get("last_folder_path").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(false) {
        e.remove("last_folder_path");
    }
    e
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// SaveSettings persists exactly what it is given, after the documented
    /// normalization; the file on disk says the same as the next GetState.
    #[test]
    fn save_settings_is_a_fixed_point(
        client_id in "[ -~]{0,30}",
        port in -5000i32..70000,
        streams in -20i32..400,
        segment in -100i32..2000,
        dir in prop_oneof![Just(String::new()), "[ -~]{1,40}"],
        folder_logs in any::<bool>(),
        update_check in any::<bool>(),
    ) {
        let sandbox = Sandbox::new();
        let app = sandbox.app();
        let rt = rt();
        let st = call(&rt, &app, "GetState", vec![]).expect("GetState");
        let base = obj(&st)["settings"].clone();
        // The real app loads settings.json at startup: give it the same view.
        sandbox.seed_settings(&base);

        let mut patch = obj(&base).clone();
        patch.insert("client_id".into(), json!(format!("  {}  ", client_id)));
        patch.insert("redirect_port".into(), json!(port));
        patch.insert("streams".into(), json!(streams));
        patch.insert("segment_mb".into(), json!(segment));
        patch.insert("download_dir".into(), json!(dir));
        patch.insert("no_folder_logs".into(), json!(folder_logs));
        patch.insert("no_update_check".into(), json!(update_check));
        let patch = Value::Object(patch);

        let mut want = obj(&patch).clone();
        want.insert("client_id".into(), json!(client_id.trim()));

        let after = call(&rt, &app, "SaveSettings", vec![patch]).expect("SaveSettings");
        let shown = obj(&after)["settings"].clone();
        let model = model_normalize(&want, sandbox.home());
        prop_assert_eq!(obj(&shown), &model, "SaveSettings did not persist what it was given");

        let saved: Value = serde_json::from_slice(&std::fs::read(sandbox.settings_file()).expect("settings.json")).expect("json");
        prop_assert_eq!(obj(&saved), &model, "settings.json disagrees with GetState");
    }

    /// A payload that is not a Settings leaves the settings alone rather than
    /// clobbering them or raising.
    #[test]
    fn a_bad_settings_payload_changes_nothing(junk in arg_value()) {
        let sandbox = Sandbox::new();
        let app = sandbox.app();
        let rt = rt();
        let before = call(&rt, &app, "GetState", vec![]).expect("GetState");
        for payload in [junk.clone(), json!({"streams": 4}), json!({}), Value::Null, Value::Bool(true)] {
            let _ = call(&rt, &app, "SaveSettings", vec![payload.clone()]);
            let after = call(&rt, &app, "GetState", vec![]).expect("GetState");
            prop_assert_eq!(&after, &before, "SaveSettings({}) changed the state", payload);
        }
    }

    /// SetUploadDestination stores the strings it is handed, verbatim, and the
    /// picker path only when it decodes as crumbs.
    #[test]
    fn upload_destination_round_trips(
        acct in "[ -~]{0,20}",
        folder in "[ -~]{0,20}",
        label in "[ -~]{0,30}",
        path in prop_oneof![
            Just(Value::Null),
            Just(json!([])),
            Just(json!([{"id": "p1", "name": "Proj", "kind": "project"}])),
            Just(json!([{"id":"a","name":"A","kind":"account"},{"id":"f","name":"F","kind":"folder","folder_id":"f"}])),
            Just(json!([{"id": 1}])),
            Just(json!({"not": "an array"})),
        ],
    ) {
        let sandbox = Sandbox::new();
        let app = sandbox.app();
        let rt = rt();
        let after = call(
            &rt,
            &app,
            "SetUploadDestination",
            vec![json!(acct), json!(folder), json!(label), path.clone()],
        )
        .expect("SetUploadDestination");
        check_state(&after);
        let s = obj(need(obj(&after), "settings", "State"));
        prop_assert_eq!(need_str(s, "last_account_id", "settings"), acct.as_str());
        prop_assert_eq!(need_str(s, "last_folder_id", "settings"), folder.as_str());
        prop_assert_eq!(need_str(s, "last_folder_label", "settings"), label.as_str());
        let got = s.get("last_folder_path").cloned().unwrap_or_else(|| json!([]));
        let want = match serde_json::from_value::<Vec<Crumb>>(path.clone()) {
            Ok(v) => serde_json::to_value(&v).unwrap(),
            Err(_) => json!([]),
        };
        prop_assert_eq!(&got, &want, "path {:?} stored as {:?}", path, got);
    }
}

