//! Watch folders.
//!
//! Watch folders pair a local folder with a Frame.io folder, keeping the
//! subfolder tree. Upload watches send files that appear locally once they've
//! stopped changing; download watches fetch files that appear on Frame.io once
//! their upload there has finished; mirror watches do both. Each side's records
//! say what the watch already moved, so a file it downloaded is never uploaded
//! back, and the other way round. A local file that changes after its record
//! (a re-render) is a new version: it uploads again, and with OnChangeOverwrite
//! its earlier copy there, if FioTransfer uploaded it, is deleted once Frame.io
//! confirms the new one. Nothing else is deleted on either side.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, UNIX_EPOCH};

use futures::future::BoxFuture;
use futures::FutureExt;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use super::complete::{media_state, MediaFormat};
use super::manager::{base_name, dirs_for, new_id, parent_dir, skip_name, FileTask};
use super::{now_ms, File, Job, Manager, Status};
use crate::api::{ApiErr, Asset, Client};
use crate::history;
use crate::history::watch::{
    valid_direction, valid_on_change, Watch, WatchFile, WatchRemote, WatchStats, DIR_DOWNLOAD, DIR_MIRROR,
    ON_CHANGE_OVERWRITE, WATCH_CANCELED, WATCH_DOWNLOADED, WATCH_FAILED, WATCH_IGNORED, WATCH_UPLOADED, WATCH_WAITING,
};
use crate::share;

// ---------- Timing ----------

pub const WATCH_INTERVAL: Duration = Duration::from_secs(5); // how often folders are checked
pub const WATCH_SETTLE: Duration = Duration::from_secs(10); // a file must be unchanged this long
pub const WATCH_WALK_EVERY: Duration = Duration::from_secs(30); // full walk even if no folder changed
pub const WATCH_LIST_EVERY: u32 = 6; // Frame.io is listed every 6th check (more rarely for big trees)
pub const WATCH_MAX_RETRIES: i64 = 3;
pub const WATCH_BATCH_MAX: usize = 500;

/// On a network share, a file mediaState can't check must stay unchanged this
/// many times the settle time: the share's view of a file another machine is
/// writing can stand still for longer than the settle time.
pub const NET_SETTLE_FACTOR: u32 = 6;

/// watch_backoff is how long a watch waits after its n-th batch in a row failed
/// before uploading anything (Frame.io unreachable, destination gone): 30s,
/// doubling up to 15 minutes, instead of a new failing batch every few seconds.
fn watch_backoff(n: u32) -> Duration {
    let shift = n.saturating_sub(1).min(5);
    (Duration::from_secs(30) * (1u32 << shift)).min(Duration::from_secs(15 * 60))
}

pub const ERR_FOLDER_UNAVAILABLE: &str = "folder unavailable";
const ERR_NOT_SIGNED_IN: &str = "not signed in";

/// A file as a scan sees it: waiting for its batch.
#[derive(Debug, Clone)]
struct Item {
    local: String,
    rel: String,
    size: i64,
    mtime: i64,
}

#[derive(Debug, Clone, Copy)]
struct Obs {
    size: i64,
    mtime: i64,
    since: Instant, // when this size/mtime was first seen
}

/// A file in a watch's Frame.io folder (a version stack counts as its current
/// version).
#[derive(Debug, Clone)]
pub struct RemoteFile {
    pub id: String,
    pub rel: String,
    pub size: i64,
    pub ready: bool, // its upload to Frame.io has finished
}

#[derive(Debug, Clone)]
pub struct RemoteListing {
    pub folder: String,
    pub files: Vec<RemoteFile>,
    /// sameKey of every file, to spot local files Frame.io already has.
    pub same: HashSet<String>,
}

fn same_key(rel: &str, size: i64) -> String {
    format!("{}\u{0}{}", rel.to_lowercase(), size)
}

/// The state one watch keeps between checks. Everything the tick loop, the
/// scans, the listings and the batches share lives here, behind one lock.
struct WatchState {
    inner: Mutex<Ws>,
    /// One scan at a time (Go's scanMu).
    scanning: AtomicBool,
}

struct Ws {
    w: Watch,
    seen: HashMap<String, Obs>, // files not uploaded yet, by relative path
    job: Option<Arc<Job>>,      // the latest batch
    busy: bool,                 // job hasn't fully finished (including its onFinish)
    err: String,
    fails: u32,              // batches in a row that failed before uploading
    retry_at: Option<Instant>, // no new batch before this (after such failures)
    pending: usize,          // local files waiting to settle
    link: String,            // Frame.io page of the destination folder

    // Frame.io side (download and mirror watches). Listings run on their own
    // task; scans use the latest one.
    remote: Option<Arc<RemoteListing>>, // None until a listing of the current folder succeeds
    listing: bool,
    next_list: Option<Instant>,
    list_fails: u32,
    list_err: String,
    waiting: usize, // Frame.io files still uploading there, or not served yet

    // While update() rewrites a watch's records, nothing scans or lists it;
    // cfg counts edits, so a listing that was running during one is dropped.
    editing: bool,
    cfg: u64,

    // A scan walks the tree only when a folder's mtime changed (an entry was
    // added, removed or renamed) or WATCH_WALK_EVERY passed; otherwise it
    // re-checks just the files in seen. The caches are replaced, never changed
    // in place, and gen counts invalidations.
    known: Option<HashMap<String, WatchFile>>, // watch_files rows; None = reload
    done: Option<HashMap<String, WatchRemote>>, // watch_remote rows; None = reload
    dirs: Option<HashMap<String, i64>>,        // folder → mtime at the last walk; None = walk
    last_walk: Option<Instant>,
    gen: u64,
}

impl Ws {
    /// invalidate drops the cached file records after they changed, and with
    /// rewalk also forces a full walk (for files the scan had stopped tracking).
    fn invalidate(&mut self, rewalk: bool) {
        self.known = None;
        self.done = None;
        if rewalk {
            self.dirs = None;
            self.fails = 0;
            self.retry_at = None; // the user changed something: try again now
        }
        self.gen += 1;
    }

    /// forget_listing makes the watch list Frame.io again before downloading.
    fn forget_listing(&mut self) {
        self.remote = None;
        self.next_list = None;
        self.list_fails = 0;
        self.list_err.clear();
        self.waiting = 0;
    }
}

impl WatchState {
    fn new(w: Watch) -> WatchState {
        WatchState {
            inner: Mutex::new(Ws {
                w,
                seen: HashMap::new(),
                job: None,
                busy: false,
                err: String::new(),
                fails: 0,
                retry_at: None,
                pending: 0,
                link: String::new(),
                remote: None,
                listing: false,
                next_list: None,
                list_fails: 0,
                list_err: String::new(),
                waiting: 0,
                editing: false,
                cfg: 0,
                known: None,
                done: None,
                dirs: None,
                last_walk: None,
                gen: 0,
            }),
            scanning: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Ws> {
        self.inner.lock().unwrap()
    }
}

pub struct Watcher {
    m: Arc<Manager>,
    mu: Mutex<HashMap<String, Arc<WatchState>>>,
    interval: Duration,
    settle: Duration,
    poll: Duration, // Frame.io listings at most this often

    /// Whether a watched folder is on a network share.
    on_network: fn(&str) -> bool,

    /// Canceled by close(), for listings and scans in flight.
    cx: CancellationToken,
    /// This watcher, so any method can spawn a task that needs the whole thing.
    me: Mutex<Weak<Watcher>>,
}

impl Watcher {
    pub fn new(m: Arc<Manager>) -> Arc<Watcher> {
        let w = Arc::new(Watcher {
            m: m.clone(),
            mu: Mutex::new(HashMap::new()),
            interval: WATCH_INTERVAL,
            settle: WATCH_SETTLE,
            poll: WATCH_INTERVAL * WATCH_LIST_EVERY,
            on_network: super::netvol::is_network_volume,
            cx: CancellationToken::new(),
            me: Mutex::new(Weak::new()),
        });
        *w.me.lock().unwrap() = Arc::downgrade(&w);
        if let Ok(list) = m.hist.watches() {
            let mut ws = w.mu.lock().unwrap();
            for x in list {
                ws.insert(x.id.clone(), Arc::new(WatchState::new(x)));
            }
        }
        // Retry hands a batch back to its watch (see Manager::retry). The watch
        // is held weakly: the app owns it, and a Retry after it closed says so.
        let weak: Weak<Watcher> = Arc::downgrade(&w);
        let cb: Arc<dyn Fn(&str, &str, Vec<history::File>) -> Result<(), String> + Send + Sync> =
            Arc::new(move |id, local, files| match weak.upgrade() {
                Some(w) => w.retry_batch(id, local, &files),
                None => Err("watch folders aren't running".to_string()),
            });
        *m.retry_watch.lock().unwrap() = Some(cb);
        let ww = w.clone();
        tokio::spawn(async move { ww.run().await });
        w
    }

    fn arc(&self) -> Option<Arc<Watcher>> {
        self.me.lock().unwrap().upgrade()
    }

    fn weak(&self) -> Weak<Watcher> {
        self.me.lock().unwrap().clone()
    }

    /// Runs a scan on its own task, as Go's `go w.scan(s)`.
    fn spawn_scan(&self, s: &Arc<WatchState>) {
        if let Some(w) = self.arc() {
            let s = s.clone();
            tokio::spawn(async move { w.scan(&s).await });
        }
    }

    pub fn close(&self) {
        self.cx.cancel();
    }

    async fn run(self: Arc<Self>) {
        let mut t = tokio::time::interval(self.interval);
        t.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = self.cx.cancelled() => return,
                _ = t.tick() => self.tick(),
            }
        }
    }

    fn tick(&self) {
        let states: Vec<Arc<WatchState>> = {
            let ws = self.mu.lock().unwrap();
            ws.values().cloned().collect()
        };
        for s in states {
            self.maybe_list(&s);
            self.spawn_scan(&s);
        }
    }

    /// add creates a watch. Unless include_existing, files already present (in
    /// the local folder for uploads, on Frame.io for downloads) are left alone.
    pub fn add(
        &self,
        local: &str,
        acct: &str,
        folder: &str,
        label: &str,
        dir: &str,
        on_change: &str,
        include_existing: bool,
    ) -> Result<(), String> {
        let local = clean(local);
        match std::fs::metadata(&local) {
            Ok(md) if md.is_dir() => {}
            _ => return Err(format!("not a folder: {}", local)),
        }
        if acct.is_empty() || folder.is_empty() {
            return Err("no Frame.io folder".to_string());
        }
        if !valid_direction(dir) {
            return Err(format!("unknown direction {:?}", dir));
        }
        if !valid_on_change(on_change) {
            return Err(format!("unknown rule for changed files {:?}", on_change));
        }
        {
            let ws = self.mu.lock().unwrap();
            if ws.values().any(|s| s.lock().w.local_path == local) {
                return Err("already watched".to_string());
            }
        }
        let mut x = Watch {
            id: new_id(),
            local_path: local.clone(),
            account_id: acct.to_string(),
            folder_id: folder.to_string(),
            dest_label: label.to_string(),
            enabled: true,
            created_at: now_ms(),
            direction: dir.to_string(),
            on_change: on_change.to_string(),
            ignore_remote: false,
            // A watch the user makes belongs to the user; the Portal reconciler
            // stamps its own watches with `set_origin`.
            origin: String::new(),
        };
        x.ignore_remote = x.downloads() && !include_existing;
        if let Err(e) = self.m.hist.add_watch(&x) {
            return Err(e);
        }
        if x.uploads() && !include_existing {
            let _ = self.m.hist.baseline_watch_files(&x.id, &existing_files(&local));
        }
        let s = Arc::new(WatchState::new(x.clone()));
        {
            let mut ws = self.mu.lock().unwrap();
            ws.insert(x.id.clone(), s.clone());
        }
        self.m.hist.log(&format!("WATCH    added    [{}] {} {} {}", x.id, local, arrow(dir), label));
        self.maybe_list(&s);
        self.spawn_scan(&s);
        Ok(())
    }

    /// Records how a watch came to exist: `""` for one the user made,
    /// `portal:<link id>` for one the Portal reconciler owns.
    ///
    /// Only the reconciler calls this. A watch with a non-empty origin is one it
    /// may later update or remove; a watch without one is never its business.
    pub fn set_origin(&self, id: &str, origin: &str) {
        let w = {
            let ws = self.mu.lock().unwrap();
            let Some(s) = ws.get(id) else { return };
            let mut g = s.lock();
            g.w.origin = origin.to_string();
            g.w.clone()
        };
        let _ = self.m.hist.update_watch(&w);
    }

    /// set_enabled pauses or resumes a watch.
    pub fn set_enabled(&self, id: &str, on: bool) {
        let ws = self.mu.lock().unwrap();
        if let Some(s) = ws.get(id) {
            let mut g = s.lock();
            g.w.enabled = on;
            self.m.hist.set_watch_enabled(id, on);
            g.seen.clear();
            g.pending = 0;
            g.dirs = None;
            g.next_list = None;
            g.list_fails = 0;
            g.waiting = 0;
        }
    }

    pub fn remove(&self, id: &str) {
        let s = {
            let mut ws = self.mu.lock().unwrap();
            ws.remove(id)
        };
        if let Some(s) = &s {
            let job = s.lock().job.clone();
            if let Some(j) = job {
                j.cx.cancel();
            }
        }
        self.m.hist.delete_watch(id);
    }

    /// update changes a watch's folders, direction and rule for changed files.
    /// A side the watch takes files from starts over when it's new or points
    /// somewhere else (existing files there are skipped unless include_existing);
    /// for a mirror both sides do when either end changes. A new destination
    /// applies to files from now on, and so does a new rule.
    pub fn update(
        &self,
        id: &str,
        local: &str,
        acct: &str,
        folder: &str,
        label: &str,
        dir: &str,
        on_change: &str,
        include_existing: bool,
    ) -> Result<(), String> {
        let local = clean(local);
        match std::fs::metadata(&local) {
            Ok(md) if md.is_dir() => {}
            _ => return Err(format!("not a folder: {}", local)),
        }
        if acct.is_empty() || folder.is_empty() {
            return Err("no Frame.io folder".to_string());
        }
        if !valid_direction(dir) {
            return Err(format!("unknown direction {:?}", dir));
        }
        if !valid_on_change(on_change) {
            return Err(format!("unknown rule for changed files {:?}", on_change));
        }
        let (s, x, flags) = {
            let ws = self.mu.lock().unwrap();
            let Some(s) = ws.get(id).cloned() else {
                return Err("watch not found".to_string());
            };
            for o in ws.values() {
                if !Arc::ptr_eq(o, &s) && o.lock().w.local_path == local {
                    return Err("already watched".to_string());
                }
            }
            let mut g = s.lock();
            let old = g.w.clone();
            let mut x = old.clone();
            x.local_path = local.clone();
            x.account_id = acct.to_string();
            x.folder_id = folder.to_string();
            x.dest_label = label.to_string();
            x.direction = dir.to_string();
            x.on_change = on_change.to_string();
            let folder_changed = old.local_path != local;
            let dest_changed = old.folder_id != folder || old.account_id != acct;
            let mirror = dir == DIR_MIRROR;
            let fresh_local = x.uploads() && (folder_changed || (mirror && dest_changed) || !old.uploads());
            let fresh_remote = x.downloads() && (dest_changed || (mirror && folder_changed) || !old.downloads());
            let reset_local = folder_changed || (mirror && dest_changed);
            let reset_remote = dest_changed || (mirror && folder_changed);
            if fresh_remote {
                x.ignore_remote = !include_existing;
            }
            if g.busy && (folder_changed || dest_changed || dir != old.direction) {
                if let Some(j) = &g.job {
                    j.cx.cancel(); // not a user cancel: its files stay eligible under the new settings
                }
            }
            g.w = x.clone();
            g.editing = true;
            g.cfg += 1;
            g.seen.clear();
            g.pending = 0;
            g.err.clear();
            g.invalidate(true);
            if dest_changed {
                g.link.clear();
            }
            if dest_changed || fresh_remote || !x.downloads() {
                g.forget_listing();
            }
            let flags = (folder_changed, dest_changed, fresh_local, reset_local, reset_remote);
            drop(g);
            (s, x, flags)
        };
        let (folder_changed, dest_changed, fresh_local, reset_local, reset_remote) = flags;
        let err = self.m.hist.update_watch(&x).err();
        if err.is_none() {
            if dest_changed || folder_changed {
                self.m.hist.clear_watch_dirs(id);
            }
            if reset_local {
                self.m.hist.reset_watch_files(id);
            }
            if reset_remote {
                self.m.hist.reset_watch_remotes(id);
            }
            if fresh_local && !include_existing {
                let _ = self.m.hist.baseline_watch_files(id, &existing_files(&local));
            }
        }
        {
            let mut g = s.lock();
            g.editing = false;
            g.invalidate(true);
        }
        if let Some(e) = err {
            return Err(e);
        }
        self.m.hist.log(&format!("WATCH    edited   [{}] {} {} {}", id, local, arrow(dir), label));
        self.maybe_list(&s);
        Ok(())
    }

    /// requeue_canceled lets files from canceled batches transfer again.
    pub fn requeue_canceled(&self, id: &str) {
        self.m.hist.requeue_canceled(id);
        let ws = self.mu.lock().unwrap();
        if let Some(s) = ws.get(id) {
            s.lock().invalidate(true);
        }
    }

    /// retry_batch makes a batch's unfinished files eligible again in their
    /// watch (found by ID, or by folder for batches recorded without one).
    /// Downloaded files are known by their Frame.io ID, uploaded ones by their
    /// path.
    fn retry_batch(&self, id: &str, local_path: &str, files: &[history::File]) -> Result<(), String> {
        let s = {
            let ws = self.mu.lock().unwrap();
            let mut found = ws.get(id).cloned();
            if found.is_none() {
                for o in ws.values() {
                    if o.lock().w.local_path == local_path {
                        found = Some(o.clone());
                        break;
                    }
                }
            }
            match found {
                Some(s) => s,
                None => return Err("this watch folder no longer exists".to_string()),
            }
        };
        let sid = s.lock().w.id.clone();
        let mut rels: Vec<String> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for f in files {
            if !f.asset_id.is_empty() {
                ids.push(f.asset_id.clone());
            } else {
                rels.push(f.rel_path.clone());
            }
        }
        let mut errs: Vec<String> = Vec::new();
        if let Err(e) = self.m.hist.requeue_watch_files(&sid, &rels) {
            errs.push(e);
        }
        if let Err(e) = self.m.hist.forget_watch_remotes(&sid, &ids) {
            errs.push(e);
        }
        s.lock().invalidate(true);
        self.spawn_scan(&s);
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("\n"))
        }
    }

    /// record saves local file states for a batch, unless its watch was removed
    /// or pointed at another folder since the batch started (the rows would
    /// describe files that are no longer watched). Holding the watch map lock
    /// keeps this in step with update().
    fn record(&self, s: &Arc<WatchState>, x: &Watch, files: &[WatchFile]) {
        let ws = self.mu.lock().unwrap();
        let current = ws.get(&x.id).map(|o| Arc::ptr_eq(o, s)).unwrap_or(false);
        let reset = {
            let g = s.lock();
            !current || local_reset(x, &g.w)
        };
        if reset {
            return;
        }
        if let Err(e) = self.m.hist.put_watch_files(&x.id, files) {
            // Go drops this error; the app log is the only place it can show.
            self.m.hist.log(&format!("WATCH    note     [{}] could not record files: {}", x.id, e));
        }
        s.lock().invalidate(false);
    }

    /// record_remote saves Frame.io file states for a batch, and the local
    /// files a download wrote, unless the watch was removed or pointed
    /// elsewhere since.
    fn record_remote(&self, s: &Arc<WatchState>, x: &Watch, rows: &[WatchRemote], local: &[WatchFile]) {
        let ws = self.mu.lock().unwrap();
        let current = ws.get(&x.id).map(|o| Arc::ptr_eq(o, s)).unwrap_or(false);
        let (rreset, lreset) = {
            let g = s.lock();
            (!current || remote_reset(x, &g.w), !current || local_reset(x, &g.w))
        };
        if rreset {
            return;
        }
        if let Err(e) = self.m.hist.put_watch_remotes(&x.id, rows) {
            self.m.hist.log(&format!("WATCH    note     [{}] could not record files: {}", x.id, e));
        }
        if !local.is_empty() && !lreset {
            if let Err(e) = self.m.hist.put_watch_files(&x.id, local) {
                self.m.hist.log(&format!("WATCH    note     [{}] could not record files: {}", x.id, e));
            }
        }
        s.lock().invalidate(false);
    }
}

/// local_reset and remote_reset tell whether an edit from old to cur started a
/// side's records over (see update), so a batch that began before it mustn't
/// write any.
fn local_reset(old: &Watch, cur: &Watch) -> bool {
    let dest_changed = old.folder_id != cur.folder_id || old.account_id != cur.account_id;
    old.local_path != cur.local_path || (cur.direction == DIR_MIRROR && dest_changed)
}

fn remote_reset(old: &Watch, cur: &Watch) -> bool {
    let dest_changed = old.folder_id != cur.folder_id || old.account_id != cur.account_id;
    dest_changed || (cur.direction == DIR_MIRROR && old.local_path != cur.local_path)
}

/// same_watch is Go's `s.w == x`: every field, so an edit made while a batch
/// runs is spotted.
fn same_watch(a: &Watch, b: &Watch) -> bool {
    a.id == b.id
        && a.local_path == b.local_path
        && a.account_id == b.account_id
        && a.folder_id == b.folder_id
        && a.dest_label == b.dest_label
        && a.enabled == b.enabled
        && a.created_at == b.created_at
        && a.direction == b.direction
        && a.ignore_remote == b.ignore_remote
        && a.on_change == b.on_change
}

fn arrow(dir: &str) -> &'static str {
    match dir {
        DIR_DOWNLOAD => "<-",
        DIR_MIRROR => "<->",
        _ => "->",
    }
}

/// existingFiles lists what's in a folder now, to be ignored by its watch.
fn existing_files(local: &str) -> Vec<WatchFile> {
    let mut files: Vec<(String, i64, i64)> = Vec::new();
    let _ = walk_watch(local, &mut files, &mut None);
    files
        .into_iter()
        .filter(|(_, size, _)| *size > 0)
        .map(|(rel, size, mtime)| wf(&rel, size, mtime, WATCH_IGNORED))
        .collect()
}

/// A watch_files row, the way the Go struct literal is spelled there.
fn wf(rel: &str, size: i64, mtime: i64, state: &str) -> WatchFile {
    WatchFile {
        rel_path: rel.to_string(),
        size,
        mtime,
        state: state.to_string(),
        attempts: 0,
        error: String::new(),
        remote_id: String::new(),
        updated_at: 0,
    }
}

/// A watch_remote row.
fn wr(remote_id: &str, rel: &str, size: i64, state: &str) -> WatchRemote {
    WatchRemote {
        remote_id: remote_id.to_string(),
        rel_path: rel.to_string(),
        size,
        state: state.to_string(),
        attempts: 0,
        error: String::new(),
        updated_at: 0,
    }
}

// ---------- Views ----------

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct WatchView {
    pub id: String,
    pub local_path: String,
    pub name: String,
    pub dest_label: String,
    pub direction: String,
    pub on_change: String,
    pub enabled: bool,
    /// watching | paused | uploading | downloading | settling | waiting | error
    pub state: String,
    pub detail: String,
    pub uploaded: i64,
    pub downloaded: i64,
    pub failed: i64,
    pub canceled: i64,
    pub pending: i64,
    pub waiting: i64,
    pub account_id: String,
    pub folder_id: String,
    pub link: String,
    pub last_transfer: i64,
    /// How this watch came to exist, from the record: empty for one the user
    /// made, `portal:<link id>` for one the Portal reconciler owns. The Watch
    /// Folders screen draws its "Portal" marker from it; nothing else here
    /// decides ownership, and the reconciler is the only writer.
    pub origin: String,
}

impl Watcher {
    pub fn views(&self) -> Vec<WatchView> {
        let mut states: Vec<Arc<WatchState>> = {
            let ws = self.mu.lock().unwrap();
            ws.values().cloned().collect()
        };
        states.sort_by_key(|s| s.lock().w.created_at);
        let mut out = Vec::with_capacity(states.len());
        for s in states {
            let sid = s.lock().w.id.clone();
            let st: WatchStats = self.m.hist.watch_stats(&sid);
            let g = s.lock();
            let mut v = WatchView {
                id: g.w.id.clone(),
                local_path: g.w.local_path.clone(),
                name: base_name_path(&g.w.local_path),
                dest_label: g.w.dest_label.clone(),
                direction: g.w.direction.clone(),
                on_change: g.w.on_change.clone(),
                enabled: g.w.enabled,
                uploaded: st.uploaded,
                downloaded: st.downloaded,
                failed: st.failed,
                canceled: st.canceled,
                last_transfer: st.last_transfer,
                pending: g.pending as i64,
                waiting: g.waiting as i64,
                account_id: g.w.account_id.clone(),
                folder_id: g.w.folder_id.clone(),
                link: g.link.clone(),
                origin: g.w.origin.clone(),
                ..Default::default()
            };
            if !g.w.enabled {
                v.state = "paused".to_string();
            } else if g.busy {
                let job = g.job.clone();
                v.state = "uploading".to_string();
                if let Some(j) = &job {
                    if j.kind == "download" {
                        v.state = "downloading".to_string();
                    }
                    v.detail = plural(j.files().len(), "file");
                }
            } else if !g.err.is_empty() {
                v.state = "error".to_string();
                v.detail = g.err.clone();
            } else if !g.list_err.is_empty() {
                v.state = "error".to_string();
                v.detail = g.list_err.clone();
            } else if g.pending > 0 {
                v.state = "settling".to_string();
                v.detail = plural(g.pending, "file");
            } else if g.waiting > 0 {
                v.state = "waiting".to_string();
                v.detail = plural(g.waiting, "file");
            } else {
                v.state = "watching".to_string();
            }
            out.push(v);
        }
        out
    }
}

fn plural(n: usize, w: &str) -> String {
    if n == 1 {
        return format!("1 {}", w);
    }
    format!("{} {}s", n, w)
}

// ---------- Scanning ----------

/// temp_name filters files that are still being written by common tools.
fn temp_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("~$")
        || n.starts_with('.')
        || n.ends_with(".part")
        || n.ends_with(".partial")
        || n.ends_with(".crdownload")
        || n.ends_with(".download")
        || n.ends_with(".tmp")
        || n.ends_with(".temp")
}

/// walk_watch calls fn for every file a watch would consider (empty ones too:
/// they may be about to fill), and dir_fn (if set) with every folder it
/// descends into and that folder's mtime.
fn walk_watch(
    root: &str,
    files: &mut Vec<(String, i64, i64)>,
    dirs: &mut Option<HashMap<String, i64>>,
) -> Result<(), String> {
    walk_dir(Path::new(root), root, true, files, dirs)
}

fn walk_dir(
    dir: &Path,
    root: &str,
    is_root: bool,
    files: &mut Vec<(String, i64, i64)>,
    dirs: &mut Option<HashMap<String, i64>>,
) -> Result<(), String> {
    if is_root {
        let md = match std::fs::metadata(dir) {
            Ok(m) => m,
            Err(_) => return Err(ERR_FOLDER_UNAVAILABLE.to_string()),
        };
        if let Some(d) = dirs.as_mut() {
            d.insert(dir.to_string_lossy().into_owned(), mtime_nanos(&md));
        }
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Ok(()), // unreadable subfolder: skip it
    };
    let mut entries: Vec<std::fs::DirEntry> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if skip_name(&name) || temp_name(&name) {
            continue; // a skipped folder is not descended into either
        }
        let p = e.path();
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            if let Some(d) = dirs.as_mut() {
                d.insert(p.to_string_lossy().into_owned(), mtime_nanos(&md));
            }
            walk_dir(&p, root, false, files, dirs)?;
            continue;
        }
        if !md.is_file() {
            continue;
        }
        let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        files.push((rel, md.len() as i64, mtime_nanos(&md)));
    }
    Ok(())
}

fn mtime_nanos(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// dirs_changed reports whether a folder seen at the last walk changed or went away.
fn dirs_changed(dirs: &HashMap<String, i64>) -> bool {
    for (d, mt) in dirs {
        match std::fs::metadata(d) {
            Ok(md) => {
                if mtime_nanos(&md) != *mt {
                    return true;
                }
            }
            Err(_) => return true,
        }
    }
    false
}

/// local_root_ok checks that downloads can go into a watch's folder. A share
/// that isn't mounted leaves nothing at /Volumes/<name>, or a stale empty
/// folder that would put the files on the startup disk instead: both are
/// refused.
fn local_root_ok(root: &str) -> Result<(), String> {
    let bad = || Err(ERR_FOLDER_UNAVAILABLE.to_string());
    match std::fs::metadata(root) {
        Ok(md) if md.is_dir() => {}
        _ => return bad(),
    }
    if let Some(rest) = root.strip_prefix("/Volumes/") {
        let vol = rest.split('/').next().unwrap_or("");
        let share = format!("/Volumes/{}", vol);
        // A share that isn't mounted leaves nothing at /Volumes/<name>, or a stale
        // empty folder on the startup disk, which is the volume /Volumes itself
        // is on. Either way it is refused.
        if std::fs::metadata(&share).is_err() || std::fs::metadata("/Volumes").is_err() {
            return bad();
        }
        if same_device(&share, "/Volumes") {
            return bad();
        }
    }
    Ok(())
}

/// Whether two paths sit on the same volume: the check that tells a mounted
/// share from the empty folder the startup disk shows in its place.
///
/// The volume is half of a file's identity, so this asks `fileio::file_id` for
/// the first half on either platform — `st_dev` on unix, the volume's serial
/// number on Windows. Where the identity could not be read for one of the two,
/// the two identities differ and the answer is "not the same volume": this
/// guard only ever refuses a download, so the doubtful answer is the one that
/// lets it through.
fn same_device(a: &str, b: &str) -> bool {
    match (crate::fileio::file_id(std::path::Path::new(a)), crate::fileio::file_id(std::path::Path::new(b))) {
        (crate::fileio::FileId::Fs { a: da, .. }, crate::fileio::FileId::Fs { a: db, .. }) => da == db,
        _ => false,
    }
}

/// written reports whether a file that has held still for steady is done being
/// written. A QuickTime/MXF file must be finished; another file on a network
/// share must hold still NET_SETTLE_FACTOR times the settle time.
fn written(path: &str, size: i64, steady: Duration, net_vol: bool, settle: Duration) -> bool {
    match media_state(path, size) {
        MediaFormat::Finished => true,
        MediaFormat::Unfinished => false,
        MediaFormat::Unknown => !net_vol || steady >= settle * NET_SETTLE_FACTOR,
    }
}

#[derive(Default)]
struct ScanOut {
    seen: HashMap<String, Obs>,
    settled: Vec<Item>,
    on_frameio: Vec<WatchFile>, // mirror: already there with the same name and size
    pending: usize,
    dirs: Option<HashMap<String, i64>>,
    err: Option<String>,
}

/// The blocking half of a scan: the walk (or the re-check of the files already
/// seen) and the settle/media checks, all of which touch the file system.
#[allow(clippy::too_many_arguments)]
fn scan_upload_side(
    root: &str,
    prev: &HashMap<String, Obs>,
    known: &HashMap<String, WatchFile>,
    walk: bool,
    settle: Duration,
    net_vol: bool,
    mirror: bool,
    same: Option<&HashSet<String>>,
) -> ScanOut {
    let now = Instant::now();
    let mut out = ScanOut::default();
    let mut files: Vec<(String, i64, i64)> = Vec::new();
    let mut dirs: Option<HashMap<String, i64>> = None;
    if walk {
        dirs = Some(HashMap::new());
        if let Err(e) = walk_watch(root, &mut files, &mut dirs) {
            out.err = Some(e);
            out.dirs = None;
            return out;
        }
    } else {
        for rel in prev.keys() {
            let local = join_path(root, rel);
            if let Ok(md) = std::fs::metadata(&local) {
                if md.is_file() {
                    files.push((rel.clone(), md.len() as i64, mtime_nanos(&md)));
                }
            }
        }
    }
    for (rel, size, mtime) in files {
        let had = known.get(&rel);
        if let Some(k) = had {
            match k.state.as_str() {
                WATCH_UPLOADED | WATCH_DOWNLOADED | WATCH_IGNORED | WATCH_CANCELED => {
                    if k.size == size && k.mtime == mtime {
                        continue;
                    }
                    // Changed since: a new version (see on_change).
                }
                WATCH_FAILED => {
                    if k.attempts >= WATCH_MAX_RETRIES && k.size == size && k.mtime == mtime {
                        continue; // gave up on this version of the file
                    }
                }
                _ => {}
            }
        }
        let mut o = Obs { size, mtime, since: now };
        if let Some(p) = prev.get(&rel) {
            if p.size == size && p.mtime == mtime {
                o.since = p.since;
            }
        }
        out.seen.insert(rel.clone(), o);
        if size == 0 {
            // Frame.io takes no empty files. Seen, so the next scan checks it
            // again: one caught just after it was created fills without its
            // folder's mtime moving.
            continue;
        }
        let local = join_path(root, &rel);
        let steady = now.duration_since(o.since);
        if steady < settle {
            out.pending += 1;
            continue;
        }
        if mirror && had.is_none() && same.map(|s| s.contains(&same_key(&rel, size))).unwrap_or(false) {
            // a known file's match there is its earlier version
            out.on_frameio.push(wf(&rel, size, mtime, WATCH_IGNORED));
            continue;
        }
        if !written(&local, size, steady, net_vol, settle) {
            out.pending += 1;
            continue;
        }
        out.settled.push(Item { local, rel, size, mtime });
    }
    out.dirs = dirs;
    out
}

struct ScanGuard<'a>(&'a AtomicBool);
impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Watcher {
    fn set_err(&self, s: &Arc<WatchState>, e: &str) {
        s.lock().err = e.to_string();
    }

    /// unsee forgets a file's last size and mtime, so it has to settle again.
    fn unsee(&self, s: &Arc<WatchState>, rel: &str) {
        s.lock().seen.remove(rel);
    }

    async fn scan(&self, s: &Arc<WatchState>) {
        if s.scanning.swap(true, Ordering::AcqRel) {
            return; // already scanning
        }
        let _guard = ScanGuard(&s.scanning);
        let t0 = Instant::now();
        let (x, prev_seen, mut known, mut done, dirs, gen, l, mut walk) = {
            let g = s.lock();
            if !g.w.enabled || g.busy || g.editing || g.retry_at.map(|t| t0 < t).unwrap_or(false) {
                return;
            }
            let walk = g.dirs.is_none() || g.last_walk.map(|t| t0.duration_since(t) >= WATCH_WALK_EVERY).unwrap_or(true);
            (g.w.clone(), g.seen.clone(), g.known.clone(), g.done.clone(), g.dirs.clone(), g.gen, g.remote.clone(), walk)
        };

        if known.is_none() {
            match self.m.hist.watch_files(&x.id) {
                Ok(v) => {
                    let mut g = s.lock();
                    if g.gen == gen {
                        g.known = Some(v.clone());
                    }
                    known = Some(v);
                }
                Err(e) => return self.set_err(s, &e),
            }
        }
        if x.downloads() && done.is_none() {
            match self.m.hist.watch_remotes(&x.id) {
                Ok(v) => {
                    let mut g = s.lock();
                    if g.gen == gen {
                        g.done = Some(v.clone());
                    }
                    done = Some(v);
                }
                Err(e) => return self.set_err(s, &e),
            }
        }
        let known_map = known.clone().unwrap_or_default();
        if x.downloads() {
            if let Some(l) = &l {
                if !x.ignore_remote {
                    let done_map = done.clone().unwrap_or_default();
                    if self.scan_remote(s, &x, l, &known_map, &done_map).await {
                        return;
                    }
                }
            }
        }
        if !x.uploads() {
            return;
        }
        let mirror = x.direction == DIR_MIRROR;
        if mirror && l.is_none() {
            return; // compare with what's on Frame.io before uploading anything
        }
        if !walk {
            if let Some(d) = &dirs {
                if dirs_changed(d) {
                    walk = true;
                }
            }
        }

        let now = Instant::now();
        let net_vol = (self.on_network)(&x.local_path);
        let root = x.local_path.clone();
        let settle = self.settle;
        let same = l.as_ref().map(|l| l.same.clone());
        let prev = prev_seen;
        let kn = known_map.clone();
        let work = tokio::task::spawn_blocking(move || {
            scan_upload_side(&root, &prev, &kn, walk, settle, net_vol, mirror, same.as_ref())
        })
        .await
        .unwrap_or_default();

        let ScanOut { seen, mut settled, on_frameio, pending, dirs: new_dirs, err } = work;
        settled.sort_by(|a, b| a.rel.cmp(&b.rel));
        let ready: Vec<Item> = settled.into_iter().take(WATCH_BATCH_MAX).collect();

        let current = {
            let ws = self.mu.lock().unwrap();
            let g = s.lock();
            ws.get(&x.id).map(|o| Arc::ptr_eq(o, s)).unwrap_or(false) && same_watch(&g.w, &x) && g.gen == gen
        };
        if current {
            let mut g = s.lock();
            g.seen = seen;
            g.pending = pending;
            match (&err, walk) {
                (Some(_), _) => g.dirs = None,
                (None, true) => {
                    g.dirs = new_dirs;
                    g.last_walk = Some(now);
                }
                _ => {}
            }
        }
        if !current {
            return; // paused, edited or removed while scanning: these results are stale
        }
        if err.is_some() {
            return self.set_err(s, ERR_FOLDER_UNAVAILABLE);
        }
        if !on_frameio.is_empty() {
            self.record(s, &x, &on_frameio);
        }
        if ready.is_empty() {
            return self.set_err(s, "");
        }
        if !self.m.api.auth.logged_in() {
            return self.set_err(s, ERR_NOT_SIGNED_IN);
        }
        self.set_err(s, "");
        self.start_batch(s, &x, &ready, &known_map);
    }
}

// ---------- Upload batches ----------

impl Watcher {
    fn start_batch(&self, s: &Arc<WatchState>, x: &Watch, items: &[Item], known: &HashMap<String, WatchFile>) {
        let m = self.m.clone();
        let j = m.new_job("upload");
        *j.account_id.lock().unwrap() = x.account_id.clone();
        *j.folder_id.lock().unwrap() = x.folder_id.clone();
        *j.dest.lock().unwrap() = x.dest_label.clone();
        *j.paths.lock().unwrap() = vec![x.local_path.clone()];
        *j.watch_id.lock().unwrap() = x.id.clone();
        j.watch_log.store(true, Ordering::Relaxed);
        *j.source.lock().unwrap() = x.local_path.clone();
        let n = items.len();
        *j.title.lock().unwrap() = if n == 1 {
            format!("{} · {}", base_name_path(&x.local_path), base_name(&items[0].rel))
        } else {
            format!("{} · {} files", base_name_path(&x.local_path), n)
        };

        // The file list is saved up front, so a retry from History knows the batch
        // even if it fails before uploading anything.
        let mut attempts: HashMap<String, i64> = HashMap::with_capacity(n);
        let mut mtimes: HashMap<String, i64> = HashMap::with_capacity(n);
        let mut list: Vec<Arc<File>> = Vec::with_capacity(n);
        for (i, it) in items.iter().enumerate() {
            attempts.insert(it.rel.clone(), known.get(&it.rel).map(|k| k.attempts).unwrap_or(0));
            mtimes.insert(it.rel.clone(), it.mtime);
            let f = Arc::new(File::new(i, base_name(&it.rel), &it.rel, &it.local, it.size));
            f.attach(&j);
            list.push(f);
            j.add_total(it.size);
        }
        j.set_files(list);

        let recorded: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        // rel path -> its earlier copy on Frame.io; set before any file starts
        let earlier: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let weak = self.weak();
        let mweak = Arc::downgrade(&self.m);

        // A batch the user cancels stays canceled: those files aren't picked up again.
        {
            let s = s.clone();
            let x = x.clone();
            let items = items.to_vec();
            let recorded = recorded.clone();
            let weak = weak.clone();
            let mweak = mweak.clone();
            *j.on_finish.lock().unwrap() = Some(Arc::new(move |jj: Arc<Job>| {
                if !jj.user_canceled.load(Ordering::Relaxed) {
                    return;
                }
                let mut canceled: Vec<WatchFile> = Vec::new();
                {
                    let rec = recorded.lock().unwrap();
                    for it in &items {
                        if !rec.contains(&it.rel) {
                            canceled.push(wf(&it.rel, it.size, it.mtime, WATCH_CANCELED));
                        }
                    }
                }
                if canceled.is_empty() {
                    return;
                }
                if let Some(w) = weak.upgrade() {
                    w.record(&s, &x, &canceled);
                }
                if let Some(m) = mweak.upgrade() {
                    m.hist.log(&format!(
                        "WATCH    canceled [{}] {} files won't be uploaded by {}",
                        jj.id,
                        canceled.len(),
                        x.local_path
                    ));
                }
            }));
        }
        {
            let s = s.clone();
            let x = x.clone();
            let recorded = recorded.clone();
            let mtimes = mtimes.clone();
            let attempts = attempts.clone();
            let earlier = earlier.clone();
            let weak = weak.clone();
            let mweak = mweak.clone();
            *j.on_file_done.lock().unwrap() = Some(Arc::new(move |f: Arc<File>| {
                let (st, e, writing) = {
                    let g = f.inner.lock().unwrap();
                    (g.status, g.err.clone(), g.still_writing)
                };
                let rid = f.remote_id.lock().unwrap().clone();
                let Some(w) = weak.upgrade() else { return };
                if writing {
                    // back to settling, not a failed attempt
                    w.unsee(&s, &f.rel_path);
                    return;
                }
                let mut row = wf(&f.rel_path, f.size(), mtimes.get(&f.rel_path).copied().unwrap_or(0), "");
                row.remote_id = rid;
                match st {
                    Status::Done | Status::Skipped => row.state = WATCH_UPLOADED.to_string(),
                    Status::Failed => {
                        row.state = WATCH_FAILED.to_string();
                        row.error = e.clone();
                        row.attempts = attempts.get(&f.rel_path).copied().unwrap_or(0) + 1;
                        if e.contains("Frame.io API error 404") {
                            // remote folder was deleted: rebuild the folder map
                            if let Some(m) = mweak.upgrade() {
                                m.hist.clear_watch_dirs(&x.id);
                            }
                        }
                    }
                    _ => return,
                }
                recorded.lock().unwrap().insert(f.rel_path.clone());
                let old = earlier.lock().unwrap().get(&f.rel_path).cloned().unwrap_or_default();
                if !old.is_empty() && st == Status::Done && x.downloads() {
                    // The earlier version stays listed (kept, or until it's deleted): it
                    // mustn't be downloaded over the new one.
                    let mut er = wr(&old, &f.rel_path, 0, WATCH_IGNORED);
                    er.error = "earlier version".to_string();
                    w.record_remote(&s, &x, &[er], &[row]);
                    return;
                }
                w.record(&s, &x, &[row]);
            }));
        }

        let (link, ok) = self.claim(s, x, &j);
        if !ok {
            return;
        }
        m.add(&j);
        j.persist_start();
        m.hist.log(&format!("WATCH    batch    [{}] {} — {} files -> {}", j.id, x.local_path, n, x.dest_label));

        let Some(w) = self.arc() else { return };
        let s = s.clone();
        let x = x.clone();
        let items = items.to_vec();
        let known = known.clone();
        tokio::spawn(async move { w.run_upload_batch(s, x, j, items, known, link, earlier).await });
    }

    async fn run_upload_batch(
        self: Arc<Self>,
        s: Arc<WatchState>,
        x: Watch,
        j: Arc<Job>,
        items: Vec<Item>,
        known: HashMap<String, WatchFile>,
        link: String,
        earlier: Arc<Mutex<HashMap<String, String>>>,
    ) {
        let mut setup_failed = false;
        j.set_status(Status::Resolving);
        if link.is_empty() {
            self.link_batch(&s, &x, &j).await;
        }
        let rels: Vec<String> = items.iter().map(|i| i.rel.clone()).collect();
        let ids = match watch_folders(&j.cx, &self.m.api, &self.m.hist, &x, &dirs_for(&rels)).await {
            Ok(v) => v,
            Err(e) => {
                setup_failed = !j.cx.is_cancelled();
                j.fail(&e);
                self.set_err(&s, &e);
                self.release(&s, &j, setup_failed).await;
                return;
            }
        };
        let list = j.files();
        for f in &list {
            *f.remote_parent.lock().unwrap() = ids.get(&parent_dir(&f.rel_path)).cloned().unwrap_or_default();
        }
        let overwrite = x.on_change == ON_CHANGE_OVERWRITE;
        if overwrite || x.downloads() {
            match earlier_versions(&j.cx, &self.m.api, &x.account_id, &list, &known).await {
                Ok(v) => *earlier.lock().unwrap() = v,
                Err(e) => {
                    setup_failed = !j.cx.is_cancelled();
                    j.fail(&e);
                    self.set_err(&s, &e);
                    self.release(&s, &j, setup_failed).await;
                    return;
                }
            }
        }
        if overwrite {
            // Only a copy FioTransfer uploaded is replaced: a same-named file
            // put there some other way stays next to the new one.
            let ids: Vec<String> = earlier.lock().unwrap().values().cloned().collect();
            let ours = self.m.hist.uploaded(&ids);
            for f in &list {
                let old = earlier.lock().unwrap().get(&f.rel_path).cloned().unwrap_or_default();
                if !old.is_empty() && ours.contains_key(&old) {
                    *f.replaces.lock().unwrap() = old;
                }
            }
        }
        j.set_status(Status::Running);
        let task: FileTask = Arc::new(|jj, ff| async move { super::upload::upload_file(jj, ff).await }.boxed());
        j.run_files(task).await;
        self.release(&s, &j, setup_failed).await;
    }

    /// claim makes j the watch's running batch, unless the watch was paused,
    /// edited or removed since the scan. It returns the folder link known so far.
    fn claim(&self, s: &Arc<WatchState>, x: &Watch, j: &Arc<Job>) -> (String, bool) {
        let ws = self.mu.lock().unwrap();
        let mut g = s.lock();
        let current = ws.get(&x.id).map(|o| Arc::ptr_eq(o, s)).unwrap_or(false);
        if !current || !same_watch(&g.w, x) || g.busy {
            return (String::new(), false);
        }
        g.job = Some(j.clone());
        g.busy = true;
        if !g.link.is_empty() {
            j.set_link(&g.link);
        }
        (g.link.clone(), true)
    }

    /// release ends a batch; one that failed before moving anything makes the
    /// watch back off before the next.
    async fn release(&self, s: &Arc<WatchState>, j: &Arc<Job>, setup_failed: bool) {
        j.finish().await;
        let mut g = s.lock();
        g.busy = false;
        if setup_failed {
            g.fails += 1;
            g.retry_at = Some(Instant::now() + watch_backoff(g.fails));
        } else {
            g.fails = 0;
        }
    }

    /// link_batch gives a batch (and the watch) the Frame.io folder's page.
    async fn link_batch(&self, s: &Arc<WatchState>, x: &Watch, j: &Arc<Job>) {
        j.link_to_folder(&x.folder_id).await;
        let l = j.link();
        let mut g = s.lock();
        if g.w.folder_id == x.folder_id {
            g.link = l;
        }
    }
}

/// earlier_versions finds, by rel path, the Frame.io copy of each file the watch
/// has a record of: the file with its name in its Frame.io folder, the one the
/// watch uploaded or downloaded if there are several. A copy that was renamed,
/// moved or put in a version stack doesn't count, nor does an ambiguous name.
async fn earlier_versions(
    cx: &CancellationToken,
    c: &Client,
    acct: &str,
    files: &[Arc<File>],
    known: &HashMap<String, WatchFile>,
) -> Result<HashMap<String, String>, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let mut kids: HashMap<String, Vec<Asset>> = HashMap::new(); // Frame.io folder ID -> its children
    for f in files {
        let Some(k) = known.get(&f.rel_path) else {
            continue; // a new file
        };
        let parent = f.remote_parent.lock().unwrap().clone();
        if !kids.contains_key(&parent) {
            let list = c
                .children(cx, acct, &parent)
                .await
                .map_err(|e| format!("listing Frame.io folder: {}", e))?;
            kids.insert(parent.clone(), list);
        }
        let same: Vec<String> = kids
            .get(&parent)
            .map(|list| list.iter().filter(|a| a.type_ == "file" && a.name == f.name).map(|a| a.id.clone()).collect())
            .unwrap_or_default();
        if !k.remote_id.is_empty() && same.iter().any(|id| *id == k.remote_id) {
            out.insert(f.rel_path.clone(), k.remote_id.clone());
        } else if same.len() == 1 {
            out.insert(f.rel_path.clone(), same[0].clone());
        }
    }
    Ok(out)
}

/// watch_folders maps relative dirs to Frame.io folder IDs, reusing folders the
/// watch created before and existing folders with the same name.
async fn watch_folders(
    cx: &CancellationToken,
    c: &Client,
    h: &history::Store,
    x: &Watch,
    dirs: &[String],
) -> Result<HashMap<String, String>, String> {
    let mut ids = h.watch_dirs(&x.id);
    ids.insert(String::new(), x.folder_id.clone());
    let mut children: HashMap<String, HashMap<String, String>> = HashMap::new(); // parent id -> name -> folder id
    for d in dirs {
        if ids.contains_key(d) {
            continue;
        }
        let pid = ids.get(&parent_dir(d)).cloned().unwrap_or_default();
        if !children.contains_key(&pid) {
            let list = c
                .children(cx, &x.account_id, &pid)
                .await
                .map_err(|e| format!("listing Frame.io folder: {}", e))?;
            let mut kids: HashMap<String, String> = HashMap::new();
            for a in list {
                if a.type_ == "folder" {
                    kids.insert(a.name.clone(), a.id.clone());
                }
            }
            children.insert(pid.clone(), kids);
        }
        let name = base_name(d).to_string();
        let mut id = children.get(&pid).and_then(|k| k.get(&name)).cloned();
        if id.is_none() {
            let f = c
                .create_folder(cx, &x.account_id, &pid, &name)
                .await
                .map_err(|e| format!("creating folder {:?}: {}", d, e))?;
            if let Some(k) = children.get_mut(&pid) {
                k.insert(name.clone(), f.id.clone());
            }
            id = Some(f.id);
        }
        let id = id.unwrap_or_default();
        ids.insert(d.clone(), id.clone());
        h.put_watch_dir(&x.id, d, &id);
    }
    Ok(ids)
}

// ---------- Frame.io side ----------

impl Watcher {
    /// maybe_list starts a listing of the watch's Frame.io folder when one is due.
    fn maybe_list(&self, s: &Arc<WatchState>) {
        let (due, x, cfg) = {
            let mut g = s.lock();
            let now = Instant::now();
            let due = g.w.enabled
                && g.w.downloads()
                && !g.listing
                && !g.editing
                && g.next_list.map(|t| now >= t).unwrap_or(true);
            let x = g.w.clone();
            let cfg = g.cfg;
            if due {
                g.listing = true;
            }
            (due, x, cfg)
        };
        if !due {
            return;
        }
        let Some(w) = self.arc() else { return };
        let s = s.clone();
        tokio::spawn(async move { w.list(s, x, cfg).await });
    }

    /// list walks the Frame.io folder and keeps the result for the scans. The
    /// first listing after ignore_remote was set records every file it finds as
    /// existing. Big trees are listed less often, so listings use at most half
    /// the time.
    async fn list(&self, s: Arc<WatchState>, x: Watch, cfg: u64) {
        let start = Instant::now();
        let mut files: Vec<RemoteFile> = Vec::new();
        let mut err: Option<ApiErr> = None;
        let signed_in = self.m.api.auth.logged_in();
        if signed_in {
            match list_remote(&self.cx, &self.m.api, &x.account_id, &x.folder_id).await {
                Ok(v) => files = v,
                Err(e) => err = Some(e),
            }
        }
        let took = start.elapsed();
        let member = {
            let ws = self.mu.lock().unwrap();
            ws.get(&x.id).map(|o| Arc::ptr_eq(o, &s)).unwrap_or(false)
        };
        let mut g = s.lock();
        g.listing = false;
        if !member || g.cfg != cfg {
            return; // removed or edited meanwhile
        }
        if let Some(e) = err {
            g.list_fails += 1;
            let delay = self.poll.max(watch_backoff(g.list_fails));
            g.next_list = Some(Instant::now() + delay);
            g.list_err = list_error(&e);
            return;
        }
        if !signed_in {
            g.next_list = Some(Instant::now() + self.poll);
            g.list_err = ERR_NOT_SIGNED_IN.to_string();
            return;
        }
        g.list_fails = 0;
        g.list_err.clear();
        g.next_list = Some(Instant::now() + self.poll.max(took * 2));
        let mut l = RemoteListing {
            folder: x.folder_id.clone(),
            files: files.clone(),
            same: HashSet::with_capacity(files.len()),
        };
        for f in &files {
            l.same.insert(same_key(&f.rel, f.size));
        }
        if g.w.ignore_remote {
            let rows: Vec<WatchRemote> = files.iter().map(|f| wr(&f.id, &f.rel, f.size, WATCH_IGNORED)).collect();
            if let Err(e) = self.m.hist.baseline_watch_remotes(&x.id, &rows) {
                g.list_err = e;
                return;
            }
            self.m.hist.set_ignore_remote(&x.id, false);
            g.w.ignore_remote = false;
            g.invalidate(false);
        }
        g.remote = Some(Arc::new(l));
        drop(g);
        self.spawn_scan(&s);
    }
}

fn list_error(e: &ApiErr) -> String {
    match e {
        ApiErr::Status(ae) => match ae.status {
            404 => "Frame.io folder not found".to_string(),
            401 | 403 => "no access to the Frame.io folder".to_string(),
            s => format!("Frame.io error {}", s),
        },
        _ => "Frame.io unreachable".to_string(),
    }
}

/// list_remote returns every file below a Frame.io folder with its path relative
/// to it, using the same safe names as share downloads.
async fn list_remote(cx: &CancellationToken, c: &Client, acct: &str, root: &str) -> Result<Vec<RemoteFile>, ApiErr> {
    let mut out: Vec<RemoteFile> = Vec::new();
    walk_remote(cx, c, acct, root, "", 0, &mut out).await?;
    dedupe_remote(&mut out);
    Ok(out)
}

fn walk_remote<'a>(
    cx: &'a CancellationToken,
    c: &'a Client,
    acct: &'a str,
    id: &'a str,
    prefix: &'a str,
    depth: u32,
    out: &'a mut Vec<RemoteFile>,
) -> BoxFuture<'a, Result<(), ApiErr>> {
    async move {
        if depth > 64 {
            return Err(ApiErr::Other("folder nesting too deep".to_string()));
        }
        let kids = c.children(cx, acct, id).await?;
        for a in kids {
            match a.type_.as_str() {
                "folder" => {
                    let next = join_path(prefix, &share::safe_name(&a.name));
                    walk_remote(cx, c, acct, &a.id, &next, depth + 1, out).await?;
                }
                "file" | "version_stack" => {
                    let f = if a.type_ == "version_stack" {
                        head_version(cx, c, acct, &a).await?
                    } else {
                        Some(a.clone())
                    };
                    let Some(f) = f else { continue };
                    let name = share::safe_name(&f.name);
                    if skip_name(&name) {
                        continue;
                    }
                    out.push(RemoteFile {
                        id: f.id.clone(),
                        rel: join_path(prefix, &name),
                        size: f.file_size,
                        ready: f.status != "created",
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }
    .boxed()
}

/// head_version returns a version stack's current file: the one the listing
/// names, or else the stack's newest file (None for an empty stack).
async fn head_version(
    cx: &CancellationToken,
    c: &Client,
    acct: &str,
    stack: &Asset,
) -> Result<Option<Asset>, ApiErr> {
    if let Some(h) = &stack.head_version {
        if !h.id.is_empty() {
            return Ok(Some((**h).clone()));
        }
    }
    let files = c.version_stack_children(cx, acct, &stack.id).await?;
    let mut head: Option<Asset> = None;
    let mut newest = OffsetDateTime::UNIX_EPOCH;
    for f in files {
        let t = OffsetDateTime::parse(&f.created_at, &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH);
        if head.is_none() || t >= newest {
            head = Some(f);
            newest = t;
        }
    }
    Ok(head)
}

/// dedupe_remote gives files whose paths collide (Frame.io allows duplicate
/// names, and macOS ignores case) their own paths: "a.mov", "a (2).mov". Files
/// are ordered by ID within a name, so the numbering doesn't shuffle between
/// listings.
fn dedupe_remote(files: &mut Vec<RemoteFile>) {
    files.sort_by(|a, b| {
        let (la, lb) = (a.rel.to_lowercase(), b.rel.to_lowercase());
        la.cmp(&lb).then_with(|| a.id.cmp(&b.id))
    });
    let mut taken: HashSet<String> = files.iter().map(|f| f.rel.to_lowercase()).collect();
    let mut used: HashSet<String> = HashSet::with_capacity(files.len());
    for i in 0..files.len() {
        let k = files[i].rel.to_lowercase();
        if used.insert(k) {
            continue;
        }
        // Go's strings.TrimSuffix(rel, ext) drops one suffix, not every
        // repetition of it: "a.mov.mov" is numbered as "a.mov (2).mov".
        let ext = path_ext(&files[i].rel).to_string();
        let base = files[i].rel.strip_suffix(&ext).unwrap_or(&files[i].rel).to_string();
        let mut n = 2;
        loop {
            let c = format!("{} ({}){}", base, n, ext);
            let lc = c.to_lowercase();
            if !taken.contains(&lc) {
                files[i].rel = c;
                taken.insert(lc.clone());
                used.insert(lc);
                break;
            }
            n += 1;
        }
    }
}

impl Watcher {
    /// scan_remote picks the Frame.io files to download from the latest listing.
    /// It reports whether it started a batch or found the folder unavailable, so
    /// the local side waits for the next scan.
    async fn scan_remote(
        &self,
        s: &Arc<WatchState>,
        x: &Watch,
        l: &Arc<RemoteListing>,
        known: &HashMap<String, WatchFile>,
        done: &HashMap<String, WatchRemote>,
    ) -> bool {
        if let Err(e) = local_root_ok(&x.local_path) {
            self.set_err(s, &e);
            return true;
        }
        let mut uploaded: HashSet<String> = HashSet::new(); // Frame.io files this watch put there
        for k in known.values() {
            if k.state == WATCH_UPLOADED && !k.remote_id.is_empty() {
                uploaded.insert(k.remote_id.clone());
            }
        }
        let mut ready: Vec<RemoteFile> = Vec::new();
        let mut recheck: Vec<RemoteFile> = Vec::new();
        let mut here: Vec<WatchRemote> = Vec::new();
        let mut waiting = 0usize;
        let now = now_ms();
        for f in &l.files {
            if uploaded.contains(&f.id) {
                continue;
            }
            if let Some(r) = done.get(&f.id) {
                match r.state.as_str() {
                    WATCH_FAILED => {
                        if r.attempts >= WATCH_MAX_RETRIES {
                            continue;
                        }
                    }
                    WATCH_WAITING => {
                        let age = now.saturating_sub(r.updated_at);
                        if age < self.wait_delay(r.attempts).as_millis() as i64 || !f.ready {
                            waiting += 1;
                            continue;
                        }
                        recheck.push(f.clone());
                        continue;
                    }
                    _ => continue,
                }
            }
            if !f.ready {
                waiting += 1;
                continue;
            }
            // The same name and size is already here: nothing to fetch.
            if let Ok(md) = std::fs::metadata(join_path(&x.local_path, &f.rel)) {
                if md.is_file() && md.len() as i64 == f.size {
                    here.push(wr(&f.id, &f.rel, f.size, WATCH_IGNORED));
                    continue;
                }
            }
            ready.push(f.clone());
        }
        if !recheck.is_empty() {
            let (served, still) = self.recheck(s, x, &recheck, done).await;
            ready.extend(served);
            waiting += still;
        }
        let member = {
            let ws = self.mu.lock().unwrap();
            ws.get(&x.id).map(|o| Arc::ptr_eq(o, s)).unwrap_or(false)
        };
        if member {
            let mut g = s.lock();
            if same_watch(&g.w, x) {
                g.waiting = waiting;
            }
        }
        if !here.is_empty() {
            self.record_remote(s, x, &here, &[]);
        }
        if ready.is_empty() {
            if !x.uploads() {
                self.set_err(s, "");
            }
            return false;
        }
        if !self.m.api.auth.logged_in() {
            self.set_err(s, ERR_NOT_SIGNED_IN);
            return true;
        }
        self.set_err(s, "");
        ready.truncate(ready.len().min(WATCH_BATCH_MAX));
        self.start_download_batch(s, x, &ready, done);
        true
    }

    /// wait_delay is how long a file Frame.io wasn't serving waits before it's
    /// asked about again: one listing interval, doubling up to 15 minutes.
    fn wait_delay(&self, waits: i64) -> Duration {
        let shift = (waits - 1).max(0).min(5) as u32;
        (self.poll * (1u32 << shift)).min(Duration::from_secs(15 * 60))
    }

    /// recheck asks Frame.io whether files it wasn't serving are available now,
    /// so batches start only for those that are (a project that doesn't allow
    /// downloads would otherwise make an empty batch every few minutes).
    async fn recheck(
        &self,
        s: &Arc<WatchState>,
        x: &Watch,
        files: &[RemoteFile],
        done: &HashMap<String, WatchRemote>,
    ) -> (Vec<RemoteFile>, usize) {
        const PER_SCAN: usize = 20;
        let mut served: Vec<RemoteFile> = Vec::new();
        let mut still = 0usize;
        let mut rows: Vec<WatchRemote> = Vec::new();
        for (i, f) in files.iter().enumerate() {
            if i == PER_SCAN {
                still += files.len() - PER_SCAN; // the next scan asks about the rest
                break;
            }
            let cx = self.cx.child_token();
            let r = tokio::time::timeout(
                Duration::from_secs(30),
                self.m.api.download_url(&cx, &x.account_id, &f.id),
            )
            .await;
            cx.cancel();
            let status = match r {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => e.status(),
                Err(_) => None, // timed out: the batch reports any other problem
            };
            match status {
                Some(403) => {
                    let mut row = wr(&f.id, &f.rel, f.size, WATCH_WAITING);
                    row.attempts = done.get(&f.id).map(|d| d.attempts).unwrap_or(0) + 1;
                    row.error = "not served by Frame.io yet".to_string();
                    rows.push(row);
                    still += 1;
                }
                Some(404) => {
                    let mut row = wr(&f.id, &f.rel, f.size, WATCH_IGNORED);
                    row.error = "deleted on Frame.io".to_string();
                    rows.push(row);
                }
                _ => served.push(f.clone()), // the batch reports any other problem
            }
        }
        if !rows.is_empty() {
            self.record_remote(s, x, &rows, &[]);
        }
        (served, still)
    }

    fn start_download_batch(&self, s: &Arc<WatchState>, x: &Watch, items: &[RemoteFile], done: &HashMap<String, WatchRemote>) {
        let m = self.m.clone();
        let j = m.new_job("download");
        *j.account_id.lock().unwrap() = x.account_id.clone();
        *j.folder_id.lock().unwrap() = x.folder_id.clone();
        *j.source.lock().unwrap() = x.dest_label.clone();
        *j.dest.lock().unwrap() = x.local_path.clone();
        *j.log_root.lock().unwrap() = x.local_path.clone();
        *j.paths.lock().unwrap() = vec![x.local_path.clone()];
        *j.watch_id.lock().unwrap() = x.id.clone();
        j.watch_log.store(true, Ordering::Relaxed);
        let n = items.len();
        *j.title.lock().unwrap() = if n == 1 {
            format!("{} · {}", base_name_path(&x.local_path), base_name(&items[0].rel))
        } else {
            format!("{} · {} files", base_name_path(&x.local_path), n)
        };
        let mut fails: HashMap<String, i64> = HashMap::with_capacity(n); // earlier failed tries
        let mut waits: HashMap<String, i64> = HashMap::with_capacity(n); // earlier times Frame.io didn't serve it
        let mut list: Vec<Arc<File>> = Vec::with_capacity(n);
        for (i, it) in items.iter().enumerate() {
            if let Some(r) = done.get(&it.id) {
                match r.state.as_str() {
                    WATCH_FAILED => {
                        fails.insert(it.id.clone(), r.attempts);
                    }
                    WATCH_WAITING => {
                        waits.insert(it.id.clone(), r.attempts);
                    }
                    _ => {}
                }
            }
            let mut f = File::new(i, base_name(&it.rel), &it.rel, &join_path(&x.local_path, &it.rel), it.size);
            f.asset_id = it.id.clone();
            let f = Arc::new(f);
            f.attach(&j);
            list.push(f);
            j.add_total(it.size);
        }
        j.set_files(list);

        let recorded: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        // by file ID: waiting | gone, set while downloading
        let outcome: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let weak = self.weak();
        let mweak = Arc::downgrade(&self.m);

        {
            let s = s.clone();
            let x = x.clone();
            let items = items.to_vec();
            let recorded = recorded.clone();
            let weak = weak.clone();
            let mweak = mweak.clone();
            *j.on_finish.lock().unwrap() = Some(Arc::new(move |jj: Arc<Job>| {
                if !jj.user_canceled.load(Ordering::Relaxed) {
                    return;
                }
                let mut canceled: Vec<WatchRemote> = Vec::new();
                {
                    let rec = recorded.lock().unwrap();
                    for it in &items {
                        if !rec.contains(&it.id) {
                            canceled.push(wr(&it.id, &it.rel, it.size, WATCH_CANCELED));
                        }
                    }
                }
                if canceled.is_empty() {
                    return;
                }
                if let Some(w) = weak.upgrade() {
                    w.record_remote(&s, &x, &canceled, &[]);
                }
                if let Some(m) = mweak.upgrade() {
                    m.hist.log(&format!(
                        "WATCH    canceled [{}] {} files won't be downloaded to {}",
                        jj.id,
                        canceled.len(),
                        x.local_path
                    ));
                }
            }));
        }
        {
            let s = s.clone();
            let x = x.clone();
            let recorded = recorded.clone();
            let fails = fails.clone();
            let waits = waits.clone();
            let outcome = outcome.clone();
            let weak = weak.clone();
            *j.on_file_done.lock().unwrap() = Some(Arc::new(move |f: Arc<File>| {
                let (st, e) = {
                    let g = f.inner.lock().unwrap();
                    (g.status, g.err.clone())
                };
                let local = f.local_path();
                let Some(w) = weak.upgrade() else { return };
                let oc = outcome.lock().unwrap().get(&f.asset_id).cloned().unwrap_or_default();
                let mut r = wr(&f.asset_id, &f.rel_path, f.size(), "");
                let mut mark: Vec<WatchFile> = Vec::new();
                if oc == "waiting" {
                    r.state = WATCH_WAITING.to_string();
                    r.error = "not served by Frame.io yet".to_string();
                    r.attempts = waits.get(&f.asset_id).copied().unwrap_or(0) + 1;
                } else if oc == "gone" {
                    r.state = WATCH_IGNORED.to_string();
                    r.error = "deleted on Frame.io".to_string();
                } else if st == Status::Done || st == Status::Skipped {
                    r.state = WATCH_DOWNLOADED.to_string();
                    // The local file now matches Frame.io: it's never uploaded back.
                    if let Ok(md) = std::fs::metadata(&local) {
                        if let Ok(rel) = Path::new(&local).strip_prefix(&x.local_path) {
                            let rel = rel.to_string_lossy().replace('\\', "/");
                            if !rel.starts_with("..") {
                                let mut row = wf(&rel, md.len() as i64, mtime_nanos(&md), WATCH_DOWNLOADED);
                                row.remote_id = f.asset_id.clone();
                                mark.push(row);
                            }
                        }
                    }
                } else if st == Status::Failed {
                    r.state = WATCH_FAILED.to_string();
                    r.error = e.clone();
                    r.attempts = fails.get(&f.asset_id).copied().unwrap_or(0) + 1;
                } else {
                    return;
                }
                recorded.lock().unwrap().insert(f.asset_id.clone());
                w.record_remote(&s, &x, &[r], &mark);
            }));
        }

        let (link, ok) = self.claim(s, x, &j);
        if !ok {
            return;
        }
        m.add(&j);
        j.persist_start();
        m.hist.log(&format!("WATCH    batch    [{}] {} — {} files <- {}", j.id, x.local_path, n, x.dest_label));

        let Some(w) = self.arc() else { return };
        let s = s.clone();
        let x = x.clone();
        let items = items.to_vec();
        tokio::spawn(async move { w.run_download_batch(s, x, j, link, outcome, items).await });
    }

    async fn run_download_batch(
        self: Arc<Self>,
        s: Arc<WatchState>,
        x: Watch,
        j: Arc<Job>,
        link: String,
        outcome: Arc<Mutex<HashMap<String, String>>>,
        _items: Vec<RemoteFile>,
    ) {
        j.set_status(Status::Resolving);
        if link.is_empty() {
            self.link_batch(&s, &x, &j).await;
        }
        let seg = ((self.m.settings)().segment_mb as i64) << 20;
        j.set_status(Status::Running);
        let x2 = x.clone();
        let task: FileTask = Arc::new(move |jj, ff| {
            let x3 = x2.clone();
            let oc = outcome.clone();
            async move {
                match super::download::download_account_file(jj.clone(), ff.clone(), &x3.account_id, seg).await {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        if jj.cx.is_cancelled() {
                            return Err(e);
                        }
                        // Not a failure: the original isn't served yet (403), or the file is gone (404).
                        match api_status(&e) {
                            Some(403) => {
                                oc.lock().unwrap().insert(ff.asset_id.clone(), "waiting".to_string());
                                ff.set_status(Status::Skipped, "not served by Frame.io yet");
                                Ok(())
                            }
                            Some(404) => {
                                oc.lock().unwrap().insert(ff.asset_id.clone(), "gone".to_string());
                                ff.set_status(Status::Skipped, "deleted on Frame.io");
                                Ok(())
                            }
                            _ => Err(e),
                        }
                    }
                }
            }
            .boxed()
        });
        j.run_files(task).await;
        self.release(&s, &j, false).await;
    }
}

/// download_account_file fetches a file from the signed-in account into
/// f.LocalPath. The ranged downloader (with its resume sidecars) lives in
/// download.rs, next to the helpers it needs, and is called from here so a
/// watch batch's files transfer exactly like a share's.
pub async fn download_account_file(j: Arc<Job>, f: Arc<File>, acct: &str, seg: i64) -> Result<(), String> {
    super::download::download_account_file(j, f, acct, seg).await
}

/// The HTTP status a download error carries, if it is a Frame.io API error
/// (download.rs reports the same text as Go's err.Error()).
fn api_status(e: &str) -> Option<u16> {
    let rest = e.split("Frame.io API error ").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// ---------- Small path helpers ----------

/// clean is Go's filepath.Clean for the slash paths this app uses.
pub(crate) fn clean(p: &str) -> String {
    if p.is_empty() {
        return ".".to_string();
    }
    let rooted = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if let Some(last) = parts.last() {
                    if *last != ".." {
                        parts.pop();
                        continue;
                    }
                }
                if !rooted {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() {
        return if rooted { "/".to_string() } else { ".".to_string() };
    }
    let body = parts.join("/");
    if rooted {
        format!("/{}", body)
    } else {
        body
    }
}

/// join_path joins a relative path onto a folder (Go's filepath.Join).
fn join_path(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        return rel.to_string();
    }
    if rel.is_empty() {
        return dir.to_string();
    }
    format!("{}/{}", dir.trim_end_matches('/'), rel)
}

/// base_name_path is Go's filepath.Base.
fn base_name_path(p: &str) -> String {
    if p.is_empty() {
        return ".".to_string();
    }
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        return "/".to_string();
    }
    t.rsplit('/').next().unwrap_or(t).to_string()
}

/// path_ext is Go's path.Ext.
fn path_ext(p: &str) -> &str {
    let base = p.rsplit('/').next().unwrap_or(p);
    match base.rfind('.') {
        Some(i) => &base[i..],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Go: TestDedupeRemote — files whose paths collide get their own, ordered by
    // ID within a name so the numbering doesn't shuffle between listings.
    #[test]
    fn dedupe_remote_numbers_colliding_paths() {
        let mut files = vec![
            RemoteFile { id: "b".into(), rel: "A/x.mov".into(), size: 0, ready: true },
            RemoteFile { id: "a".into(), rel: "a/X.mov".into(), size: 0, ready: true },
            RemoteFile { id: "c".into(), rel: "A/x (2).mov".into(), size: 0, ready: true },
        ];
        dedupe_remote(&mut files);
        let got: HashMap<String, String> = files.iter().map(|f| (f.id.clone(), f.rel.clone())).collect();
        assert_eq!(got["a"], "a/X.mov");
        assert_eq!(got["b"], "A/x (3).mov");
        assert_eq!(got["c"], "A/x (2).mov");
    }

    // Regression: a name that ends in the same extension twice is numbered
    // before its last extension (Go's strings.TrimSuffix), not before its first,
    // so a listing of two "a.mov.mov" files gives "a.mov (2).mov".
    #[test]
    fn dedupe_remote_keeps_a_repeated_extension_in_the_stem() {
        let mut files = vec![
            RemoteFile { id: "a".into(), rel: "a.mov.mov".into(), size: 0, ready: true },
            RemoteFile { id: "b".into(), rel: "a.mov.mov".into(), size: 0, ready: true },
        ];
        dedupe_remote(&mut files);
        assert_eq!(files[0].rel, "a.mov.mov");
        assert_eq!(files[1].rel, "a.mov (2).mov");
    }

    // A walk can catch a file just after it's created, still empty. Filling it
    // doesn't change its folder, so the quick re-check (no walk) must keep
    // checking it instead of waiting for the next full walk.
    #[test]
    fn a_file_caught_empty_is_rechecked_without_a_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        let p = tmp.path().join("a.wav");
        std::fs::write(&p, b"").unwrap();
        let settle = Duration::from_millis(20);
        let known = HashMap::new();
        let out = scan_upload_side(&root, &HashMap::new(), &known, true, settle, false, false, None);
        assert!(out.seen.contains_key("a.wav"), "an empty file is seen");
        assert_eq!((out.pending, out.settled.len()), (0, 0), "but neither settling nor uploaded");

        std::fs::write(&p, b"RIFF0000WAVEdata").unwrap();
        let inc = scan_upload_side(&root, &out.seen, &known, false, settle, false, false, None);
        assert_eq!(inc.pending, 1, "the re-check sees it fill");
        std::thread::sleep(settle * 2);
        let inc = scan_upload_side(&root, &inc.seen, &known, false, settle, false, false, None);
        assert_eq!(inc.settled.iter().map(|i| i.rel.as_str()).collect::<Vec<_>>(), ["a.wav"]);
    }

    // An empty file at creation isn't left alone with the existing ones: once it
    // fills, it's new.
    #[test]
    fn existing_files_leave_out_empty_ones() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("empty.mov"), b"").unwrap();
        std::fs::write(tmp.path().join("full.mov"), b"x").unwrap();
        let got: Vec<String> = existing_files(&tmp.path().to_string_lossy()).into_iter().map(|f| f.rel_path).collect();
        assert_eq!(got, ["full.mov"]);
    }

    // Go: TestLocalRootOK — a share that isn't mounted is refused.
    #[test]
    fn local_root_ok_refuses_missing_folders() {
        let tmp = std::env::temp_dir().join(format!("fio-watch-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(local_root_ok(&tmp.to_string_lossy()).is_ok());
        let missing = tmp.join("missing");
        assert!(local_root_ok(&missing.to_string_lossy()).is_err());
        assert!(local_root_ok("/Volumes/fiotransfer-test-not-mounted/Media").is_err());
    }

    #[test]
    fn temp_names_are_filtered() {
        assert!(temp_name("a.mov.part"));
        assert!(temp_name(".DS_Store"));
        assert!(temp_name("~$notes.docx"));
        assert!(!temp_name("a.mov"));
    }

    #[test]
    fn same_key_ignores_case() {
        assert_eq!(same_key("Day 1/A.mov", 10), same_key("day 1/a.MOV", 10));
        assert_ne!(same_key("a.mov", 10), same_key("a.mov", 11));
    }

    #[test]
    fn backoff_doubles_up_to_fifteen_minutes() {
        assert_eq!(watch_backoff(1), Duration::from_secs(30));
        assert_eq!(watch_backoff(2), Duration::from_secs(60));
        assert_eq!(watch_backoff(3), Duration::from_secs(120));
        assert_eq!(watch_backoff(6), Duration::from_secs(900));
        assert_eq!(watch_backoff(20), Duration::from_secs(900));
    }

    #[test]
    fn clean_matches_filepath_clean() {
        assert_eq!(clean(""), ".");
        assert_eq!(clean("/"), "/");
        assert_eq!(clean("/a/b/"), "/a/b");
        assert_eq!(clean("/a//b/./c"), "/a/b/c");
        assert_eq!(clean("/a/b/../c"), "/a/c");
        assert_eq!(clean("a/.."), ".");
        assert_eq!(clean("../a"), "../a");
    }

    #[test]
    fn path_helpers() {
        assert_eq!(join_path("/a/b", "x/y.mov"), "/a/b/x/y.mov");
        assert_eq!(join_path("/a/b/", "x"), "/a/b/x");
        assert_eq!(base_name_path("/a/b"), "b");
        assert_eq!(base_name_path("/a/b/"), "b");
        assert_eq!(path_ext("a/b/x.mov"), ".mov");
        assert_eq!(path_ext("a/b/x"), "");
        assert_eq!(path_ext("a.b/x"), "");
    }
}


// ---------- Property tests (proptest-praxis discovery suite) ----------
//
// Everything below tests the private decision machinery in this module: the
// walk, the temp-name filter, the batch key, the backoff curve, the remote
// dedupe, and the Go-parity path helpers. The remote listing and the folder log
// are covered in `tests/prop_watch.rs`.
//
// Run with: cargo test --lib --offline xfer::watch::props
#[cfg(test)]
mod props {
    use super::*;
    use proptest::collection::vec as pvec;
    use proptest::prelude::*;
    use std::collections::{BTreeMap, BTreeSet};

    // ---------- temp_name ----------

    /// The six suffixes temp_name's doc names, lowercased as the code does.
    const DOC_SUFFIXES: [&str; 6] = [".part", ".partial", ".crdownload", ".download", ".tmp", ".temp"];

    /// temp_model is the rule the doc states, written from the doc text rather
    /// than from the code: lowercased, starts with "~$" or ".", or ends with one
    /// of the six partial-write suffixes.
    fn temp_model(name: &str) -> bool {
        let n = name.to_lowercase();
        n.starts_with("~$") || n.starts_with('.') || DOC_SUFFIXES.iter().any(|s| n.ends_with(*s))
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// For every string, the filter agrees with the rule it documents.
        #[test]
        fn temp_name_matches_the_documented_rule(name in ".{0,12}") {
            prop_assert_eq!(temp_name(&name), temp_model(&name), "name {:?}", name);
        }

        /// Every documented partial-write suffix is filtered, in any case, and
        /// the stem in front of it doesn't matter; so is a "~$" or "." prefix.
        #[test]
        fn every_documented_temp_form_is_filtered(
            stem in "[A-Za-z0-9 _-]{0,10}",
            which in 0usize..DOC_SUFFIXES.len(),
            upper in any::<bool>(),
        ) {
            let suffix = DOC_SUFFIXES[which];
            let suffix = if upper { suffix.to_uppercase() } else { suffix.to_string() };
            prop_assert!(temp_name(&format!("{}{}", stem, suffix)), "stem {:?} suffix {:?}", stem, suffix);
            prop_assert!(temp_name(&format!("~${}", stem)), "~$ + {:?}", stem);
            prop_assert!(temp_name(&format!(".{}", stem)), "dot + {:?}", stem);
        }

        /// A name that is none of the documented forms is not filtered: this is
        /// what stops a real media file from being ignored.
        #[test]
        fn ordinary_media_names_are_not_filtered(stem in "[A-Za-z0-9_-]{1,10}", ext in "mov|mp4|mxf|wav|r3d") {
            let name = format!("{}.{}", stem, ext);
            prop_assert!(!temp_name(&name), "name {:?}", name);
        }
    }

    // ---------- same_key ----------

    /// The batch logic groups by (case-insensitive relative path, size): a local
    /// file whose key is already in the listing is not uploaded again.
    fn key_model(rel: &str, size: i64) -> (String, i64) {
        (rel.to_lowercase(), size)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// Two (rel, size) pairs share a key exactly when they are the same path
        /// ignoring case and the same size.
        #[test]
        fn same_key_groups_exactly_by_lowercased_path_and_size(
            a in "[A-Za-z0-9 ./_-]{0,12}",
            b in "[A-Za-z0-9 ./_-]{0,12}",
            sa in 0i64..1_000_000,
            sb in 0i64..1_000_000,
        ) {
            prop_assert_eq!(
                same_key(&a, sa) == same_key(&b, sb),
                key_model(&a, sa) == key_model(&b, sb),
                "a {:?}/{} b {:?}/{}", a, sa, b, sb
            );
        }

        /// A different size never shares a key, and case never splits one.
        #[test]
        fn different_size_never_matches_and_case_never_splits(
            rel in "[A-Za-z0-9 ./_-]{1,12}",
            size in 1i64..1_000_000,
            delta in 1i64..1000,
        ) {
            prop_assert_ne!(same_key(&rel, size), same_key(&rel, size + delta));
            prop_assert_eq!(same_key(&rel, size), same_key(&rel.to_uppercase(), size));
        }
    }

    // ---------- watch_backoff ----------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

        /// The backoff never decreases as failures pile up and never exceeds the
        /// documented 15 minutes; it starts at 30s and doubles.
        #[test]
        fn watch_backoff_is_monotone_and_capped(n in 0u32..64) {
            let b = watch_backoff(n);
            prop_assert!(b <= Duration::from_secs(15 * 60), "n {} gave {:?}", n, b);
            prop_assert!(b >= Duration::from_secs(30), "n {} gave {:?}", n, b);
            prop_assert!(watch_backoff(n + 1) >= b, "{} then {}: {:?} then {:?}", n, n + 1, b, watch_backoff(n + 1));
            match n {
                1..=5 => prop_assert_eq!(b, Duration::from_secs(30 * (1 << (n - 1)))),
                6..=63 => prop_assert_eq!(b, Duration::from_secs(15 * 60)),
                _ => prop_assert_eq!(b, Duration::from_secs(30)), // 0: the first failure's wait
            }
        }
    }

    // ---------- path helpers ----------

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// clean is idempotent, keeps the root "/" and the leading ".."s, and
        /// every other form has no empty or "." segment and no trailing slash.
        #[test]
        fn clean_is_a_normal_form(p in "[A-Za-z0-9 ./]{0,14}") {
            let c = clean(&p);
            prop_assert_eq!(c.clone(), clean(&c), "p {:?}", p);
            prop_assert!(!c.is_empty(), "p {:?}", p);
            prop_assert_eq!(c.starts_with('/'), p.starts_with('/'), "p {:?} -> {:?}", p, c);
            if c != "." && c != ".." && !c.starts_with("../") && c != "/" {
                prop_assert!(!c.ends_with('/'), "p {:?} -> {:?}", p, c);
                for seg in c.trim_start_matches('/').split('/') {
                    prop_assert!(seg != "." && !seg.is_empty(), "p {:?} -> {:?}", p, c);
                }
            }
        }

        /// Every caller passes a clean relative path (from the walk or a
        /// listing), so this is the domain that matters: one separator, no
        /// trailing slash on the folder, and the joined path is the folder's
        /// normal form plus the file.
        #[test]
        fn join_path_has_one_separator(
            dir in "/[A-Za-z0-9]{1,6}(/[A-Za-z0-9]{1,6}){0,2}",
            rel in "[A-Za-z0-9]{1,6}(/[A-Za-z0-9]{1,6}){0,2}",
        ) {
            let j = join_path(&dir, &rel);
            prop_assert!(!j.contains("//"), "dir {:?} rel {:?} -> {:?}", dir, rel, j);
            prop_assert_eq!(j.clone(), format!("{}/{}", clean(&dir), rel));
            prop_assert_eq!(join_path(&(dir.clone() + "/"), &rel), j);
        }
    }

    /// What Go 1.27.3's filepath.Dir, path.Ext and filepath.Base return, from
    /// `go run` — the model the three helpers document.
    const GO_DB: [(&str, &str, &str, &str); 11] = [
        //  path,   Dir,      Ext,   Base
        ("", ".", "", "."),
        (".", ".", ".", "."),
        ("/", "/", "", "/"),
        ("//", "/", "", "/"),
        ("a/b/", "a/b", "", "b"),
        ("/a/b/", "/a/b", "", "b"),
        ("a", ".", "", "a"),
        ("/a", "/", "", "a"),
        ("a.b/x", "a.b", "", "x"),
        ("a/..", "a", ".", ".."),
        ("../a", "..", "", "a"),
    ];

    #[test]
    fn go_parity_helpers_match_go() {
        let mut bad: Vec<String> = Vec::new();
        for (p, dir, ext, base) in GO_DB {
            let got = crate::xfer::folderlog::dir_of(p);
            if got != dir {
                bad.push(format!("dir_of({:?}) = {:?}, Go says {:?}", p, got, dir));
            }
            if path_ext(p) != ext {
                bad.push(format!("path_ext({:?}) = {:?}, Go says {:?}", p, path_ext(p), ext));
            }
            if base_name_path(p) != base {
                bad.push(format!("base_name_path({:?}) = {:?}, Go says {:?}", p, base_name_path(p), base));
            }
        }
        assert!(bad.is_empty(), "{} differences from Go:\n{}", bad.len(), bad.join("\n"));
    }

    // ---------- the walk ----------

    /// A folder tree held in the test, built from the same entries the disk tree
    /// is made from.
    #[derive(Default, Debug)]
    struct Model {
        dirs: BTreeMap<String, Model>,
        files: BTreeMap<String, i64>, // name -> size
    }

    impl Model {
        fn node(&mut self, segs: &[String]) -> &mut Model {
            let mut n = self;
            for s in segs {
                n = n.dirs.entry(s.clone()).or_default();
            }
            n
        }

        /// eligible is what a watch must consider, from the doc: every regular
        /// file below the root, except empty ones, except names that are skipped
        /// or are a temp name, and except anything under a folder whose name is
        /// skipped.
        fn eligible(&self) -> BTreeSet<(String, i64)> {
            let mut out = BTreeSet::new();
            self.collect("", &mut out);
            out
        }

        fn collect(&self, prefix: &str, out: &mut BTreeSet<(String, i64)>) {
            let join = |p: &str, n: &str| if p.is_empty() { n.to_string() } else { format!("{}/{}", p, n) };
            for (name, sub) in &self.dirs {
                if skip_name(name) || temp_name(name) {
                    continue;
                }
                sub.collect(&join(prefix, name), out);
            }
            for (name, size) in &self.files {
                if skip_name(name) || temp_name(name) {
                    continue;
                }
                out.insert((join(prefix, name), *size));
            }
        }

        /// descended lists the folders a walk goes into, relative to the root.
        fn descended(&self) -> BTreeSet<String> {
            let mut out = BTreeSet::new();
            self.descend("", &mut out);
            out
        }

        fn descend(&self, prefix: &str, out: &mut BTreeSet<String>) {
            for (name, sub) in &self.dirs {
                if skip_name(name) || temp_name(name) {
                    continue;
                }
                let p = if prefix.is_empty() { name.clone() } else { format!("{}/{}", prefix, name) };
                out.insert(p.clone());
                sub.descend(&p, out);
            }
        }
    }

    /// A name a user could plausibly have, plus the metadata and partial-write
    /// names a watch must leave alone.
    fn any_name() -> impl Strategy<Value = String> {
        let plain = "[A-Za-z0-9][A-Za-z0-9 ._-]{0,6}".prop_map(|s| s.trim_end().to_string());
        let hidden = "[A-Za-z]{0,4}".prop_map(String::from);
        prop_oneof![
            5 => plain,
            1 => hidden.prop_map(|s| format!(".{}", s)),
            2 => prop::sample::select(vec![
                "~$notes.docx".to_string(),
                "a.mov.part".to_string(),
                "b.partial".to_string(),
                "c.crdownload".to_string(),
                "d.download".to_string(),
                "e.tmp".to_string(),
                "f.TEMP".to_string(),
                ".DS_Store".to_string(),
                ".Spotlight-V100".to_string(),
                "Thumbs.db".to_string(),
                "x.fiopart".to_string(),
                "y.fiostate.tmp".to_string(),
                "FioTransfer Upload 2026-01-02 030405.txt".to_string(),
            ]),
        ]
    }

    /// Puts one generated entry into the model and on disk, skipping anything
    /// that would make one path both a folder and a file.
    /// Whether this machine's filesystem treats `probe` and `PROBE` as the same
    /// file. Asked once, by writing both names in a temporary folder.
    fn fs_ignores_case() -> bool {
        static INSENSITIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *INSENSITIVE.get_or_init(|| {
            let Ok(tmp) = tempfile::tempdir() else { return false };
            if std::fs::write(tmp.path().join("probe"), b"x").is_err() {
                return false;
            }
            tmp.path().join("PROBE").exists()
        })
    }

    fn place(model: &mut Model, root: &std::path::Path, segs: &[String], name: &str, size: u8) -> bool {
        {
            let mut n: &Model = model;
            for s in segs {
                if n.files.contains_key(s) {
                    return false;
                }
                n = match n.dirs.get(s) {
                    Some(sub) => sub,
                    None => return false, // make_parents makes them first
                };
            }
            // Case-insensitive volumes (APFS by default) treat `B` and `b` as one
            // file, so the second write overwrites the first and the walk — which is
            // right to — lists one file where the model would hold two. Ask the
            // filesystem once, and let the model follow the same rule it does.
            if n.files.contains_key(name)
                || (fs_ignores_case() && n.files.keys().any(|k| k.eq_ignore_ascii_case(name)))
            {
                return false;
            }
        }
        let dir = segs.iter().fold(root.to_path_buf(), |p, s| p.join(s));
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        if std::fs::write(dir.join(name), vec![b'x'; size as usize]).is_err() {
            return false;
        }
        model.node(segs).files.insert(name.to_string(), size as i64);
        true
    }

    /// Makes the entry's parent folders in the model, failing if one of them is
    /// already a file.
    fn make_parents(model: &mut Model, segs: &[String]) -> bool {
        {
            let mut n: &Model = model;
            for s in segs {
                if n.files.contains_key(s) {
                    return false;
                }
                n = match n.dirs.get(s) {
                    Some(sub) => sub,
                    None => break,
                };
            }
        }
        model.node(segs);
        true
    }

    fn walk_with_dirs(root: &str) -> Result<(Vec<(String, i64, i64)>, HashMap<String, i64>), String> {
        let mut files = Vec::new();
        let mut dirs = Some(HashMap::new());
        walk_watch(root, &mut files, &mut dirs)?;
        Ok((files, dirs.unwrap_or_default()))
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// walk_watch returns every eligible file exactly once, and nothing under
        /// a skipped or temp-named folder.
        #[test]
        fn walk_lists_each_eligible_file_once(entries in pvec((0u8..3, 0u8..3, any_name(), 0u8..3), 0..10)) {
            let tmp = tempfile::tempdir().expect("temp dir");
            let root = tmp.path().to_path_buf();
            let root_s = root.to_string_lossy().into_owned();
            let mut model = Model::default();
            for (depth, tag, name, size) in &entries {
                if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                    continue; // not a filename
                }
                let segs: Vec<String> = (0..*depth).map(|l| format!("d{}_{}", l, tag % 3)).collect();
                if !make_parents(&mut model, &segs) {
                    continue;
                }
                place(&mut model, &root, &segs, name, *size);
            }

            let mut files: Vec<(String, i64, i64)> = Vec::new();
            let mut dirs: Option<HashMap<String, i64>> = Some(HashMap::new());
            walk_watch(&root_s, &mut files, &mut dirs).expect("root exists");
            let got: BTreeSet<(String, i64)> = files.iter().map(|(r, s, _)| (r.clone(), *s)).collect();
            prop_assert_eq!(got.len(), files.len(), "a file was listed twice: {:?}", files);
            prop_assert_eq!(got, model.eligible());
            for (rel, _, _) in &files {
                prop_assert!(!rel.starts_with('/'), "rel {:?} is not relative", rel);
                for seg in rel.split('/') {
                    prop_assert!(!seg.is_empty() && seg != ".." && seg != ".", "rel {:?}", rel);
                }
            }
        }

        /// The walk reports exactly the folders it descended into, keyed by full
        /// path and with an mtime, and the root is one of them.
        #[test]
        fn walk_reports_the_folders_it_descends(entries in pvec((0u8..3, 0u8..3, any_name(), 1u8..3), 0..8)) {
            let tmp = tempfile::tempdir().expect("temp dir");
            let root = tmp.path().to_path_buf();
            let root_s = root.to_string_lossy().into_owned();
            let mut model = Model::default();
            for (depth, tag, name, size) in &entries {
                if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                    continue;
                }
                let segs: Vec<String> = (0..*depth).map(|l| format!("d{}_{}", l, tag % 3)).collect();
                if !make_parents(&mut model, &segs) {
                    continue;
                }
                place(&mut model, &root, &segs, name, *size);
            }
            let (files, dirs) = walk_with_dirs(&root_s).expect("root exists");
            let mut want: BTreeSet<String> = BTreeSet::new();
            want.insert(root_s.clone());
            for d in model.descended() {
                want.insert(root.join(&d).to_string_lossy().into_owned());
            }
            let got: BTreeSet<String> = dirs.keys().cloned().collect();
            prop_assert_eq!(got, want);
            for (d, mtime) in &dirs {
                prop_assert!(*mtime > 0, "folder {:?} has no mtime", d);
            }
            prop_assert!(files.iter().all(|(rel, _, _)| !rel.starts_with('/')));
        }
    }

    #[test]
    fn walk_refuses_a_missing_root() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let missing = tmp.path().join("not-there");
        let mut files: Vec<(String, i64, i64)> = Vec::new();
        let mut dirs: Option<HashMap<String, i64>> = None;
        assert_eq!(
            walk_watch(&missing.to_string_lossy(), &mut files, &mut dirs),
            Err(ERR_FOLDER_UNAVAILABLE.to_string())
        );
        assert!(files.is_empty());
    }

    // ---------- dedupe_remote ----------

    fn dedupe_name_cases() -> impl Strategy<Value = String> {
        prop_oneof![
            prop::sample::select(vec![
                "a.mov", "A.MOV", "a.mov.mov", "x.txt.txt", "b.tar.gz", ".hidden", "no_ext",
                "Sub/a.mov", "sub/A.mov", "a (2).mov", "c.", "d..e",
            ]).prop_map(String::from),
            "[a-z]{1,3}\\.[a-z]{1,3}".prop_map(String::from),
        ]
    }

    /// Go's strings.TrimSuffix(rel, path.Ext(rel)): the name without its last
    /// extension, which is what a de-duplicated path is numbered from.
    fn go_stem(rel: &str) -> &str {
        let e = path_ext(rel);
        &rel[..rel.len() - e.len()]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// Colliding paths get " (2)", " (3)" … before their extension, the first
        /// one in (lowercased path, ID) order keeps its name, no path is taken
        /// from another file, and the numbering doesn't depend on the order the
        /// listing arrived in.
        #[test]
        fn dedupe_remote_renames_the_later_copies(
            clusters in pvec((dedupe_name_cases(), 1usize..4), 0..4),
            rotate in 0usize..7,
        ) {
            // Clusters, not a flat list: the interesting case is the same name
            // in 2..3 files, which a flat generator reaches almost never.
            let mut names: Vec<String> = Vec::new();
            for (name, copies) in &clusters {
                for _ in 0..*copies {
                    names.push(name.clone());
                }
            }
            let mut files: Vec<RemoteFile> = names
                .iter()
                .enumerate()
                .map(|(i, rel)| RemoteFile { id: format!("{:04x}", i), rel: rel.clone(), size: 1, ready: true })
                .collect();
            let originals: HashMap<String, String> = files.iter().map(|f| (f.id.clone(), f.rel.clone())).collect();
            let before: Vec<String> = files.iter().map(|f| f.id.clone()).collect();
            let mut sorted = before.clone();
            sorted.sort_by_key(|id| (originals[id].to_lowercase(), id.clone()));

            dedupe_remote(&mut files);

            prop_assert_eq!(files.len(), before.len());
            let mut ids: Vec<String> = files.iter().map(|f| f.id.clone()).collect();
            let mut want_ids = before.clone();
            ids.sort();
            want_ids.sort();
            prop_assert_eq!(&ids, &want_ids, "the IDs changed");
            prop_assert_eq!(files.iter().map(|f| f.id.clone()).collect::<Vec<_>>(), sorted, "order");
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for f in &files {
                prop_assert!(seen.insert(f.rel.to_lowercase()), "two files at {:?}", f.rel);
            }
            let mut kept: BTreeSet<String> = BTreeSet::new();
            for f in &files {
                let orig = &originals[&f.id];
                let lc = orig.to_lowercase();
                if f.rel == *orig && kept.insert(lc.clone()) {
                    continue; // the first copy keeps its name
                }
                let stem = go_stem(orig);
                let ext = path_ext(orig);
                let mut matched = false;
                for n in 2..64 {
                    if f.rel == format!("{} ({}){}", stem, n, ext) {
                        matched = true;
                        break;
                    }
                }
                prop_assert!(
                    matched,
                    "{:?} was renamed to {:?}; the numbered form is stem {:?} + \" (n)\" + ext {:?}",
                    orig, f.rel, stem, ext
                );
                prop_assert!(
                    !originals.values().any(|o| o.to_lowercase() == f.rel.to_lowercase()),
                    "{:?} took another file's name",
                    f.rel
                );
                kept.insert(f.rel.to_lowercase());
            }

            // The same listing in another order numbers the same way.
            if !files.is_empty() {
                let mut again: Vec<RemoteFile> = before
                    .iter()
                    .map(|id| RemoteFile { id: id.clone(), rel: originals[id].clone(), size: 1, ready: true })
                    .collect();
                let k = rotate % again.len();
                again.rotate_left(k);
                dedupe_remote(&mut again);
                let a: BTreeMap<String, String> = files.iter().map(|f| (f.id.clone(), f.rel.clone())).collect();
                let b: BTreeMap<String, String> = again.iter().map(|f| (f.id.clone(), f.rel.clone())).collect();
                prop_assert_eq!(a, b, "the numbering depends on listing order");
            }
        }
    }

    // ---------- the upload side's decision ----------

    /// What the scan decided about one file.
    #[derive(Debug, PartialEq, Eq)]
    enum Kind {
        Pending,
        OnFrameio,
        Settled,
    }

    const ROW_STATES: [&str; 7] = [
        WATCH_UPLOADED, WATCH_DOWNLOADED, WATCH_IGNORED, WATCH_CANCELED, WATCH_FAILED, "queued", "",
    ];

    /// expected is the rule set the module doc and the state comments state,
    /// applied to one file in the order that matters: a record that still
    /// matches the file means the watch already moved it (or gave up on it);
    /// anything else is seen, must hold still for the settle time, is left alone
    /// when a mirror sees the same name-and-size on Frame.io, and finally must be
    /// finished being written.
    fn expected(
        rel: &str,
        size: i64,
        mtime: i64,
        row: Option<&WatchFile>,
        prev: Option<&Obs>,
        settle: Duration,
        net_vol: bool,
        mirror: bool,
        same: &HashSet<String>,
    ) -> (bool, Option<Kind>) {
        let now = Instant::now();
        if let Some(k) = row {
            let unchanged = k.size == size && k.mtime == mtime;
            let moved = matches!(
                k.state.as_str(),
                WATCH_UPLOADED | WATCH_DOWNLOADED | WATCH_IGNORED | WATCH_CANCELED
            );
            if moved && unchanged {
                return (false, None);
            }
            if k.state == WATCH_FAILED && k.attempts >= WATCH_MAX_RETRIES && unchanged {
                return (false, None); // gave up on this version of the file
            }
        }
        if size == 0 {
            return (true, None); // seen, so it's checked again, but never uploaded
        }
        let since = match prev {
            Some(p) if p.size == size && p.mtime == mtime => p.since,
            _ => now,
        };
        let steady = now.saturating_duration_since(since);
        if steady < settle {
            return (true, Some(Kind::Pending));
        }
        if mirror && row.is_none() && same.contains(&same_key(rel, size)) {
            return (true, Some(Kind::OnFrameio));
        }
        let finished = !net_vol || steady >= settle * NET_SETTLE_FACTOR;
        if !finished {
            return (true, Some(Kind::Pending));
        }
        (true, Some(Kind::Settled))
    }

    fn bucket_age(bucket: usize, settle: Duration) -> Duration {
        match bucket % 4 {
            0 => Duration::ZERO,          // just appeared
            1 => settle / 4,              // still inside the settle window
            2 => settle * 2,              // settled, but short of a share's 6x
            _ => settle * 8,              // settled, and long past a share's 6x
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// The upload side's decision matches the documented rules for every
        /// combination of records, earlier observations, settle age, mirror and
        /// network share; every seen file lands in exactly one of settled, on
        /// Frame.io or pending; and re-checking only the files already seen (no
        /// walk) decides the same as the full walk.
        #[test]
        fn scan_upload_side_follows_the_documented_rules(
            n in 1usize..4,
            sizes in pvec(0u8..4, 3),
            rows in pvec((0usize..4, 0usize..7, 0i64..5, 0usize..3), 0..4),
            prevs in pvec((0usize..4, 0usize..4), 0..4),
            mirror in any::<bool>(),
            sames in pvec((0usize..4, 0usize..2), 0..4),
            settle_ms in prop::sample::select(vec![1_000u64, 10_000]),
            net_vol in any::<bool>(),
        ) {
            let settle = Duration::from_millis(settle_ms);
            let tmp = tempfile::tempdir().expect("temp dir");
            let root = tmp.path().to_path_buf();
            let root_s = root.to_string_lossy().into_owned();

            let mut disk: Vec<(String, i64, i64)> = Vec::new(); // rel, size, mtime
            for i in 0..n {
                let rel = format!("take{}.mov", i);
                let size = *sizes.get(i % sizes.len()).unwrap_or(&1) as i64;
                std::fs::write(root.join(&rel), vec![b'x'; size as usize]).expect("file");
                let md = std::fs::metadata(root.join(&rel)).expect("meta");
                disk.push((rel, md.len() as i64, mtime_nanos(&md)));
            }

            let mut known: HashMap<String, WatchFile> = HashMap::new();
            for (idx, st, attempts, mismatch) in &rows {
                let (rel, size, mtime) = &disk[idx % disk.len()];
                let mut k = wf(rel, *size, *mtime, ROW_STATES[st % ROW_STATES.len()]);
                k.attempts = *attempts;
                match mismatch {
                    1 => k.size = size + 1,     // the file grew: a new version
                    2 => k.mtime += 1,          // the file was rewritten: a new version
                    _ => {}
                }
                known.insert(rel.clone(), k);
            }

            let now = Instant::now();
            let mut prev: HashMap<String, Obs> = HashMap::new();
            for (idx, bucket) in &prevs {
                let (rel, size, mtime) = &disk[idx % disk.len()];
                let age = bucket_age(*bucket, settle);
                prev.insert(rel.clone(), Obs { size: *size, mtime: *mtime, since: now - age });
            }

            let mut same: HashSet<String> = HashSet::new();
            for (idx, mode) in &sames {
                let (rel, size, _) = &disk[idx % disk.len()];
                if *mode == 0 {
                    same.insert(same_key(rel, *size));
                } else {
                    same.insert(same_key(rel, size + 7)); // a different size: not a match
                }
            }

            let out = scan_upload_side(&root_s, &prev, &known, true, settle, net_vol, mirror, if mirror { Some(&same) } else { None });
            prop_assert!(out.err.is_none(), "walk failed: {:?}", out.err);

            let mut want_seen: BTreeSet<String> = BTreeSet::new();
            let mut want_settled: BTreeSet<String> = BTreeSet::new();
            let mut want_frameio: BTreeSet<String> = BTreeSet::new();
            let mut want_pending = 0usize;
            let mut want_empty = 0usize;
            for (rel, size, mtime) in &disk {
                let (seen, kind) = expected(rel, *size, *mtime, known.get(rel), prev.get(rel), settle, net_vol, mirror, &same);
                if !seen {
                    prop_assert!(!out.seen.contains_key(rel), "{:?} was seen although its record still matches", rel);
                    continue;
                }
                want_seen.insert(rel.clone());
                match kind {
                    Some(Kind::Pending) => want_pending += 1,
                    Some(Kind::OnFrameio) => { want_frameio.insert(rel.clone()); }
                    Some(Kind::Settled) => { want_settled.insert(rel.clone()); }
                    None => want_empty += 1,
                }
            }
            let got_seen: BTreeSet<String> = out.seen.keys().cloned().collect();
            prop_assert_eq!(&got_seen, &want_seen);
            prop_assert_eq!(
                out.settled.iter().map(|i| i.rel.clone()).collect::<BTreeSet<_>>(),
                want_settled
            );
            prop_assert_eq!(
                out.on_frameio.iter().map(|r| r.rel_path.clone()).collect::<BTreeSet<_>>(),
                want_frameio
            );
            prop_assert_eq!(out.pending, want_pending);
            prop_assert_eq!(
                out.seen.len(),
                out.settled.len() + out.on_frameio.len() + out.pending + want_empty,
                "every seen file lands in exactly one bucket"
            );
            for (rel, size, mtime) in &disk {
                if let Some(o) = out.seen.get(rel) {
                    prop_assert_eq!((o.size, o.mtime), (*size, *mtime));
                }
            }
            for it in &out.settled {
                let (_, size, mtime) = disk.iter().find(|(r, _, _)| *r == it.rel).expect("a known file");
                prop_assert_eq!((it.size, it.mtime), (*size, *mtime));
                prop_assert_eq!(&it.local, &join_path(&root_s, &it.rel));
                prop_assert!(std::fs::metadata(&it.local).map(|m| m.len() as i64 == it.size).unwrap_or(false));
            }
            for r in &out.on_frameio {
                prop_assert_eq!(r.state.as_str(), WATCH_IGNORED);
            }
            prop_assert_eq!(out.dirs.is_some(), true, "a walk reports the folders it saw");

            // The incremental path (no walk, only the files already seen) must
            // decide exactly the same as the full walk.
            let prev2 = out.seen.clone();
            let inc = scan_upload_side(&root_s, &prev2, &known, false, settle, net_vol, mirror, if mirror { Some(&same) } else { None });
            prop_assert!(inc.err.is_none());
            prop_assert_eq!(
                inc.settled.iter().map(|i| i.rel.clone()).collect::<BTreeSet<_>>(),
                out.settled.iter().map(|i| i.rel.clone()).collect::<BTreeSet<_>>(),
                "the incremental re-check disagrees with the walk"
            );
            prop_assert_eq!(inc.pending, out.pending, "pending count");
            prop_assert_eq!(
                inc.on_frameio.iter().map(|r| r.rel_path.clone()).collect::<BTreeSet<_>>(),
                out.on_frameio.iter().map(|r| r.rel_path.clone()).collect::<BTreeSet<_>>()
            );
        }
    }

    // ---------- the remote listing ----------

    /// A one-thread HTTP server for the Frame.io replies a listing needs. Bounded
    /// by `stop`: set it, then `join`.
    fn serve(routes: HashMap<String, String>) -> (String, Arc<AtomicBool>, std::net::SocketAddr, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let join = std::thread::spawn(move || {
            for conn in l.incoming() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut c) = conn else { continue };
                let Ok(peek) = c.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" || h == "\n" {
                        break;
                    }
                }
                let path = line.split(' ').nth(1).unwrap_or("").split('?').next().unwrap_or("").to_string();
                let body = routes.get(&path).cloned().unwrap_or_else(|| r#"{"data":[],"links":{"next":null}}"#.to_string());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = c.write_all(resp.as_bytes());
            }
        });
        (format!("http://{}", addr), stop, addr, join)
    }

    #[test]
    fn list_remote_walks_the_tree_skips_names_and_takes_the_newest_version() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let mut routes: HashMap<String, String> = HashMap::new();
        routes.insert(
            "/v4/accounts/acct/folders/ROOT/children".into(),
            format!(
                r#"{{"data":[
  {{"id":"f1","name":"A.MOV","type":"file","file_size":5,"status":"uploaded"}},
  {{"id":"f2","name":"a.mov","type":"file","file_size":5,"status":"uploaded"}},
  {{"id":"f3","name":"b.fiopart","type":"file","file_size":5,"status":"uploaded"}},
  {{"id":"f6","name":"{}","type":"file","file_size":5,"status":"uploaded"}},
  {{"id":"f7","name":"pending.mov","type":"file","file_size":6,"status":"created"}},
  {{"id":"d1","name":"Sub","type":"folder"}},
  {{"id":"s1","name":"stack1","type":"version_stack","head_version":{{"id":"h1","name":"v2.mov","type":"file","file_size":7,"status":"uploaded"}}}},
  {{"id":"s2","name":"stack2","type":"version_stack"}}
],"links":{{"next":null}}}}"#,
                "FioTransfer Upload 2026-01-02 030405.txt"
            ),
        );
        routes.insert(
            "/v4/accounts/acct/folders/d1/children".into(),
            r#"{"data":[{"id":"f5","name":"deep.mov","type":"file","file_size":9,"status":"uploaded"}],"links":{}}"#.into(),
        );
        // The stack that names its head must not be asked for its children; if it
        // is, the reply says so.
        routes.insert(
            "/v4/accounts/acct/version_stacks/s1/children".into(),
            r#"{"data":[{"id":"WRONG","name":"wrong.mov","type":"file","file_size":1,"created_at":"2030-01-01T00:00:00Z"}],"links":{}}"#.into(),
        );
        routes.insert(
            "/v4/accounts/acct/version_stacks/s2/children".into(),
            r#"{"data":[
  {"id":"x1","name":"old.mov","type":"file","file_size":3,"created_at":"2020-01-01T00:00:00Z"},
  {"id":"x2","name":"new.mov","type":"file","file_size":4,"created_at":"2021-01-01T00:00:00Z"}
],"links":{}}"#.into(),
        );

        let (url, stop, addr, join) = serve(routes);
        let client = {
            let mut c = Client::new(crate::auth::AuthManager::new_static("test-token"));
            c.base_url = url;
            std::sync::Arc::new(c)
        };
        let cx = CancellationToken::new();
        let got = rt.block_on(list_remote(&cx, &client, "acct", "ROOT"));
        stop.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(addr); // wake the accept loop
        join.join().ok();

        let mut files = got.expect("listing");
        files.sort_by(|a, b| a.rel.cmp(&b.rel));
        let rows: Vec<(&str, &str, i64, bool)> =
            files.iter().map(|f| (f.id.as_str(), f.rel.as_str(), f.size, f.ready)).collect();
        assert_eq!(
            rows,
            vec![
                ("f1", "A.MOV", 5, true),
                ("f5", "Sub/deep.mov", 9, true),
                ("f2", "a (2).mov", 5, true),
                ("x2", "new.mov", 4, true),
                ("f7", "pending.mov", 6, false),
                ("h1", "v2.mov", 7, true),
            ],
            "listing: {:?}",
            rows
        );
    }
        /// The guard itself, without a seed: on a case-insensitive volume the second
        /// of two names that differ only by case must not enter the model, because
        /// the filesystem holds one file, not two.
        #[test]
        fn a_name_that_differs_only_by_case_does_not_enter_the_model_twice() {
            let tmp = tempfile::tempdir().expect("temp dir");
            let root = tmp.path().to_path_buf();
            let mut model = Model::default();
            assert!(place(&mut model, &root, &[], "B", 1), "the first name goes in");
            let second = place(&mut model, &root, &[], "b", 2);
            if fs_ignores_case() {
                assert!(!second, "a case-insensitive volume holds one file, so the second name is taken");
                assert_eq!(model.files.len(), 1, "{:?}", model.files);
            } else {
                assert!(second, "a case-sensitive volume holds two files");
                assert_eq!(model.files.len(), 2);
            }
        }

}
