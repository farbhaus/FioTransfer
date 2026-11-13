//! Big parts take minutes each, and nothing may count that against them.
//!
//! On 2026-11-13 a watch folder couldn't upload two ProRes renders (38 and
//! 49 GB, so 35 parts of 1.1–1.4 GB, ~150 s each at 16 streams):
//!  * the upload client had reqwest's `read_timeout`, whose clock runs from the
//!    start of the request until the reply, so every part was cut off at 120 s
//!    ("error sending request"), ten times, then the file failed;
//!  * the idle watchdog was one per file, started before its parts had streams,
//!    so the second file of the batch, waiting behind the first one's parts,
//!    was cancelled after 90 s ("context canceled").
//!
//! The stub is Frame.io and its storage in one: `local_upload` answers a single
//! part on the stub, and the first PUT can be told to get no reply.
//!
//! The resume ledger lives under `HOME` (`config::dir()`), so the binary moves
//! `HOME` to a scratch folder before anything runs.

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
use fiotransfer::xfer::{Manager, Status, UploadOptions};
use tokio_util::sync::CancellationToken;

const SIZE: usize = 64 * 1024;
const IDLE: Duration = Duration::from_millis(300);

fn scratch_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = tempfile::tempdir().expect("scratch HOME").keep();
        std::env::set_var("HOME", &home);
        std::env::set_var("FIO_NO_KEYCHAIN", "1");
    });
}

#[derive(Default)]
struct Seen {
    creates: usize,
    puts: usize,
    deletes: Vec<String>,
}

struct Stub {
    base: String,
    seen: Arc<Mutex<Seen>>,
}

impl Stub {
    /// `silent_first`: the first PUT gets its body read and no reply for 10× the
    /// idle limit.
    fn start(silent_first: bool) -> Stub {
        Stub::start_with(silent_first, Duration::ZERO)
    }

    /// `put_delay`: every PUT takes this long to be answered.
    fn start_with(silent_first: bool, put_delay: Duration) -> Stub {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let silent = Arc::new(AtomicBool::new(silent_first));
        let (b, s) = (base.clone(), seen.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (b, s, q) = (b.clone(), s.clone(), silent.clone());
                std::thread::spawn(move || serve(c, &b, &s, &q, put_delay));
            }
        });
        Stub { base, seen }
    }
}

fn serve(c: TcpStream, base: &str, seen: &Mutex<Seen>, silent: &AtomicBool, put_delay: Duration) {
    let mut r = BufReader::new(c.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut it = line.split_whitespace();
    let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        let (name, value) = h.split_once(':').unwrap_or((&h, ""));
        if name.trim().eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap_or(0);
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
            let n = {
                let mut g = seen.lock().unwrap();
                g.creates += 1;
                g.creates
            };
            reply(
                "200 OK",
                &format!(
                    r#"{{"data":{{"id":"file-{n}","name":"clip.bin","media_type":"application/octet-stream","view_url":"","upload_urls":[{{"size":{SIZE},"url":"{base}/part/0"}}]}}}}"#
                ),
            );
        }
        ("PUT", p) if p.starts_with("/part/") => {
            seen.lock().unwrap().puts += 1;
            if silent.swap(false, Ordering::SeqCst) {
                std::thread::sleep(IDLE * 10);
            }
            std::thread::sleep(put_delay);
            reply("200 OK", "");
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
        idle_limit: IDLE,
    };
    let settings = || Settings { streams: 1, ..Settings::default() };
    new_tuned(Arc::new(api), store, Box::new(settings), tune)
}

/// A one-part file that finished being written a minute ago.
fn clip(dir: &std::path::Path) -> String {
    let src = dir.join("clip.bin");
    std::fs::write(&src, vec![7u8; SIZE]).unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options().write(true).open(&src).unwrap().set_modified(old).unwrap();
    src.to_string_lossy().into_owned()
}

fn start(m: &Arc<Manager>, src: &str) -> Arc<fiotransfer::xfer::Job> {
    m.start_upload(UploadOptions {
        paths: vec![src.to_string()],
        account_id: "acct".into(),
        folder_id: "dest".into(),
        dest_label: "P".into(),
        ..Default::default()
    })
    .expect("upload starts")
}

async fn finish(j: Arc<fiotransfer::xfer::Job>) -> Status {
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

/// The only stream is busy (another file's part) for many idle limits: the
/// file waits for it and then goes up, instead of being cancelled as idle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_waiting_for_a_stream_is_not_idle() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start(false);
    let m = manager(&stub, tmp.path());
    let src = clip(tmp.path());

    let limiter = m.limiter.clone();
    let cx = CancellationToken::new();
    let busy = limiter.acquire(&cx).await.expect("the only stream");
    let j = start(&m, &src);
    tokio::time::sleep(IDLE * 5).await;
    {
        let seen = stub.seen.lock().unwrap();
        assert_eq!(seen.creates, 1, "the file was created and its part is waiting");
        assert_eq!(seen.puts, 0, "nothing sent without a stream");
    }
    drop(busy);

    assert_eq!(finish(j).await, Status::Done);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.puts, 1);
    assert!(seen.deletes.is_empty(), "nothing deleted: {:?}", seen.deletes);
}

/// Storage takes the part and never answers: that attempt is given up after the
/// idle limit and the part sent again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_part_storage_never_answers_is_sent_again() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start(true);
    let m = manager(&stub, tmp.path());
    let src = clip(tmp.path());

    let t = Instant::now();
    assert_eq!(finish(start(&m, &src)).await, Status::Done);
    assert!(t.elapsed() < IDLE * 10, "the silent attempt was given up, not waited out: {:?}", t.elapsed());
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.puts, 2, "sent again after the silence");
    assert!(seen.deletes.is_empty(), "nothing deleted: {:?}", seen.deletes);
}

/// The same file uploaded twice at once, to the same folder (a second drop, a
/// watch and a manual upload): each upload makes its own file on Frame.io. The
/// resume ledger is keyed by the source path, and the port let the second
/// upload resume into the first one's file, so a failure of either deleted the
/// copy the other was sending (found 2026-11-13).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_file_uploaded_twice_at_once_makes_two_copies() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start_with(false, Duration::from_millis(150));
    let m = manager(&stub, tmp.path());
    let src = clip(tmp.path());

    // The second starts once the first has made its file (and saved its
    // ledger), while its part is still on the way.
    let a = start(&m, &src);
    let t = Instant::now();
    while stub.seen.lock().unwrap().creates == 0 {
        assert!(t.elapsed() < Duration::from_secs(5), "the first upload never made its file");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let b = start(&m, &src);
    assert_eq!(finish(a).await, Status::Done);
    assert_eq!(finish(b).await, Status::Done);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.creates, 2, "two files on Frame.io, one per upload");
    assert_eq!(seen.puts, 2, "each sent its own part");
    assert!(seen.deletes.is_empty(), "nothing deleted: {:?}", seen.deletes);
}

/// Uploads must not use a client with reqwest's read timeout: its clock covers
/// the whole request body, so it caps how long one part may take to send.
/// (`Debug` lists `read_timeout` only when one is set; the download client keeps
/// one, which shows the check can see it.)
#[test]
fn the_upload_client_has_no_read_timeout() {
    assert!(format!("{:?}", fiotransfer::netx::data_client()).contains("read_timeout"));
    assert!(!format!("{:?}", fiotransfer::netx::upload_client()).contains("read_timeout"));
}
