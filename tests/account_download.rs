//! Downloading a folder of the signed-in account (Transfers → Frame.io…): the
//! folder is walked the way watch folders walk it (subfolders, version stacks
//! by their head version), each original is fetched through a signed URL the
//! API hands out, and a file still uploading to Frame.io fails without holding
//! up the rest. Retry lists the folder again and skips what is already on disk.
//!
//! Frame.io is a one-thread-per-connection stub that serves the listing, the
//! `media_links.original` lookups and the files themselves (with Range).
//!
//! `HOME` moves to a scratch folder before anything runs: a test must never
//! write into the developer's own app folder.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use fiotransfer::api::Client;
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history::Store;
use fiotransfer::xfer::manager::{is_account_download, new_tuned, Tune};
use fiotransfer::xfer::{AccountDownloadOptions, Job, Manager, Status};

fn scratch_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = tempfile::tempdir().expect("scratch HOME").keep();
        std::env::set_var("HOME", &home);
        std::env::set_var("FIO_NO_KEYCHAIN", "1");
    });
}

/// The bytes of a file, by its Frame.io ID.
fn content(id: &str) -> Vec<u8> {
    let n = match id {
        "f1" => 3000,
        "f2" => 1500,
        "h1" => 2048,
        "p1" => 700,
        _ => 0,
    };
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(id.len() as u8)).collect()
}

struct Stub {
    base: String,
    /// `pending.mov` has finished uploading to Frame.io.
    pending_ready: Arc<AtomicBool>,
    /// File GETs per Frame.io ID.
    fetched: Arc<Mutex<HashMap<String, usize>>>,
}

impl Stub {
    fn start() -> Stub {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let s = Stub {
            base: base.clone(),
            pending_ready: Arc::new(AtomicBool::new(false)),
            fetched: Arc::new(Mutex::new(HashMap::new())),
        };
        let (ready, fetched) = (s.pending_ready.clone(), s.fetched.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (base, ready, fetched) = (base.clone(), ready.clone(), fetched.clone());
                std::thread::spawn(move || serve(c, &base, &ready, &fetched));
            }
        });
        s
    }
}

fn file_json(id: &str, name: &str, status: &str) -> String {
    format!(
        r#"{{"id":"{}","name":"{}","type":"file","file_size":{},"status":"{}"}}"#,
        id,
        name,
        content(id).len(),
        status
    )
}

fn serve(c: TcpStream, base: &str, ready: &AtomicBool, fetched: &Mutex<HashMap<String, usize>>) {
    let mut r = BufReader::new(c.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut range: Option<(usize, usize)> = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("range:") {
            let v = v.trim().trim_start_matches("bytes=");
            if let Some((a, b)) = v.split_once('-') {
                range = Some((a.parse().unwrap_or(0), b.parse().unwrap_or(usize::MAX)));
            }
        }
    }
    let mut out = &c;
    let json = |status: &str, body: &str| {
        let mut out = &c;
        let _ = write!(
            out,
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        );
    };
    if method != "GET" {
        return json("405 Method Not Allowed", "{}");
    }
    match path.as_str() {
        "/v4/accounts/acct/folders/root1/children" => {
            let pending = if ready.load(Ordering::Relaxed) { "uploaded" } else { "created" };
            json(
                "200 OK",
                &format!(
                    r#"{{"data":[{{"id":"sub1","name":"Sub","type":"folder"}},{},{{"id":"s1","name":"stack","type":"version_stack","head_version":{}}},{}],"links":{{"next":null}}}}"#,
                    file_json("f1", "a.mov", "transcoded"),
                    file_json("h1", "v2.mov", "uploaded"),
                    file_json("p1", "pending.mov", pending),
                ),
            )
        }
        "/v4/accounts/acct/folders/sub1/children" => json(
            "200 OK",
            &format!(r#"{{"data":[{}],"links":{{"next":null}}}}"#, file_json("f2", "b.wav", "uploaded")),
        ),
        "/v4/accounts/acct/folders/root1" => {
            json("200 OK", r#"{"data":{"id":"root1","name":"Day 3","type":"folder","view_url":"https://next.frame.io/view/root1"}}"#)
        }
        p if p.starts_with("/v4/accounts/acct/files/") => {
            let id = p.trim_start_matches("/v4/accounts/acct/files/");
            json(
                "200 OK",
                &format!(r#"{{"data":{{"id":"{}","media_links":{{"original":{{"download_url":"{}/blob/{}"}}}}}}}}"#, id, base, id),
            )
        }
        p if p.starts_with("/blob/") => {
            let id = p.trim_start_matches("/blob/").to_string();
            let body = content(&id);
            *fetched.lock().unwrap().entry(id).or_default() += 1;
            match range {
                Some((a, b)) => {
                    let b = b.min(body.len() - 1);
                    let _ = write!(
                        out,
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        b + 1 - a,
                        a,
                        b,
                        body.len()
                    );
                    let _ = out.write_all(&body[a..=b]);
                }
                None => {
                    let _ = write!(out, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = out.write_all(&body);
                }
            }
        }
        _ => json("404 Not Found", r#"{"errors":[{"detail":"not here"}]}"#),
    }
}

fn manager(stub: &Stub, tmp: &std::path::Path) -> Arc<Manager> {
    let mut api = Client::new(AuthManager::new_static("test-token"));
    api.base_url = stub.base.clone();
    let (data, logs) = (tmp.join("data"), tmp.join("logs"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    let store = Arc::new(Store::open(&data, &logs).expect("history store"));
    let settings = || {
        let mut s = Settings::default();
        s.streams = 4;
        s.no_folder_logs = true;
        s
    };
    new_tuned(Arc::new(api), store, Box::new(settings), Tune::default())
}

async fn finished(j: &Arc<Job>) {
    let t = Instant::now();
    while !j.status().terminal() {
        assert!(t.elapsed() < Duration::from_secs(20), "the download never finished: {:?}", j.status());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_downloads_with_its_subfolders_and_retry_fetches_only_what_was_missing() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    let m = manager(&stub, tmp.path());
    let dest = tmp.path().join("Downloads");
    std::fs::create_dir_all(&dest).unwrap();

    let j = m
        .start_account_download(AccountDownloadOptions {
            account_id: "acct".into(),
            folder_id: "root1".into(),
            name: "Day 3".into(),
            label: "Spring Campaign / Day 3".into(),
            dest_dir: dest.to_string_lossy().into_owned(),
            subfolder: true,
            ..Default::default()
        })
        .expect("the download starts");
    finished(&j).await;

    let root = dest.join("Day 3");
    assert_eq!(std::fs::read(root.join("a.mov")).unwrap(), content("f1"));
    assert_eq!(std::fs::read(root.join("Sub").join("b.wav")).unwrap(), content("f2"));
    assert_eq!(std::fs::read(root.join("v2.mov")).unwrap(), content("h1"), "a version stack is its head version");
    assert!(!root.join("pending.mov").exists(), "a file still uploading is not fetched");

    let v = j.view();
    assert_eq!(v.status, Status::Partial, "{:?}", v.error);
    assert_eq!((v.files_total, v.files_done, v.files_failed), (4, 3, 1));
    assert_eq!(v.title, "Day 3");
    assert_eq!(v.source, "Spring Campaign / Day 3");
    let pending = j.files().into_iter().find(|f| f.rel_path == "pending.mov").expect("listed");
    assert_eq!(pending.error(), "still uploading to Frame.io");

    let t = m.hist.get(&j.id).unwrap().expect("in history");
    assert!(is_account_download(&t), "history knows it came from the account: {:?}", t.options);
    assert_eq!((t.account_id.as_str(), t.folder_id.as_str()), ("acct", "root1"));

    // Frame.io finishes the upload; Retry lists again and fetches only that.
    stub.pending_ready.store(true, Ordering::Relaxed);
    let before = stub.fetched.lock().unwrap().clone();
    let again = m.retry(&j.id, "").await.expect("retry starts").expect("a new job");
    finished(&again).await;
    let v = again.view();
    assert_eq!(v.status, Status::Done, "{:?}", v.error);
    assert_eq!(std::fs::read(root.join("pending.mov")).unwrap(), content("p1"));
    let after = stub.fetched.lock().unwrap().clone();
    for id in ["f1", "f2", "h1"] {
        assert_eq!(after.get(id), before.get(id), "{} was already on disk", id);
    }
    let skipped = again.files().iter().filter(|f| f.status() == Status::Skipped).count();
    assert_eq!(skipped, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_new_folder_the_contents_land_in_the_download_folder() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    stub.pending_ready.store(true, Ordering::Relaxed);
    let m = manager(&stub, tmp.path());
    let dest = tmp.path().join("Downloads");
    std::fs::create_dir_all(&dest).unwrap();
    let j = m
        .start_account_download(AccountDownloadOptions {
            account_id: "acct".into(),
            folder_id: "root1".into(),
            name: "Day 3".into(),
            label: "Spring Campaign / Day 3".into(),
            dest_dir: dest.to_string_lossy().into_owned(),
            subfolder: false,
            ..Default::default()
        })
        .unwrap();
    finished(&j).await;
    assert_eq!(j.view().status, Status::Done, "{:?}", j.view().error);
    assert_eq!(std::fs::read(dest.join("a.mov")).unwrap(), content("f1"));
    assert_eq!(std::fs::read(dest.join("Sub").join("b.wav")).unwrap(), content("f2"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_folder_or_destination_says_so() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    let m = manager(&stub, tmp.path());
    let dest = tmp.path().to_string_lossy().into_owned();

    let err = m
        .start_account_download(AccountDownloadOptions {
            account_id: "acct".into(),
            folder_id: "root1".into(),
            dest_dir: tmp.path().join("nope").to_string_lossy().into_owned(),
            ..Default::default()
        })
        .err()
        .expect("refused");
    assert!(err.starts_with("destination folder not found"), "{}", err);

    let j = m
        .start_account_download(AccountDownloadOptions {
            account_id: "acct".into(),
            folder_id: "gone".into(),
            name: "Gone".into(),
            dest_dir: dest,
            ..Default::default()
        })
        .unwrap();
    finished(&j).await;
    assert_eq!(j.view().status, Status::Failed);
    assert_eq!(j.view().error, "Frame.io folder not found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_picked_items_download_and_retry_keeps_the_pick() {
    scratch_home();
    let tmp = tempfile::tempdir().unwrap();
    let stub = Stub::start();
    let m = manager(&stub, tmp.path());
    let dest = tmp.path().join("Downloads");
    std::fs::create_dir_all(&dest).unwrap();

    // A folder (with what's in it), a version stack, and the file still uploading.
    let j = m
        .start_account_download(AccountDownloadOptions {
            account_id: "acct".into(),
            folder_id: "root1".into(),
            name: "Day 3".into(),
            title: "Sub + 2 more".into(),
            items: vec!["sub1".into(), "s1".into(), "p1".into()],
            label: "Spring Campaign / Day 3".into(),
            dest_dir: dest.to_string_lossy().into_owned(),
            subfolder: true,
        })
        .unwrap();
    finished(&j).await;
    let root = dest.join("Day 3");
    assert_eq!(std::fs::read(root.join("Sub").join("b.wav")).unwrap(), content("f2"), "paths stay relative to the folder");
    assert_eq!(std::fs::read(root.join("v2.mov")).unwrap(), content("h1"));
    assert!(!root.join("a.mov").exists(), "a.mov wasn't picked");
    let v = j.view();
    assert_eq!(v.title, "Sub + 2 more");
    assert_eq!((v.files_total, v.files_done, v.files_failed), (3, 2, 1));

    stub.pending_ready.store(true, Ordering::Relaxed);
    let again = m.retry(&j.id, "").await.unwrap().unwrap();
    finished(&again).await;
    assert_eq!(again.view().status, Status::Done, "{:?}", again.view().error);
    assert_eq!(again.view().title, "Sub + 2 more");
    assert_eq!(again.files().len(), 3, "the retry downloads the same pick");
    assert_eq!(std::fs::read(root.join("pending.mov")).unwrap(), content("p1"));
    assert!(!root.join("a.mov").exists());
    assert!(stub.fetched.lock().unwrap().get("f1").is_none(), "a.mov was never fetched");
}
