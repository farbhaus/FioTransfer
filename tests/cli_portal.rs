//! The Portal commands of the CLI, as a user runs them: the `fio` binary itself,
//! started with `std::process::Command`, one command per process, under its own
//! temporary `HOME`.
//!
//! What is exercised is the real path: argument parsing -> `App::portal_sign_in`
//! / `App::portal_reconcile` / `App::portal_status` -> `PortalClient` over HTTP to
//! a one-connection stub Portal -> sqlite. Only the network is simulated, and it
//! is simulated on loopback: no case here reaches a real Portal, and none needs
//! one.
//!
//! The two things that are process-wide in the child are set per process, so the
//! cases can run side by side: `HOME` is the case's own temporary directory and
//! `FIO_NO_KEYCHAIN=1` keeps the real Keychain out of it (the token and the
//! session then live in 0600 files there, which is what a headless run gets).
//!
//! Covered: the exit codes of `status` (0 configured, 1 not), of `signin` (1 for
//! a refusal, 2 for a second factor), of `reconcile` (0, and 1 for an error);
//! that a reconcile makes exactly the one watch and reports the two hostile
//! markers with their reasons; that a second reconcile changes nothing; and that
//! no password and no token ever reaches the output or a file the app writes.
//!
//! Run with:
//!   CARGO_TARGET_DIR=<own dir> cargo test --offline --test cli_portal

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

const EMAIL: &str = "chaos@localhost";
const PASSWORD: &str = "portal-local-dev-pass-1";
const TOKEN: &str = "local-dev-token-0000000000000000000000";
/// The one-time code the stub's second-factor account accepts. Any other six
/// digits are refused, as Portal refuses them.
const GOOD_CODE: &str = "135790";
/// The cookie the stub hands out on the password step when a second factor is
/// wanted, and the one it hands out when the code is accepted. Two values, so a
/// case can prove the code was sent on the *pending* session and not on a new
/// one.
const PENDING_COOKIE: &str = "portal_session=eyJ0b3RwX3BlbmRpbmdfdWlkIjoiYWJjIn0.auQbjA.pending";
const SIGNED_COOKIE: &str = "portal_session=eyJ1c2VyX2lkIjoiYWJjIn0.auQbjA.signed";

// ---------- the stub Portal ----------

#[derive(Default)]
struct StubState {
    /// Answer the next sign-in with `totp_required`, as a deployment that wants
    /// a one-time code does.
    totp: AtomicBool,
    /// How many requests came in, so a case can prove none did.
    hits: AtomicUsize,
    /// The `Cookie:` header every `/api/auth/login/totp` request carried, in
    /// order: the proof that the code finished the session the password started.
    totp_cookies: Mutex<Vec<String>>,
    /// Every request line, for a case that wants to see what was called.
    lines: Mutex<Vec<String>>,
}

struct Stub {
    base: String,
    state: Arc<StubState>,
}

/// The three links the brief seeds: one good marker, one that tries to leave the
/// root, and one that is not a folder name at all.
fn seeded_links() -> String {
    json!({ "upload_links": [
        {
            "id": "L1", "name": "Farbhaus", "turbosort": "Farbhaus",
            "target_folder_id": "folder-1", "target_folder_name": "Farbhaus",
            "folder": { "id": "folder-1", "name": "Farbhaus", "account_id": "acct-1", "turbosort": "Farbhaus" },
        },
        { "id": "L2", "name": "Hostile", "turbosort": "../etc", "target_folder_id": "folder-2",
          "folder": { "id": "folder-2", "account_id": "acct-2" } },
        { "id": "L3", "name": "NoAccount", "turbosort": ".", "target_folder_id": "folder-3",
          "folder": { "id": "folder-3" } },
    ]})
    .to_string()
}

impl Stub {
    fn start() -> Stub {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", l.local_addr().expect("addr"));
        let state = Arc::new(StubState::default());
        let st = state.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                match c {
                    Ok(c) => {
                        let st = st.clone();
                        std::thread::spawn(move || answer(c, st));
                    }
                    Err(_) => return,
                }
            }
        });
        Stub { base, state }
    }

    fn want_totp(&self) {
        self.state.totp.store(true, Ordering::SeqCst);
    }

    fn hits(&self) -> usize {
        self.state.hits.load(Ordering::SeqCst)
    }

    /// Every request line Portal saw, in order.
    fn lines(&self) -> Vec<String> {
        self.state.lines.lock().unwrap().clone()
    }

    /// Every `Cookie:` header a `/api/auth/login/totp` request carried.
    fn totp_cookies(&self) -> Vec<String> {
        self.state.totp_cookies.lock().unwrap().clone()
    }
}

fn answer(mut c: TcpStream, st: Arc<StubState>) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p;
        }
        match c.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.len() > 1 << 20 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < head_end + 4 + len {
        match c.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body = String::from_utf8_lossy(&buf[(head_end + 4).min(buf.len())..]).into_owned();
    st.hits.fetch_add(1, Ordering::SeqCst);
    let request_line = head.lines().next().unwrap_or("").to_string();
    st.lines.lock().unwrap().push(request_line.clone());

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").split('?').next().unwrap_or("").to_string();
    let header = |name: &str| {
        head.lines().find_map(|l| {
            let (n, v) = l.split_once(':')?;
            if n.trim().eq_ignore_ascii_case(name) {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
    };
    // The bearer token Portal was seeded with, and only that one: a refused token
    // has to be refused here as it would be by the real service.
    let authorised = header("authorization").is_some_and(|v| v == format!("Bearer {}", TOKEN))
        || header("cookie").is_some_and(|v| v.contains("portal_session="));

    let (status, cookies, body) = match (method.as_str(), path.as_str()) {
        ("POST", "/api/auth/login") => {
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let email = v.get("email").and_then(|e| e.as_str()).unwrap_or("");
            let password = v.get("password").and_then(|p| p.as_str()).unwrap_or("");
            if email == EMAIL && password == PASSWORD {
                let totp = st.totp.load(Ordering::SeqCst);
                let body = if totp {
                    json!({ "totp_required": true, "id": null, "email": null }).to_string()
                } else {
                    json!({ "totp_required": false, "id": "u1", "email": EMAIL }).to_string()
                };
                // A second factor leaves Portal's session *pending* in the
                // cookie it sets; a plain sign-in leaves it signed in.
                let pair = if totp { PENDING_COOKIE } else { SIGNED_COOKIE };
                let cookie = format!("{}; path=/; Max-Age=1209600; httponly; samesite=lax; secure", pair);
                (200u16, Some(cookie), body)
            } else {
                (401, None, json!({ "detail": "Invalid email or password" }).to_string())
            }
        }
        // The second step, on the session the first one left: Portal looks the
        // pending uid up in that session's cookie, so a code sent on any other
        // jar is "your login session expired".
        ("POST", "/api/auth/login/totp") => {
            let cookie = header("cookie").unwrap_or_default();
            st.totp_cookies.lock().unwrap().push(cookie.clone());
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let code = v.get("code").and_then(|c| c.as_str()).unwrap_or("");
            if !cookie.contains(PENDING_COOKIE) {
                (401, None, json!({ "detail": "Your login session expired — start again" }).to_string())
            } else if code != GOOD_CODE {
                (401, None, json!({ "detail": "Invalid code" }).to_string())
            } else {
                let pair = format!("{}; path=/; Max-Age=1209600; httponly; samesite=lax; secure", SIGNED_COOKIE);
                (200u16, Some(pair), json!({ "id": "u1", "email": EMAIL }).to_string())
            }
        }
        ("GET", "/api/auth/me") if authorised => (200, None, json!({ "id": "u1", "email": EMAIL }).to_string()),
        ("GET", "/api/upload-links") if authorised => (200, None, seeded_links()),
        ("GET", "/api/sync-rules") if authorised => (200, None, json!({ "data": [] }).to_string()),
        ("GET" | "POST", "/api/auth/me" | "/api/upload-links" | "/api/sync-rules") => {
            (401, None, json!({ "detail": "Authentication required" }).to_string())
        }
        _ => (404, None, json!({ "detail": "no route" }).to_string()),
    };
    let mut resp = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        status,
        body.len()
    );
    if let Some(cookie) = cookies {
        resp.push_str(&format!("Set-Cookie: {}\r\n", cookie));
    }
    resp.push_str("\r\n");
    let _ = c.write_all(resp.as_bytes());
    let _ = c.write_all(body.as_bytes());
    let _ = c.flush();
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------- one case's world ----------

struct Home {
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Home {
    fn new() -> Home {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("home");
        std::fs::create_dir_all(&dir).expect("home");
        Home { dir, _tmp: tmp }
    }

    /// `fio`, in this home, with the whole world's environment wiped of anything
    /// Portal-shaped first: what a case sets is all the child gets.
    fn fio(&self, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fio"));
        cmd.args(args)
            .env_remove("FIO_PORTAL_PASSWORD")
            .env("HOME", &self.dir)
            .env("FIO_NO_KEYCHAIN", "1")
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("fio starts");
        if let Some(text) = stdin {
            child.stdin.as_mut().expect("stdin is piped").write_all(text.as_bytes()).expect("write the password");
        }
        drop(child.stdin.take());
        let out = child.wait_with_output().expect("fio finishes");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// The app's own data folder under this home.
    fn data(&self) -> PathBuf {
        self.dir.join("Library/Application Support/FioTransfer")
    }

    fn settings(&self) -> String {
        std::fs::read_to_string(self.data().join("settings.json")).unwrap_or_default()
    }

    /// The folder Portal's project folders go under: the default, `~/Portal`.
    fn root(&self) -> PathBuf {
        self.dir.join("Portal")
    }

    /// Every file under the home, as text: what the app wrote, wherever it wrote
    /// it. A password or a token in any of these is a leak.
    fn every_document(&self) -> BTreeMap<PathBuf, String> {
        let mut out = BTreeMap::new();
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if let Ok(b) = std::fs::read(&p) {
                    out.insert(p.clone(), String::from_utf8_lossy(&b).into_owned());
                }
            }
        }
        walk(&self.dir, &mut out);
        out
    }

    /// Nothing this command printed carries `secret`.
    fn assert_not_printed(&self, secret: &str, what: &str, out: &Out) {
        assert!(!out.stdout.contains(secret), "{}: the {what} reached stdout: {}", out.stdout, what);
        assert!(!out.stderr.contains(secret), "{}: the {what} reached stderr: {}", out.stderr, what);
    }

    /// The password is nowhere at all: not printed, not in settings.json, not in
    /// the session jar, not in the transfer log, not beside any of them.
    fn assert_password_nowhere(&self, out: &Out) {
        self.assert_not_printed(PASSWORD, "password", out);
        for (path, text) in self.every_document() {
            assert!(!text.contains(PASSWORD), "{} carries the password", path.display());
        }
    }

    /// The token is not printed, not a setting and not in a log. The one file it
    /// is meant to be in is its own 0600 store, which is the credential itself.
    fn assert_token_kept_out_of_settings(&self, out: &Out) {
        self.assert_not_printed(TOKEN, "token", out);
        assert!(!self.settings().contains(TOKEN), "settings.json carries the token: {}", self.settings());
        let logs = self.dir.join("Library/Logs/FioTransfer");
        for (path, text) in self.every_document() {
            if !path.starts_with(&logs) {
                continue;
            }
            assert!(!text.contains(TOKEN), "{} carries the token", path.display());
        }
    }

    /// The one watch Portal's good marker asks for, as `path <- account/folder`.
    /// The stub's one link carries only a `turbosort`: an older Portal's
    /// two-way folder.
    fn watch_line(&self) -> String {
        format!("  {} ↔ acct-1/folder-1", self.root().join("Farbhaus").display())
    }
}

// ---------- status ----------

#[test]
fn status_with_no_settings_says_not_set_up_and_exits_one() {
    let h = Home::new();
    let out = h.fio(&["portal", "status"], &[], None);
    assert_eq!(out.code, 1, "an unconfigured Portal is not a success: {}", out.stdout);
    assert!(out.stdout.contains("Not set up: no Portal address and no token yet."), "{}", out.stdout);
    assert!(out.stdout.contains("url:        (none)"), "{}", out.stdout);
    assert!(out.stdout.contains("credential: no credential"), "{}", out.stdout);
    assert!(out.stdout.contains("root:       "), "the resolved root is shown: {}", out.stdout);
    assert!(out.stderr.is_empty(), "nothing to complain about: {}", out.stderr);
}

#[test]
fn a_stored_token_makes_status_configured_and_exits_zero() {
    let h = Home::new();
    let stub = Stub::start();

    let stored = h.fio(&["portal", "token", TOKEN], &[], None);
    assert_eq!(stored.code, 0, "{}", stored.stderr);
    assert!(stored.stdout.contains("stored the Portal access token."), "{}", stored.stdout);
    h.assert_token_kept_out_of_settings(&stored);

    let out = h.fio(&["portal", "status", "--url", &stub.base], &[], None);
    assert_eq!(out.code, 0, "a token and an address are configured: {}", out.stdout);
    assert!(out.stdout.contains("Configured. Portal has not been checked yet."), "{}", out.stdout);
    assert!(out.stdout.contains(&format!("url:        {}", stub.base)), "{}", out.stdout);
    assert!(out.stdout.contains("credential: an access token is stored"), "{}", out.stdout);
    h.assert_token_kept_out_of_settings(&out);

    // …and forgetting it is exactly what puts it back to "not signed in".
    let cleared = h.fio(&["portal", "token", "--clear"], &[], None);
    assert_eq!(cleared.code, 0, "{}", cleared.stderr);
    assert!(cleared.stdout.contains("forgot the stored Portal access token."), "{}", cleared.stdout);
    let out = h.fio(&["portal", "status", "--url", &stub.base], &[], None);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stdout.contains("Not signed in yet"), "{}", out.stdout);
    assert!(out.stdout.contains("credential: no credential"), "{}", out.stdout);
}

// ---------- signin ----------

#[test]
fn signin_with_a_wrong_password_exits_one_and_stores_nothing() {
    let h = Home::new();
    let stub = Stub::start();
    let wrong = "not-the-password";
    let out = h.fio(
        &["portal", "signin", &stub.base, EMAIL],
        &[("FIO_PORTAL_PASSWORD", wrong)],
        None,
    );
    assert_eq!(out.code, 1, "a refused password is exit 1: {}", out.stdout);
    assert!(out.stderr.contains("Portal refused the sign-in (401)"), "{}", out.stderr);
    assert!(out.stderr.contains("Invalid email or password"), "Portal's own word reaches the user: {}", out.stderr);
    assert!(out.stdout.is_empty(), "nothing was signed in: {}", out.stdout);
    h.assert_password_nowhere(&out);

    // A refused sign-in keeps no session and no address: a fresh machine is left
    // as it was.
    assert!(out.code != 0);
    assert!(!h.data().join("portal-session").exists(), "a refused sign-in stored a session");
    assert!(!h.settings().contains(&stub.base), "a refused sign-in stored the address");

    // And the next command says so.
    let st = h.fio(&["portal", "status"], &[], None);
    assert_eq!(st.code, 1, "{}", st.stdout);
    assert!(st.stdout.contains("Not set up: no Portal address and no token yet."), "{}", st.stdout);
}

#[test]
fn signin_with_the_password_on_stdin_signs_in_and_keeps_the_session() {
    let h = Home::new();
    let stub = Stub::start();
    let out = h.fio(&["portal", "signin", &stub.base, EMAIL], &[], Some(&format!("{}\n", PASSWORD)));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.trim(), format!("signed in as {}", EMAIL));
    // The password is read from stdin, and the prompt says where from; it is not
    // echoed, and it is not written anywhere.
    assert!(out.stderr.contains("Portal password:"), "the prompt is on stderr, not in the output: {}", out.stderr);
    h.assert_password_nowhere(&out);

    // The address and the email are settings; the password is not.
    let settings = h.settings();
    assert!(settings.contains(&stub.base), "the address is stored: {settings}");
    assert!(settings.contains(EMAIL), "the email is stored: {settings}");
    assert!(!settings.contains(PASSWORD), "the password is not a setting: {settings}");
    // The session is a jar of cookies, and the password is not in it either.
    let jar = std::fs::read_to_string(h.data().join("portal-session")).expect("a stored session");
    assert!(jar.contains("portal_session="), "the cookie is the session: {jar}");
    assert!(!jar.contains(PASSWORD), "the session carries the password: {jar}");

    // A later process signs in on the strength of that jar alone.
    let st = h.fio(&["portal", "status"], &[], None);
    assert_eq!(st.code, 0, "{}", st.stdout);
    assert!(st.stdout.contains("Configured. Portal has not been checked yet."), "{}", st.stdout);
    assert!(st.stdout.contains(&format!("credential: signed in as {}", EMAIL)), "{}", st.stdout);
    h.assert_password_nowhere(&st);
}

#[test]
fn signin_that_needs_a_second_factor_and_has_no_code_exits_two() {
    let h = Home::new();
    let stub = Stub::start();
    stub.want_totp();
    // Nothing on stdin, and no code anywhere: the one case that is exit 2.
    let out = h.fio(
        &["portal", "signin", &stub.base, EMAIL],
        &[("FIO_PORTAL_PASSWORD", PASSWORD)],
        Some(""),
    );
    assert_eq!(out.code, 2, "a missing code is its own exit code: {} {}", out.stdout, out.stderr);
    assert!(out.stderr.contains("one-time code"), "{}", out.stderr);
    // The way on is named, and it is no longer "find a bearer token": the same
    // command takes the code.
    assert!(out.stderr.contains("--totp <code>"), "{}", out.stderr);
    assert!(out.stderr.contains("FIO_PORTAL_TOTP"), "{}", out.stderr);
    assert!(out.stderr.contains("fio portal signin <url> <email> --totp <code>"), "{}", out.stderr);
    // The prompt is the password's own idiom: it says where the code is read
    // from, and it is on stderr, not in the output.
    assert!(out.stderr.contains("Portal one-time code:"), "{}", out.stderr);
    assert!(out.stdout.is_empty(), "nothing was signed in: {}", out.stdout);
    h.assert_password_nowhere(&out);
    assert!(!h.data().join("portal-session").exists(), "a half-finished sign-in stores no session");
    assert!(!h.settings().contains(&stub.base), "a sign-in that did not finish stores no address");
    // One password step was made; the code step was not, because there was no
    // code to send.
    assert!(stub.totp_cookies().is_empty(), "no code step without a code: {:?}", stub.totp_cookies());
}

#[test]
fn signin_with_a_wrong_code_exits_one_with_portals_sentence() {
    let h = Home::new();
    let stub = Stub::start();
    stub.want_totp();
    let out = h.fio(
        &["portal", "signin", &stub.base, EMAIL, "--totp", "000000"],
        &[("FIO_PORTAL_PASSWORD", PASSWORD)],
        None,
    );
    assert_eq!(out.code, 1, "a refused code is a refusal: {} {}", out.stdout, out.stderr);
    assert!(out.stderr.contains("Portal refused the one-time code (401)"), "{}", out.stderr);
    assert!(out.stderr.contains("Invalid code"), "Portal's own sentence: {}", out.stderr);
    assert!(out.stdout.is_empty(), "{}", out.stdout);
    h.assert_password_nowhere(&out);
    // Nothing is kept, and the address is not adopted either.
    assert!(!h.data().join("portal-session").exists(), "a refused code stored a session");
    assert!(!h.settings().contains(&stub.base), "a refused code stored the address");
    // The code did reach Portal, on the pending session the password made.
    let cookies = stub.totp_cookies();
    assert_eq!(cookies.len(), 1, "{:?}", cookies);
    assert!(cookies[0].contains("pending"), "the code went on the pending jar: {:?}", cookies);
    // And it was not written anywhere the app writes.
    for (path, text) in h.every_document() {
        assert!(!text.contains("000000"), "{} carries the code", path.display());
    }
}

#[test]
fn signin_with_the_right_code_finishes_the_same_sign_in() {
    let h = Home::new();
    let stub = Stub::start();
    stub.want_totp();
    // The code from `$FIO_PORTAL_TOTP` this time: `--totp` is one way in, not
    // the only one.
    let out = h.fio(
        &["portal", "signin", &stub.base, EMAIL],
        &[("FIO_PORTAL_PASSWORD", PASSWORD), ("FIO_PORTAL_TOTP", GOOD_CODE)],
        None,
    );
    assert_eq!(out.code, 0, "{} {}", out.stdout, out.stderr);
    assert_eq!(out.stdout.trim(), format!("signed in as {}", EMAIL), "{}", out.stdout);

    // The code was sent on the cookie the password step left — the same jar, so
    // the same session — and the cookie Portal re-set is the one kept.
    let cookies = stub.totp_cookies();
    assert_eq!(cookies.len(), 1, "{:?}", cookies);
    assert!(cookies[0].contains("pending"), "the code went on the pending jar: {:?}", cookies);
    let jar = std::fs::read_to_string(h.data().join("portal-session")).expect("a stored session");
    assert!(jar.contains("signed"), "the signed-in cookie is the one stored: {jar}");
    assert!(!jar.contains("pending"), "the pending jar is not kept: {jar}");

    // The address and the email are settings; the password and the code are not.
    let settings = h.settings();
    assert!(settings.contains(&stub.base), "{settings}");
    assert!(settings.contains(EMAIL), "{settings}");
    assert!(!settings.contains(PASSWORD), "{settings}");
    // No file the app wrote carries the code, and nothing printed it.
    assert!(!out.stdout.contains(GOOD_CODE) && !out.stderr.contains(GOOD_CODE), "{} {}", out.stdout, out.stderr);
    for (path, text) in h.every_document() {
        if path.ends_with("portal-session") {
            // The jar is the credential itself; check it does not name the code.
            assert!(!text.contains(GOOD_CODE), "{} carries the code", path.display());
            continue;
        }
        assert!(!text.contains(GOOD_CODE), "{} carries the code", path.display());
    }

    // And a later process is signed in on the strength of that jar alone.
    let st = h.fio(&["portal", "status"], &[], None);
    assert_eq!(st.code, 0, "{}", st.stdout);
    assert!(st.stdout.contains(&format!("credential: signed in as {}", EMAIL)), "{}", st.stdout);
}

#[test]
fn signin_with_nothing_to_sign_in_with_is_an_error_and_a_usage() {
    let h = Home::new();
    let stub = Stub::start();
    // No email at all: the usage, exit 2, and nothing is called.
    let out = h.fio(&["portal", "signin", &stub.base], &[], None);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("fio portal signin <url> <email>"), "{}", out.stderr);
    assert_eq!(stub.hits(), 0, "a usage error must not call Portal");

    // An address that is not one: refused before the request, for the same reason
    // the app refuses it.
    let out = h.fio(&["portal", "signin", "127.0.0.1:18080", EMAIL], &[("FIO_PORTAL_PASSWORD", PASSWORD)], None);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("has to start with http:// or https://"), "{}", out.stderr);
    assert_eq!(stub.hits(), 0, "a refused address must not call Portal");

    // No password to send: the app's own sentence.
    let out = h.fio(&["portal", "signin", &stub.base, EMAIL], &[], Some("\n"));
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("enter the password"), "{}", out.stderr);
}

// ---------- reconcile ----------

#[test]
fn reconcile_makes_one_watch_reports_two_skips_and_changes_nothing_the_second_time() {
    let h = Home::new();
    let stub = Stub::start();
    assert_eq!(h.fio(&["portal", "token", TOKEN], &[], None).code, 0);

    let out = h.fio(&["portal", "reconcile", "--url", &stub.base], &[], None);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines[0], "links seen: 3", "{}", out.stdout);
    assert_eq!(lines[1], "kept in step: 1", "{}", out.stdout);
    assert_eq!(lines[2], "removed: 0", "{}", out.stdout);
    let skips: Vec<&str> = lines.iter().copied().filter(|l| l.starts_with("skipped ")).collect();
    assert_eq!(skips.len(), 2, "one line per skip: {}", out.stdout);
    assert!(skips[0].contains("../etc"), "{}", out.stdout);
    assert!(skips[0].contains("contains '/'"), "the reason is on the line: {}", out.stdout);
    assert!(skips[1].contains('.'), "{}", out.stdout);
    assert!(skips[1].contains("is not a folder name"), "{}", out.stdout);
    assert!(out.stdout.contains("watch folders:"), "{}", out.stdout);
    assert!(out.stdout.contains(&h.watch_line()), "{}", out.stdout);
    h.assert_token_kept_out_of_settings(&out);

    // The folder exists, and nothing was created outside the root.
    assert!(h.root().join("Farbhaus").is_dir(), "the mirror folder is there");
    assert!(!h.dir.join("etc").exists(), "the hostile marker left the root");
    assert!(!h.root().join("etc").exists(), "the hostile marker made a folder");

    // A second run changes nothing: the same watch, and no folder made or edited.
    let again = h.fio(&["portal", "reconcile", "--url", &stub.base], &[], None);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(again.stdout.contains("kept in step: 0"), "the second run had nothing to do: {}", again.stdout);
    assert!(again.stdout.contains("removed: 0"), "{}", again.stdout);
    assert!(again.stdout.contains(&h.watch_line()), "{}", again.stdout);
    let skips2 = again.stdout.lines().filter(|l| l.starts_with("skipped ")).count();
    assert_eq!(skips2, 2, "{}", again.stdout);
    // Reading the links is all a reconcile does on Portal's side. Two runs, two
    // reads, and nothing else.
    let called = stub.lines();
    assert!(called.iter().all(|l| l.contains("/api/upload-links")), "a reconcile called more than the links: {:?}", called);
    assert_eq!(called.len(), 2, "one read per run: {:?}", called);

    // And `status` says what that run was: the CLI keeps the report for it.
    let st = h.fio(&["portal", "status", "--url", &stub.base], &[], None);
    assert_eq!(st.code, 0, "{}", st.stdout);
    assert!(st.stdout.contains("3 links"), "{}", st.stdout);
    assert!(st.stdout.contains("2 skipped"), "{}", st.stdout);
    assert!(!st.stdout.contains("not been checked yet"), "{}", st.stdout);
}

#[test]
fn reconcile_without_a_credential_exits_one_and_creates_nothing() {
    let h = Home::new();
    let stub = Stub::start();
    let out = h.fio(&["portal", "reconcile", "--url", &stub.base], &[], None);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("no Portal token or signed-in session is stored"), "{}", out.stderr);
    assert!(!h.root().exists(), "nothing may be created without a credential");
    assert_eq!(stub.hits(), 0, "nothing may be asked of Portal without a credential");
}

#[test]
fn reconcile_with_a_refused_token_exits_one_and_status_shows_the_failure() {
    let h = Home::new();
    let stub = Stub::start();
    assert_eq!(h.fio(&["portal", "token", "definitely-not-the-token"], &[], None).code, 0);

    let out = h.fio(&["portal", "reconcile", "--url", &stub.base], &[], None);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stderr.contains("refused the token"), "{}", out.stderr);
    assert!(!out.stdout.contains("links seen"), "a failed run prints no counts: {}", out.stdout);
    assert!(!h.root().join("Farbhaus").exists(), "nothing may be created on a refused token");

    let st = h.fio(&["portal", "status", "--url", &stub.base], &[], None);
    assert_eq!(st.code, 0, "a credential is stored, so status itself is configured: {}", st.stdout);
    assert!(st.stdout.contains("Failed"), "a failed run must never read as a success: {}", st.stdout);
    assert!(st.stdout.contains("refused the token"), "{}", st.stdout);
}

// ---------- signout ----------

#[test]
fn signout_forgets_the_session_and_status_says_so() {
    let h = Home::new();
    let stub = Stub::start();
    assert_eq!(
        h.fio(&["portal", "signin", &stub.base, EMAIL], &[("FIO_PORTAL_PASSWORD", PASSWORD)], None).code,
        0
    );
    let out = h.fio(&["portal", "signout"], &[], None);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("signed out: the stored Portal session is gone."), "{}", out.stdout);
    assert!(!h.data().join("portal-session").exists(), "the jar is gone");

    let st = h.fio(&["portal", "status"], &[], None);
    assert_eq!(st.code, 1, "with no credential there is nothing to be configured: {}", st.stdout);
    assert!(st.stdout.contains("Not signed in yet"), "{}", st.stdout);
    assert!(st.stdout.contains("credential: no credential"), "{}", st.stdout);
    // The address is not a credential: signing out leaves it alone.
    assert!(st.stdout.contains(&stub.base), "{}", st.stdout);

    // Signing out again says there was nothing to sign out of.
    let again = h.fio(&["portal", "signout"], &[], None);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert!(again.stdout.contains("no Portal session was stored."), "{}", again.stdout);
}

#[test]
fn signout_keeps_a_bearer_token_and_says_how_to_forget_it() {
    let h = Home::new();
    assert_eq!(h.fio(&["portal", "token", TOKEN], &[], None).code, 0);
    let out = h.fio(&["portal", "signout"], &[], None);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("no Portal session was stored."), "{}", out.stdout);
    assert!(out.stdout.contains("fio portal token --clear"), "{}", out.stdout);
    h.assert_token_kept_out_of_settings(&out);
}

// ---------- the usage ----------

#[test]
fn the_usage_names_every_portal_command() {
    let h = Home::new();
    let help = h.fio(&["portal", "--help"], &[], None);
    assert_eq!(help.code, 0, "{}", help.stderr);
    for word in ["status", "signin", "signout", "reconcile", "token"] {
        assert!(help.stdout.contains(word), "the usage does not name {}: {}", word, help.stdout);
    }
    // A second factor is a code on the same command now, and the usage says so.
    assert!(help.stdout.contains("--totp CODE"), "{}", help.stdout);
    assert!(help.stdout.contains("FIO_PORTAL_TOTP"), "{}", help.stdout);
    // No subcommand, and an unknown one, are usage errors — and call nothing.
    let bare = h.fio(&["portal"], &[], None);
    assert_eq!(bare.code, 2, "{}", bare.stderr);
    assert!(bare.stderr.contains("fio portal signin <url> <email>"), "{}", bare.stderr);
    let unknown = h.fio(&["portal", "sync"], &[], None);
    assert_eq!(unknown.code, 2, "{}", unknown.stderr);
    // A `--url` with no value is no override, not a crash: the stored setting (or
    // its absence) decides, and a fresh machine is not configured.
    let no_value = h.fio(&["portal", "status", "--url"], &[], None);
    assert_eq!(no_value.code, 1, "{}", no_value.stdout);
    // The bare `fio` usage names the portal commands too.
    let usage = h.fio(&[], &[], None);
    assert_eq!(usage.code, 2, "{}", usage.stderr);
    assert!(usage.stderr.contains("fio portal"), "{}", usage.stderr);
}
