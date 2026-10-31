
//! Transfers: the job model, the upload and download engines, watch folders.
//!
//! A direct port of the Go `internal/xfer` package. The shapes here are the
//! contract between the transfer engine and everything that shows it.

pub mod bufpool;
pub mod complete;
pub mod download;
pub mod folderlog;
pub mod manager;
pub mod netvol;
pub mod sharelink;
pub mod upload;
pub mod uploadstate;
pub mod watch;

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;

use crate::api::Client;
use crate::config::Settings;
use crate::history;
use crate::limiter::{Limiter, Meter};

// ---------- Status ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Queued,
    Resolving,
    Running,
    Done,
    Partial,
    Failed,
    Canceled,
    Skipped,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Resolving => "resolving",
            Status::Running => "running",
            Status::Done => "done",
            Status::Partial => "partial",
            Status::Failed => "failed",
            Status::Canceled => "canceled",
            Status::Skipped => "skipped",
        }
    }

    pub fn terminal(&self) -> bool {
        matches!(self, Status::Done | Status::Partial | Status::Failed | Status::Canceled | Status::Skipped)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------- File ----------

/// One file in a transfer.
pub struct File {
    pub job: Mutex<Weak<Job>>,
    pub idx: usize,

    pub name: String,
    pub rel_path: String,
    pub local_path: Mutex<String>,
    /// The file's size in bytes. It can change while the upload waits for the
    /// file to settle, so it is atomic rather than fixed at construction.
    pub size: AtomicI64,

    // uploads
    pub remote_parent: Mutex<String>,
    pub remote_id: Mutex<String>,
    pub view_url: Mutex<String>,
    /// An earlier version on Frame.io, deleted once Frame.io confirms this upload.
    pub replaces: Mutex<String>,

    // downloads
    pub asset_id: String,
    pub url: Mutex<String>,

    pub done: AtomicI64,
    pub retries: AtomicI64,
    /// e.g. part count and speed, for the log.
    pub detail: Mutex<String>,

    pub inner: Mutex<FileState>,
}

#[derive(Default)]
pub struct FileState {
    pub status: Status,
    pub note: String,
    pub err: String,
    pub finished: Option<SystemTime>,
    /// Frame.io's confirmation is pending; its check logs the outcome.
    pub check_later: bool,
    /// It changed while uploading; a watch sends it again once it settles.
    pub still_writing: bool,
}

impl File {
    pub fn new(idx: usize, name: &str, rel_path: &str, local_path: &str, size: i64) -> Self {
        File {
            job: Mutex::new(Weak::new()),
            idx,
            name: name.to_string(),
            rel_path: rel_path.to_string(),
            local_path: Mutex::new(local_path.to_string()),
            size: AtomicI64::new(size),
            remote_parent: Mutex::new(String::new()),
            remote_id: Mutex::new(String::new()),
            view_url: Mutex::new(String::new()),
            replaces: Mutex::new(String::new()),
            asset_id: String::new(),
            url: Mutex::new(String::new()),
            done: AtomicI64::new(0),
            retries: AtomicI64::new(0),
            detail: Mutex::new(String::new()),
            inner: Mutex::new(FileState::default()),
        }
    }

    pub fn attach(&self, j: &Arc<Job>) {
        *self.job.lock().unwrap() = Arc::downgrade(j);
    }

    pub fn job(&self) -> Option<Arc<Job>> {
        self.job.lock().unwrap().upgrade()
    }

    /// Records bytes moved over the network.
    pub fn add(&self, n: i64) {
        self.done.fetch_add(n, Ordering::Relaxed);
        if let Some(j) = self.job() {
            j.done.fetch_add(n, Ordering::Relaxed);
            j.meter.add(n);
            if let Some(m) = j.mgr() {
                m.global.add(n);
            }
        }
    }

    /// Records bytes that were already present (resume / already downloaded).
    pub fn credit(&self, n: i64) {
        self.done.fetch_add(n, Ordering::Relaxed);
        if let Some(j) = self.job() {
            j.done.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Removes progress that will be redone (failed attempt).
    pub fn unwind(&self, n: i64) {
        self.done.fetch_add(-n, Ordering::Relaxed);
        if let Some(j) = self.job() {
            j.done.fetch_add(-n, Ordering::Relaxed);
        }
    }

    pub fn set_status(&self, s: Status, note: &str) {
        let mut g = self.inner.lock().unwrap();
        self.set_status_locked(&mut g, s);
        if !note.is_empty() {
            g.note = note.to_string();
        }
    }

    /// Changes the status and keeps the job's per-status counts in step, so
    /// views never have to walk the file list.
    pub fn set_status_locked(&self, g: &mut FileState, s: Status) {
        if g.status == s {
            return;
        }
        if let Some(j) = self.job() {
            if let Some(c) = j.counter(g.status) {
                c.fetch_sub(1, Ordering::Relaxed);
            }
            if let Some(c) = j.counter(s) {
                c.fetch_add(1, Ordering::Relaxed);
            }
        }
        g.status = s;
    }

    pub fn set_note(&self, n: &str) {
        self.inner.lock().unwrap().note = n.to_string();
    }

    pub fn set_error(&self, e: &str) {
        self.inner.lock().unwrap().err = e.to_string();
    }

    pub fn error(&self) -> String {
        self.inner.lock().unwrap().err.clone()
    }

    pub fn status(&self) -> Status {
        self.inner.lock().unwrap().status
    }

    pub fn note(&self) -> String {
        self.inner.lock().unwrap().note.clone()
    }

    pub fn local_path(&self) -> String {
        self.local_path.lock().unwrap().clone()
    }

    pub fn size(&self) -> i64 {
        self.size.load(Ordering::Relaxed)
    }

    pub fn set_size(&self, n: i64) {
        self.size.store(n, Ordering::Relaxed);
    }

    pub fn view_url(&self) -> String {
        self.view_url.lock().unwrap().clone()
    }
}

// ---------- Job ----------

/// One transfer: a set of files moved to or from Frame.io.
pub struct Job {
    pub mgr: Weak<Manager>,
    pub id: String,
    pub kind: String, // upload | download
    pub title: Mutex<String>,
    pub source: Mutex<String>,
    pub dest: Mutex<String>,
    pub account_id: Mutex<String>,
    pub folder_id: Mutex<String>,
    pub share_id: Mutex<String>,
    pub passphrase: Mutex<String>,
    pub subfolder: AtomicBool,
    pub paths: Mutex<Vec<String>>,
    pub log_root: Mutex<String>,
    pub log_name: Mutex<String>,

    pub link: Mutex<String>,
    pub share_link: Mutex<String>,
    pub link_from_file: AtomicBool,
    pub user_canceled: AtomicBool,
    pub verify_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,

    pub watch_id: Mutex<String>,
    pub watch_log: AtomicBool,
    pub on_file_done: Mutex<Option<Arc<dyn Fn(Arc<File>) + Send + Sync>>>,
    pub on_finish: Mutex<Option<Arc<dyn Fn(Arc<Job>) + Send + Sync>>>,

    pub files: Mutex<Vec<Arc<File>>>,

    // Files per status, kept current by File::set_status_locked.
    pub n_done: AtomicI32,
    pub n_failed: AtomicI32,
    pub n_skipped: AtomicI32,
    pub n_canceled: AtomicI32,
    pub n_active: AtomicI32,

    pub total: AtomicI64,
    pub done: AtomicI64,
    pub meter: Meter,

    pub state: Mutex<JobState>,
    pub cx: CancellationToken,
}

pub struct JobState {
    pub status: Status,
    /// The outcome; finish makes it the status once it's saved.
    pub final_status: Option<Status>,
    pub err: String,
    pub phase: String,
    pub created: SystemTime,
    pub started: Option<SystemTime>,
    pub finished: Option<SystemTime>,
}

impl Default for JobState {
    fn default() -> Self {
        JobState {
            status: Status::Queued,
            final_status: None,
            err: String::new(),
            phase: String::new(),
            created: SystemTime::now(),
            started: None,
            finished: None,
        }
    }
}

impl Job {
    pub fn mgr(&self) -> Option<Arc<Manager>> {
        self.mgr.upgrade()
    }

    pub fn add_total(&self, n: i64) {
        self.total.fetch_add(n, Ordering::Relaxed);
    }

    pub fn counter(&self, s: Status) -> Option<&AtomicI32> {
        match s {
            Status::Done => Some(&self.n_done),
            Status::Failed => Some(&self.n_failed),
            Status::Skipped => Some(&self.n_skipped),
            Status::Canceled => Some(&self.n_canceled),
            Status::Running => Some(&self.n_active),
            _ => None,
        }
    }

    pub fn set_link(&self, u: &str) {
        if u.is_empty() {
            return;
        }
        *self.link.lock().unwrap() = u.to_string();
        if let Some(m) = self.mgr() {
            m.touch();
        }
    }

    pub fn link(&self) -> String {
        self.link.lock().unwrap().clone()
    }

    /// Sets the link to a Frame.io folder's page (one quick API call).
    pub async fn link_to_folder(self: &Arc<Self>, folder_id: &str) {
        let Some(m) = self.mgr() else { return };
        let cx = self.cx.child_token();
        let acct = self.account_id.lock().unwrap().clone();
        let t = tokio::time::timeout(Duration::from_secs(10), m.api.folder(&cx, &acct, folder_id)).await;
        if let Ok(Ok(f)) = t {
            self.set_link(&f.view_url);
        }
    }

    /// Publishes the job's file list (read concurrently by the UI) and seeds
    /// the per-status counts.
    ///
    /// The published list is the authority for the counts, so they are
    /// recounted from it rather than added to what they were: a file that
    /// changed status before its list went out (or a republished list) must not
    /// be counted twice, or the view reports more done files than the job has.
    pub fn set_files(&self, fs: Vec<Arc<File>>) {
        let mut counts = [0i32; 5];
        for f in &fs {
            let slot = match f.status() {
                Status::Done => 0,
                Status::Failed => 1,
                Status::Skipped => 2,
                Status::Canceled => 3,
                Status::Running => 4,
                _ => continue,
            };
            counts[slot] += 1;
        }
        *self.files.lock().unwrap() = fs;
        self.n_done.store(counts[0], Ordering::Relaxed);
        self.n_failed.store(counts[1], Ordering::Relaxed);
        self.n_skipped.store(counts[2], Ordering::Relaxed);
        self.n_canceled.store(counts[3], Ordering::Relaxed);
        self.n_active.store(counts[4], Ordering::Relaxed);
    }

    pub fn files(&self) -> Vec<Arc<File>> {
        self.files.lock().unwrap().clone()
    }

    pub fn set_status(&self, s: Status) {
        {
            let mut g = self.state.lock().unwrap();
            g.status = s;
            if s == Status::Running && g.started.is_none() {
                g.started = Some(SystemTime::now());
            }
        }
        if let Some(m) = self.mgr() {
            m.touch();
        }
    }

    pub fn set_phase(&self, p: &str) {
        self.state.lock().unwrap().phase = p.to_string();
        if let Some(m) = self.mgr() {
            m.touch();
        }
    }

    pub fn status(&self) -> Status {
        self.state.lock().unwrap().status
    }

    pub fn phase(&self) -> String {
        self.state.lock().unwrap().phase.clone()
    }

    pub fn error(&self) -> String {
        self.state.lock().unwrap().err.clone()
    }

    pub fn counts(&self) -> (i32, i32, i32, i32) {
        (
            self.n_done.load(Ordering::Relaxed),
            self.n_failed.load(Ordering::Relaxed),
            self.n_skipped.load(Ordering::Relaxed),
            self.n_canceled.load(Ordering::Relaxed),
        )
    }
}

// ---------- Manager ----------

pub struct Manager {
    pub api: Arc<Client>,
    pub hist: Arc<history::Store>,
    pub limiter: Arc<Limiter>,
    pub global: Meter,
    pub settings: Box<dyn Fn() -> Settings + Send + Sync>,
    /// Receives a fresh snapshot whenever something changes (≤ 2/sec).
    pub on_update: Mutex<Option<Arc<dyn Fn(Snapshot) + Send + Sync>>>,
    /// Re-queues a watch-folder batch's unfinished files in their watch.
    pub retry_watch: Mutex<Option<Arc<dyn Fn(&str, &str, Vec<history::File>) -> Result<(), String> + Send + Sync>>>,

    pub verify_delays: Vec<Duration>,
    pub upload_settle: Duration,
    pub stall_limit: Duration,
    pub change_check: Duration,

    pub jobs: Mutex<Vec<Arc<Job>>>,
    pub dirty: AtomicBool,

    pub inspected: Mutex<Option<sharelink::ShareListing>>,
    pub share_lock: Mutex<()>,

    pub progress_logged: Mutex<std::collections::HashMap<String, Instant>>,

    pub awake: AtomicBool,
    pub caffeinate: Mutex<Option<tokio::process::Child>>,

    /// Tells this process's unfinished uploads from an earlier one's with the
    /// same PID.
    pub run: String,
}

/// How many finished watch-folder batches stay in the list.
pub const KEEP_WATCH_JOBS: usize = 10;

/// The post-upload Frame.io status checks.
pub const VERIFY_DELAYS: [u64; 6] = [3, 6, 12, 24, 45, 90];

// ---------- Views sent to the frontend ----------

/// `#[serde(default)]` is for the one reader that is not the app: a snapshot
/// written by hand (`--snapshot`) may name only the fields it cares about.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct JobView {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub source: String,
    pub dest: String,
    pub status: Status,
    pub phase: String,
    pub error: String,
    pub total: i64,
    pub done: i64,
    pub speed: f64,
    pub eta: f64,
    pub files_total: i64,
    pub files_done: i64,
    pub files_failed: i64,
    pub files_active: i64,
    pub started_at: i64,
    pub finished_at: i64,
    pub elapsed: f64,
    pub avg_speed: f64,
    pub link: String,
    /// A watch-folder batch.
    pub watch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct FileView {
    pub name: String,
    pub rel_path: String,
    pub local_path: String,
    pub size: i64,
    pub done: i64,
    pub status: Status,
    pub error: String,
    pub note: String,
    pub view_url: String,
    pub retries: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Snapshot {
    pub jobs: Vec<JobView>,
    pub speed: f64,
    pub active_streams: i64,
    pub stream_limit: i64,
}

impl JobView {
    /// Done, one way or another: `status.terminal()`.
    pub fn terminal(&self) -> bool {
        self.status.terminal()
    }

    /// 0.0–1.0, or `None` while the size isn't known yet.
    pub fn progress(&self) -> Option<f32> {
        if self.total <= 0 {
            None
        } else {
            Some((self.done as f32 / self.total as f32).clamp(0.0, 1.0))
        }
    }

    /// Whether this job is sending files to Frame.io.
    pub fn is_upload(&self) -> bool {
        self.kind == "upload"
    }
}

/// A link as the composer shows it before downloading.
#[derive(Debug, Clone, Serialize, Default)]
pub struct LinkInfo {
    pub share_id: String,
    pub kind: String,
    pub name: String,
    pub files: i64,
    pub unavailable: i64,
    pub total_bytes: i64,
    pub needs_passphrase: bool,
    pub error: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DownloadOptions {
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub passphrase: String,
    #[serde(default)]
    pub dest_dir: String,
    #[serde(default)]
    pub subfolder: bool,
}

impl DownloadOptions {
    pub fn from_value(v: &serde_json::Value) -> DownloadOptions {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UploadOptions {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub folder_id: String,
    #[serde(default)]
    pub dest_label: String,
}

impl UploadOptions {
    pub fn from_value(v: &serde_json::Value) -> UploadOptions {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }
}

/// A file found by scanning upload paths.
pub struct Scanned {
    pub local: String,
    pub rel: String,
    pub size: i64,
}

// ---------- Small helpers the whole package shares ----------

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The frontend's `fmtBytes`, as the menu bar and notifications word it.
pub fn fmt_bytes(n: i64) -> String {
    crate::limiter::human_bytes(n)
}

/// The frontend's `fmtDur`.
pub fn fmt_dur(sec: f64) -> String {
    let s = sec.round() as i64;
    let (h, m) = (s / 3600, s % 3600 / 60);
    if h > 0 {
        format!("{}h {:02}m", h, m)
    } else if m > 0 {
        format!("{}m {:02}s", m, s % 60)
    } else {
        format!("{}s", s)
    }
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    /// A manager whose history and log dirs are in a tempdir: the job model
    /// needs one to make a job, and nothing here may touch the real profile.
    fn manager() -> (tokio::runtime::Runtime, Arc<Manager>, tempfile::TempDir) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("hist")).expect("hist");
        std::fs::create_dir_all(base.join("logs")).expect("logs");
        let m = rt.block_on(async {
            let api = Arc::new(crate::api::Client::new(crate::auth::AuthManager::new_static("t")));
            let hist = Arc::new(history::Store::open(&base.join("hist"), &base.join("logs")).expect("store"));
            manager::new_tuned(
                api,
                hist,
                Box::new(|| {
                    let mut s = Settings::default();
                    s.streams = 4;
                    s.no_folder_logs = true;
                    s
                }),
                manager::Tune::default(),
            )
        });
        (rt, m, dir)
    }

    /// A file that is finished before its list is published must be counted
    /// once. `set_files` seeds the per-status counts from the list, and a
    /// `set_status` before the publish has already moved them, so seeding has
    /// to start from zero: otherwise the UI reports more done files than the
    /// job has (`files_done > files_total`), which no view may ever show.
    #[test]
    fn publishing_the_file_list_counts_each_file_once() {
        let (_rt, m, _dir) = manager();
        let j = m.new_job("download");
        let a = Arc::new(File::new(0, "a.mov", "a.mov", "/tmp/a.mov", 10));
        let b = Arc::new(File::new(1, "b.mov", "b.mov", "/tmp/b.mov", 20));
        let c = Arc::new(File::new(2, "c.mov", "c.mov", "/tmp/c.mov", 30));
        for f in [&a, &b, &c] {
            f.attach(&j);
        }
        // two files were settled before the list went out: one already on disk,
        // one skipped for being empty
        a.set_status(Status::Done, "");
        c.set_status(Status::Skipped, "");
        j.set_files(vec![a.clone(), b.clone(), c.clone()]);
        let v = j.view();
        assert_eq!(v.files_total, 3);
        assert_eq!(j.counts(), (1, 0, 1, 0), "each file is counted once, and only once");
        assert_eq!(v.files_done, 2, "one done plus one skipped");
        assert_eq!(v.files_active, 0);
        assert!(v.files_done + v.files_failed + v.files_active <= v.files_total);
        // republishing the same list must not count them again either
        j.set_files(vec![a.clone(), b.clone(), c.clone()]);
        let v = j.view();
        assert_eq!(j.counts(), (1, 0, 1, 0));
        assert!(v.files_done + v.files_failed + v.files_active <= v.files_total);
        // and moving a file on afterwards still lands on the right count
        b.set_status(Status::Running, "");
        let v = j.view();
        assert_eq!((v.files_done, v.files_active), (2, 1));
        b.set_status(Status::Failed, "");
        let v = j.view();
        assert_eq!((v.files_done, v.files_active, v.files_failed), (2, 0, 1));
        assert_eq!(j.counts(), (1, 1, 1, 0));
    }

}
