//! Every part of an upload is sent with the Content-Type that `local_upload`
//! names (`media_type`) and `x-amz-acl: private`: the part URLs are presigned
//! for both, and the storage refuses anything else with 403. The first Rust
//! builds sent every part as `application/octet-stream`, so every .mp4 and .mov
//! upload failed.
//!
//! The stub is Frame.io and its storage in one: `local_upload` answers
//! `video/mp4` and two part URLs on the stub, a PUT with any other type or acl
//! gets the storage's 403, and `status` confirms the file.
//!
//! The resume ledger lives under `HOME` (`config::dir()`), so the binary moves
//! `HOME` to a scratch folder before anything runs.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant, SystemTime};

use fiotransfer::api::Client;
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history::Store;
use fiotransfer::xfer::manager::{new_tuned, Tune};
use fiotransfer::xfer::uploadstate::{self, Ledger, PartState, UploadState};
use fiotransfer::xfer::{Manager, Status, UploadOptions};

const PART: usize = 4096;
const MEDIA_TYPE: &str = "video/mp4";

fn scratch_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = tempfile::tempdir().expect("scratch HOME").keep();
        std::env::set_var("HOME", &home);
        std::env::set_var("FIO_NO_KEYCHAIN", "1");
    });
}

/// What the stub saw: each PUT's path and Content-Type, and the other calls.
#[derive(Default)]
struct Seen {
    puts: Vec<(String, String)>,
    refused: usize,
    creates: usize,
    deletes: Vec<String>,
}

struct Stub {
    base: String,
    seen: Arc<Mutex<Seen>>,
}

impl Stub {
    fn start() -> Stub {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (b, s) = (base.clone(), seen.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (b, s) = (b.clone(), s.clone());
                std::thread::spawn(move || serve(c, &b, &s));
            }
        });
        Stub { base, seen }
    }
}

fn serve(c: TcpStream, base: &str, seen: &Mutex<Seen>) {
    let mut r = BufReader::new(c.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut it = line.split_whitespace();
    let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
    let (mut length, mut content_type, mut acl) = (0usize, String::new(), String::new());
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        let (name, value) = h.split_once(':').unwrap_or((&h, ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "content-type" => content_type = value.trim().to_string(),
            "x-amz-acl" => acl = value.trim().to_string(),
            _ => {}
        }
    }
    let mut body = vec![0u8; length];
    let _ = r.read_exact(&mut body);
    let reply = |status: &str, body: &str| {
        let mut c = &c;
        let _ = write!(
            c,
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nETag: \"e\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        );
    };
    match (method.as_str(), path.as_str()) {
        ("POST", p) if p.ends_with("/files/local_upload") => {
            seen.lock().unwrap().creates += 1;
            reply(
                "200 OK",
                &format!(
                    r#"{{"data":{{"id":"file-1","name":"clip.mp4","media_type":"{MEDIA_TYPE}","view_url":"","upload_urls":[{{"size":{PART},"url":"{base}/part/0"}},{{"size":{PART},"url":"{base}/part/1"}}]}}}}"#
                ),
            );
        }
        ("PUT", p) if p.starts_with("/part/") => {
            let mut s = seen.lock().unwrap();
            s.puts.push((p.to_string(), content_type.clone()));
            if content_type != MEDIA_TYPE || acl != "private" {
                s.refused += 1;
                drop(s);
                reply("403 Forbidden", "<Error><Code>SignatureDoesNotMatch</Code></Error>");
            } else {
                drop(s);
                reply("200 OK", "");
            }
        }
        ("GET", p) if p.ends_with("/status") => reply("200 OK", r#"{"data":{"upload_complete":true}}"#),
        ("DELETE", p) => {
            seen.lock().unwrap().deletes.push(p.to_string());
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

/// A two-part .mp4 that finished being written a minute ago.
fn clip(dir: &std::path::Path) -> String {
    let src = dir.join("clip.mp4");
    std::fs::write(&src, vec![7u8; 2 * PART]).unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options().write(true).open(&src).unwrap().set_modified(old).unwrap();
    src.to_string_lossy().into_owned()
}

async fn upload(m: &Arc<Manager>, src: &str) -> Status {
    let j = m
        .start_upload(UploadOptions {
            paths: vec![src.to_string()],
            account_id: "acct".into(),
            folder_id: "dest".into(),
            dest_label: "P".into(),
            ..Default::default()
        })
        .expect("upload starts");
    tokio::task::spawn_blocking(move || {
        let t = Instant::now();
        while !j.status().terminal() {
            assert!(t.elapsed() < Duration::from_secs(20), "the upload never finished");
            std::thread::sleep(Duration::from_millis(20));
        }
        j.status()
    })
    .await
    .unwrap()
}

/// A resume ledger for `src`, as `upload.rs` writes one: the stub's two parts,
/// the first already sent.
fn ledger(stub: &Stub, src: &str, media_type: &str) -> Ledger {
    let l = Ledger::for_source(src);
    let st = UploadState {
        source: src.to_string(),
        file_id: "old-file".into(),
        account_id: "acct".into(),
        folder_id: "dest".into(),
        name: "clip.mp4".into(),
        size: 2 * PART as i64,
        mtime_ms: uploadstate::mtime_ms(std::path::Path::new(src)),
        created_at: uploadstate::now_ms(),
        view_url: String::new(),
        media_type: media_type.to_string(),
        parts: (0..2)
            .map(|i| PartState { off: (i * PART) as i64, size: PART as i64, url: format!("{}/part/{}", stub.base, i) })
            .collect(),
    };
    l.save(&st).expect("ledger saved");
    l.mark_done(0);
    l
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_part_goes_up_as_the_media_type_frameio_named() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    let m = manager(&stub, tmp.path());
    let src = clip(tmp.path());

    assert_eq!(upload(&m, &src).await, Status::Done);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.refused, 0, "parts refused: {:?}", seen.puts);
    assert_eq!(seen.puts.len(), 2);
    assert!(seen.puts.iter().all(|(_, ct)| ct == MEDIA_TYPE), "{:?}", seen.puts);
    assert!(seen.deletes.is_empty(), "nothing deleted: {:?}", seen.deletes);
}

/// A resumed upload sends the parts still missing, as the type it started with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resumed_upload_sends_the_rest_as_the_type_it_started_with() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    let m = manager(&stub, tmp.path());
    let src = clip(tmp.path());
    let l = ledger(&stub, &src, MEDIA_TYPE);

    assert_eq!(upload(&m, &src).await, Status::Done);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.creates, 0, "resumed, not created again");
    assert_eq!(seen.puts, vec![("/part/1".to_string(), MEDIA_TYPE.to_string())]);
    assert!(seen.deletes.is_empty(), "{:?}", seen.deletes);
    assert!(!l.exists(), "a finished upload leaves no ledger");
}

/// A ledger an earlier build wrote names no type: its parts were signed for one
/// it never sent. It is dropped, its copy deleted, and the file sent again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ledger_without_a_media_type_starts_over() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    let m = manager(&stub, tmp.path());
    let src = clip(tmp.path());
    ledger(&stub, &src, "");

    assert_eq!(upload(&m, &src).await, Status::Done);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.deletes, ["/v4/accounts/acct/files/old-file"], "the old copy goes");
    assert_eq!(seen.creates, 1, "and the file is created again");
    assert_eq!(seen.puts.len(), 2);
    assert!(seen.puts.iter().all(|(_, ct)| ct == MEDIA_TYPE), "{:?}", seen.puts);
}
