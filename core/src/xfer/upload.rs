//! Uploading: presigned parts, verification, replacing versions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures::future::FutureExt;
use futures::Stream;
use tokio_util::sync::CancellationToken;

use super::complete::{media_state, MediaFormat};
use super::manager::{base_name, kind_tag, parent_dir};
use super::uploadstate;
use super::{File, Job, Manager, Status};
use crate::api::{Asset, Client};
use crate::history;
use crate::limiter::human_bytes;
use crate::netx;

/// `errChanged` stops an upload whose file changed while it was being sent.
const ERR_CHANGED: &str = "file changed while uploading";

/// The error Go's `context.Canceled` turns into.
const ERR_CANCELED: &str = "context canceled";

/// How long a part waits for the file to move before its attempt is abandoned.
const IDLE_LIMIT: Duration = Duration::from_secs(90);

/// How much of a part one read of the file hands the HTTP client.
/// How much of a part is read and written at a time. Bigger chunks mean fewer
/// read/write alternations on the socket, and so fewer moments when the socket
/// has nothing to send because the file read hasn't finished.
fn chunk_size() -> i64 {
    static SIZE: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("FIO_UPLOAD_CHUNK_MB")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .map(|mb| (mb.max(1) << 20))
            .unwrap_or(1 << 20)
    })
}

/// The read buffers every upload shares, so a part is read into memory that is
/// already faulted in and is reused for the next chunk.
fn read_pool() -> &'static std::sync::Arc<super::bufpool::BufPool> {
    static POOL: std::sync::OnceLock<std::sync::Arc<super::bufpool::BufPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| super::bufpool::BufPool::new(chunk_size() as usize))
}

/// How often CleanUp looks for unfinished uploads to delete.
const CLEAN_UP_EVERY: Duration = Duration::from_secs(10 * 60);

/// How long CleanUp waits for Frame.io to confirm a replacement before keeping
/// the earlier version for good.
const REPLACE_GIVE_UP: Duration = Duration::from_secs(24 * 60 * 60);

/// upload_file creates the file on Frame.io and PUTs all parts in parallel to
/// the presigned URLs Frame.io hands back. If the upload doesn't get all the
/// bytes there, the file is deleted from Frame.io again.
pub async fn upload_file(j: Arc<Job>, f: Arc<File>) -> Result<(), String> {
    upload_file_inner(j, f, true).await
}

/// `restart` allows one fresh attempt when a saved upload turns out to be dead:
/// its presigned URLs expire, and a resumed part then answers 403. Without this
/// the file would keep resuming into the same dead upload and never succeed.
fn upload_file_inner(
    j: Arc<Job>,
    f: Arc<File>,
    restart: bool,
) -> futures::future::BoxFuture<'static, Result<(), String>> {
    async move { upload_file_body(j, f, restart).await }.boxed()
}

async fn upload_file_body(j: Arc<Job>, f: Arc<File>, restart: bool) -> Result<(), String> {
    let Some(m) = j.mgr() else {
        return Err("transfer manager stopped".to_string());
    };
    let t0 = std::time::Instant::now();
    let st = wait_written(&j, &m, &f).await?;
    let path = f.local_path();
    let fh = match std::fs::File::open(&path) {
        Ok(h) => Arc::new(h),
        Err(e) => return Err(format!("open {}: {}", path, e)),
    };
    let size = st.size;
    if size != f.size() {
        j.add_total(size - f.size());
        // The file grew or shrank while the upload waited for it to settle.
        f.set_size(size);
    }
    if size == 0 {
        f.set_status(Status::Skipped, "empty file (Frame.io does not accept 0-byte files)");
        return Ok(());
    }

    let acct = j.account_id.lock().unwrap().clone();
    let parent = f.remote_parent.lock().unwrap().clone();

    // A previous attempt at this same file, to the same place, that stopped
    // before every part was on Frame.io: pick it up rather than resend it all.
    let ledger = Arc::new(uploadstate::Ledger::for_source(&path));
    let mtime = uploadstate::mtime_ms(std::path::Path::new(&path));
    let now = uploadstate::now_ms();
    let candidate = ledger.load(size, mtime, now);
    let saved = match candidate {
        Some((saved, done)) if saved.account_id == acct && saved.folder_id == parent && saved.name == f.name => {
            Some((saved, done))
        }
        other => {
            // Wrong destination, or a stale ledger: drop the copy it belongs to
            // and start again.
            if let Some((old, _)) = other {
                let _ = m.api.delete_file(&j.cx, &old.account_id, &old.file_id).await;
                mark_deleted(&m.hist, &old.file_id);
                ledger.remove();
            } else if ledger.exists() {
                ledger.remove();
            }
            None
        }
    };

    let (file_id, view_url, parts, done, resuming) = match saved {
        Some((saved, done)) => {
            let parts: Vec<Part> = saved
                .parts
                .iter()
                .map(|p| Part { off: p.off, size: p.size, url: p.url.clone() })
                .collect();
            let already = done.iter().filter(|d| **d).count();
            (saved.file_id, saved.view_url, parts, done, already)
        }
        None => {
            let lu = match m.api.create_local_upload(&j.cx, &acct, &parent, &f.name, size).await {
                Ok(l) => l,
                Err(e) => return Err(format!("creating file on Frame.io: {}", e)),
            };
            let mut parts: Vec<Part> = Vec::with_capacity(lu.upload_urls.len());
            let mut off: i64 = 0;
            for p in &lu.upload_urls {
                parts.push(Part { off, size: p.size, url: p.url.clone() });
                off += p.size;
            }
            let state = uploadstate::UploadState {
                source: path.clone(),
                file_id: lu.id.clone(),
                account_id: acct.clone(),
                folder_id: parent.clone(),
                name: f.name.clone(),
                size,
                mtime_ms: mtime,
                created_at: now,
                view_url: lu.view_url.clone(),
                parts: parts
                    .iter()
                    .map(|p| uploadstate::PartState { off: p.off, size: p.size, url: p.url.clone() })
                    .collect(),
            };
            if let Err(e) = ledger.save(&state) {
                // Not fatal: the upload still works, it just can't be resumed.
                m.hist.log(&format!("{} resume   [{}] {} — couldn't record the upload for resuming: {}", kind_tag(&j.kind), j.id, f.rel_path, e));
            }
            let fresh_flags = vec![false; parts.len()];
            (lu.id, lu.view_url, parts, fresh_flags, 0)
        }
    };

    *f.remote_id.lock().unwrap() = file_id.clone();
    *f.view_url.lock().unwrap() = view_url.clone();
    if j.link_from_file.load(Ordering::Relaxed) {
        j.set_link(&view_url);
    }
    // Until every byte is sent, Frame.io holds an unusable file. The row lets
    // clean_up delete it should the app quit or crash first.
    mark_unfinished(&m.hist, &file_id, &acct, &j.id, &f.rel_path, std::process::id() as i64, &m.run);
    let t_settle = t0.elapsed();
    let started = Instant::now();

    let content_type = "application/octet-stream".to_string();
    if resuming > 0 {
        m.hist.log(&format!(
            "{} resume   [{}] {} — {}/{} parts already on Frame.io",
            kind_tag(&j.kind),
            j.id,
            f.rel_path,
            resuming,
            parts.len()
        ));
        f.set_note(&format!("resuming, {} of {} parts already sent", resuming, parts.len()));
    }

    // Everything from here on owns a file on Frame.io: if the upload doesn't
    // get all the bytes there, the copy is deleted again — unless the user
    // stopped it, in which case it is kept so the next attempt can carry on.
    let outcome = send_parts(&j, &f, &m, fh, &parts, size, &st, &content_type, &done, &ledger).await;
    let stopped = j.cx.is_cancelled();
    let expired = resuming > 0 && matches!(&outcome, Err(e) if e.contains("403")) && !stopped;
    let outcome = match outcome {
        Ok(()) => Ok(()),
        Err(_) if expired && restart => {
            // The saved upload's URLs are dead: drop it and start this file
            // again from nothing, once.
            ledger.remove();
            discard(&j, &f, &file_id).await;
            m.hist.log(&format!(
                "{} resume   [{}] {} — the saved upload had expired; starting it again",
                kind_tag(&j.kind),
                j.id,
                f.rel_path
            ));
            return upload_file_inner(j.clone(), f.clone(), false).await;
        }
        Err(e) => {
            if stopped {
                m.hist.log(&format!(
                    "{} paused   [{}] {} — kept on Frame.io so it can be resumed ({})",
                    kind_tag(&j.kind),
                    j.id,
                    f.rel_path,
                    e
                ));
            } else {
                discard(&j, &f, &file_id).await;
                ledger.remove();
            }
            Err(e)
        }
    };
    outcome?;
    if std::env::var("FIO_TRACE").is_ok() {
        eprintln!("[trace] {} settle+create={:?} parts={:?}", f.rel_path, t_settle, started.elapsed());
    }

    // Every byte is there: the ledger has done its job.
    ledger.remove();
    mark_sent(&m.hist, &file_id);
    let secs = started.elapsed().as_secs_f64();
    let first = parts.first().map(|p| p.size).unwrap_or(0);
    *f.detail.lock().unwrap() = format!(
        "{} parts × {}, {:.1}s, {}/s",
        parts.len(),
        human_bytes(first),
        secs,
        human_bytes((size as f64 / secs.max(0.001)) as i64)
    );
    let old = f.replaces.lock().unwrap().clone();
    if !old.is_empty() {
        // clean_up finishes the replace should the app quit before Frame.io confirms.
        mark_replaced(&m.hist, &old, &acct, &file_id, &j.id, &f.rel_path, std::process::id() as i64, &m.run);
    }

    // All bytes are on S3; Frame.io assembles the parts. Confirm in the background.
    verify_later(&j, &f, &file_id);
    Ok(())
}

/// One presigned part: where its bytes start in the file, and where to PUT them.
struct Part {
    off: i64,
    size: i64,
    url: String,
}

/// Why the upload stopped, when it wasn't plain success.
enum Cause {
    /// The file changed while it was being sent.
    Changed,
    /// A part gave up.
    Part(String),
}

/// Sends every part, stopping as soon as the file changes or a part fails.
async fn send_parts(
    j: &Arc<Job>,
    f: &Arc<File>,
    m: &Arc<Manager>,
    fh: Arc<std::fs::File>,
    parts: &[Part],
    size: i64,
    st: &Stat,
    content_type: &str,
    done: &[bool],
    ledger: &Arc<uploadstate::Ledger>,
) -> Result<(), String> {
    let mut total = 0i64;
    for p in parts {
        total += p.size;
    }
    if total != size {
        return Err(format!(
            "Frame.io part sizes ({}) don't add up to the file size ({})",
            total, size
        ));
    }

    let cx = j.cx.child_token(); // context.WithCancelCause
    let cause: Arc<Mutex<Option<Cause>>> = Arc::new(Mutex::new(None));

    // Stop as soon as the file changes, rather than after sending all of it.
    let watcher = {
        let cx = cx.clone();
        let cause = cause.clone();
        let path = f.local_path();
        let st = st.clone();
        let period = m.change_check.max(Duration::from_millis(50));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            tick.tick().await; // a ticker waits one period before its first tick
            loop {
                tokio::select! {
                    _ = cx.cancelled() => return,
                    _ = tick.tick() => {
                        if changed_since(&path, &st) {
                            let mut g = cause.lock().unwrap();
                            if g.is_none() {
                                *g = Some(Cause::Changed);
                            }
                            drop(g);
                            cx.cancel();
                            return;
                        }
                    }
                }
            }
        })
    };

    // One idle watchdog for the whole file, not one per part: a task and a timer
    // per part is pure overhead on a fast link.
    let last = Arc::new(AtomicI64::new(now_nanos()));
    let watchdog = {
        let cx = cx.clone();
        let last = last.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = cx.cancelled() => return,
                    _ = tick.tick() => {
                        let idle = Duration::from_nanos((now_nanos() - last.load(Ordering::Relaxed)).max(0) as u64);
                        if idle > IDLE_LIMIT {
                            cx.cancel();
                            return;
                        }
                    }
                }
            }
        })
    };

    let mut handles = Vec::with_capacity(parts.len());
    for (index, p) in parts.iter().enumerate() {
        let f = f.clone();
        let fh = fh.clone();
        let cx = cx.clone();
        let cause = cause.clone();
        let limiter = m.limiter.clone();
        let (off, psize, url) = (p.off, p.size, p.url.clone());
        let ct = content_type.to_string();
        let last = last.clone();
        if done.get(index).copied().unwrap_or(false) {
            // This part is already on Frame.io from an earlier attempt: count
            // its bytes as done and leave it alone.
            f.credit(psize);
            continue;
        }
        let ledger = ledger.clone();
        handles.push(tokio::spawn(async move {
            let permit = match limiter.acquire(&cx).await {
                Ok(p) => p,
                Err(_) => return,
            };
            match put_part(f, fh, off, psize, &url, &ct, &cx, last).await {
                Ok(()) => ledger.mark_done(index),
                Err(e) => {
                    let mut g = cause.lock().unwrap();
                    if g.is_none() {
                        *g = Some(Cause::Part(e));
                    }
                    drop(g);
                    cx.cancel();
                }
            }
            drop(permit);
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    watcher.abort();
    watchdog.abort();

    // Frame.io only knows the bytes it was sent, so it would accept a copy taken
    // while something was still writing the file.
    let cause = cause.lock().unwrap().take();
    match cause {
        Some(Cause::Changed) => Err(still_writing(j, f)),
        Some(Cause::Part(e)) => Err(e),
        None if changed_since(&f.local_path(), st) => Err(still_writing(j, f)),
        None if cx.is_cancelled() => Err(ERR_CANCELED.to_string()),
        None => Ok(()),
    }
}

/// A file's size and modification time, as the settle and change checks compare them.
#[derive(Clone)]
struct Stat {
    size: i64,
    modified: Option<SystemTime>,
}

fn stat_of(path: &str) -> Result<Stat, String> {
    let md = std::fs::metadata(path).map_err(|e| format!("stat {}: {}", path, e))?;
    Ok(Stat { size: md.len() as i64, modified: md.modified().ok() })
}

/// wait_written returns the file's info once nothing has written to it for the
/// settle time, so a render or copy still in progress isn't sent half-finished.
/// It counts as settled by the file's own timestamp or, should the source's
/// clock be off, by watching it stay unchanged for that long. A QuickTime/MXF
/// file must also be finished (see media_state); one that stops changing before
/// it is fails after the stall limit.
async fn wait_written(j: &Arc<Job>, m: &Arc<Manager>, f: &Arc<File>) -> Result<Stat, String> {
    let settle = m.upload_settle;
    let mut last: Option<Stat> = None;
    let mut steady = Instant::now();
    let mut waiting = false;
    let outcome = loop {
        let path = f.local_path();
        let st = match stat_of(&path) {
            Ok(s) => s,
            Err(e) => break Err(e),
        };
        let moved = match &last {
            None => true,
            Some(l) => l.size != st.size || l.modified != st.modified,
        };
        if moved {
            last = Some(st.clone());
            steady = Instant::now();
        }
        let by_mtime = match st.modified {
            Some(mt) => match SystemTime::now().duration_since(mt) {
                Ok(d) => d >= settle,
                Err(_) => false, // its clock is ahead of ours; the steady time decides
            },
            None => false,
        };
        if by_mtime || steady.elapsed() >= settle {
            if media_state(&path, st.size) != MediaFormat::Unfinished {
                break Ok(st);
            }
            if steady.elapsed() >= m.stall_limit {
                break Err("file is incomplete and stopped changing".to_string());
            }
        }
        if !waiting {
            waiting = true;
            f.set_note("still being written");
            m.touch();
        }
        let wait = (settle / 4).min(Duration::from_secs(1)).max(Duration::from_millis(10));
        if netx::sleep(&j.cx, wait).await.is_err() {
            break Err(ERR_CANCELED.to_string());
        }
    };
    if waiting {
        f.set_note("");
        m.touch();
    }
    outcome
}

/// changed_since reports whether a file's size or mtime differ from st.
fn changed_since(path: &str, st: &Stat) -> bool {
    match stat_of(path) {
        Ok(now) => now.size != st.size || now.modified != st.modified,
        Err(_) => true,
    }
}

/// still_writing marks a file that changed while it was being sent.
fn still_writing(j: &Arc<Job>, f: &Arc<File>) -> String {
    f.inner.lock().unwrap().still_writing = true;
    if !j.watch_id.lock().unwrap().is_empty() {
        return "still being written; uploads once finished".to_string();
    }
    ERR_CHANGED.to_string()
}

/// discard deletes a file's copy on Frame.io after its upload didn't get all
/// the bytes there, so nothing unusable is left behind. If Frame.io can't be
/// reached, clean_up tries again later.
async fn discard(j: &Arc<Job>, f: &Arc<File>, id: &str) {
    let Some(m) = j.mgr() else { return };
    let h = m.hist.clone();
    let acct = j.account_id.lock().unwrap().clone();
    mark_unfinished(&h, id, &acct, &j.id, &f.rel_path, 0, ""); // no owner: delete it
    match delete_with(&m, &acct, id, Duration::from_secs(30)).await {
        Err(e) => {
            h.log(&format!(
                "{} cleanup  [{}] {} — couldn't delete the unfinished copy from Frame.io (will retry): {}",
                kind_tag(&j.kind),
                j.id,
                f.rel_path,
                e
            ));
            return;
        }
        Ok(()) => {}
    }
    mark_deleted(&h, id);
    let view = f.view_url.lock().unwrap().clone();
    let mine = f.remote_id.lock().unwrap().clone() == id;
    if mine {
        *f.remote_id.lock().unwrap() = String::new();
        *f.view_url.lock().unwrap() = String::new();
    }
    if !view.is_empty() && j.link() == view {
        // a single file's page: gone now
        *j.link.lock().unwrap() = String::new();
    }
    h.log(&format!(
        "{} cleanup  [{}] {} — deleted the unfinished copy from Frame.io",
        kind_tag(&j.kind),
        j.id,
        f.rel_path
    ));
}

/// delete_replaced deletes the earlier version a confirmed upload replaces. If
/// Frame.io can't be reached, clean_up tries again later.
async fn delete_replaced(j: &Arc<Job>, f: &Arc<File>, old: &str, new_id: &str) {
    let Some(m) = j.mgr() else { return };
    let h = m.hist.clone();
    let acct = j.account_id.lock().unwrap().clone();
    // No owner: clean_up's to finish if this delete fails.
    mark_replaced(&h, old, &acct, new_id, &j.id, &f.rel_path, 0, "");
    // Deletes are 100/min, so a big batch of re-renders queues for them.
    if let Err(e) = delete_with(&m, &acct, old, Duration::from_secs(120)).await {
        h.log(&format!(
            "{} replace  [{}] {} — couldn't delete the earlier version {} from Frame.io (will retry): {}",
            kind_tag(&j.kind),
            j.id,
            f.rel_path,
            old,
            e
        ));
        return;
    }
    mark_deleted(&h, old);
    f.set_note("replaced the earlier version");
    h.log(&format!(
        "{} replaced [{}] {} — deleted the earlier version {} from Frame.io",
        kind_tag(&j.kind),
        j.id,
        f.rel_path,
        old
    ));
}

/// Deletes a file on Frame.io within `within`, with no owner to cancel it: it
/// runs whether or not the transfer is being stopped.
async fn delete_with(m: &Arc<Manager>, acct: &str, id: &str, within: Duration) -> Result<(), String> {
    let cx = CancellationToken::new();
    match tokio::time::timeout(within, delete_file(m, &cx, acct, id)).await {
        Ok(r) => r,
        Err(_) => Err("context deadline exceeded".to_string()),
    }
}

/// Deletes a file on Frame.io; one that's already gone counts as deleted.
async fn delete_file(m: &Arc<Manager>, cx: &CancellationToken, acct: &str, id: &str) -> Result<(), String> {
    match m.api.delete_file(cx, acct, id).await {
        Ok(()) => Ok(()),
        Err(e) if e.status() == Some(404) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// clean_up deletes Frame.io copies of uploads that never finished, 30 s after
/// it starts and then every 10 minutes: ones whose delete failed, and ones left
/// by a run that quit or crashed mid-upload. It also deletes earlier versions
/// whose replacement Frame.io has confirmed since. The app spawns it.
pub async fn clean_up(m: Arc<Manager>) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        clean_up_once(&m).await;
        tokio::time::sleep(CLEAN_UP_EVERY).await;
    }
}

async fn clean_up_once(m: &Arc<Manager>) {
    if !m.api.auth.logged_in() {
        return;
    }
    let Ok(list) = m.hist.unfinished_uploads() else {
        return;
    };
    let now = uploadstate::now_ms();
    for u in list {
        if owned(m, u.pid, &u.run).await {
            continue; // still being sent (here, or by the CLI)
        }
        if uploadstate::keeps_file(&u.file_id, now) {
            continue; // an interrupted upload that can still be resumed
        }
        if delete_with(m, &u.account_id, &u.file_id, Duration::from_secs(30)).await.is_err() {
            continue;
        }
        mark_deleted(&m.hist, &u.file_id);
        m.hist.log(&format!(
            "UPLOAD   cleanup  [{}] {} — deleted the unfinished copy from Frame.io",
            u.transfer_id, u.rel_path
        ));
    }
    finish_replaces(m).await;
}

/// finish_replaces deletes earlier versions whose replacement Frame.io confirmed
/// after its upload's own checks stopped (the app quit, or the batch was
/// canceled). If the replacement failed or is gone, the earlier version stays.
async fn finish_replaces(m: &Arc<Manager>) {
    let Ok(list) = m.hist.replaced_files() else {
        return;
    };
    for r in list {
        if owned(m, r.pid, &r.run).await {
            continue; // its upload is still checking
        }
        let scx = CancellationToken::new();
        let st = tokio::time::timeout(
            Duration::from_secs(30),
            m.api.file_status(&scx, &r.account_id, &r.new_id),
        )
        .await;
        let gone = matches!(&st, Ok(Err(e)) if e.status() == Some(404));
        let failed = matches!(&st, Ok(Ok(s)) if s.upload_failed);
        let stale = matches!(&st, Ok(Ok(s)) if !s.upload_complete)
            && now_ms() - r.created_at > REPLACE_GIVE_UP.as_millis() as i64;
        if gone || failed || stale {
            keep_replaced(&m.hist, &r.file_id);
            m.hist.log(&format!(
                "UPLOAD   cleanup  [{}] {} — kept the earlier version on Frame.io: the new one didn't finish there",
                r.transfer_id, r.rel_path
            ));
            continue;
        }
        match &st {
            Ok(Ok(s)) if s.upload_complete => {}
            _ => continue, // ask again next time
        }
        if delete_with(m, &r.account_id, &r.file_id, Duration::from_secs(30)).await.is_err() {
            continue;
        }
        mark_deleted(&m.hist, &r.file_id);
        m.hist.log(&format!(
            "UPLOAD   replaced [{}] {} — deleted the earlier version {} from Frame.io",
            r.transfer_id, r.rel_path, r.file_id
        ));
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// owned reports whether a row's owner (PID and run) is still working on it:
/// this run, or another live process (the CLI).
async fn owned(m: &Manager, pid: i64, run: &str) -> bool {
    pid != 0 && (run == m.run || (pid != std::process::id() as i64 && process_alive(pid).await))
}

/// Whether a process is there. Go used `kill(pid, 0)`: no error, or EPERM
/// because it belongs to someone else, both mean it is running.
async fn process_alive(pid: i64) -> bool {
    match tokio::process::Command::new("/bin/kill").arg("-0").arg(pid.to_string()).output().await {
        Ok(out) if out.status.success() => true,
        Ok(out) => String::from_utf8_lossy(&out.stderr).contains("not permitted"),
        Err(_) => false,
    }
}

/// verify_later waits for Frame.io to confirm it assembled the file, without
/// holding an upload slot. The file is logged as uploaded only once Frame.io
/// confirms it; a failure it reports, or no confirmation at all, fails it.
fn verify_later(j: &Arc<Job>, f: &Arc<File>, file_id: &str) {
    {
        let mut g = f.inner.lock().unwrap();
        g.check_later = true;
        g.note = "verifying on Frame.io".to_string();
    }
    let (j2, f2, id) = (j.clone(), f.clone(), file_id.to_string());
    let handle = tokio::spawn(async move { verify_run(j2, f2, id).await });
    j.verify_tasks.lock().unwrap().push(handle);
}

async fn verify_run(j: Arc<Job>, f: Arc<File>, file_id: String) {
    let old = f.replaces.lock().unwrap().clone();
    let Some(m) = j.mgr() else { return };
    let acct = j.account_id.lock().unwrap().clone();
    let mut last_err: Option<String> = None;
    for d in m.verify_delays.clone() {
        if netx::sleep(&j.cx, d).await.is_err() {
            f.set_note("not verified");
            j.persist_file(&f);
            if !old.is_empty() {
                // clean_up checks the new upload and deletes the old one
                mark_replaced(&m.hist, &old, &acct, &file_id, &j.id, &f.rel_path, 0, "");
            }
            return;
        }
        match m.api.file_status(&j.cx, &acct, &file_id).await {
            Err(e) => last_err = Some(e.to_string()),
            Ok(s) if s.upload_failed => {
                verify_failed(&j, &f, "Frame.io reported the upload as failed", None).await;
                return;
            }
            Ok(s) if s.upload_complete => {
                f.set_note("");
                j.log_file_ok(&f);
                if !old.is_empty() {
                    delete_replaced(&j, &f, &old, &file_id).await;
                }
                j.persist_file(&f);
                if let Some(mm) = j.mgr() {
                    mm.touch();
                }
                return;
            }
            Ok(_) => {}
        }
    }
    verify_failed(&j, &f, "Frame.io didn't confirm the upload", last_err).await;
}

async fn verify_failed(j: &Arc<Job>, f: &Arc<File>, msg: &str, cause: Option<String>) {
    let id = f.remote_id.lock().unwrap().clone();
    let old = f.replaces.lock().unwrap().clone();
    if !id.is_empty() {
        discard(j, f, &id).await;
    }
    if !old.is_empty() {
        if let Some(m) = j.mgr() {
            keep_replaced(&m.hist, &old);
        }
    }
    {
        let mut g = f.inner.lock().unwrap();
        f.set_status_locked(&mut g, Status::Failed);
        g.err = msg.to_string();
        g.note = String::new();
    }
    j.persist_file(f);
    let mut line = msg.to_string();
    if let Some(c) = cause {
        line = format!("{} (last check: {})", msg, c);
    }
    if let Some(m) = j.mgr() {
        m.hist
            .log(&format!("{} FAILED   [{}] {} — {}", kind_tag(&j.kind), j.id, f.rel_path, line));
    }
    if let Some(cb) = j.on_file_done.lock().unwrap().clone() {
        cb(f.clone());
    }
    if let Some(m) = j.mgr() {
        m.touch();
    }
}

/// PUTs one part, retrying as the Go code does: a rejected URL or a refusal by
/// storage stops it at once, anything else is tried again.
async fn put_part(
    f: Arc<File>,
    fh: Arc<std::fs::File>,
    off: i64,
    size: i64,
    url: &str,
    content_type: &str,
    cx: &CancellationToken,
    last: Arc<AtomicI64>,
) -> Result<(), String> {
    let mut last_err = String::new();
    for attempt in 0..10u32 {
        if cx.is_cancelled() {
            return Err(ERR_CANCELED.to_string());
        }
        if attempt > 0 {
            netx::sleep(cx, netx::backoff(attempt)).await.map_err(|_| ERR_CANCELED.to_string())?;
        }
        let sent = Arc::new(AtomicI64::new(0));
        let progress: Progress = {
            let sent = sent.clone();
            let f = f.clone();
            Arc::new(move |n: i64| {
                sent.fetch_add(n, Ordering::Relaxed);
                f.add(n);
            })
        };
        match put_once(cx, url, fh.clone(), off, size, content_type, progress, last.clone()).await {
            Ok(status) if (200..300).contains(&status) => {
                // Make sure progress matches exactly what the part holds.
                let d = size - sent.load(Ordering::Relaxed);
                if d != 0 {
                    f.credit(d);
                }
                return Ok(());
            }
            Ok(status) => {
                f.unwind(sent.load(Ordering::Relaxed)); // this attempt's bytes will be resent
                f.retries.fetch_add(1, Ordering::Relaxed);
                match status {
                    403 => {
                        return Err("upload URL rejected or expired (HTTP 403) — retry the transfer".to_string());
                    }
                    400 | 404 | 405 | 411 | 413 => {
                        return Err(format!("upload rejected by storage (HTTP {})", status));
                    }
                    _ => last_err = format!("HTTP {}", status),
                }
            }
            Err(e) => {
                f.unwind(sent.load(Ordering::Relaxed));
                f.retries.fetch_add(1, Ordering::Relaxed);
                last_err = e;
            }
        }
    }
    Err(format!("giving up after retries: {}", last_err))
}

/// What a part reports as it is read from the file.
type Progress = Arc<dyn Fn(i64) + Send + Sync>;

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// PUTs one part once and reports the status. An idle watchdog aborts it if
/// nothing moves for 90 s, which covers the wait for S3's reply too.
async fn put_once(
    cx: &CancellationToken,
    url: &str,
    fh: Arc<std::fs::File>,
    off: i64,
    size: i64,
    content_type: &str,
    progress: Progress,
    last: Arc<AtomicI64>,
) -> Result<u16, String> {
    let actx = cx.child_token();
    let body = reqwest::Body::wrap_stream(SectionStream {
        fh,
        pos: off,
        remaining: size,
        progress,
        last: last.clone(),
        inflight: std::collections::VecDeque::new(),
        short: false,
    });
    let request = netx::data_client()
        .put(url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .header("x-amz-acl", "private")
        .header(reqwest::header::CONTENT_LENGTH, size)
        .body(body);

    let resp = tokio::select! {
        r = request.send() => r,
        _ = actx.cancelled() => return Err(ERR_CANCELED.to_string()),
    };
    let mut resp = match resp {
        Ok(r) => r,
        Err(e) => return Err(e.to_string()),
    };
    let status = resp.status().as_u16();
    // Drain the reply (S3's is a few hundred bytes) with one bounded read, so
    // the connection goes back to the pool ready for the next part.
    let _ = tokio::time::timeout(Duration::from_secs(10), resp.bytes()).await;
    Ok(status)
}

/// One read in flight: a chunk of the file being brought into memory on the
/// blocking pool, with the offset it starts at.
type ReadHandle = tokio::task::JoinHandle<Result<super::bufpool::PooledBuf, std::io::Error>>;

/// The bytes of one part, read from the file as the request body asks for them
/// and reported as they go, so the meter keeps up with what is really sent.
///
/// Two things make this keep a socket busy:
///  * the read happens on the blocking pool, because a megabyte `pread` is a
///    blocking syscall and would otherwise stop a runtime worker from driving
///    the socket that is waiting for those bytes;
///  * one chunk is always read *ahead*, so the socket never runs dry while the
///    file is being read.
struct SectionStream {
    fh: Arc<std::fs::File>,
    pos: i64,
    remaining: i64,
    progress: Progress,
    last: Arc<AtomicI64>,
    /// Reads started and not yet handed to the socket, oldest first, with the
    /// length each was asked for.
    inflight: std::collections::VecDeque<(usize, ReadHandle)>,
    /// A read came back short: what the file has goes first, then the part
    /// fails, because a short part would upload a truncated copy.
    short: bool,
}

/// How many chunks may be read ahead of the socket.
const READ_AHEAD: usize = 2;

impl SectionStream {
    fn start_read(&mut self) {
        let want = self.remaining.min(chunk_size()) as usize;
        if want == 0 {
            return;
        }
        let fh = self.fh.clone();
        let off = self.pos;
        let handle = tokio::task::spawn_blocking(move || {
            let mut buf = read_pool().take();
            buf.set_len(want);
            let mut got = 0usize;
            {
                let dst = buf.as_mut_slice();
                while got < want {
                    match crate::fileio::read_at(&fh, &mut dst[got..], off as u64 + got as u64) {
                        Ok(0) => break,
                        Ok(k) => got += k,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
            if got == 0 {
                // The file shrank under us: the part can't be sent whole.
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the file ended before its part did",
                ));
            }
            buf.set_len(got);
            Ok(buf)
        });
        let _ = off;
        self.inflight.push_back((want, handle));
        self.pos += want as i64;
        self.remaining -= want as i64;
    }
}

impl Stream for SectionStream {
    type Item = Result<bytes::Bytes, std::io::Error>;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let me = &mut *self;
        if me.short {
            return std::task::Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the file ended before its part did",
            ))));
        }
        // Keep the read-ahead window full.
        while me.inflight.len() < READ_AHEAD && me.remaining > 0 {
            me.start_read();
        }
        let Some((want, handle)) = me.inflight.front_mut() else {
            return std::task::Poll::Ready(None);
        };
        let want = *want;
        match std::future::Future::poll(std::pin::Pin::new(handle), cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(joined) => {
                me.inflight.pop_front();
                let buf = match joined {
                    Ok(Ok(b)) => b,
                    Ok(Err(e)) => return std::task::Poll::Ready(Some(Err(e))),
                    Err(e) => {
                        return std::task::Poll::Ready(Some(Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        ))))
                    }
                };
                if buf.as_ref().len() < want {
                    // The file ended early: send what is there, then fail.
                    me.short = true;
                }
                (me.progress)(buf.as_ref().len() as i64);
                me.last.store(now_nanos(), Ordering::Relaxed);
                std::task::Poll::Ready(Some(Ok(buf.into_bytes())))
            }
        }
    }
}

impl Drop for SectionStream {
    fn drop(&mut self) {
        // A cancelled or failed request leaves reads that nobody will collect.
        for (_, h) in self.inflight.drain(..) {
            h.abort();
        }
    }
}

/// ensure_folders creates the remote folder tree for an upload, returning a map
/// from relative directory ("" = destination root) to Frame.io folder ID, and
/// one from relative directory to the folder's Frame.io page.
pub async fn ensure_folders(
    c: &Client,
    cx: &CancellationToken,
    acct: &str,
    root: &str,
    dirs: &[String],
) -> Result<(HashMap<String, String>, HashMap<String, String>), String> {
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut urls: HashMap<String, String> = HashMap::new();
    ids.insert(String::new(), root.to_string());
    for d in dirs {
        // dirs are sorted so parents come first
        if ids.contains_key(d) {
            continue;
        }
        let parent = parent_dir(d);
        let Some(pid) = ids.get(&parent).cloned() else {
            return Err(format!("internal error: parent of {:?} not created", d));
        };
        let folder = create_folder(c, cx, acct, &pid, base_name(d))
            .await
            .map_err(|e| format!("creating folder {:?}: {}", d, e))?;
        ids.insert(d.clone(), folder.id.clone());
        urls.insert(d.clone(), folder.view_url.clone());
    }
    Ok((ids, urls))
}

/// create_folder creates a folder. When a request fails in a way that leaves it
/// unclear whether Frame.io made the folder (a timeout or gateway error), the
/// parent is checked for it before trying again, so a retry never duplicates it.
async fn create_folder(c: &Client, cx: &CancellationToken, acct: &str, parent: &str, name: &str) -> Result<Asset, String> {
    let mut attempt: u32 = 1;
    loop {
        let err = match c.create_folder(cx, acct, parent, name).await {
            Ok(f) => return Ok(f),
            Err(e) => e,
        };
        if !err.ambiguous() || attempt > 3 {
            return Err(err.to_string());
        }
        if netx::sleep(cx, netx::backoff(attempt)).await.is_err() {
            return Err(err.to_string());
        }
        attempt += 1;
        let kids = match c.children(cx, acct, parent).await {
            Ok(k) => k,
            Err(_) => continue,
        };
        let found = kids.into_iter().find(|k| k.type_ == "folder" && k.name == name);
        if let Some(k) = found {
            if let Ok(full) = c.folder(cx, acct, &k.id).await {
                return Ok(full); // includes the view URL
            }
            return Ok(k);
        }
    }
}

// ---------- History bookkeeping ----------
//
// The rows live in src/history/unfinished.rs; these are the only calls into it,
// so this file needs one place changed if that module's names differ.

fn mark_unfinished(h: &history::Store, file_id: &str, acct: &str, transfer_id: &str, rel_path: &str, pid: i64, run: &str) {
    h.add_unfinished(&history::unfinished::Unfinished {
        file_id: file_id.to_string(),
        account_id: acct.to_string(),
        transfer_id: transfer_id.to_string(),
        rel_path: rel_path.to_string(),
        pid,
        run: run.to_string(),
        created_at: 0, // the store stamps it
    });
}

fn mark_replaced(h: &history::Store, file_id: &str, acct: &str, new_id: &str, transfer_id: &str, rel_path: &str, pid: i64, run: &str) {
    h.add_replaced(&history::unfinished::Replaced {
        file_id: file_id.to_string(),
        account_id: acct.to_string(),
        new_id: new_id.to_string(),
        transfer_id: transfer_id.to_string(),
        rel_path: rel_path.to_string(),
        pid,
        run: run.to_string(),
        created_at: 0, // the store stamps it
    });
}

fn mark_sent(h: &history::Store, file_id: &str) {
    h.upload_sent(file_id);
}

fn mark_deleted(h: &history::Store, file_id: &str) {
    h.upload_deleted(file_id);
}

fn keep_replaced(h: &history::Store, file_id: &str) {
    h.keep_replaced(file_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// A file of `bytes` in the temp folder, ready to be read.
    fn temp_file(bytes: &[u8]) -> (std::path::PathBuf, Arc<std::fs::File>) {
        let mut p = std::env::temp_dir();
        p.push(format!("fio-section-{}-{}", std::process::id(), now_nanos()));
        std::fs::write(&p, bytes).expect("write");
        let fh = Arc::new(std::fs::File::open(&p).expect("open"));
        (p, fh)
    }

    fn counting_progress(moved: Arc<AtomicI64>) -> Progress {
        Arc::new(move |n: i64| {
            moved.fetch_add(n, Ordering::Relaxed);
        })
    }

    async fn read_all(s: &mut SectionStream) -> Vec<u8> {
        let mut got = Vec::new();
        while let Some(chunk) = s.next().await {
            got.extend_from_slice(&chunk.expect("chunk"));
        }
        got
    }

    fn stream_at(fh: std::sync::Arc<std::fs::File>, pos: i64, remaining: i64, moved: std::sync::Arc<AtomicI64>) -> SectionStream {
        SectionStream {
            fh,
            pos,
            remaining,
            progress: counting_progress(moved),
            last: std::sync::Arc::new(AtomicI64::new(now_nanos())),
            inflight: std::collections::VecDeque::new(),
            short: false,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_part_carries_only_its_own_bytes() {
        let chunk = chunk_size();
        let data: Vec<u8> = (0..(chunk as usize * 2 + 7)).map(|i| (i % 251) as u8).collect();
        let (path, fh) = temp_file(&data);
        let moved = std::sync::Arc::new(AtomicI64::new(0));
        // Two parts of the same file read at once, as the parallel PUTs do.
        let mut a = stream_at(fh.clone(), 0, chunk, moved.clone());
        let mut b = stream_at(fh.clone(), chunk, 7, moved.clone());
        let (ga, gb) = (read_all(&mut a).await, read_all(&mut b).await);
        assert_eq!(ga, data[..chunk as usize].to_vec());
        assert_eq!(gb, data[chunk as usize..chunk as usize + 7].to_vec());
        assert_eq!(moved.load(Ordering::Relaxed), chunk + 7);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_part_past_the_end_of_the_file_fails() {
        let (path, fh) = temp_file(b"short");
        let mut s = stream_at(fh.clone(), 2, 100, std::sync::Arc::new(AtomicI64::new(0)));
        let first = s.next().await.expect("a chunk").expect("bytes");
        assert_eq!(first, b"ort".to_vec()); // what is there goes first
        let second = s.next().await.expect("a chunk");
        assert!(second.is_err()); // then the part can't be sent whole
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn changed_since_sees_a_grown_file_and_a_vanished_one() {
        let (path, _fh) = temp_file(b"aaaa");
        let p = path.to_str().unwrap_or_default().to_string();
        let st = stat_of(&p).expect("stat");
        assert!(!changed_since(&p, &st));
        std::fs::write(&path, b"aaaaaa").expect("write");
        assert!(changed_since(&p, &st));
        assert!(changed_since("/nonexistent/does-not-exist", &st));
        let _ = std::fs::remove_file(path);
    }
}
