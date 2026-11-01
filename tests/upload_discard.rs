//! An upload that stops before every byte is on Frame.io deletes its copy there:
//! a user cancel and a quit (`cancel_all`) both, the same as a failure. Only a
//! crash leaves the resume ledger, and nothing here crashes.
//!
//! Frame.io is a one-thread-per-connection stub: `local_upload` hands out one
//! part URL on the stub itself, the PUT to it is held open until the test ends,
//! and every DELETE is recorded.
//!
//! The resume ledger lives under `HOME` (`config::dir()`), so the binary moves
//! `HOME` to a scratch folder before anything runs: a test must never write into
//! the developer's own app folder.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant, SystemTime};

use fiotransfer::api::Client;
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history::Store;
use fiotransfer::xfer::manager::{new_tuned, Tune};
use fiotransfer::xfer::uploadstate::Ledger;
use fiotransfer::xfer::{Manager, UploadOptions};

const PART: usize = 4096;

fn scratch_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = tempfile::tempdir().expect("scratch HOME").keep();
        std::env::set_var("HOME", &home);
        std::env::set_var("FIO_NO_KEYCHAIN", "1");
    });
}

struct Stub {
    base: String,
    put_started: Arc<AtomicBool>,
    deletes: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Stub {
    fn start() -> Stub {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let s = Stub {
            base: base.clone(),
            put_started: Arc::new(AtomicBool::new(false)),
            deletes: Arc::new(Mutex::new(Vec::new())),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let (put_started, deletes, stop) = (s.put_started.clone(), s.deletes.clone(), s.stop.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (base, put_started, deletes, stop) = (base.clone(), put_started.clone(), deletes.clone(), stop.clone());
                std::thread::spawn(move || serve(c, &base, &put_started, &deletes, &stop));
            }
        });
        s
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(c: TcpStream, base: &str, put_started: &AtomicBool, deletes: &Mutex<Vec<String>>, stop: &AtomicBool) {
    let mut r = BufReader::new(c.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let reply = |status: &str, body: &str| {
        let mut c = &c;
        let _ = write!(
            c,
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        );
    };
    match (method.as_str(), path.as_str()) {
        ("POST", p) if p.ends_with("/files/local_upload") => {
            let mut body = vec![0u8; length];
            let _ = r.read_exact(&mut body);
            reply(
                "200 OK",
                &format!(
                    r#"{{"data":{{"id":"file-1","name":"a.bin","media_type":"application/octet-stream","view_url":"","upload_urls":[{{"size":{},"url":"{}/part/0"}}]}}}}"#,
                    PART, base
                ),
            );
        }
        ("PUT", "/part/0") => {
            // Held open: the upload is mid-part when the test stops it.
            put_started.store(true, Ordering::Relaxed);
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        ("DELETE", p) => {
            deletes.lock().unwrap().push(p.to_string());
            reply("204 No Content", "");
        }
        _ => reply("404 Not Found", r#"{"errors":[{"detail":"not here"}]}"#),
    }
}

fn manager(stub: &Stub, tmp: &std::path::Path) -> Arc<Manager> {
    let mut api = Client::new(AuthManager::new_static("test-token"));
    api.base_url = stub.base.clone();
    let (data, logs) = (tmp.join("data"), tmp.join("logs"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    let store = Arc::new(Store::open(&data, &logs).expect("history store"));
    let tune = Tune {
        upload_settle: Duration::from_millis(10),
        verify_delays: vec![Duration::from_millis(10)],
        change_check: Duration::from_millis(50),
        stall_limit: Duration::from_secs(5),
    };
    new_tuned(Arc::new(api), store, Box::new(Settings::default), tune)
}

fn wait_for(what: &str, within: Duration, ok: impl Fn() -> bool) {
    let t = Instant::now();
    while !ok() {
        assert!(t.elapsed() < within, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts a one-file upload, waits until its part is on the wire, stops it with
/// `stop`, and answers what the stub was asked to delete and whether the
/// resume ledger is still there.
async fn stopped_upload(stop: impl FnOnce(&Arc<Manager>, &str)) -> (Vec<String>, bool) {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    let m = manager(&stub, tmp.path());
    let src = tmp.path().join("a.bin");
    std::fs::write(&src, vec![7u8; PART]).unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options().write(true).open(&src).unwrap().set_modified(old).unwrap();

    let j = m
        .start_upload(UploadOptions {
            paths: vec![src.to_string_lossy().into_owned()],
            account_id: "acct".into(),
            folder_id: "dest".into(),
            dest_label: "P".into(),
            ..Default::default()
        })
        .expect("upload starts");
    let started = stub.put_started.clone();
    tokio::task::spawn_blocking(move || wait_for("the part PUT", Duration::from_secs(10), || started.load(Ordering::Relaxed)))
        .await
        .unwrap();
    stop(&m, &j.id);
    let deletes = stub.deletes.clone();
    tokio::task::spawn_blocking(move || {
        wait_for("the delete", Duration::from_secs(10), || !deletes.lock().unwrap().is_empty())
    })
    .await
    .unwrap();
    let ledger = Ledger::for_source(&src.to_string_lossy()).exists();
    let got = stub.deletes.lock().unwrap().clone();
    (got, ledger)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_canceled_upload_is_deleted_from_frameio() {
    let (deletes, ledger) = stopped_upload(|m, id| m.cancel(id)).await;
    assert_eq!(deletes, ["/v4/accounts/acct/files/file-1"]);
    assert!(!ledger, "a canceled upload leaves no resume ledger");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quitting_mid_upload_deletes_it_from_frameio() {
    let (deletes, ledger) = stopped_upload(|m, _| m.cancel_all()).await;
    assert_eq!(deletes, ["/v4/accounts/acct/files/file-1"]);
    assert!(!ledger, "a quit leaves no resume ledger");
}
