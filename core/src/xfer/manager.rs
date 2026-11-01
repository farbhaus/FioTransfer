
//! The transfer manager: the job list, its lifecycle and the 2 Hz snapshot loop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures::future::BoxFuture;
use futures::FutureExt;
use tokio_util::sync::CancellationToken;

use super::sharelink::ShareListing;
use super::{
    fmt_bytes, fmt_dur, DownloadOptions, File, FileView, Job, JobState, JobView, LinkInfo, Manager, Scanned,
    Snapshot, Status, UploadOptions, KEEP_WATCH_JOBS, VERIFY_DELAYS,
};
use crate::api::Client;
use crate::config::Settings;
use crate::history;
use crate::limiter::{Limiter, Meter};
use crate::share;
use crate::xfer::folderlog;

/// The work one file needs: a boxed future the job runs with bounded concurrency.
pub type FileTask = Arc<dyn Fn(Arc<Job>, Arc<File>) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// The transfer engine's timings, which a measuring harness may shorten.
pub struct Tune {
    pub upload_settle: Duration,
    pub verify_delays: Vec<Duration>,
    pub change_check: Duration,
    pub stall_limit: Duration,
}

impl Default for Tune {
    fn default() -> Self {
        Tune {
            upload_settle: Duration::from_secs(10),
            verify_delays: VERIFY_DELAYS.iter().map(|s| Duration::from_secs(*s)).collect(),
            change_check: Duration::from_secs(2),
            stall_limit: Duration::from_secs(120),
        }
    }
}

pub fn new_manager(
    api: Arc<Client>,
    hist: Arc<history::Store>,
    settings: Box<dyn Fn() -> Settings + Send + Sync>,
) -> Arc<Manager> {
    new_tuned(api, hist, settings, Tune::default())
}

pub fn new_tuned(
    api: Arc<Client>,
    hist: Arc<history::Store>,
    settings: Box<dyn Fn() -> Settings + Send + Sync>,
    tune: Tune,
) -> Arc<Manager> {
    let s = settings();
    let m = Arc::new(Manager {
        api,
        hist,
        limiter: Arc::new(Limiter::new(s.streams.max(1) as usize)),
        global: Meter::default(),
        settings,
        on_update: Mutex::new(None),
        retry_watch: Mutex::new(None),
        verify_delays: tune.verify_delays,
        upload_settle: tune.upload_settle,
        stall_limit: tune.stall_limit,
        change_check: tune.change_check,
        jobs: Mutex::new(Vec::new()),
        dirty: AtomicBool::new(false),
        inspected: Mutex::new(None),
        share_lock: Mutex::new(()),
        progress_logged: Mutex::new(HashMap::new()),
        awake: AtomicBool::new(false),
        caffeinate: Mutex::new(None),
        run: new_id(),
    });
    let mm = m.clone();
    tokio::spawn(async move { manager_loop(mm).await });
    m
}

async fn manager_loop(m: Arc<Manager>) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        m.global.sample();
        let mut active = false;
        for j in m.jobs.lock().unwrap().clone() {
            if !j.status().terminal() {
                active = true;
                j.meter.sample();
            }
        }
        m.hold_awake(active && m.awake.load(Ordering::Relaxed));
        m.log_progress();
        let dirty = m.dirty.swap(false, Ordering::Relaxed);
        if active || dirty {
            let cb = { m.on_update.lock().unwrap().clone() };
            if let Some(cb) = cb {
                cb(m.snapshot());
            }
        }
    }
}

impl Manager {
    /// Keeps a caffeinate assertion while on, so the Mac doesn't idle-sleep
    /// mid-transfer.
    fn hold_awake(&self, on: bool) {
        let mut g = self.caffeinate.lock().unwrap();
        if on && g.is_none() {
            if let Ok(c) = tokio::process::Command::new("/usr/bin/caffeinate")
                .arg("-i")
                .arg("-w")
                .arg(std::process::id().to_string())
                .spawn()
            {
                *g = Some(c);
            }
        } else if !on && g.is_some() {
            if let Some(mut c) = g.take() {
                tokio::spawn(async move {
                    let _ = c.kill().await;
                });
            }
        }
    }

    pub fn set_keep_awake(&self, on: bool) {
        self.awake.store(on, Ordering::Relaxed);
    }

    pub fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn set_streams(&self, n: i32) {
        self.limiter.set_limit(n.max(1) as usize);
    }

    /// Makes a job; it appears in the list once its fields are set and add is called.
    pub fn new_job(self: &Arc<Self>, kind: &str) -> Arc<Job> {
        Arc::new(Job {
            mgr: Arc::downgrade(self),
            id: new_id(),
            kind: kind.to_string(),
            title: Mutex::new(String::new()),
            source: Mutex::new(String::new()),
            dest: Mutex::new(String::new()),
            account_id: Mutex::new(String::new()),
            folder_id: Mutex::new(String::new()),
            share_id: Mutex::new(String::new()),
            passphrase: Mutex::new(String::new()),
            subfolder: AtomicBool::new(false),
            paths: Mutex::new(Vec::new()),
            log_root: Mutex::new(String::new()),
            log_name: Mutex::new(String::new()),
            link: Mutex::new(String::new()),
            share_link: Mutex::new(String::new()),
            link_from_file: AtomicBool::new(false),
            user_canceled: AtomicBool::new(false),
            verify_tasks: Mutex::new(Vec::new()),
            watch_id: Mutex::new(String::new()),
            watch_log: AtomicBool::new(false),
            on_file_done: Mutex::new(None),
            on_finish: Mutex::new(None),
            files: Mutex::new(Vec::new()),
            n_done: Default::default(),
            n_failed: Default::default(),
            n_skipped: Default::default(),
            n_canceled: Default::default(),
            n_active: Default::default(),
            total: Default::default(),
            done: Default::default(),
            meter: Meter::default(),
            state: Mutex::new(JobState::default()),
            cx: CancellationToken::new(),
        })
    }

    pub fn add(&self, j: &Arc<Job>) {
        self.jobs.lock().unwrap().push(j.clone());
        self.touch();
    }

    pub fn job(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    /// Stops a job at the user's request (shutdown uses cancel_all instead).
    pub fn cancel(&self, id: &str) {
        if let Some(j) = self.job(id) {
            j.user_canceled.store(true, Ordering::Relaxed);
            j.cx.cancel();
        }
    }

    pub fn cancel_all(&self) {
        for j in self.jobs.lock().unwrap().iter() {
            j.cx.cancel();
        }
    }

    /// Removes finished jobs from the active list (history keeps them).
    pub fn clear_finished(&self) {
        self.jobs.lock().unwrap().retain(|j| !j.status().terminal());
        self.touch();
    }

    /// Drops all but the newest finished watch-folder batches, so a
    /// long-running watch doesn't grow the list without end.
    pub fn prune_watch_jobs(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        let mut n = 0usize;
        let mut drop: Vec<usize> = Vec::new();
        for i in (0..jobs.len()).rev() {
            if !jobs[i].watch_id.lock().unwrap().is_empty() && jobs[i].status().terminal() {
                n += 1;
                if n > KEEP_WATCH_JOBS {
                    drop.push(i);
                }
            }
        }
        for i in drop.into_iter() {
            jobs.remove(i);
        }
    }

    pub fn active(&self) -> bool {
        self.jobs.lock().unwrap().iter().any(|j| !j.status().terminal())
    }

    /// Sums the speed of running transfers by direction (for the menu bar).
    pub fn activity(&self) -> (f64, f64, usize) {
        let (mut up, mut down, mut active) = (0.0, 0.0, 0usize);
        for j in self.jobs.lock().unwrap().clone() {
            if j.status().terminal() {
                continue;
            }
            active += 1;
            if j.kind == "upload" {
                up += j.meter.rate();
            } else {
                down += j.meter.rate();
            }
        }
        (up, down, active)
    }

    // ---------- Download ----------

    pub fn start_download(self: &Arc<Self>, mut o: DownloadOptions) -> Result<Arc<Job>, String> {
        o.link = o.link.trim().to_string();
        if o.link.is_empty() {
            return Err("paste a share link".to_string());
        }
        match std::fs::metadata(&o.dest_dir) {
            Ok(md) if md.is_dir() => {}
            _ => return Err(format!("destination folder not found: {}", o.dest_dir)),
        }
        let j = self.new_job("download");
        *j.source.lock().unwrap() = o.link.clone();
        *j.dest.lock().unwrap() = o.dest_dir.clone();
        *j.passphrase.lock().unwrap() = o.passphrase.clone();
        j.subfolder.store(o.subfolder, Ordering::Relaxed);
        *j.title.lock().unwrap() = "Share link".to_string();
        self.add(&j);
        let _ = self.hist.start(&j.record(), &[]);
        self.hist.log(&format!("DOWNLOAD queued   [{}] {} -> {}", j.id, o.link, o.dest_dir));
        let jj = j.clone();
        tokio::spawn(async move { super::download::run_download(jj).await });
        Ok(j)
    }

    /// Resolves a link and lists its files. The result is returned even on
    /// error once the link resolved, so callers can report what it was.
    pub async fn list_share(
        &self,
        cx: &CancellationToken,
        raw: &str,
        passphrase: &str,
        phase: &(dyn Fn(&str) + Send + Sync),
    ) -> (Option<ShareListing>, Result<(), String>) {
        phase("Opening share link…");
        let link = match share::resolve_link(cx, raw).await {
            Ok(l) => l,
            Err(e) => return (None, Err(e)),
        };
        let mut l = ShareListing { key: listing_key(raw, passphrase), link: link.clone(), pass: passphrase.to_string(), ..Default::default() };
        if l.pass.is_empty() {
            if let Some(p) = crate::secret::get(&passphrase_key(&link)) {
                l.pass = p;
            }
        }
        let sc = share::open(&link, &l.pass);
        match sc.info(cx).await {
            Ok(i) => l.info = i,
            Err(e) => return (Some(l), Err(e)),
        }
        if !l.info.downloading_enabled {
            return (Some(l), Err(share::ERR_DOWNLOAD_DISABLED.to_string()));
        }
        if !l.pass.is_empty() {
            let _ = crate::secret::set(&passphrase_key(&link), &l.pass);
        }
        phase("Listing files…");
        let progress = |n: usize| phase(&format!("Listing files… {} found", n));
        match sc.list(cx, &progress).await {
            Ok(files) => l.files = files,
            Err(e) => return (Some(l), Err(e)),
        }
        l.at = Some(Instant::now());
        (Some(l), Ok(()))
    }

    /// Returns (and forgets) the last InspectLink result if it is for this link
    /// and passphrase and still fresh, so a download started from the preview
    /// doesn't list the share a second time.
    pub fn take_inspected(&self, raw: &str, passphrase: &str) -> Option<ShareListing> {
        let mut g = self.inspected.lock().unwrap();
        let keep = match g.as_ref() {
            Some(l) => l.key == listing_key(raw, passphrase) && l.at.map(|t| t.elapsed() <= INSPECT_TTL).unwrap_or(false),
            None => false,
        };
        if keep {
            g.take()
        } else {
            None
        }
    }

    /// Resolves a link and summarizes it without downloading.
    pub async fn inspect_link(&self, cx: &CancellationToken, link: &str, passphrase: &str) -> LinkInfo {
        let (l, res) = self.list_share(cx, link, passphrase, &|_| {}).await;
        let mut li = LinkInfo::default();
        if let Some(l) = &l {
            li.share_id = l.link.id.clone();
            li.kind = l.link.kind.clone();
            li.name = l.info.name.clone();
        }
        if let Err(e) = res {
            li.needs_passphrase = e == share::ERR_PASSPHRASE || e == share::ERR_BAD_PASSPHRASE;
            li.error = e;
            return li;
        }
        let l = l.expect("listing");
        for f in &l.files {
            li.files += 1;
            li.total_bytes += f.size;
            if !f.problem.is_empty() {
                li.unavailable += 1;
            }
        }
        *self.inspected.lock().unwrap() = Some(l);
        li
    }

    // ---------- Upload ----------

    pub fn start_upload(self: &Arc<Self>, o: UploadOptions) -> Result<Arc<Job>, String> {
        if o.paths.is_empty() {
            return Err("nothing to upload".to_string());
        }
        if o.account_id.is_empty() || o.folder_id.is_empty() {
            return Err("choose a Frame.io destination folder first".to_string());
        }
        if !self.api.auth.logged_in() {
            return Err("sign in to Frame.io first".to_string());
        }
        let j = self.new_job("upload");
        *j.account_id.lock().unwrap() = o.account_id.clone();
        *j.folder_id.lock().unwrap() = o.folder_id.clone();
        *j.dest.lock().unwrap() = o.dest_label.clone();
        *j.source.lock().unwrap() = summarize_paths(&o.paths);
        *j.title.lock().unwrap() = summarize_paths(&o.paths);
        *j.paths.lock().unwrap() = o.paths.clone();
        let ts = std::time::Instant::now();
        let trace = std::env::var("FIO_TRACE").is_ok();
        self.add(&j);
        let _ = self.hist.start(&j.record(), &[]);
        if trace {
            eprintln!("[trace] start_upload: hist.start {:?}", ts.elapsed());
        }
        let summary = summarize_paths(&o.paths);
        self.hist.log(&format!("UPLOAD   queued   [{}] {} -> {}", j.id, summary, o.dest_label));
        if trace {
            eprintln!("[trace] start_upload: log {:?}", ts.elapsed());
        }
        let jj = j.clone();
        tokio::spawn(async move { run_upload(jj).await });
        Ok(j)
    }

    // ---------- Retry ----------

    /// Re-runs a transfer from history.
    pub async fn retry(self: &Arc<Self>, id: &str, passphrase: &str) -> Result<Option<Arc<Job>>, String> {
        let t = self.hist.get(id)?.ok_or_else(|| "no such transfer".to_string())?;
        let opts: JobOptions = serde_json::from_str(&t.options).unwrap_or_default();
        if t.kind == "download" && opts.watch.is_empty() {
            let j = self.start_download(DownloadOptions {
                link: t.source.clone(),
                passphrase: passphrase.to_string(),
                dest_dir: t.dest.clone(),
                subfolder: opts.subfolder,
            })?;
            return Ok(Some(j));
        }
        let files = self.hist.files(id)?;
        let todo: Vec<history::File> = files
            .iter()
            .filter(|f| f.status != Status::Done.as_str() && f.status != Status::Skipped.as_str())
            .cloned()
            .collect();
        // Batches recorded before the watch ID was kept are recognizable by
        // their source: the watched folder itself, where uploads show a file name.
        if !opts.watch.is_empty() || (opts.paths.len() == 1 && t.source == opts.paths[0]) {
            let cb = self.retry_watch.lock().unwrap().clone();
            let Some(cb) = cb else { return Err("watch folders aren't running".to_string()) };
            cb(&opts.watch, &t.source, todo)?;
            return Ok(None);
        }
        if files.is_empty() {
            let j = self.start_upload(UploadOptions {
                paths: opts.paths.clone(),
                account_id: t.account_id.clone(),
                folder_id: t.folder_id.clone(),
                dest_label: t.dest.clone(),
            })?;
            return Ok(Some(j));
        }
        if todo.is_empty() {
            return Err("every file in this transfer already finished".to_string());
        }
        let j = self.new_job("upload");
        *j.account_id.lock().unwrap() = t.account_id.clone();
        *j.folder_id.lock().unwrap() = t.folder_id.clone();
        *j.dest.lock().unwrap() = t.dest.clone();
        *j.source.lock().unwrap() = t.source.clone();
        *j.title.lock().unwrap() = format!("Retry: {}", t.title);
        *j.paths.lock().unwrap() = opts.paths.clone();
        self.add(&j);
        let _ = self.hist.start(&j.record(), &[]);
        self.hist.log(&format!("UPLOAD   retry    [{}] of [{}] {} files", j.id, t.id, todo.len()));
        let m = self.clone();
        let jj = j.clone();
        tokio::spawn(async move { retry_upload(m, jj, todo).await });
        Ok(Some(j))
    }

    pub fn snapshot(&self) -> Snapshot {
        let jobs = self.jobs.lock().unwrap().clone();
        let s = (self.settings)();
        let mut snap = Snapshot {
            jobs: Vec::new(),
            speed: self.global.rate(),
            active_streams: self.limiter.active() as i64,
            stream_limit: s.streams as i64,
        };
        for j in jobs.iter().rev() {
            snap.jobs.push(j.view());
        }
        snap
    }

    /// Per-file progress for one job (active list), or none.
    pub fn files(&self, id: &str) -> Vec<FileView> {
        let Some(j) = self.job(id) else { return Vec::new() };
        j.files()
            .iter()
            .map(|f| FileView {
                name: f.name.clone(),
                rel_path: f.rel_path.clone(),
                local_path: f.local_path(),
                size: f.size(),
                done: f.done.load(Ordering::Relaxed),
                status: f.status(),
                error: f.error(),
                note: f.note(),
                view_url: f.view_url(),
                retries: f.retries.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// Writes a line per running job every minute, so a slow transfer leaves a
    /// speed trace in the app log.
    fn log_progress(&self) {
        let now = Instant::now();
        let logged = self.progress_logged.lock().unwrap().clone();
        let mut next: HashMap<String, Instant> = HashMap::new();
        for j in self.jobs.lock().unwrap().clone() {
            if j.status() != Status::Running {
                continue;
            }
            match logged.get(&j.id).copied() {
                None => {
                    next.insert(j.id.clone(), now);
                }
                Some(t) if now.duration_since(t) >= Duration::from_secs(60) => {
                    next.insert(j.id.clone(), now);
                    let v = j.view();
                    let pct = if v.total > 0 { 100.0 * v.done as f64 / v.total as f64 } else { 0.0 };
                    self.hist.log(&format!(
                        "{} progress [{}] {:.0}% {} of {}, {}/s, {}/{} streams busy",
                        kind_tag(&j.kind),
                        j.id,
                        pct,
                        fmt_bytes(v.done),
                        fmt_bytes(v.total),
                        fmt_bytes(v.speed as i64),
                        self.limiter.active(),
                        (self.settings)().streams
                    ));
                }
                Some(t) => {
                    next.insert(j.id.clone(), t);
                }
            }
        }
        *self.progress_logged.lock().unwrap() = next;
    }
}

// ---------- Job bookkeeping ----------

impl Job {
    pub fn record(&self) -> history::Transfer {
        let (done, failed, skipped, _) = self.counts();
        let opts = serde_json::to_string(&JobOptions {
            subfolder: self.subfolder.load(Ordering::Relaxed),
            paths: self.paths.lock().unwrap().clone(),
            watch: self.watch_id.lock().unwrap().clone(),
        })
        .unwrap_or_default();
        let g = self.state.lock().unwrap();
        let status = g.final_status.unwrap_or(g.status);
        let mut t = history::Transfer {
            id: self.id.clone(),
            kind: self.kind.clone(),
            title: self.title.lock().unwrap().clone(),
            source: self.source.lock().unwrap().clone(),
            dest: self.dest.lock().unwrap().clone(),
            account_id: self.account_id.lock().unwrap().clone(),
            folder_id: self.folder_id.lock().unwrap().clone(),
            share_id: self.share_id.lock().unwrap().clone(),
            status: status.as_str().to_string(),
            error: g.err.clone(),
            total_bytes: self.total.load(Ordering::Relaxed),
            done_bytes: self.done.load(Ordering::Relaxed),
            files_total: self.files.lock().unwrap().len() as i64,
            files_done: (done + skipped) as i64,
            files_failed: failed as i64,
            created_at: ms_from_system(g.created),
            peak_bps: self.meter.peak(),
            options: opts,
            link: self.link.lock().unwrap().clone(),
            share_link: self.share_link.lock().unwrap().clone(),
            ..Default::default()
        };
        if let Some(st) = g.started {
            t.started_at = ms_from_system(st);
            let end = g.finished.unwrap_or_else(SystemTime::now);
            t.finished_at = ms_from_system(end);
            if let Ok(d) = end.duration_since(st) {
                let secs = d.as_secs_f64();
                if secs > 0.0 {
                    t.avg_bps = self.meter.total() as f64 / secs;
                }
            }
        }
        t
    }

    pub fn file_record(&self, f: &File) -> history::File {
        let g = f.inner.lock().unwrap();
        history::File {
            transfer_id: self.id.clone(),
            idx: f.idx as i64,
            rel_path: f.rel_path.clone(),
            local_path: f.local_path(),
            size: f.size(),
            status: g.status.as_str().to_string(),
            error: g.err.clone(),
            note: g.note.clone(),
            remote_parent: f.remote_parent.lock().unwrap().clone(),
            remote_id: f.remote_id.lock().unwrap().clone(),
            view_url: f.view_url(),
            asset_id: f.asset_id.clone(),
            finished_at: g.finished.map(ms_from_system).unwrap_or(0),
        }
    }

    pub fn persist_file(&self, f: &File) {
        if let Some(m) = self.mgr() {
            m.hist.update_file(&self.file_record(f));
        }
    }

    pub fn persist_start(&self) {
        let list = self.files();
        let files: Vec<history::File> = list.iter().map(|f| self.file_record(f)).collect();
        let mut rec = self.record();
        rec.status = Status::Running.as_str().to_string();
        if let Some(m) = self.mgr() {
            let _ = m.hist.start(&rec, &files);
            if self.kind == "download" {
                let _ = std::fs::create_dir_all(self.log_root.lock().unwrap().clone());
            }
            folderlog::write_folder_log(self, false);
            m.hist.log(&format!(
                "{} START    [{}] {} — {} files, {} — {} -> {}",
                kind_tag(&self.kind),
                self.id,
                self.title.lock().unwrap(),
                list.len(),
                fmt_bytes(self.total.load(Ordering::Relaxed)),
                self.source.lock().unwrap(),
                self.dest.lock().unwrap()
            ));
        }
    }

    pub fn fail(&self, err: &str) {
        let mut g = self.state.lock().unwrap();
        if self.cx.is_cancelled() {
            g.final_status = Some(Status::Canceled);
        } else {
            g.final_status = Some(Status::Failed);
            g.err = err.to_string();
        }
    }

    pub fn log_file_ok(&self, f: &File) {
        let detail = f.detail.lock().unwrap().clone();
        if let Some(m) = self.mgr() {
            if detail.is_empty() {
                m.hist.log(&format!("{} file ok  [{}] {} ({})", kind_tag(&self.kind), self.id, f.rel_path, fmt_bytes(f.size())));
            } else {
                m.hist.log(&format!(
                    "{} file ok  [{}] {} ({}; {})",
                    kind_tag(&self.kind),
                    self.id,
                    f.rel_path,
                    fmt_bytes(f.size()),
                    detail
                ));
            }
        }
    }

    /// Runs `task` over all pending files with bounded file-level concurrency.
    /// Stream-level concurrency is enforced separately by the shared Limiter.
    pub async fn run_files(self: &Arc<Self>, task: FileTask) {
        let slots = self.mgr().map(|m| (m.settings)().streams.max(4) as usize).unwrap_or(16);
        let sem = Arc::new(tokio::sync::Semaphore::new(slots));
        let mut handles = Vec::new();
        let files = self.files();
        for file in files {
            if file.status().terminal() {
                self.persist_file(&file);
                continue;
            }
            let permit = tokio::select! {
                p = sem.clone().acquire_owned() => match p { Ok(p) => p, Err(_) => break },
                _ = self.cx.cancelled() => break,
            };
            let j = self.clone();
            let f = task.clone();
            let file2 = file.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit;
                file2.set_status(Status::Running, "");
                if let Some(m) = j.mgr() {
                    m.touch();
                }
                let res = (f)(j.clone(), file2.clone()).await;
                {
                    let mut g = file2.inner.lock().unwrap();
                    g.finished = Some(SystemTime::now());
                    if let Err(e) = &res {
                        g.err = e.clone();
                    }
                    match (&res, j.cx.is_cancelled()) {
                        (Err(_), true) => file2.set_status_locked(&mut g, Status::Canceled),
                        (Err(_), false) => file2.set_status_locked(&mut g, Status::Failed),
                        (Ok(()), _) if g.status != Status::Skipped && g.status != Status::Failed => {
                            file2.set_status_locked(&mut g, Status::Done)
                        }
                        _ => {}
                    }
                }
                j.persist_file(&file2);
                let on_done = { j.on_file_done.lock().unwrap().clone() };
                if let Some(cb) = on_done {
                    cb(file2.clone());
                }
                let (st, e, later) = {
                    let g = file2.inner.lock().unwrap();
                    (g.status, g.err.clone(), g.check_later)
                };
                if let Some(m) = j.mgr() {
                    if !later {
                        match st {
                            Status::Failed => m.hist.log(&format!("{} FAILED   [{}] {} — {}", kind_tag(&j.kind), j.id, file2.rel_path, e)),
                            Status::Done => j.log_file_ok(&file2),
                            _ => {}
                        }
                    }
                    m.touch();
                }
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        for file in self.files() {
            if !file.status().terminal() {
                file.set_status(Status::Canceled, "");
                self.persist_file(&file);
            }
        }
    }

    pub fn settle_files(&self) {
        let reason = {
            let g = self.state.lock().unwrap();
            if g.err.is_empty() { "not started".to_string() } else { g.err.clone() }
        };
        for f in self.files() {
            {
                let mut g = f.inner.lock().unwrap();
                if g.status.terminal() {
                    continue;
                }
                if self.cx.is_cancelled() {
                    f.set_status_locked(&mut g, Status::Canceled);
                } else {
                    f.set_status_locked(&mut g, Status::Failed);
                    g.err = reason.clone();
                }
            }
            self.persist_file(&f);
        }
    }

    pub async fn finish(self: &Arc<Self>) {
        let tf = std::time::Instant::now();
        let trace = std::env::var("FIO_TRACE").is_ok();
        // Join the Frame.io confirmation checks directly rather than polling
        // them: polling added up to one sleep's worth of dead time per job.
        let checks: Vec<_> = {
            let mut g = self.verify_tasks.lock().unwrap();
            std::mem::take(&mut *g)
        };
        for h in checks {
            let _ = h.await;
        }
        if trace {
            eprintln!("[trace] finish: verify wait {:?}", tf.elapsed());
        }
        self.settle_files();
        {
            let mut g = self.state.lock().unwrap();
            g.finished = Some(SystemTime::now());
            if g.final_status.is_none() {
                let (done, failed, skipped, canceled) = self.counts();
                let nfiles = self.files().len();
                if self.cx.is_cancelled() && (canceled > 0 || nfiles == 0) {
                    g.final_status = Some(Status::Canceled);
                } else if failed == 0 {
                    g.final_status = Some(Status::Done);
                } else if done + skipped > 0 {
                    g.final_status = Some(Status::Partial);
                    g.err = format!("{} of {} files failed", failed, nfiles);
                } else {
                    g.final_status = Some(Status::Failed);
                    if g.err.is_empty() {
                        for f in self.files() {
                            let e = f.error();
                            if !e.is_empty() {
                                g.err = e;
                                break;
                            }
                        }
                    }
                }
            }
        }
        self.cx.cancel();
        let rec = self.record();
        if trace {
            eprintln!("[trace] finish: record {:?}", tf.elapsed());
        }
        if let Some(m) = self.mgr() {
            m.hist.update(&rec);
            if !self.files().is_empty() {
                folderlog::write_folder_log(self, true);
            }
            let mut msg = format!(
                "{} {:8} [{}] {} — {}/{} files, {} in {}, avg {}/s",
                kind_tag(&self.kind),
                rec.status.to_uppercase(),
                self.id,
                self.title.lock().unwrap(),
                rec.files_done,
                rec.files_total,
                fmt_bytes(rec.done_bytes),
                fmt_dur((rec.finished_at - rec.started_at) as f64 / 1000.0),
                fmt_bytes(rec.avg_bps as i64)
            );
            if !rec.error.is_empty() {
                msg.push_str(&format!(" — {}", rec.error));
            }
            m.hist.log(&msg);
        }
        if trace {
            eprintln!("[trace] finish: history+log {:?}", tf.elapsed());
        }
        // The guard must be gone before set_status locks the same state again:
        // an `if let` keeps the scrutinee's temporary alive for the whole body.
        let final_status = { self.state.lock().unwrap().final_status };
        if let Some(f) = final_status {
            self.set_status(f);
        }
        let on_finish = { self.on_finish.lock().unwrap().clone() };
        if let Some(cb) = on_finish {
            cb(self.clone());
        }
        if !self.watch_id.lock().unwrap().is_empty() {
            if let Some(m) = self.mgr() {
                m.prune_watch_jobs();
            }
        }
    }

    pub fn view(&self) -> JobView {
        let (done, failed, skipped, _) = self.counts();
        let g = self.state.lock().unwrap();
        let mut v = JobView {
            id: self.id.clone(),
            kind: self.kind.clone(),
            title: self.title.lock().unwrap().clone(),
            source: self.source.lock().unwrap().clone(),
            dest: self.dest.lock().unwrap().clone(),
            status: g.status,
            phase: g.phase.clone(),
            error: g.err.clone(),
            total: self.total.load(Ordering::Relaxed),
            done: self.done.load(Ordering::Relaxed),
            files_total: self.files.lock().unwrap().len() as i64,
            files_done: (done + skipped) as i64,
            files_failed: failed as i64,
            files_active: self.n_active.load(Ordering::Relaxed) as i64,
            link: self.link.lock().unwrap().clone(),
            watch: !self.watch_id.lock().unwrap().is_empty(),
            ..Default::default()
        };
        if !g.status.terminal() {
            v.speed = self.meter.rate();
            if v.speed > 1.0 && v.total > v.done {
                v.eta = (v.total - v.done) as f64 / v.speed;
            }
        }
        if let Some(st) = g.started {
            v.started_at = ms_from_system(st);
            let end = g.finished.unwrap_or_else(SystemTime::now);
            if g.finished.is_some() {
                v.finished_at = ms_from_system(end);
                v.elapsed = end.duration_since(st).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            } else {
                v.elapsed = SystemTime::now().duration_since(st).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            }
            if v.elapsed > 0.0 {
                v.avg_speed = self.meter.total() as f64 / v.elapsed;
            }
        }
        v
    }
}

// ---------- Runs ----------

pub async fn run_upload(j: Arc<Job>) {
    let tr = std::time::Instant::now();
    let trace = std::env::var("FIO_TRACE").is_ok();
    if trace {
        eprintln!("[trace] run_upload started");
    }
    j.set_status(Status::Resolving);
    j.set_phase("Scanning files…");
    let paths = j.paths.lock().unwrap().clone();
    let items = match scan_paths(&paths) {
        Ok(v) => v,
        Err(e) => {
            j.fail(&e);
            j.finish().await;
            return;
        }
    };
    if items.is_empty() {
        j.fail("no files found to upload");
        j.finish().await;
        return;
    }
    let rels: Vec<String> = items.iter().map(|i| i.rel.clone()).collect();
    let dirs = dirs_for(&rels);
    if trace {
        eprintln!("[trace] run_upload: scanned {:?}", tr.elapsed());
    }
    if !dirs.is_empty() {
        j.set_phase(&format!("Creating {} folders on Frame.io…", dirs.len()));
    }
    let Some(m) = j.mgr() else {
        j.fail("transfer manager stopped");
        return;
    };
    let folder = j.folder_id.lock().unwrap().clone();
    let acct = j.account_id.lock().unwrap().clone();
    let (ids, urls) = match super::upload::ensure_folders(&m.api, &j.cx, &acct, &folder, &dirs).await {
        Ok(v) => v,
        Err(e) => {
            j.fail(&e);
            j.finish().await;
            return;
        }
    };
    if trace {
        eprintln!("[trace] run_upload: folders {:?}", tr.elapsed());
    }
    let top = top_folder(&paths, &items);
    if !top.is_empty() {
        if let Some(u) = urls.get(&top) {
            if !u.is_empty() {
                j.set_link(u);
            }
        }
    } else if items.len() == 1 {
        j.link_from_file.store(true, Ordering::Relaxed);
    } else {
        j.link_to_folder(&folder).await;
    }
    let mut list = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        let f = Arc::new(File::new(i, base_name(&it.rel), &it.rel, &it.local, it.size));
        f.attach(&j);
        *f.remote_parent.lock().unwrap() = ids.get(&parent_dir(&it.rel)).cloned().unwrap_or_default();
        list.push(f);
        j.add_total(it.size);
    }
    if trace {
        eprintln!("[trace] run_upload: files built {:?}", tr.elapsed());
    }
    j.set_files(list);
    j.persist_start();
    if trace {
        eprintln!("[trace] run_upload: persist_start {:?}", tr.elapsed());
    }
    j.set_status(Status::Running);
    j.set_phase("");
    let task: FileTask = Arc::new(|jj, ff| async move { super::upload::upload_file(jj, ff).await }.boxed());
    j.run_files(task).await;
    j.finish().await;
}

async fn retry_upload(m: Arc<Manager>, j: Arc<Job>, todo: Vec<history::File>) {
    j.set_status(Status::Resolving);
    let mut need = Vec::new();
    for f in &todo {
        if f.remote_parent.is_empty() {
            need.push(f.rel_path.clone());
        }
    }
    let mut ids: HashMap<String, String> = HashMap::new();
    let folder = j.folder_id.lock().unwrap().clone();
    ids.insert(String::new(), folder.clone());
    if todo.len() == 1 {
        j.link_from_file.store(true, Ordering::Relaxed);
    } else {
        j.link_to_folder(&folder).await;
    }
    if !need.is_empty() {
        let acct = j.account_id.lock().unwrap().clone();
        match super::upload::ensure_folders(&m.api, &j.cx, &acct, &folder, &dirs_for(&need)).await {
            Ok((i, _)) => ids = i,
            Err(e) => {
                j.fail(&e);
                j.finish().await;
                return;
            }
        }
    }
    let mut list = Vec::with_capacity(todo.len());
    for (i, hf) in todo.iter().enumerate() {
        let (size, status, err) = match std::fs::metadata(&hf.local_path) {
            Ok(md) => (md.len() as i64, Status::Queued, String::new()),
            Err(_) => (0, Status::Failed, format!("local file missing: {}", hf.local_path)),
        };
        let f = Arc::new(File::new(i, base_name(&hf.rel_path), &hf.rel_path, &hf.local_path, size));
        f.attach(&j);
        *f.remote_parent.lock().unwrap() = if hf.remote_parent.is_empty() {
            ids.get(&parent_dir(&hf.rel_path)).cloned().unwrap_or_default()
        } else {
            hf.remote_parent.clone()
        };
        *f.inner.lock().unwrap() = super::FileState { status, err, ..Default::default() };
        list.push(f);
        j.add_total(size);
    }
    j.set_files(list);
    j.persist_start();
    j.set_status(Status::Running);
    let task: FileTask = Arc::new(|jj, ff| async move { super::upload::upload_file(jj, ff).await }.boxed());
    j.run_files(task).await;
    j.finish().await;
}

// ---------- Helpers ----------

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct JobOptions {
    #[serde(default)]
    subfolder: bool,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    watch: String,
}

pub const INSPECT_TTL: Duration = Duration::from_secs(5 * 60);

pub fn passphrase_key(l: &share::Link) -> String {
    format!("{}-{}", l.kind, l.id)
}

pub fn listing_key(raw: &str, passphrase: &str) -> String {
    format!("{}\u{0}{}", raw.trim(), passphrase)
}

pub fn new_id() -> String {
    use rand::RngExt;
    let mut b = [0u8; 6];
    rand::rng().fill(&mut b[..]);
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

pub fn kind_tag(kind: &str) -> &'static str {
    if kind == "upload" { "UPLOAD  " } else { "DOWNLOAD" }
}

pub fn summarize_paths(p: &[String]) -> String {
    if p.len() == 1 {
        return base_name(&p[0]).to_string();
    }
    format!("{} + {} more", base_name(&p[0]), p.len() - 1)
}

/// Filters macOS/Windows metadata files and our own temp files.
pub fn skip_name(name: &str) -> bool {
    name == ".DS_Store"
        || name.starts_with("._")
        || name == "Thumbs.db"
        || name == "desktop.ini"
        || name == ".localized"
        || folderlog::is_folder_log(name)
        || name.ends_with(".fiopart")
        || name.ends_with(".fiostate")
        || name.ends_with(".fiostate.tmp")
        || name == ".Spotlight-V100"
        || name == ".Trashes"
        || name == ".fseventsd"
        || name == ".TemporaryItems"
}

pub fn base_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

pub fn parent_dir(rel: &str) -> String {
    match rel.rfind('/') {
        Some(0) => String::new(),
        Some(i) => rel[..i].to_string(),
        None => String::new(),
    }
}

/// Every directory (and ancestor) needed for `rels`, parents first.
pub fn dirs_for(rels: &[String]) -> Vec<String> {
    let mut set = std::collections::HashSet::new();
    for r in rels {
        let mut d = parent_dir(r);
        while !d.is_empty() {
            set.insert(d.clone());
            d = parent_dir(&d);
        }
    }
    let mut dirs: Vec<String> = set.into_iter().collect();
    dirs.sort_by(|a, b| a.matches('/').count().cmp(&b.matches('/').count()).then_with(|| a.cmp(b)));
    dirs
}

pub fn scan_paths(paths: &[String]) -> Result<Vec<Scanned>, String> {
    let mut out = Vec::new();
    for p in paths {
        let path = Path::new(p);
        let md = std::fs::metadata(path).map_err(|e| format!("{}: {}", p, e))?;
        if !md.is_dir() {
            let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if md.is_file() && !skip_name(&name) {
                out.push(Scanned { local: path.to_string_lossy().into_owned(), rel: name, size: md.len() as i64 });
            }
            continue;
        }
        let parent = path.parent().map(|x| x.to_path_buf()).unwrap_or_else(|| PathBuf::from("/"));
        walk(path, &parent, &mut out)?;
    }
    Ok(out)
}

fn walk(dir: &Path, parent: &Path, out: &mut Vec<Scanned>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    walk_inner(dir, parent, out, &mut seen)
}

/// One directory level of the scan.
///
/// Symbolic links are not followed: a link that points at one of its own
/// parents used to recurse until the stack ran out, and an upload that walks a
/// tree twice sends the same bytes twice. `seen` holds file identities as a
/// second guard, so a tree that reaches the same directory by two paths is
/// walked once: `(device, inode)` on unix, `(volume serial, file index)` on
/// Windows. See `fileio::FileId` for what the identity means and for the weaker
/// answer that stands in when the file system will not give one.
fn walk_inner(
    dir: &Path,
    parent: &Path,
    out: &mut Vec<Scanned>,
    seen: &mut std::collections::HashSet<crate::fileio::FileId>,
) -> Result<(), String> {
    // A folder the walk cannot read is the caller's error to report, and
    // `file_id` answers a weaker fallback for it rather than failing, so ask
    // here first.
    if let Err(e) = std::fs::metadata(dir) {
        return Err(format!("{}: {}", dir.display(), e));
    }
    if !seen.insert(crate::fileio::file_id(dir)) {
        return Ok(()); // this directory was already walked by another path
    }
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if skip_name(&name) {
            continue;
        }
        let fp = e.path();
        // symlink_metadata: a link is reported as a link, not as what it points at.
        let md = match std::fs::symlink_metadata(&fp) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.file_type().is_symlink() {
            continue;
        }
        if md.is_dir() {
            walk_inner(&fp, parent, out, seen)?;
        } else if md.is_file() {
            let rel = fp.strip_prefix(parent).unwrap_or(&fp).to_string_lossy().replace('\\', "/");
            out.push(Scanned { local: fp.to_string_lossy().into_owned(), rel, size: md.len() as i64 });
        }
    }
    Ok(())
}

/// The relative path of the single folder being uploaded, if the upload is
/// exactly one folder.
pub fn top_folder(paths: &[String], items: &[Scanned]) -> String {
    if paths.len() != 1 || items.is_empty() {
        return String::new();
    }
    let p = Path::new(&paths[0]);
    match std::fs::metadata(p) {
        Ok(md) if md.is_dir() => p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn ms_from_system(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

// unix only: the one test here builds a symlink cycle, and `std::fs::symlink`
// needs a privilege on Windows. The guard it checks — a symlink is not
// descended into — is the same code on both platforms.
#[cfg(all(test, unix))]
mod scan_tests {
    use super::*;

    /// A link that points back at its own parent used to recurse until the
    /// stack ran out: following it means walking the tree for ever.
    #[test]
    fn scan_paths_does_not_follow_a_directory_symlink_cycle() {
        let dir = std::env::temp_dir().join(format!("fio-scan-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inner")).expect("tree");
        std::fs::write(dir.join("a.mov"), b"a").expect("file");
        std::fs::write(dir.join("inner/b.mov"), b"bb").expect("file");
        std::os::unix::fs::symlink(&dir, dir.join("inner/loop")).expect("symlink");
        std::os::unix::fs::symlink(dir.join("a.mov"), dir.join("inner/link.mov")).expect("symlink");
        let root = dir.to_string_lossy().into_owned();
        let found = scan_paths(&[root]).expect("scan");
        // A dropped folder keeps its own name in the relative path, so the
        // files land in a folder of that name on the far side.
        let top = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut names: Vec<String> = found.iter().map(|s| s.rel.clone()).collect();
        names.sort();
        assert_eq!(names, vec![format!("{}/a.mov", top), format!("{}/inner/b.mov", top)],
                   "each real file once, no link followed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

pub fn snapshot(m: &Manager) -> Snapshot {
    m.snapshot()
}
