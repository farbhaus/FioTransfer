//! The Space screen inside a project, driven for real: `App::open_space_folder`
//! and `App::delete_space_items` with a real `Client` pointed at a one-thread
//! stub Frame.io, as `prop_delete.rs` does for whole projects.
//!
//! The stub holds one project:
//!
//! ```text
//! rootA ─ Jobs (fjobs) ─ a.mov 100
//!       │              └ Sub (fsub) ─ b.mov 30
//!       ├ Old (fold) ─ c.mov 50
//!       ├ Cut (vs1, version stack: v1 9, v2 4; head 9)
//!       └ Brief.pdf (fbrief) 7
//! ```
//!
//! What a delete did is read from the stub's request log, not from the app.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fiotransfer::api::Client;
use fiotransfer::app::{App, SpaceTarget};
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history::Store;

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
}

/// Status codes for DELETEs, by item id; anything not listed is deleted (204).
type Refusals = Arc<Mutex<HashMap<String, u16>>>;

struct Stub {
    base: String,
    reqs: Arc<Mutex<Vec<Req>>>,
    refuse: Refusals,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    fn start() -> Stub {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap();
        let reqs = Arc::new(Mutex::new(Vec::new()));
        let refuse: Refusals = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, f, s) = (reqs.clone(), refuse.clone(), stop.clone());
        let join = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((c, _)) => {
                        let _ = c.set_nonblocking(false);
                        if let Some(req) = answer(c, &f) {
                            r.lock().unwrap().push(req);
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Stub { base: format!("http://{}", addr), reqs, refuse, stop, join: Some(join) }
    }

    fn log(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }

    fn deletes(&self) -> Vec<String> {
        self.log().into_iter().filter(|r| r.method == "DELETE").map(|r| r.path).collect()
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

fn page(items: &str) -> String {
    format!(r#"{{"data":[{items}],"links":{{"next":null}}}}"#)
}

fn answer(mut c: TcpStream, refuse: &Refusals) -> Option<Req> {
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
    let bare = path.split('?').next().unwrap_or("").to_string();
    let file = |id: &str, name: &str, size: i64| format!(r#"{{"id":"{id}","name":"{name}","type":"file","file_size":{size}}}"#);
    let folder = |id: &str, name: &str| format!(r#"{{"id":"{id}","name":"{name}","type":"folder"}}"#);
    let (status, body) = if method == "DELETE" {
        let id = bare.rsplit('/').next().unwrap_or("").to_string();
        match refuse.lock().unwrap().get(&id) {
            Some(code) => (*code, r#"{"errors":[{"title":"refused"}]}"#.to_string()),
            None => (204, String::new()),
        }
    } else {
        let body = match bare.as_str() {
            "/v4/accounts" => Some(page(r#"{"id":"acct","display_name":"One"}"#)),
            "/v4/accounts/acct/workspaces" => Some(page(r#"{"id":"ws","name":"Shows"}"#)),
            "/v4/accounts/acct/workspaces/ws/projects" => {
                Some(page(r#"{"id":"projA","name":"Client A","root_folder_id":"rootA","storage":1000}"#))
            }
            "/v4/accounts/acct/folders/rootA/children" => Some(page(&[
                folder("fjobs", "Jobs"),
                folder("fold", "Old"),
                r#"{"id":"vs1","name":"Cut","type":"version_stack","head_version":{"id":"v1","file_size":9}}"#.to_string(),
                file("fbrief", "Brief.pdf", 7),
            ]
            .join(","))),
            "/v4/accounts/acct/folders/fjobs/children" => Some(page(&[file("fa", "a.mov", 100), folder("fsub", "Sub")].join(","))),
            "/v4/accounts/acct/folders/fsub/children" => Some(page(&file("fb", "b.mov", 30))),
            "/v4/accounts/acct/folders/fold/children" => Some(page(&file("fc", "c.mov", 50))),
            "/v4/accounts/acct/version_stacks/vs1/children" => {
                Some(page(&[file("v1", "cut_v1.mov", 9), file("v2", "cut_v2.mov", 4)].join(",")))
            }
            _ => None,
        };
        match body {
            Some(b) => (200, b),
            None => (404, r#"{"errors":[{"title":"no route"}]}"#.to_string()),
        }
    };
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        _ => "Not Found",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-ratelimit-limit: 100000\r\nx-ratelimit-window: 1000\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = c.write_all(resp.as_bytes());
    let _ = c.write_all(body.as_bytes());
    let _ = c.flush();
    Some(Req { method, path })
}

struct World {
    _tmp: tempfile::TempDir,
    media: std::path::PathBuf,
    stub: Stub,
    app: Arc<App>,
    rt: tokio::runtime::Runtime,
}

impl World {
    fn new() -> World {
        let tmp = tempfile::TempDir::new().unwrap();
        let data = tmp.path().join("data");
        let logs = tmp.path().join("logs");
        let media = tmp.path().join("media");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&logs).unwrap();
        // Same names as the stub's folders: a delete that reached the disk would
        // find something to delete.
        std::fs::create_dir_all(media.join("Jobs/Sub")).unwrap();
        std::fs::create_dir_all(media.join("Old")).unwrap();
        std::fs::write(media.join("Jobs/a.mov"), b"local a").unwrap();
        std::fs::write(media.join("Jobs/Sub/b.mov"), b"local b").unwrap();
        std::fs::write(media.join("Old/c.mov"), b"local c").unwrap();
        std::fs::write(media.join("Brief.pdf"), b"local brief").unwrap();

        let stub = Stub::start();
        let mut c = Client::new(AuthManager::new_static("test-token"));
        c.base_url = stub.base.clone();
        let app = App::with_api(Settings::default(), Arc::new(c));
        *app.hist.lock().unwrap() = Some(Arc::new(Store::open(&data, &logs).expect("history store")));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        World { _tmp: tmp, media, stub, app, rt }
    }

    /// The project row, as a refresh reads it, and its root walked.
    fn opened() -> World {
        let w = World::new();
        w.rt.block_on(w.app.refresh_space());
        w.open("rootA", None);
        w
    }

    fn open(&self, folder: &str, parent: Option<&str>) {
        self.rt.block_on(self.app.open_space_folder("acct", folder, parent));
    }

    fn delete(&self, folder: &str, items: &[(&str, &str, &str)]) -> Result<fiotransfer::app::SpaceDeleteReport, String> {
        let targets: Vec<SpaceTarget> = items
            .iter()
            .map(|(id, kind, name)| SpaceTarget { id: id.to_string(), kind: kind.to_string(), name: name.to_string() })
            .collect();
        self.rt.block_on(self.app.delete_space_items("acct", "projA", folder, &targets))
    }

    fn rows(&self, folder: &str) -> Vec<(String, i64, i64)> {
        self.app.space_folder("acct", folder).items.into_iter().map(|i| (i.id, i.bytes, i.files)).collect()
    }

    fn project_bytes(&self) -> i64 {
        self.app.space().projects.iter().find(|p| p.project_id == "projA").map(|p| p.bytes).unwrap_or(-1)
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(self.app.hist().unwrap().log_path()).unwrap_or_default()
    }
}

fn snap(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if p.is_dir() {
                out.insert(rel, Vec::new());
                walk(root, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn opening_a_project_adds_up_each_folder_once() {
    let w = World::opened();
    assert_eq!(
        w.rows("rootA"),
        vec![
            ("fjobs".to_string(), 130, 2),
            ("fold".to_string(), 50, 1),
            ("vs1".to_string(), 9, 1),
            ("fbrief".to_string(), 7, 1),
        ],
        "biggest first once everything is counted"
    );
    let listed: Vec<String> = w.stub.log().into_iter().filter(|r| r.path.contains("/children")).map(|r| r.path).collect();
    assert_eq!(listed.len(), 4, "root, Jobs, Sub and Old, once each: {listed:?}");
    let folder = w.app.space_folder("acct", "rootA");
    assert!(folder.listed && !folder.counting, "the walk is over");

    // A folder the walk has been through opens without asking Frame.io.
    w.open("fjobs", Some("rootA"));
    assert_eq!(w.rows("fjobs"), vec![("fa".to_string(), 100, 1), ("fsub".to_string(), 30, 1)]);
    assert_eq!(w.stub.log().iter().filter(|r| r.path.contains("/children")).count(), 4);
}

#[test]
fn deleting_a_folder_sends_one_delete_and_takes_its_bytes_off() {
    let w = World::opened();
    let before = snap(&w.media);
    assert_eq!(w.project_bytes(), 1000);

    let report = w.delete("rootA", &[("fjobs", "folder", "Jobs")]).expect("sent");
    assert_eq!(report.deleted, vec!["Jobs".to_string()]);
    assert!(report.failed.is_empty());
    assert_eq!(w.stub.deletes(), vec!["/v4/accounts/acct/folders/fjobs".to_string()]);
    assert!(!w.rows("rootA").iter().any(|(id, ..)| id == "fjobs"));
    assert_eq!(w.project_bytes(), 870, "Frame.io's 1000 minus the folder's 130");
    assert!(!w.app.space_folder("acct", "fsub").listed, "a folder inside it is forgotten too");

    let log = w.log_text();
    assert!(log.contains("deleted folder Jobs (fjobs) in project projA"), "{log}");
    assert!(log.contains("nothing local was touched"), "{log}");
    assert_eq!(snap(&w.media), before, "nothing local changed");
}

#[test]
fn emptying_a_project_deletes_each_item_by_its_own_route() {
    let w = World::opened();
    let before = snap(&w.media);
    let report = w
        .delete(
            "rootA",
            &[("fjobs", "folder", "Jobs"), ("fold", "folder", "Old"), ("vs1", "version_stack", "Cut"), ("fbrief", "file", "Brief.pdf")],
        )
        .expect("sent");
    assert_eq!(report.deleted.len(), 4);
    assert_eq!(
        w.stub.deletes(),
        vec![
            "/v4/accounts/acct/folders/fjobs",
            "/v4/accounts/acct/folders/fold",
            // No delete exists for a version stack: each version goes.
            "/v4/accounts/acct/files/v1",
            "/v4/accounts/acct/files/v2",
            "/v4/accounts/acct/files/fbrief",
        ]
    );
    assert!(w.rows("rootA").is_empty());
    assert!(w.app.space_folder("acct", "rootA").listed, "the project's root stays");
    assert_eq!(w.project_bytes(), 1000 - 196);
    assert_eq!(snap(&w.media), before, "nothing local changed");
}

#[test]
fn a_refused_delete_keeps_its_row_and_says_why() {
    let w = World::opened();
    w.stub.refuse.lock().unwrap().insert("fold".into(), 403);
    let report = w.delete("rootA", &[("fold", "folder", "Old"), ("fbrief", "file", "Brief.pdf")]).expect("sent");
    assert_eq!(report.deleted, vec!["Brief.pdf".to_string()]);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, "Old");
    assert!(w.rows("rootA").iter().any(|(id, ..)| id == "fold"), "the refused folder stays listed");
    assert_eq!(w.project_bytes(), 993, "only the file's 7 bytes came off");
    assert!(w.log_text().contains("could not delete folder Old (fold)"));
}

#[test]
fn a_404_is_already_gone() {
    let w = World::opened();
    w.stub.refuse.lock().unwrap().insert("fold".into(), 404);
    let report = w.delete("rootA", &[("fold", "folder", "Old")]).expect("sent");
    assert_eq!(report.deleted, vec!["Old".to_string()]);
    assert!(!w.rows("rootA").iter().any(|(id, ..)| id == "fold"));
}

#[test]
fn ids_that_are_not_one_path_segment_are_refused_before_any_request() {
    let w = World::opened();
    let sent = w.stub.log().len();
    for bad in ["", ".", "..", "../projects/projA", "a/b", "a\\b", "%2e%2e", "x\ny"] {
        assert!(w.delete("rootA", &[(bad, "folder", "x")]).is_err(), "item id {bad:?}");
        assert!(w.delete(bad, &[("fold", "folder", "Old")]).is_err(), "folder id {bad:?}");
        let r = w.rt.block_on(w.app.delete_space_items(
            bad,
            "projA",
            "rootA",
            &[SpaceTarget { id: "fold".into(), kind: "folder".into(), name: "Old".into() }],
        ));
        assert!(r.is_err(), "account id {bad:?}");
    }
    assert_eq!(w.stub.log().len(), sent, "nothing was sent: {:?}", &w.stub.log()[sent..]);
    assert!(w.rows("rootA").iter().any(|(id, ..)| id == "fold"));
}

#[test]
fn an_unknown_kind_is_refused() {
    let w = World::opened();
    let sent = w.stub.log().len();
    assert!(w.delete("rootA", &[("fold", "project", "Old")]).is_err());
    assert!(w.delete("rootA", &[("fbrief", "file", "Brief.pdf"), ("fold", "", "Old")]).is_err(), "one bad kind refuses all");
    assert_eq!(w.stub.log().len(), sent);
}

#[test]
fn refresh_inside_a_project_walks_only_that_folder_again() {
    let w = World::opened();
    let before = w.stub.log().len();
    w.app.forget_space_folder("acct", "fjobs");
    w.open("rootA", None);
    let again: Vec<String> = w.stub.log()[before..].iter().map(|r| r.path.clone()).collect();
    assert_eq!(again.len(), 2, "Jobs and Sub, not Old: {again:?}");
    assert_eq!(w.rows("rootA")[0], ("fjobs".to_string(), 130, 2));
}
