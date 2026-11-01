//! Live end-to-end check against a running Portal instance.
//!
//! Skipped unless the variables each case needs are set, so a normal
//! `cargo test` never touches the network. Everything else is the real path:
//! `App::portal_reconcile` -> `portal::agent::reconcile` -> `PortalClient` over
//! HTTP to the live host -> sqlite.
//!
//!   # with a bearer token (`PORTAL_API_TOKEN` set on that Portal)
//!   PORTAL_LIVE_URL=http://127.0.0.1:18080 PORTAL_LIVE_TOKEN=… \
//!     cargo test --offline -p fiotransfer --test portal_live -- --nocapture
//!
//!   # signing in instead, for a Portal that holds no token
//!   PORTAL_LIVE_URL=http://127.0.0.1:18080 PORTAL_LIVE_EMAIL=… PORTAL_LIVE_PASSWORD=… \
//!     cargo test --offline -p fiotransfer --test portal_live -- --nocapture
//!
//! The password is read from the environment and from nowhere else: a password
//! written into a source file is a password in a repository.

use std::path::PathBuf;
use std::sync::Arc;

use fiotransfer::app::App;
use fiotransfer::config::Settings;
use fiotransfer::history::Store;
use fiotransfer::portal;
use fiotransfer::xfer::{self, manager as xfer_manager};

fn live() -> Option<(String, String)> {
    match (std::env::var("PORTAL_LIVE_URL"), std::env::var("PORTAL_LIVE_TOKEN")) {
        (Ok(u), Ok(t)) if !u.trim().is_empty() && !t.trim().is_empty() => Some((u, t)),
        _ => {
            eprintln!("skipped: PORTAL_LIVE_URL / PORTAL_LIVE_TOKEN not set");
            None
        }
    }
}

/// The admin sign-in's inputs: the same base URL, and the email and password
/// from the environment. Portal's own login is the only place a password is
/// typed, so it is never a constant in this file.
fn live_login() -> Option<(String, String, String)> {
    let url = std::env::var("PORTAL_LIVE_URL").ok().filter(|u| !u.trim().is_empty());
    let email = std::env::var("PORTAL_LIVE_EMAIL").ok().filter(|e| !e.trim().is_empty());
    let password = std::env::var("PORTAL_LIVE_PASSWORD").ok().filter(|p| !p.is_empty());
    match (url, email, password) {
        (Some(u), Some(e), Some(p)) => Some((u, e, p)),
        _ => {
            eprintln!("skipped: PORTAL_LIVE_URL / PORTAL_LIVE_EMAIL / PORTAL_LIVE_PASSWORD not set");
            None
        }
    }
}

/// One live case at a time. `world` sets `HOME` and `FIO_NO_KEYCHAIN` for the
/// whole process, so two cases at once would share — and clobber — the folder
/// the stored credentials live in.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Every file under `dir`, as text: what the app wrote, wherever it wrote it.
fn every_document_under(dir: &std::path::Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(b) = std::fs::read(&p) {
                out.push((p.clone(), String::from_utf8_lossy(&b).into_owned()));
            }
        }
    }
    out
}

struct World {
    rt: tokio::runtime::Runtime,
    root: PathBuf,
    media: PathBuf,
    app: Arc<App>,
}

fn world(portal_url: &str) -> World {
    let tmp = tempfile::tempdir().expect("temp dir");
    let home = tmp.path().join("home");
    let media = tmp.path().join("media");
    let data = home.join("Library/Application Support/FioTransfer");
    let logs = home.join("Library/Logs/FioTransfer");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::create_dir_all(&media).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("FIO_NO_KEYCHAIN", "1");

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let hist = Arc::new(Store::open(&data, &logs).expect("history store"));
    let root = media.join("Live_Work");
    let _g = rt.enter();
    let mut s = Settings::default();
    s.portal_url = portal_url.to_string();
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
    watcher.close(); // these cases assert on the watch list, not on transfers
    *app.wat.lock().unwrap() = Some(watcher);
    *app.mgr.lock().unwrap() = Some(mgr);
    // keep the temp dir alive for the life of the world
    std::mem::forget(tmp);
    World { rt, root, media, app }
}

#[test]
fn live_portal_reconciles_into_watch_folders() {
    let _serial = one_at_a_time();
    let Some((url, token)) = live() else { return };
    let w = world(&url);
    portal::set_token(&token).expect("token stored");

    let _g = w.rt.enter();
    let report = w.rt.block_on(async { w.app.portal_reconcile().await });
    println!("report: {:#?}", report);
    assert!(report.error.is_empty(), "reconcile reported an error: {:?}", report.error);

    let watches = w.app.hist.lock().unwrap().as_ref().unwrap().watches().expect("watches");
    for r in &watches {
        println!("watch  origin={:?} local={} acct={} folder={} label={:?} dir={}", r.origin, r.local_path, r.account_id, r.folder_id, r.dest_label, r.direction);
    }

    let portal_watches: Vec<_> = watches.iter().filter(|r| r.origin.starts_with("portal:")).collect();
    assert_eq!(portal_watches.len(), 1, "exactly one link carries a usable marker");
    let pw = portal_watches[0];
    assert_eq!(PathBuf::from(&pw.local_path), w.root.join("Farbhaus"));
    assert_eq!(pw.account_id, "acct-1");
    assert_eq!(pw.folder_id, "folder-1");
    assert_eq!(pw.direction, "mirror");
    assert!(std::path::Path::new(&pw.local_path).is_dir(), "the mirror folder exists");

    // A hostile marker must never have created a folder, anywhere.
    assert!(!w.media.join("etc").exists());
    assert!(!w.root.join("etc").exists());
    // The account-less link is skipped, so no folder for it either.
    assert!(!w.root.join("Dots").exists());

    // Idempotent: the second run changes nothing.
    let before: Vec<(String, String, String)> =
        watches.iter().map(|r| (r.local_path.clone(), r.account_id.clone(), r.folder_id.clone())).collect();
    let _r2 = w.rt.block_on(async { w.app.portal_reconcile().await });
    let after: Vec<(String, String, String)> = w
        .app
        .hist
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .watches()
        .expect("watches")
        .into_iter()
        .map(|r| (r.local_path, r.account_id, r.folder_id))
        .collect();
    assert_eq!(before, after, "a second reconcile changed the watch list");
}

/// A live Portal is the only place the real `folder` object is, and that object is
/// what decides whether a link is one mirrored watch or two one-way folders. So:
/// read the same rows the app reads, and check the watch list against each link's
/// own mapping — `mirror` at `root/<marker>` when the link carries no `sync`, a
/// `download` watch at `root/<marker>/<drive.local>` and an `upload` watch at
/// `root/<marker>/<exports.local>` when it does.
///
/// Written for both shapes, so it is true of a Portal that seeds a `sync` link
/// and of one that does not yet.
#[test]
fn live_portal_watches_match_each_links_own_mapping() {
    let _serial = one_at_a_time();
    let Some((url, token)) = live() else { return };
    let w = world(&url);
    portal::set_token(&token).expect("token stored");

    let _g = w.rt.enter();
    let links = {
        let c = portal::PortalClient::from_settings(&w.app.settings()).expect("a client");
        w.rt.block_on(async { c.upload_links().await }).expect("GET /api/upload-links")
    };
    let rep = w.rt.block_on(async { w.app.portal_reconcile().await });
    println!("report: {:#?}", rep);
    assert!(rep.error.is_empty(), "reconcile reported an error: {:?}", rep.error);

    let root = w.root.to_string_lossy().into_owned();
    let rows = w.app.hist.lock().unwrap().as_ref().unwrap().watches().expect("watches");
    // Portal's own watches, by origin: nothing the user made is in here.
    let by_origin: std::collections::BTreeMap<String, _> =
        rows.iter().filter(|r| r.origin.starts_with("portal:")).map(|r| (r.origin.clone(), r)).collect();

    for l in &links {
        let marker = l.marker();
        if marker.is_empty() || l.is_disabled() || l.acct().is_empty() {
            continue; // Portal is not asking for a watch for this one
        }
        match l.folder.sync.as_ref() {
            None => {
                let origin = format!("portal:{}", l.id);
                let row = by_origin.get(&origin).unwrap_or_else(|| panic!("no watch for {} ({})", l.id, marker));
                assert_eq!(row.local_path, format!("{}/{}", root, marker), "a link with no sync is one mirror");
                assert_eq!(row.direction, "mirror");
            }
            Some(sync) => {
                println!("link {} is two one-way folders: {:?}", l.id, sync);
                if let Some(d) = sync.drive.as_ref() {
                    let local = format!("{}/{}/{}", root, marker, d.local);
                    let row = by_origin
                        .get(&format!("portal:{}", l.id))
                        .unwrap_or_else(|| panic!("no drive watch for {} at {}", l.id, local));
                    assert_eq!(row.local_path, local, "the drive folder, under the marker");
                    assert_eq!(row.direction, "download", "the client's folder only comes down");
                    let want = if d.folder_id.is_empty() { l.dest_folder() } else { d.folder_id.clone() };
                    assert_eq!(row.folder_id, want, "drive.folder_id, or the link's own folder");
                }
                if let Some(x) = sync.exports.as_ref() {
                    let local = format!("{}/{}/{}", root, marker, x.local);
                    let row = by_origin
                        .get(&format!("portal:{}#exports", l.id))
                        .unwrap_or_else(|| panic!("no exports watch for {} at {}", l.id, local));
                    assert_eq!(row.local_path, local, "the exports folder, under the marker");
                    assert_eq!(row.direction, "upload", "what we make only goes up");
                    assert_eq!(row.folder_id, x.folder_id);
                    assert_ne!(
                        row.direction, "mirror",
                        "no mapping may ever be widened to a mirror: that is what would copy a deletion over"
                    );
                }
            }
        }
    }

    // Every Portal watch is one of these mappings, and the second run changes
    // nothing at all.
    let snapshot: Vec<(String, String, String)> =
        rows.iter().map(|r| (r.id.clone(), r.origin.clone(), r.direction.clone())).collect();
    let rep2 = w.rt.block_on(async { w.app.portal_reconcile().await });
    assert!(rep2.error.is_empty(), "second run failed: {}", rep2.error);
    assert_eq!((rep2.ensured, rep2.removed), (0, 0), "a second reconcile must change nothing");
    let again: Vec<(String, String, String)> = w
        .app
        .hist
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .watches()
        .expect("watches")
        .into_iter()
        .map(|r| (r.id, r.origin, r.direction))
        .collect();
    assert_eq!(snapshot, again, "the watch list changed on a second run");
}

/// The same host, with a token that is not the right one: the status line must say
/// the token was refused, not print Portal's JSON. Runs against any Portal; skipped
/// unless PORTAL_LIVE_URL is set.
#[test]
fn live_portal_refuses_a_bad_token() {
    let _serial = one_at_a_time();
    let Ok(url) = std::env::var("PORTAL_LIVE_URL") else {
        eprintln!("skipped: PORTAL_LIVE_URL not set");
        return;
    };
    if url.trim().is_empty() {
        return;
    }
    let w = world(&url);
    portal::set_token("definitely-not-the-token").expect("token stored");

    let _g = w.rt.enter();
    let report = w.rt.block_on(async { w.app.portal_reconcile().await });
    println!("report error: {}", report.error);
    assert!(!report.error.is_empty(), "a refused token must be an error, not a quiet success");
    assert!(report.error.contains("refused the token"), "got: {}", report.error);
    assert!(!report.error.contains("\"detail\""), "the JSON wrapper reached the user: {}", report.error);
    assert_eq!(report.ensured, 0, "nothing may be created when the token is refused");

    // And nothing reached the watch list either.
    let watches = w.app.hist.lock().unwrap().as_ref().unwrap().watches().expect("watches");
    assert!(watches.is_empty(), "{} watch(es) created without authorisation", watches.len());
}


// ---------- signing in instead of a bearer token ----------
//
// A Portal deployed by `deploy.sh` holds no `PORTAL_API_TOKEN`, and its `/api`
// endpoints then accept one thing only: the admin's session cookie. These three
// cases are that path, against a live Portal: the sign-in itself, a wrong
// password, and a session that has expired.

/// Sign in with Portal's own login: the cookie is kept as a jar, no password is
/// written anywhere, and the reconcile it authorises makes exactly the watch the
/// bearer-token case makes.
#[test]
fn live_portal_signs_in_and_reconciles_the_same_watch() {
    let _serial = one_at_a_time();
    let Some((url, email, password)) = live_login() else { return };
    let w = world(&url);
    // A stored token wins over a session (that is the precedence), and this case
    // is about the session.
    portal::clear_token();

    let _g = w.rt.enter();
    let st = w
        .rt
        .block_on(async { w.app.portal_sign_in(&email, &password).await })
        .unwrap_or_else(|e| panic!("the sign-in failed: {e}"));
    println!("signed in as {:?} (signed_in={})", st.email, st.signed_in);
    assert!(st.signed_in, "a session is a stored credential");
    assert_eq!(st.email, email, "Portal names the admin it signed in");
    assert_eq!(portal::signed_in_email().as_deref(), Some(email.as_str()));

    // The jar is kept, as JSON, in the app's own data folder (there is no
    // Keychain under FIO_NO_KEYCHAIN), and it names the Portal that issued it.
    let stored = portal::session().expect("a stored session");
    assert_eq!(stored.base, url.trim().trim_end_matches('/'));
    assert!(!stored.is_empty(), "the session cookie is kept");
    assert_eq!(w.app.portal_status().email, email);

    // The password is nowhere the app writes: not in settings.json, not beside
    // it, not in the log folder.
    for (path, text) in every_document_under(&fiotransfer::config::dir()) {
        assert!(!text.contains(&password), "{} carries the password", path.display());
    }
    for (path, text) in every_document_under(&fiotransfer::config::log_dir()) {
        assert!(!text.contains(&password), "{} carries the password", path.display());
    }
    // … and the email, which is not a secret, is a setting so the field can be
    // prefilled next time.
    let saved = std::fs::read_to_string(fiotransfer::config::path()).expect("settings.json");
    assert!(saved.contains(&email), "the email is prefilled from settings: {saved}");

    // The stored session alone is a usable credential: no token is stored.
    assert_eq!(portal::token(), None);
    let c = portal::PortalClient::from_settings(&w.app.settings()).expect("the session is a credential");
    assert_eq!(c.credential(), "session");
    let who = w.rt.block_on(async { c.me().await }).expect("GET /api/auth/me with the session");
    assert_eq!(who.email, email, "me() names the signed-in admin");
    let links = w
        .rt
        .block_on(async { c.upload_links().await })
        .expect("GET /api/upload-links with the session");
    println!("links read with the session: {}", links.len());
    assert!(!links.is_empty(), "a signed-in session reads the links");
    assert!(links.iter().any(|l| l.marker() == "Farbhaus"), "the seeded link is there");

    // And the reconcile it authorises makes the same folder the token does.
    let report = w.rt.block_on(async { w.app.portal_reconcile().await });
    println!("report: {:#?}", report);
    assert!(report.error.is_empty(), "reconcile reported an error: {:?}", report.error);
    let watches = w.app.hist.lock().unwrap().as_ref().unwrap().watches().expect("watches");
    let portal_watches: Vec<_> = watches.iter().filter(|r| r.origin.starts_with("portal:")).collect();
    assert_eq!(portal_watches.len(), 1, "exactly one link carries a usable marker");
    let pw = portal_watches[0];
    assert_eq!(PathBuf::from(&pw.local_path), w.root.join("Farbhaus"));
    assert_eq!(pw.account_id, "acct-1");
    assert_eq!(pw.folder_id, "folder-1");
    assert!(std::path::Path::new(&pw.local_path).is_dir(), "the mirror folder exists");
    assert!(!w.media.join("etc").exists(), "the hostile marker created nothing");

    // Signing out forgets the jar — and nothing else — and the next reconcile
    // says there is no credential instead of calling Portal with none.
    let st = w.app.portal_sign_out();
    assert!(portal::session().is_none(), "the jar is gone");
    assert!(!portal::signed_in());
    assert!(!st.signed_in);
    let rep = w.rt.block_on(async { w.app.portal_reconcile().await });
    assert!(rep.error.contains("token"), "got: {}", rep.error);
}

/// A wrong password: Portal's answer is the message, no cookie is kept, and
/// nothing that could be sent to Portal is stored.
#[test]
fn live_portal_refuses_a_wrong_password_and_keeps_nothing() {
    let _serial = one_at_a_time();
    let Some((url, email, password)) = live_login() else { return };
    let w = world(&url);
    portal::clear_token();
    portal::clear_session();

    let wrong = format!("{password}-and-then-some");
    let _g = w.rt.enter();
    let err = w
        .rt
        .block_on(async { w.app.portal_sign_in(&email, &wrong).await })
        .expect_err("a wrong password must not sign in");
    println!("refused: {err}");
    assert!(err.contains("refused the sign-in"), "got: {err}");
    assert!(err.contains("401"), "got: {err}");
    assert!(!err.contains(&wrong), "the password reached the message: {err}");

    // No cookie, no credential, and the app knows neither.
    assert!(portal::session().is_none(), "a refused sign-in stores no cookie");
    assert!(!portal::signed_in());
    assert!(!w.app.portal_status().signed_in);
    let rep = w.rt.block_on(async { w.app.portal_reconcile().await });
    assert!(rep.error.contains("token"), "got: {}", rep.error);

    // The password is not in settings.json, not beside it, not in a log.
    for dir in [fiotransfer::config::dir(), fiotransfer::config::log_dir()] {
        for (path, text) in every_document_under(&dir) {
            assert!(!text.contains(&wrong), "{} carries the password", path.display());
        }
    }
}

// ---------- the second factor ----------
//
// A Portal account can have TOTP on (Portal's own Settings → Security), and then
// the password sign-in is only half of one: `POST /api/auth/login` answers
// `totp_required` and Portal keeps a pending uid in the same session until the
// six digits arrive at `POST /api/auth/login/totp`. These cases are that path
// against a live Portal, and the enrolment they need is done through Portal's own
// three routes — the app deliberately has no code that creates or reads a TOTP
// secret.
//
// The instance this runs against must be left exactly as it was found. The
// restore therefore runs whether the body succeeded or not (and also after a
// panic in it), and the last thing the case asserts is that a password-only
// sign-in works again. If the process is killed outright the restore cannot run;
// the documented manual step is, with the admin's own password:
//
//   curl -s -c /tmp/j -X POST http://127.0.0.1:18080/api/auth/login \
//     -H 'Content-Type: application/json' \
//     -d '{"email":"chaos@localhost","password":"portal-local-dev-pass-1"}'
//   curl -s -b /tmp/j -X POST http://127.0.0.1:18080/api/settings/totp/disable \
//     -H 'Content-Type: application/json' -d '{"password":"portal-local-dev-pass-1"}'
//
// (with the password in the second call too: `totp_disable` accepts the password
// or a current code, and the password is the one that is always to hand).

/// The secret Portal handed out at `totp/begin`, so the restore can still finish
/// a sign-in after the body has panicked. A test file's scratch slot, and the
/// only place a secret lives: it is not printed and not written anywhere.
static ENROLLED_SECRET: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn remember_secret(secret: Option<String>) {
    *ENROLLED_SECRET.lock().unwrap_or_else(|e| e.into_inner()) = secret;
}

fn remembered_secret() -> Option<String> {
    ENROLLED_SECRET.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ---------- RFC 6238, over `std` ----------
//
// Portal verifies with `pyotp`'s defaults: HMAC-SHA-1, a 30 s step, six digits,
// and a ±1 step window. The arithmetic below is the same and is about sixty
// lines, which is cheaper than a crate added to the lock file for one test — and
// it is checked against the RFC's own published vectors first.

/// RFC 4648 base32, the shape Portal's `generate_secret` writes: uppercase,
/// unpadded.
fn base32_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut bits: u32 = 0;
    let mut acc: u64 = 0;
    for ch in s.chars() {
        let v = match ch.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            '=' | ' ' | '-' => continue,
            other => return Err(format!("{:?} is not a base32 character", other)),
        };
        acc = (acc << 5) | v as u64;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1u64 << bits) - 1;
    }
    Ok(out)
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..20].copy_from_slice(&sha1(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + msg.len());
    inner.extend(block.iter().map(|b| b ^ 0x36));
    inner.extend_from_slice(msg);
    let digest = sha1(&inner);
    let mut outer = Vec::with_capacity(84);
    outer.extend(block.iter().map(|b| b ^ 0x5c));
    outer.extend_from_slice(&digest);
    sha1(&outer)
}

/// The six digits a authenticator app would show at `unix` for `secret`.
fn totp_at(secret: &str, unix: u64) -> Result<String, String> {
    let key = base32_decode(secret)?;
    let counter = unix / 30;
    let mac = hmac_sha1(&key, &counter.to_be_bytes());
    let offset = (mac[19] & 0x0f) as usize;
    let bin = ((mac[offset] as u32 & 0x7f) << 24)
        | ((mac[offset + 1] as u32) << 16)
        | ((mac[offset + 2] as u32) << 8)
        | (mac[offset + 3] as u32);
    Ok(format!("{:06}", bin % 1_000_000))
}

/// The same, for now.
fn totp_now(secret: &str) -> Result<String, String> {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| e.to_string())?;
    totp_at(secret, unix)
}

/// A six-digit code that is not `current`: Portal answers it with "Invalid code"
/// and the pending step survives, which is what a typo looks like.
fn a_wrong_code(current: &str) -> String {
    let n: u32 = current.parse().unwrap_or(0);
    let wrong = (n + 1) % 1_000_000;
    let text = format!("{wrong:06}");
    if text == current { "000000".to_string() } else { text }
}

/// Portal's own security routes, called with the session cookie the app keeps.
/// Nothing here is product code: the app never creates or reads a TOTP secret.
struct Raw {
    base: String,
    cookie: String,
}

impl Raw {
    fn new(base: &str, jar: &portal::Session) -> Raw {
        let cookie = jar
            .cookies
            .iter()
            .filter_map(|c| c.split(';').next())
            .map(|c| c.trim().to_string())
            .collect::<Vec<_>>()
            .join("; ");
        Raw { base: base.trim().trim_end_matches('/').to_string(), cookie }
    }

    async fn post(&self, path: &str, body: serde_json::Value) -> Result<(u16, String), String> {
        let res = reqwest::Client::new()
            .post(format!("{}{}", self.base, path))
            .header("Accept", "application/json")
            .header("Cookie", &self.cookie)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("{}: {}", path, e))?;
        let status = res.status().as_u16();
        let text = res.text().await.map_err(|e| format!("{}: {}", path, e))?;
        Ok((status, text))
    }
}

/// What one run of the body saw, so the case can assert after the instance is
/// back the way it was found.
#[derive(Debug, Default)]
struct TotpRun {
    /// `portal_sign_in` on an instance with no second factor.
    found_without_totp: bool,
    /// What the next password step said, and what the app reported.
    needs_code: bool,
    needs_code_said: String,
    nothing_stored_by_the_password_step: bool,
    /// What Portal said about a wrong code, and what the app reported.
    wrong_code_said: String,
    wrong_code_stored_nothing: bool,
    /// The right code's answer.
    signed_in_as: String,
    signed_in: bool,
    needs_code_cleared: bool,
    /// `GET /api/upload-links` with the session the code produced.
    links: usize,
    /// The jar's keys: the credential is the only thing kept.
    kept_keys: Vec<String>,
    /// The secret appears in no file the app wrote.
    secret_in_no_document: bool,
}

/// The body of the case: sign in, enrol, prove the three answers, sign in with
/// the code, read the links. Returns what it saw; it does not assert, because the
/// instance has to be restored before anything can fail the test.
async fn totp_round_trip(w: &World, url: &str, email: &str, password: &str) -> Result<TotpRun, String> {
    let mut run = TotpRun::default();
    let app = &w.app;

    // 1. As found: the password alone signs in, so this instance had no second
    //    factor.
    portal::clear_token();
    portal::clear_session();
    let st = app.portal_sign_in(email, password).await?;
    run.found_without_totp = st.signed_in && !st.needs_code;
    let jar = portal::session().ok_or("the password sign-in stored no session")?;

    // 2. Enrol, through Portal's own routes.
    let raw = Raw::new(url, &jar);
    let (status, text) = raw.post("/api/settings/totp/begin", serde_json::json!({})).await?;
    if status != 200 {
        return Err(format!("totp/begin answered {}: {}", status, text));
    }
    let begun: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("totp/begin: {}", e))?;
    let secret = begun
        .get("secret")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or("totp/begin sent no secret")?;
    remember_secret(Some(secret.clone()));
    let code = totp_now(&secret)?;
    let (status, text) = raw.post("/api/settings/totp/enable", serde_json::json!({ "code": code })).await?;
    if status != 200 {
        return Err(format!("totp/enable answered {}: {}", status, text));
    }

    // 3. A fresh password step now stops at the code.
    app.portal_sign_out();
    let said = app.portal_sign_in(email, password).await.expect_err("a second factor must not sign in");
    run.needs_code_said = said;
    run.needs_code = app.portal_status().needs_code;
    run.nothing_stored_by_the_password_step = portal::session().is_none() && !app.portal_status().signed_in;

    // 4. A wrong code is a refusal with Portal's sentence, and keeps nothing.
    let right = totp_now(&secret)?;
    let wrong = a_wrong_code(&right);
    let said = app.portal_sign_in_totp(&wrong).await.expect_err("a wrong code must not sign in");
    run.wrong_code_said = said;
    run.wrong_code_stored_nothing = portal::session().is_none() && !app.portal_status().signed_in;

    // 5. The right code finishes the same sign-in.
    let code = totp_now(&secret)?;
    let st = app.portal_sign_in_totp(&code).await?;
    run.signed_in_as = st.email.clone();
    run.signed_in = st.signed_in;
    run.needs_code_cleared = !st.needs_code;

    // 6. The session it produced works: the links are what a reconcile reads.
    let client = portal::PortalClient::from_settings(&app.settings()).map_err(|e| e.to_string())?;
    run.links = client.upload_links().await.map_err(|e| e.to_string())?.len();

    // 7. What is kept is the session and nothing else: the jar has its three
    //    fields, and no file the app wrote carries the secret.
    let kept = portal::session().ok_or("the code left no session")?;
    let as_json: serde_json::Value = serde_json::to_value(&kept).map_err(|e| e.to_string())?;
    run.kept_keys = as_json
        .as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    run.secret_in_no_document = [fiotransfer::config::dir(), fiotransfer::config::log_dir()]
        .iter()
        .all(|dir| every_document_under(dir).iter().all(|(_, text)| !text.contains(&secret)));
    Ok(run)
}

/// Puts the instance back: a password-only sign-in works again, or there is a
/// sentence saying what is still on. Takes the secret when there is one, so a
/// body that stopped halfway can still be finished.
async fn restore_without_totp(
    w: &World,
    email: &str,
    password: &str,
    secret: Option<&str>,
) -> Result<String, String> {
    let app = &w.app;
    app.portal_sign_out();
    // The state to restore is "the password alone signs in". When it already
    // does, there is nothing to turn off.
    if app.portal_sign_in(email, password).await.is_ok() {
        app.portal_sign_out();
        return Ok("left without two-factor: the password alone signs in".to_string());
    }
    // A code is wanted. With the secret, finish that sign-in and turn the second
    // factor off with the password Portal still accepts for it.
    let Some(secret) = secret else {
        return Err(
            "the instance still has two-factor on and the test does not hold its secret: sign in with a current code and POST /api/settings/totp/disable {\"password\":…} (see the note at the head of these cases)".to_string(),
        );
    };
    let code = totp_now(secret)?;
    let st = app.portal_sign_in_totp(&code).await?;
    let jar = portal::session().ok_or("the code left no session")?;
    let raw = Raw::new(&st.url, &jar);
    let (status, text) = raw
        .post("/api/settings/totp/disable", serde_json::json!({ "password": password }))
        .await?;
    if status != 200 {
        return Err(format!("totp/disable answered {}: {}", status, text));
    }
    // …and the proof it worked is the same check the test opened with.
    app.portal_sign_out();
    let st = app.portal_sign_in(email, password).await?;
    if st.needs_code {
        return Err("two-factor is still on after totp/disable".to_string());
    }
    app.portal_sign_out();
    Ok("left without two-factor: totp/disable ran and the password alone signs in".to_string())
}

/// The code generator, against RFC 6238's own SHA-1 vectors — no Portal needed,
/// so this one runs in every `cargo test`. The eight-digit values are the RFC's;
/// the six digits are the same truncation with the modulus Portal verifies.
#[test]
fn the_code_generator_matches_the_rfc_6238_vectors() {
    // "12345678901234567890", base32, unpadded: the seed every vector uses.
    let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    assert_eq!(base32_decode(secret).expect("base32"), b"12345678901234567890".to_vec());
    let cases: [(u64, &str); 6] = [
        (59, "94287082"),
        (1_111_111_109, "07081804"),
        (1_111_111_111, "14050471"),
        (1_234_567_890, "89005924"),
        (2_000_000_000, "69279037"),
        (20_000_000_000, "65353130"),
    ];
    for (t, eight) in cases {
        assert_eq!(totp_at(secret, t).expect("a code"), eight[2..].to_string(), "at {}", t);
    }
    // A secret that is not base32 is refused rather than hashed as nonsense.
    assert!(base32_decode("not base32!").is_err());
    // The wrong-code helper never hands back the right one.
    for code in ["000000", "999999", "123456"] {
        let wrong = a_wrong_code(code);
        assert_ne!(wrong, code, "{}", code);
        assert_eq!(wrong.len(), 6);
    }
}

/// The whole second-factor path against a live Portal: a password-only sign-in
/// that works, the enrolment, a password step that stops at a code, a wrong code
/// that is refused, the right code that signs in, the links it reads — and the
/// instance restored, whether or not any of that worked.
#[test]
fn live_portal_signs_in_with_a_one_time_code_and_leaves_the_instance_as_it_was() {
    let _serial = one_at_a_time();
    let Some((url, email, password)) = live_login() else { return };
    let w = world(&url);
    // Whatever the last run left, nothing of it is reused here.
    remember_secret(None);
    let _g = w.rt.enter();

    // The body may not assert: a panic inside `block_on` would skip the restore
    // below, which is the one thing that must always happen.
    let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        w.rt.block_on(async { totp_round_trip(&w, &url, &email, &password).await })
    }));
    let restored = w.rt.block_on(async {
        restore_without_totp(&w, &email, &password, remembered_secret().as_deref()).await
    });
    println!("restore: {:?}", restored);
    assert!(
        restored.is_ok(),
        "the live Portal was NOT put back the way it was found — see the manual step at the head of these cases: {:?}",
        restored
    );

    // Now the body's own result: a panic and a described failure both fail here.
    let run = match body {
        Ok(Ok(run)) => run,
        Ok(Err(e)) => panic!("the second-factor round trip failed: {e}"),
        Err(_) => panic!("the second-factor round trip panicked (the instance was restored first)"),
    };
    println!("the run: {:#?}", run);

    assert!(run.found_without_totp, "the case starts on an instance whose password sign-in works");
    assert!(run.needs_code, "Portal reported the second factor, and the app says a code is owed");
    assert!(run.needs_code_said.contains("one-time code"), "{}", run.needs_code_said);
    assert!(
        run.nothing_stored_by_the_password_step,
        "a half-finished sign-in must store nothing: {:?}",
        portal::session()
    );
    assert!(run.wrong_code_said.contains("refused the one-time code"), "{}", run.wrong_code_said);
    assert!(run.wrong_code_said.contains("Invalid code"), "Portal's own sentence: {}", run.wrong_code_said);
    assert!(run.wrong_code_stored_nothing, "a wrong code must store nothing");
    assert!(run.signed_in, "the right code signs in");
    assert_eq!(run.signed_in_as, email, "Portal names the admin the code signed in");
    assert!(run.needs_code_cleared, "the code row is done with");
    assert!(run.links > 0, "GET /api/upload-links with the session the code produced");
    let mut keys = run.kept_keys.clone();
    keys.sort();
    assert_eq!(
        keys,
        vec!["base".to_string(), "cookies".to_string(), "email".to_string()],
        "the session and nothing else is kept"
    );
    assert!(run.secret_in_no_document, "a file the app wrote carries the TOTP secret");
}

/// A session Portal will not accept says the session expired, and offers the fix
/// — never the token's message, because the fix is not the same one.
#[test]
fn live_portal_says_the_session_expired_not_that_the_token_was_refused() {
    let _serial = one_at_a_time();
    let Some((url, email, _password)) = live_login() else { return };
    let w = world(&url);
    portal::clear_token();

    // Two jars in the shape a real sign-in writes: one holding a cookie Portal
    // never issued, and one emptied — its cookies gone, which is what an expired
    // session looks like on disk.
    let forged = portal::Session {
        base: url.trim().trim_end_matches('/').to_string(),
        email: email.clone(),
        cookies: vec![
            "portal_session=eyJ1c2VyX2lkIjoiMDAwMDAwMDAtMDAwMC0wMDAwLTAwMDAtMDAwMDAwMDAwMDAwIn0.auQbjA.not-a-signature".to_string(),
        ],
    };
    let cases = [
        ("a cookie Portal never issued", forged.clone()),
        ("an emptied jar", portal::Session { cookies: Vec::new(), ..forged.clone() }),
    ];

    let _g = w.rt.enter();
    for (what, jar) in cases {
        portal::save_session(&jar).expect("the jar is stored");
        let c = portal::PortalClient::from_settings(&w.app.settings()).expect("the jar is the credential");
        assert_eq!(c.credential(), "session", "{what}: a session, not a token");

        let rep = w.rt.block_on(async { w.app.portal_reconcile().await });
        println!("{what}: {}", rep.error);
        assert!(rep.error.contains("session expired"), "{what}: {}", rep.error);
        assert!(rep.error.contains("sign in again"), "{what}: {}", rep.error);
        assert!(
            !rep.error.contains("refused the token"),
            "{what}: the token's message is not this one: {}",
            rep.error
        );
        assert_eq!(rep.ensured, 0, "{what}: nothing may be created");
        assert_eq!(rep.links_seen, 0, "{what}: no links were read");
        let watches = w.app.hist.lock().unwrap().as_ref().unwrap().watches().expect("watches");
        assert!(
            watches.iter().all(|r| !r.origin.starts_with("portal:")),
            "{what}: a folder was made without a working credential"
        );
    }
}


/// Connecting to a Portal whose projects already hold media must not push a second
/// copy of it. A watch made by the reconciler is added with `include_existing =
/// false`, which baselines what is already in the folder and tells the next remote
/// listing to record matches instead of fetching them. This asserts that against a
/// folder that was populated before the reconcile ever ran — the state a machine
/// coming from Project Zeus is actually in.
#[test]
fn a_folder_that_already_holds_media_is_baselined_not_re_uploaded() {
    let Some((url, token)) = live() else { return };
    let w = world(&url);
    portal::set_token(&token).expect("token stored");

    // Populate the folder the marker names *before* anything reconciles: three
    // files and a nested one, as a project folder from the old workflow would look.
    let folder = w.root.join("Farbhaus");
    std::fs::create_dir_all(folder.join("Rushes/Day 1")).unwrap();
    std::fs::write(folder.join("slate.txt"), b"slate").unwrap();
    std::fs::write(folder.join("Rushes/Day 1/A001.bin"), vec![7u8; 4096]).unwrap();
    std::fs::write(folder.join("Rushes/Day 1/A002.bin"), vec![8u8; 2048]).unwrap();

    let _g = w.rt.enter();
    let report = w.rt.block_on(async { w.app.portal_reconcile().await });
    assert!(report.error.is_empty(), "{:?}", report.error);
    assert_eq!(report.ensured, 1, "the marker's folder should have been kept in step");

    let watches = w.app.hist.lock().unwrap().as_ref().unwrap().watches().expect("watches");
    let farb = watches
        .iter()
        .find(|r| r.local_path.ends_with("Farbhaus"))
        .expect("the mirrored watch");
    assert!(farb.ignore_remote, "the next remote listing must record what is already here, not fetch it");

    let known = w
        .app
        .hist
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .watch_files(&farb.id)
        .expect("the baseline");
    assert_eq!(known.len(), 3, "three files were there before the first reconcile: {:?}", known.keys().collect::<Vec<_>>());
    for name in ["slate.txt", "Rushes/Day 1/A001.bin", "Rushes/Day 1/A002.bin"] {
        assert!(known.contains_key(name), "{} is not in the baseline: {:?}", name, known.keys().collect::<Vec<_>>());
    }

    // And the files themselves are untouched — a reconcile is not a transfer.
    assert_eq!(std::fs::read(folder.join("slate.txt")).unwrap(), b"slate");
    assert_eq!(std::fs::metadata(folder.join("Rushes/Day 1/A001.bin")).unwrap().len(), 4096);
}


/// A dry run is the answer to "what will you do to my library?", and it has to be
/// honest in both directions: the plan it prints is the plan it would carry out,
/// and running it leaves nothing behind — no folder, no watch, no record.
#[test]
fn a_dry_run_plans_the_same_watch_and_creates_nothing() {
    let Some((url, token)) = live() else { return };
    let w = world(&url);
    portal::set_token(&token).expect("token stored");

    let _g = w.rt.enter();
    let plan = w.rt.block_on(async { fiotransfer::portal::agent::preview(&w.app).await });
    assert!(plan.error.is_empty(), "{:?}", plan.error);
    assert_eq!(plan.links_seen, 3);
    assert_eq!(plan.ensured, 1, "one marker asks for a folder");
    assert_eq!(plan.planned.len(), 1);
    assert!(plan.planned[0].ends_with("Farbhaus"), "{:?}", plan.planned);
    assert_eq!(plan.skipped.len(), 2, "the two hostile markers are still refused, with reasons");

    // Nothing was created and nothing was watched.
    assert!(!std::path::Path::new(&plan.planned[0]).exists(), "a dry run made the folder");
    let watches = w.app.hist.lock().unwrap().as_ref().unwrap().watches().expect("watches");
    assert!(watches.is_empty(), "a dry run created {} watch(es)", watches.len());

    // And the real run then does exactly what the plan said.
    let done = w.rt.block_on(async { w.app.portal_reconcile().await });
    assert!(done.error.is_empty(), "{:?}", done.error);
    assert_eq!(done.ensured, plan.ensured, "the plan and the run disagree");
    assert!(std::path::Path::new(&plan.planned[0]).is_dir(), "the run did not make what the plan named");
}
