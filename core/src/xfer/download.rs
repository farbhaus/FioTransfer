//! Downloading: parallel ranged segments, resume, atomic placement.
//!
//! A direct port of Go's `internal/xfer/download.go`.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use futures::FutureExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::manager::{base_name, FileTask};
use super::{File, Job, Status};
use crate::netx;
use crate::share;

/// The longest file name macOS volumes accept, in bytes.
const MAX_NAME: usize = 255;

/// One write block, and one read buffer: 1 MiB.
const BLOCK: usize = 1 << 20;

/// How long a stream may deliver nothing before it is given up on.
const STALL_LIMIT: Duration = Duration::from_secs(60);

const ERR_NO_RANGE: &str = "server does not support ranged downloads";
const ERR_RANGE_IGNORED: &str = "server ignored the Range header";
const ERR_UNEXPECTED_EOF: &str = "unexpected EOF";
const ERR_CANCELED: &str = "context canceled";

/// Holds the write blocks, one per stream.
static BUF_POOL: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

fn get_block() -> Vec<u8> {
    match BUF_POOL.lock().unwrap().pop() {
        Some(b) if b.len() == BLOCK => b,
        _ => vec![0u8; BLOCK],
    }
}

fn put_block(b: Vec<u8>) {
    if b.len() == BLOCK {
        BUF_POOL.lock().unwrap().push(b);
    }
}

/// A download failure, with the two conditions the engine acts on kept apart
/// from plain messages (Go's `errNoRange` / `errRangeIgnored`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DlErr {
    /// The server ignored a range for a download that was already split.
    NoRange,
    /// The server ignored the Range header.
    RangeIgnored,
    Msg(String),
}

impl DlErr {
    fn text(&self) -> String {
        match self {
            DlErr::NoRange => ERR_NO_RANGE.to_string(),
            DlErr::RangeIgnored => ERR_RANGE_IGNORED.to_string(),
            DlErr::Msg(m) => m.clone(),
        }
    }
}

fn dlerr(msg: impl Into<String>) -> DlErr {
    DlErr::Msg(msg.into())
}

/// Renders an OS error the way Go's `os` package does: "<op> <path>: <err>".
fn io_msg(op: &str, path: &str, e: std::io::Error) -> String {
    format!("{} {}: {}", op, path, os_text(&e))
}

/// Go prints the bare errno text ("no such file or directory"); std adds the
/// error kind and the number, so both are trimmed off.
fn os_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    let s = match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    };
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => s,
    }
}

// ---------- URL sources ----------

/// A refresh for an expired signed URL: the URL, or why it could not be had.
pub type Refresh = Arc<dyn Fn(CancellationToken) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

struct UrlState {
    url: String,
    gen: u64,
}

/// A signed URL that can be refreshed when it expires.
pub struct UrlSource {
    inner: tokio::sync::Mutex<UrlState>,
    refresh: Option<Refresh>,
}

impl UrlSource {
    pub fn new(url: &str, refresh: Option<Refresh>) -> UrlSource {
        UrlSource {
            inner: tokio::sync::Mutex::new(UrlState { url: url.to_string(), gen: 0 }),
            refresh,
        }
    }

    /// A source that renews a share file's signed URL through its share client.
    pub fn from_share(url: &str, source: Arc<dyn share::Source>, asset_id: &str) -> UrlSource {
        let source = source.clone();
        let asset = asset_id.to_string();
        let refresh: Refresh = Arc::new(move |cx: CancellationToken| {
            let source = source.clone();
            let asset = asset.clone();
            async move { source.fresh_url(&cx, &asset).await }.boxed()
        });
        UrlSource::new(url, Some(refresh))
    }

    pub async fn get(&self) -> (String, u64) {
        let g = self.inner.lock().await;
        (g.url.clone(), g.gen)
    }

    /// Refreshes the URL once per generation (concurrent callers share it).
    pub async fn renew(&self, cx: &CancellationToken, seen_gen: u64) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        if g.gen != seen_gen {
            return Ok(()); // someone else already refreshed
        }
        let Some(refresh) = self.refresh.clone() else {
            return Err("download link expired".to_string());
        };
        let u = refresh(cx.clone()).await?;
        g.url = u;
        g.gen += 1;
        Ok(())
    }

    /// Gets a URL when the listing gave none (Smash hands them out per file).
    pub async fn ensure(&self, cx: &CancellationToken) -> Result<(), String> {
        let (u, gen) = self.get().await;
        if u.is_empty() {
            return self.renew(cx, gen).await;
        }
        Ok(())
    }
}

// ---------- Resume state ----------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct DlState {
    size: i64,
    seg_size: i64,
    done: Vec<bool>,
    /// Bytes already in the part file for each segment, so an interrupted
    /// download resumes at the last finished block rather than at the start of
    /// the segment — with 64 MiB segments, that is the difference between
    /// losing 64 MiB per stream and losing 1 MiB.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    filled: Vec<i64>,
}

/// The state a resumed download starts from: the segment size the download was
/// split with back then (even if the setting has changed since), the number of
/// segments, and the recorded progress, with `filled` brought up to date for
/// the segments that are already done. `None` when the sidecar describes another
/// file or another layout, or when the part file it belongs to is gone — the
/// download then starts from nothing.
fn resume_state(previous: Option<&[u8]>, part_exists: bool, size: i64) -> Option<(i64, usize, DlState)> {
    let old: DlState = serde_json::from_slice(previous?).ok()?;
    if old.seg_size <= 0 || old.size != size || !part_exists {
        return None;
    }
    if old.done.len() as i64 != (size + old.seg_size - 1) / old.seg_size {
        return None; // another segment count: not this download's layout
    }
    let seg = old.seg_size;
    let nseg = old.done.len();
    let mut st = old;
    st.filled.resize(nseg, 0);
    for i in 0..nseg {
        let len = std::cmp::min(seg, size - i as i64 * seg);
        // `filled` counts bytes already in the part file, so it cannot be
        // negative or longer than the segment. A sidecar that says otherwise
        // (edited, truncated, written by another version) must not move the
        // resume point outside its segment: a start past the segment's end would
        // ask for a negative range and then mark the segment done with none of
        // its bytes fetched.
        st.filled[i] = st.filled[i].clamp(0, len);
        if st.done[i] {
            st.filled[i] = len;
        }
    }
    Some((seg, nseg, st))
}

struct SaverInner {
    st: DlState,
    last_save: Instant,
}

/// Writes the `.fiostate` sidecar, throttled to once a second unless forced.
struct Saver {
    inner: Mutex<SaverInner>,
    path: String,
}

impl Saver {
    fn save(&self, force: bool) {
        let mut g = self.inner.lock().unwrap();
        if !force && g.last_save.elapsed() < Duration::from_secs(1) {
            return;
        }
        g.last_save = Instant::now();
        let Ok(b) = serde_json::to_vec(&g.st) else {
            return;
        };
        let tmp = format!("{}.tmp", self.path);
        if std::fs::write(&tmp, &b).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    fn mark_done(&self, i: usize) {
        let mut g = self.inner.lock().unwrap();
        let seg_len = std::cmp::min(g.st.seg_size, g.st.size - i as i64 * g.st.seg_size);
        if let Some(d) = g.st.done.get_mut(i) {
            *d = true;
        }
        if let Some(filled) = g.st.filled.get_mut(i) {
            *filled = seg_len;
        }
    }

    /// How much of segment `i` is already in the part file.
    fn filled(&self, i: usize) -> i64 {
        self.inner.lock().unwrap().st.filled.get(i).copied().unwrap_or(0)
    }

    /// Records progress inside a segment, in whole blocks.
    fn fill(&self, i: usize, n: i64) {
        let mut g = self.inner.lock().unwrap();
        let seg_len = std::cmp::min(g.st.seg_size, g.st.size - i as i64 * g.st.seg_size);
        if let Some(filled) = g.st.filled.get_mut(i) {
            *filled = n.min(seg_len);
        }
    }

    fn is_done(&self, i: usize) -> bool {
        self.inner.lock().unwrap().st.done.get(i).copied().unwrap_or(true)
    }
}

// ---------- The engine ----------

impl Job {
    /// Fetches `f` into its local path using parallel ranged GETs written
    /// directly into place. Progress is resumable via a .fiostate sidecar.
    pub async fn download_file(
        &self,
        cx: &CancellationToken,
        f: &Arc<File>,
        seg_size: i64,
        src: &Arc<UrlSource>,
    ) -> Result<(), String> {
        let local = f.local_path();
        if let Some(dir) = Path::new(&local).parent() {
            if !dir.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    return Err(io_msg("mkdir", &dir.to_string_lossy(), e));
                }
            }
        }
        let mut size = f.size();
        if size <= 0 {
            // Unknown (or empty) in the listing: ask the server.
            src.ensure(cx).await?;
            let s = probe_size(cx, src).await?;
            size = s;
            self.add_total(s);
            f.set_size(s);
        }
        if self.place_download(f, size)? {
            return Ok(());
        }
        if size == 0 {
            return std::fs::write(&f.local_path(), b"").map_err(|e| io_msg("open", &f.local_path(), e));
        }
        src.ensure(cx).await?;
        let prefix = sidecar(&f.local_path());
        let part_path = format!("{}.fiopart", prefix);
        let state_path = format!("{}.fiostate", prefix);
        let mut seg = seg_size;
        loop {
            match self.download_segments(cx, f, size, seg, src, &part_path, &state_path).await {
                Ok(()) => break,
                Err(DlErr::NoRange) if seg < size => {
                    // Fall back to a single stream; reset progress for this file.
                    f.unwind(f.done.load(Ordering::Relaxed));
                    let _ = std::fs::remove_file(&part_path);
                    let _ = std::fs::remove_file(&state_path);
                    seg = size;
                    continue;
                }
                Err(e) => return Err(e.text()),
            }
        }
        let local = f.local_path();
        std::fs::rename(&part_path, &local).map_err(|e| io_msg("rename", &part_path, e))?;
        let _ = std::fs::remove_file(&state_path);
        Ok(())
    }

    /// Settles where `f` is written. It goes back to the path this asset was
    /// saved to before (it may have been renamed after a name clash), skips a
    /// file that is already complete, and picks a free name when a different
    /// file is in the way. It reports done when there is nothing left to
    /// download.
    fn place_download(&self, f: &Arc<File>, size: i64) -> Result<bool, String> {
        let mgr = self.mgr();
        let mut p = f.local_path();
        if let Some(m) = &mgr {
            let prev = hist_asset_path(&m.hist, &f.asset_id, &dir_of(&p), &self.id);
            if !prev.is_empty() && prev != p && (exists(&prev) || exists(&format!("{}.fiopart", sidecar(&prev)))) {
                p = prev;
            }
        }
        if let Ok(md) = std::fs::metadata(&p) {
            if !exists(&format!("{}.fiopart", sidecar(&p))) {
                // The same size alone isn't proof: a file another asset was
                // downloaded to is a different file.
                let owner = match &mgr {
                    Some(m) => hist_path_owner(&m.hist, &p, &self.id),
                    None => String::new(),
                };
                if md.len() as i64 == size && (owner.is_empty() || owner == f.asset_id) {
                    self.set_local_path(f, &p);
                    f.credit(size);
                    f.set_status(Status::Skipped, "already downloaded");
                    return Ok(true);
                }
                p = unique_path(&p)?;
            }
        }
        self.set_local_path(f, &p);
        Ok(false)
    }

    /// Moves `f` to `p` and records it, so an interrupted download resumes there.
    fn set_local_path(&self, f: &Arc<File>, p: &str) {
        let moved = {
            let mut g = f.local_path.lock().unwrap();
            if *g == p {
                false
            } else {
                *g = p.to_string();
                true
            }
        };
        if moved {
            self.persist_file(f);
        }
    }

    async fn download_segments(
        &self,
        cx: &CancellationToken,
        f: &Arc<File>,
        size: i64,
        seg_size: i64,
        src: &Arc<UrlSource>,
        part_path: &str,
        state_path: &str,
    ) -> Result<(), DlErr> {
        let mut seg = seg_size;
        let mut nseg = (size + seg - 1) as usize / seg as usize;
        let mut st = DlState { size, seg_size: seg, done: vec![false; nseg], filled: vec![0; nseg] };
        // Resume with the segment size the download started with, even if the
        // setting has changed since.
        let previous = std::fs::read(state_path).ok();
        let part_exists = std::fs::metadata(part_path).is_ok();
        let mut resumed = false;
        if let Some((oseg, onseg, ost)) = resume_state(previous.as_deref(), part_exists, size) {
            seg = oseg;
            nseg = onseg;
            st = ost;
            resumed = true;
        }
        // Create (and, for a fresh download, size) the part file before any
        // segment opens it.
        {
            let fh = tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(part_path)
                .await
                .map_err(|e| dlerr(io_msg("open", part_path, e)))?;
            if !resumed {
                fh.set_len(size as u64).await.map_err(|e| dlerr(io_msg("truncate", part_path, e)))?;
            }
        }
        let seg_len = |i: usize| -> i64 {
            let start = i as i64 * seg;
            std::cmp::min(seg, size - start)
        };
        let already: i64 = (0..nseg)
            .map(|i| if st.done[i] { seg_len(i) } else { st.filled.get(i).copied().unwrap_or(0).min(seg_len(i)) })
            .sum();
        if already > 0 {
            f.credit(already);
        }

        let saver = Arc::new(Saver {
            inner: Mutex::new(SaverInner { st, last_save: Instant::now() }),
            path: state_path.to_string(),
        });
        saver.save(true);

        // Go's WithCancelCause: the first segment to fail stops the rest and
        // its error is the one reported.
        let child = cx.child_token();
        let cause: Arc<Mutex<Option<DlErr>>> = Arc::new(Mutex::new(None));
        let mgr = self.mgr();
        let mut handles = Vec::new();
        for i in 0..nseg {
            if saver.is_done(i) {
                continue;
            }
            let child = child.clone();
            let file = f.clone();
            let src = src.clone();
            let cause = cause.clone();
            let saver = saver.clone();
            let mgr = mgr.clone();
            let path = part_path.to_string();
            handles.push(tokio::spawn(async move {
                let _permit = match mgr.as_ref() {
                    Some(m) => match m.limiter.acquire(&child).await {
                        Ok(p) => Some(p),
                        Err(_) => return,
                    },
                    None => None,
                };
                let mut fh = match tokio::fs::OpenOptions::new().read(true).write(true).open(&path).await {
                    Ok(h) => h,
                    Err(e) => {
                        let e = dlerr(io_msg("open", &path, e));
                        {
                            let mut g = cause.lock().unwrap();
                            if g.is_none() {
                                *g = Some(e);
                            }
                        }
                        child.cancel();
                        return;
                    }
                };
                // Continue where the last attempt stopped, at a 1 MiB boundary.
                let done_before = if saver.is_done(i) { std::cmp::min(seg, size - i as i64 * seg) } else { saver.filled(i) };
                let start = i as i64 * seg + done_before;
                let length = std::cmp::min(seg, size - i as i64 * seg) - done_before;
                let progress = {
                    let saver = saver.clone();
                    let i = i;
                    let base = done_before;
                    move |n: i64| {
                        saver.fill(i, base + n);
                        saver.save(false);
                    }
                };
                if let Err(e) = fetch_range(&child, &file, &src, &mut fh, start, length, nseg > 1, &progress).await {
                    {
                        let mut g = cause.lock().unwrap();
                        if g.is_none() {
                            *g = Some(e);
                        }
                    }
                    child.cancel();
                    return;
                }
                saver.mark_done(i);
                saver.save(false);
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        saver.save(true);
        if let Some(e) = cause.lock().unwrap().take() {
            return Err(e);
        }
        if cx.is_cancelled() {
            return Err(dlerr(ERR_CANCELED));
        }
        Ok(())
    }
}

/// Downloads [start, start+length) into `fh`, resuming within the segment
/// after transient failures and refreshing expired URLs.
async fn fetch_range(
    cx: &CancellationToken,
    f: &Arc<File>,
    src: &Arc<UrlSource>,
    fh: &mut tokio::fs::File,
    start: i64,
    length: i64,
    ranged: bool,
    progress: &(dyn Fn(i64) + Send + Sync),
) -> Result<(), DlErr> {
    let mut got: i64 = 0;
    let mut last_err: Option<String> = None;
    for attempt in 0..12u32 {
        if cx.is_cancelled() {
            return Err(dlerr(ERR_CANCELED));
        }
        if attempt > 0 {
            let _ = netx::sleep(cx, netx::backoff(attempt)).await;
        }
        let (u, gen) = src.get().await;
        let on_block = |n: i64| progress(got + n);
        let (n, status, err) = get_into(cx, &u, fh, start + got, length - got, ranged || got > 0, f, &on_block).await;
        got += n;
        if got >= length {
            return Ok(());
        }
        match err {
            Some(DlErr::RangeIgnored) if ranged => return Err(DlErr::NoRange),
            Some(DlErr::RangeIgnored) => {
                // Range ignored while resuming a single-stream download: start over.
                f.unwind(got);
                got = 0;
                last_err = Some(ERR_NO_RANGE.to_string());
            }
            other => {
                if matches!(status, 401 | 403 | 410 | 400) {
                    if let Err(rerr) = src.renew(cx, gen).await {
                        return Err(dlerr(format!("download link expired and could not be refreshed: {}", rerr)));
                    }
                    last_err = Some(format!("HTTP {}", status));
                } else if status == 404 {
                    return Err(dlerr("file not found on the server (HTTP 404)"));
                } else if status == 416 {
                    return Err(dlerr("server rejected byte range (HTTP 416)"));
                } else if let Some(e) = other {
                    last_err = Some(e.text());
                } else {
                    last_err = Some(format!("HTTP {}", status));
                }
            }
        }
        f.retries.fetch_add(1, Ordering::Relaxed);
    }
    Err(dlerr(format!("giving up after retries: {}", last_err.unwrap_or_default())))
}

/// Reads one response body, never past `remaining` bytes.
struct Body {
    resp: reqwest::Response,
    leftover: Vec<u8>,
    pos: usize,
    remaining: i64,
}

impl Body {
    fn new(resp: reqwest::Response, remaining: i64) -> Body {
        Body { resp, leftover: Vec::new(), pos: 0, remaining }
    }

    /// Fills `buf` like `io.ReadFull`: fewer bytes come back only at the end of
    /// the body.
    async fn read_full(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let mut n = 0usize;
        while n < buf.len() {
            if self.pos >= self.leftover.len() {
                if self.remaining <= 0 {
                    return Ok(n);
                }
                // Idle watchdog: abort if no bytes arrive for 60s.
                match tokio::time::timeout(STALL_LIMIT, self.resp.chunk()).await {
                    Err(_) => return Err(ERR_CANCELED.to_string()),
                    Ok(Err(e)) => return Err(e.to_string()),
                    Ok(Ok(None)) => return Ok(n),
                    Ok(Ok(Some(b))) => {
                        let take = std::cmp::min(b.len() as i64, self.remaining) as usize;
                        self.remaining -= take as i64;
                        if take == 0 {
                            return Ok(n);
                        }
                        self.leftover.clear();
                        self.leftover.extend_from_slice(&b[..take]);
                        self.pos = 0;
                    }
                }
            }
            let take = std::cmp::min(self.leftover.len() - self.pos, buf.len() - n);
            buf[n..n + take].copy_from_slice(&self.leftover[self.pos..self.pos + take]);
            self.pos += take;
            n += take;
        }
        Ok(n)
    }
}

/// Reads and discards up to `limit` bytes, so the connection can be reused.
async fn drain(resp: &mut reqwest::Response, limit: i64) {
    let mut left = limit;
    while left > 0 {
        match resp.chunk().await {
            Ok(Some(b)) => left -= b.len() as i64,
            _ => break,
        }
    }
}

/// Performs one GET for [off, off+length) and writes the body at off. It
/// returns bytes written, the HTTP status, and any error.
async fn get_into(
    cx: &CancellationToken,
    url: &str,
    fh: &mut tokio::fs::File,
    off: i64,
    length: i64,
    ranged: bool,
    f: &Arc<File>,
    on_block: &(dyn Fn(i64) + Send + Sync),
) -> (i64, u16, Option<DlErr>) {
    let mut req = netx::data_client().get(url);
    if ranged {
        req = req.header(RANGE, format!("bytes={}-{}", off, off + length - 1));
    }
    let mut resp = match tokio::select! {
        r = req.send() => r,
        _ = cx.cancelled() => return (0, 0, Some(dlerr(ERR_CANCELED))),
    } {
        Ok(r) => r,
        Err(e) => return (0, 0, Some(dlerr(e.to_string()))),
    };
    let status = resp.status().as_u16();
    if status != 206 && status != 200 {
        drain(&mut resp, 64 << 10).await;
        return (0, status, None);
    }
    if status == 200 && ranged {
        return (0, 200, Some(DlErr::RangeIgnored));
    }

    // Write whole blocks that end on a block boundary of the file. The network
    // hands over ~16 KB at a time, and on an SMB share each small write that
    // covers part of a page makes macOS read the page back from the server and
    // send it again (measured: 1 byte read and 2 written per byte downloaded).
    let mut buf = get_block();
    let block = buf.len() as i64;
    let mut written: i64 = 0;
    if let Err(e) = fh.seek(std::io::SeekFrom::Start(off as u64)).await {
        put_block(buf);
        return (0, status, Some(dlerr(io_msg("seek", "part file", e))));
    }
    let mut body = Body::new(resp, length);
    loop {
        if written >= length {
            break;
        }
        let n = std::cmp::min(block - (off + written) % block, length - written) as usize;
        let m = match body.read_full(&mut buf[..n]).await {
            Ok(m) => m,
            Err(e) => {
                put_block(buf);
                return (written, status, Some(dlerr(e)));
            }
        };
        if m > 0 {
            if let Err(e) = fh.write_all(&buf[..m]).await {
                put_block(buf);
                return (written, status, Some(dlerr(format!("writing to disk: {}", os_text(&e)))));
            }
            written += m as i64;
            f.add(m as i64);
            on_block(written);
        }
        if m < n {
            break;
        }
    }
    put_block(buf);
    if written < length {
        return (written, status, Some(dlerr(ERR_UNEXPECTED_EOF)));
    }
    (written, status, None)
}

/// Asks for the first byte to learn the total size from Content-Range.
async fn probe_size(cx: &CancellationToken, src: &Arc<UrlSource>) -> Result<i64, String> {
    for _attempt in 0..3 {
        let (u, gen) = src.get().await;
        let mut resp = match tokio::select! {
            r = netx::data_client().get(&u).header(RANGE, "bytes=0-0").send() => r,
            _ = cx.cancelled() => return Err(ERR_CANCELED.to_string()),
        } {
            Ok(r) => r,
            Err(e) => return Err(e.to_string()),
        };
        let status = resp.status().as_u16();
        drain(&mut resp, 1 << 16).await;
        match status {
            // a 416 still reports the size ("bytes */0" for an empty file)
            206 | 416 => {
                let cr = resp
                    .headers()
                    .get(CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if let Some(i) = cr.rfind('/') {
                    if let Ok(n) = cr[i + 1..].trim().parse::<i64>() {
                        return Ok(n);
                    }
                }
            }
            200 => {
                if let Some(len) = resp.content_length() {
                    return Ok(len as i64);
                }
            }
            401 | 403 | 410 | 400 => {
                src.renew(cx, gen).await?;
                continue;
            }
            _ => {}
        }
        return Err(format!("could not determine file size (HTTP {})", status));
    }
    Err("could not determine file size".to_string())
}

// ---------- Placement ----------

fn exists(p: &str) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// The directory part of `p`, as Go's `filepath.Dir` words it.
fn dir_of(p: &str) -> String {
    match p.rfind('/') {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(i) => {
            let d = p[..i].trim_end_matches('/');
            if d.is_empty() {
                "/".to_string()
            } else {
                d.to_string()
            }
        }
    }
}

fn split(p: &str) -> (String, &str) {
    match p.rfind('/') {
        Some(i) => (p[..i + 1].to_string(), &p[i + 1..]),
        None => (String::new(), p),
    }
}

/// The path prefix for a download's temp files (.fiopart and .fiostate). A name
/// too long to take those suffixes gets a short hashed one.
pub fn sidecar(p: &str) -> String {
    let (dir, name) = split(p);
    if name.len() + ".fiostate.tmp".len() <= MAX_NAME {
        return p.to_string();
    }
    let sum = Sha256::digest(name.as_bytes());
    let mut hex = String::new();
    for b in &sum[..8] {
        hex.push_str(&format!("{:02x}", b));
    }
    format!("{}.fio-{}", dir, hex)
}

/// Returns a free variant of `p` ("name (2).ext", …), shortening the name when
/// the suffix would push it past the file-name limit.
pub fn unique_path(p: &str) -> Result<String, String> {
    let (dir, name) = split(p);
    // A leading dot that is the only dot is part of the name, not an extension:
    // ".hidden" must dedupe to ".hidden (2)" and stay hidden, not to " (2).hidden".
    let ext = match name.rfind('.') {
        Some(0) | None => "",
        Some(i) => &name[i..],
    };
    let base = &name[..name.len() - ext.len()];
    for i in 2..10000 {
        let suffix = format!(" ({}){}", i, ext);
        let mut b = base.to_string();
        let over = b.len() as i64 + suffix.len() as i64 - MAX_NAME as i64;
        if over > 0 {
            b = truncate_utf8(&b, b.len().saturating_sub(over as usize));
        }
        let c = format!("{}{}{}", dir, b, suffix);
        if !path_free(&c)? {
            continue;
        }
        if path_free(&format!("{}.fiopart", sidecar(&c)))? {
            return Ok(c);
        }
    }
    Err(format!("no free file name for {}", name))
}

/// Whether nothing exists at `p`. Errors other than "not found" (a name that's
/// too long, a folder that can't be read) are returned.
fn path_free(p: &str) -> Result<bool, String> {
    match std::fs::symlink_metadata(p) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(os_text(&e)),
    }
}

/// Cuts `s` to at most `n` bytes without splitting a character.
pub fn truncate_utf8(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if s.len() <= n {
        return s.to_string();
    }
    s[..truncate_len(s, n)].to_string()
}

/// The largest char boundary of `s` that is at most `n`.
fn truncate_len(s: &str, n: usize) -> usize {
    let mut n = n.min(s.len());
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

// ---------- History lookups ----------

/// Where `asset_id` was most recently downloaded inside `dir` by a transfer
/// other than `exclude`, or "" if never (Go's `history.AssetPath`).
fn hist_asset_path(store: &crate::history::Store, asset_id: &str, dir: &str, exclude: &str) -> String {
    if asset_id.is_empty() {
        return String::new();
    }
    let db = store.db.lock().unwrap();
    let Ok(mut stmt) = db.prepare(
        "SELECT local_path FROM transfer_files WHERE asset_id=?1 AND transfer_id<>?2 ORDER BY rowid DESC LIMIT 50",
    ) else {
        return String::new();
    };
    let Ok(rows) = stmt.query_map([asset_id, exclude], |r| r.get::<_, Option<String>>(0)) else {
        return String::new();
    };
    for row in rows {
        if let Ok(Some(p)) = row {
            if dir_of(&p) == dir {
                return p;
            }
        }
    }
    String::new()
}

/// The asset last downloaded to `local_path` by a transfer other than
/// `exclude`, or "" if history has no finished download there (Go's
/// `history.PathOwner`).
fn hist_path_owner(store: &crate::history::Store, local_path: &str, exclude: &str) -> String {
    let db = store.db.lock().unwrap();
    let Ok(mut stmt) = db.prepare(
        "SELECT asset_id FROM transfer_files WHERE local_path=?1 AND transfer_id<>?2 AND asset_id<>''
         AND status IN ('done','skipped') ORDER BY rowid DESC LIMIT 1",
    ) else {
        return String::new();
    };
    stmt.query_row([local_path, exclude], |r| r.get::<_, String>(0)).unwrap_or_default()
}

/// Fetches a file from the signed-in account into `f`'s local path, renewing
/// its signed URL through the API when one expires (Go's
/// `downloadAccountFile`, used by watch folders).
pub async fn download_account_file(j: Arc<Job>, f: Arc<File>, acct: &str, seg: i64) -> Result<(), String> {
    let Some(m) = j.mgr() else {
        return Err("transfer manager stopped".to_string());
    };
    let api = m.api.clone();
    let acct = acct.to_string();
    let asset = f.asset_id.clone();
    let refresh: Refresh = Arc::new(move |cx: CancellationToken| {
        let api = api.clone();
        let acct = acct.clone();
        let asset = asset.clone();
        async move { api.download_url(&cx, &acct, &asset).await.map_err(|e| e.to_string()) }.boxed()
    });
    let cx = j.cx.clone();
    let u = refresh(cx.clone()).await?;
    let src = Arc::new(UrlSource::new(&u, Some(refresh)));
    j.download_file(&cx, &f, seg, &src).await
}

// ---------- The job ----------

/// Downloads a share link's files into the job's destination folder.
pub async fn run_download(j: Arc<Job>) {
    run_download_inner(j.clone()).await;
    j.finish().await;
}

async fn run_download_inner(j: Arc<Job>) {
    j.set_status(Status::Resolving);
    let Some(m) = j.mgr() else {
        j.fail("transfer manager stopped");
        return;
    };
    let raw = j.source.lock().unwrap().clone();
    let pass = j.passphrase.lock().unwrap().clone();
    let mut l = m.take_inspected(&raw, &pass);
    let mut err: Option<String> = None;
    if l.is_none() {
        let (res, e) = {
            let phase = |p: &str| j.set_phase(p);
            m.list_share(&j.cx, &raw, &pass, &phase).await
        };
        l = res;
        err = e.err();
    }
    if let Some(l) = &l {
        *j.share_id.lock().unwrap() = l.link.id.clone();
    }
    if let Some(e) = err {
        j.fail(&e);
        return;
    }
    let Some(l) = l else {
        j.fail(share::ERR_INVALID_SHARE);
        return;
    };
    if l.files.is_empty() {
        j.fail("this share has no files");
        return;
    }
    let sc: Arc<dyn share::Source> = Arc::from(share::open(&l.link, &l.pass));

    let dest = j.dest.lock().unwrap().clone();
    let mut root = dest.clone();
    if j.subfolder.load(Ordering::Relaxed) {
        let mut name = share::safe_name(&l.link.label());
        if !l.info.name.is_empty() {
            name = share::safe_name(&l.info.name);
        }
        root = Path::new(&dest).join(&name).to_string_lossy().into_owned();
    }
    *j.log_root.lock().unwrap() = root.clone();
    {
        let title = if !l.info.name.is_empty() {
            l.info.name.clone()
        } else if l.files.len() == 1 {
            base_name(&l.files[0].rel_path).to_string()
        } else {
            format!("{} + {} more", base_name(&l.files[0].rel_path), l.files.len() - 1)
        };
        *j.title.lock().unwrap() = title;
    }

    let mut list: Vec<Arc<File>> = Vec::with_capacity(l.files.len());
    for (i, sf) in l.files.iter().enumerate() {
        // Go's filepath.Join keeps a share's relative path under the root even
        // if it ever arrived with a leading slash; Path::join would replace it.
        let rel = sf.rel_path.trim_start_matches('/').replace('/', std::path::MAIN_SEPARATOR_STR);
        let local = Path::new(&root).join(&rel).to_string_lossy().into_owned();
        let mut f = File::new(i, base_name(&sf.rel_path), &sf.rel_path, &local, sf.size);
        f.asset_id = sf.asset_id.clone();
        *f.url.lock().unwrap() = sf.url.clone();
        if !sf.problem.is_empty() {
            let mut g = f.inner.lock().unwrap();
            g.status = Status::Failed;
            g.err = sf.problem.clone();
        }
        let f = Arc::new(f);
        f.attach(&j);
        list.push(f);
        j.add_total(sf.size);
    }
    j.set_files(list.clone());
    j.persist_start();

    let seg = (m.settings)().segment_mb as i64 * (1 << 20);
    j.set_status(Status::Running);
    j.set_phase("");
    let task: FileTask = {
        let sc = sc.clone();
        Arc::new(move |jj: Arc<Job>, ff: Arc<File>| {
            let sc = sc.clone();
            async move {
                let url = ff.url.lock().unwrap().clone();
                let src = Arc::new(UrlSource::from_share(&url, sc, &ff.asset_id));
                jj.download_file(&jj.cx, &ff, seg, &src).await
            }
            .boxed()
        })
    };
    j.run_files(task).await;

    let ids: Vec<String> = list
        .iter()
        .filter(|f| f.status() == Status::Done)
        .map(|f| f.asset_id.clone())
        .collect();
    if !ids.is_empty() {
        let mcx = CancellationToken::new();
        let _ = tokio::time::timeout(Duration::from_secs(20), sc.mark_downloaded(&mcx, &ids)).await;
        mcx.cancel();
    }
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_utf8_on_a_character_boundary() {
        assert_eq!(truncate_utf8("abcdef", 0), "");
        assert_eq!(truncate_utf8("abcdef", 3), "abc");
        assert_eq!(truncate_utf8("abcdef", 99), "abcdef");
        // "é" is two bytes: cutting at 1 byte gives "" rather than half a character.
        assert_eq!(truncate_utf8("éé", 1), "");
        assert_eq!(truncate_utf8("éé", 3), "é");
        assert_eq!(truncate_utf8("aé", 2), "a");
    }

    #[test]
    fn sidecar_keeps_short_names_and_hashes_long_ones() {
        let short = "/tmp/Downloads/clip.mov";
        assert_eq!(sidecar(short), short);
        let long = format!("/tmp/{}.mov", "x".repeat(251));
        let s = sidecar(&long);
        assert!(s.starts_with("/tmp/.fio-"), "{}", s);
        assert_eq!(s.len(), "/tmp/.fio-".len() + 16);
        // The same name always hashes the same way, so a resume finds it again.
        assert_eq!(sidecar(&long), s);
    }

    #[test]
    fn an_old_sidecar_still_resumes_by_segment() {
        // A .fiostate written before block-level progress existed has no
        // `filled` field: it must still parse, and its done segments count.
        let old = r#"{"size":268435456,"seg_size":67108864,"done":[true,false,false,false]}"#;
        let st: DlState = serde_json::from_str(old).expect("an old sidecar parses");
        assert_eq!(st.done.len(), 4);
        assert!(st.filled.is_empty(), "no block progress recorded");
        assert!(st.done[0] && !st.done[1]);
    }

    // Regression: a sidecar that claimed more bytes than a segment holds (a
    // truncated write, a hand-edited file, a sidecar from a version with another
    // block size) made the resume point land past the segment's end, which is a
    // negative length: the segment was then marked done having fetched nothing,
    // leaving a hole of zeros in the finished file. What the sidecar claims is
    // clamped to what the segment can hold.
    #[test]
    fn a_sidecar_cannot_claim_more_than_a_segment_holds() {
        let dir = std::env::temp_dir().join(format!("fio-sidecar-clamp-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let part = dir.join("a.fiopart");
        std::fs::File::create(&part).unwrap().set_len(8).unwrap();
        let (seg, size) = (4i64, 8i64);

        let over = DlState { size, seg_size: seg, done: vec![false, false], filled: vec![1_000, -5] };
        let body = serde_json::to_vec(&over).unwrap();
        let (got_seg, nseg, st) = resume_state(Some(&body), true, size).expect("it resumes");
        assert_eq!((got_seg, nseg), (seg, 2));
        assert_eq!(st.filled, vec![4, 0], "a claim is clamped to the segment it belongs to");
        assert!(!st.done[0], "a full segment that was never marked done is still fetched");

        let done = DlState { size, seg_size: seg, done: vec![true, false], filled: vec![-1, 3] };
        let body = serde_json::to_vec(&done).unwrap();
        let (_, _, st) = resume_state(Some(&body), true, size).expect("it resumes");
        assert_eq!(st.filled, vec![4, 3], "a done segment is filled to its length");

        // A zero segment size is not a layout, and must not divide by zero.
        let zero = DlState { size, seg_size: 0, done: vec![false], filled: vec![] };
        let body = serde_json::to_vec(&zero).unwrap();
        assert!(resume_state(Some(&body), true, size).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn block_progress_survives_a_sidecar_round_trip() {
        let st = DlState {
            size: 268435456,
            seg_size: 67108864,
            done: vec![true, false, false, false],
            filled: vec![67108864, 3 << 20, 0, 0],
        };
        let text = serde_json::to_string(&st).expect("serialise");
        let back: DlState = serde_json::from_str(&text).expect("parse");
        assert_eq!(back.filled, st.filled);
        assert_eq!(back.done, st.done);
    }

    #[test]
    fn state_json_matches_the_go_sidecar() {
        let st = DlState { size: 10, seg_size: 4, done: vec![true, false, false], filled: vec![] };
        assert_eq!(serde_json::to_string(&st).unwrap(), r#"{"size":10,"seg_size":4,"done":[true,false,false]}"#);
        // With block progress, the extra field rides along; the Go app ignores
        // unknown fields, so a sidecar written here is still readable there.
        let mut with = st;
        with.filled = vec![4, 2, 0];
        let text = serde_json::to_string(&with).unwrap();
        assert!(text.contains(r#""filled":[4,2,0]"#), "{}", text);
    }

    #[test]
    fn dir_of_words_directories_like_go() {
        assert_eq!(dir_of("a.mov"), ".");
        assert_eq!(dir_of("/a.mov"), "/");
        assert_eq!(dir_of("/a/b.mov"), "/a");
        assert_eq!(dir_of("a/b/c.mov"), "a/b");
    }

    #[test]
    fn unique_path_counts_up_and_stays_inside_the_name_limit() {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("fiotransfer-unique-{}-{}", std::process::id(), stamp));
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().into_owned();

        let first = format!("{}/a.mov", d);
        std::fs::write(&first, b"one").unwrap();
        assert_eq!(unique_path(&first).unwrap(), format!("{}/a (2).mov", d));
        std::fs::write(format!("{}/a (2).mov", d), b"two").unwrap();
        assert_eq!(unique_path(&first).unwrap(), format!("{}/a (3).mov", d));

        // A name at the limit is shortened instead of failing (or looping).
        let long_name = format!("{}.mov", "x".repeat(251));
        let long = format!("{}/{}", d, long_name);
        assert_eq!(long_name.len(), MAX_NAME);
        std::fs::write(&long, b"one").unwrap();
        let renamed = unique_path(&long).unwrap();
        assert_eq!(renamed, format!("{}/{} (2).mov", d, "x".repeat(247)));
        assert_eq!(split(&renamed).1.len(), MAX_NAME);

        let _ = std::fs::remove_dir_all(&dir);
    }
    // ---------- property tests: the naming helpers against a model ----------
    //
    // These live next to the private helpers they exercise (`split`,
    // `path_free`, `truncate_len`), so the model can be written without
    // reaching through a public wrapper.

    use proptest::prelude::*;

    /// The longest prefix of `s` that ends on a character boundary and is at
    /// most `n` bytes long — the model, by walking characters rather than bytes.
    fn longest_bounded_prefix(s: &str, n: usize) -> &str {
        let mut end = 0;
        for (i, c) in s.char_indices() {
            if i + c.len_utf8() > n {
                break;
            }
            end = i + c.len_utf8();
        }
        &s[..end]
    }

    fn multi_byte_text() -> impl Strategy<Value = String> {
        // plain ASCII, 2-byte Latin-1, 3-byte CJK, 4-byte emoji, and combining
        // marks, at any length including empty
        proptest::collection::vec(
            prop_oneof![
                Just("a"), Just("Z"), Just("-"), Just(" "), Just("/"),
                Just("é"), Just("ñ"), Just("ß"),
                Just("日"), Just("本"), Just("語"),
                Just("🙂"), Just("🎬"),
                Just("e\u{0301}"),
            ],
            0..12,
        )
        .prop_map(|v| v.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// `truncate_utf8` must cut at the largest character boundary that fits
        /// in `n` bytes: never mid-character, never longer than asked, and
        /// never shorter than it has to be.
        #[test]
        fn truncate_utf8_is_the_longest_prefix_that_fits(s in multi_byte_text(), n in 0usize..40) {
            let got = truncate_utf8(&s, n);
            prop_assert_eq!(&got, longest_bounded_prefix(&s, n), "s={:?} n={}", s, n);
            prop_assert!(got.len() <= n, "{} > {} for {:?}", got.len(), n, s);
            prop_assert!(s.starts_with(&got), "{:?} is not a prefix of {:?}", got, s);
            prop_assert!(got.is_char_boundary(got.len()));
            // idempotent: cutting an already-cut string changes nothing
            prop_assert_eq!(truncate_utf8(&got, n), got);
        }

        /// The sidecar prefix must always leave room for the longest suffix
        /// any caller appends (`.fiostate.tmp`), must be stable for the same
        /// name, and must depend on the name alone, not on the directory.
        #[test]
        fn sidecar_leaves_room_for_its_suffixes(name in sidecar_name(), dir in dir_choice()) {
            let p = format!("{}{}", dir, name);
            let s = sidecar(&p);
            let (sdir, sname) = split(&s);
            prop_assert_eq!(sdir, dir.clone(), "the directory is kept");
            prop_assert!(sname.len() + ".fiostate.tmp".len() <= MAX_NAME,
                         "{:?} leaves no room for .fiostate.tmp", sname);
            prop_assert!(sname.len() + ".fiopart".len() <= MAX_NAME);
            // stable: a resume finds the same sidecar again
            prop_assert_eq!(sidecar(&p), s.clone());
            prop_assert_eq!(sidecar(&format!("{}{}", dir, name)), s.clone());
            // the same name in another directory hashes the same way
            let other = split(&sidecar(&format!("/elsewhere/{}", name))).1.to_string();
            prop_assert_eq!(&other, sname, "the hash may depend on the name only");
            // a short name is its own sidecar prefix
            if name.len() + ".fiostate.tmp".len() <= MAX_NAME {
                prop_assert_eq!(&s, &p);
            } else {
                prop_assert!(sname.starts_with(".fio-"), "a long name is hashed: {:?}", sname);
            }
            // and the hashed name is itself short enough to take the suffix
            prop_assert!(sidecar(&s).ends_with(&sname));
        }

        /// `unique_path` never hands back a name that is taken (nor one whose
        /// part file is), always stays inside the file-name limit, and counts
        /// up without gaps when a series of files of the same name arrives.
        #[test]
        fn unique_path_never_clobbers_and_counts_up(name in file_name(), taken in 0usize..4, mask in 0u8..16) {
            let dir = tempfile::tempdir().expect("tempdir");
            let d = dir.path().to_string_lossy().into_owned();
            let p = format!("{}/{}", d, name);
            // `taken` names of this series are already on disk; some of them are
            // only their `.fiopart` (an interrupted download that left no file)
            std::fs::write(&p, b"x").expect("write the original");
            let mut made = vec![p.clone()];
            let mut series: Vec<String> = vec![p.clone()];
            for i in 0..taken {
                let next = unique_path(&p).expect("a free name");
                prop_assert!(!made.contains(&next), "{:?} was already made", next);
                prop_assert!(!std::fs::symlink_metadata(&next).is_ok(), "{:?} already exists", next);
                prop_assert!(!std::fs::symlink_metadata(format!("{}.fiopart", sidecar(&next))).is_ok(),
                             "the part file of {:?} exists", next);
                if mask & (1 << i) != 0 {
                    std::fs::write(format!("{}.fiopart", sidecar(&next)), b"part").expect("write a part");
                } else {
                    std::fs::write(&next, b"y").expect("write a collision");
                }
                made.push(next.clone());
                series.push(next);
            }
            // the next free name is one past the last one made
            let last = series.last().unwrap().clone();
            let next = unique_path(&p).expect("a free name");
            prop_assert!(!std::fs::symlink_metadata(&next).is_ok(), "{:?} already exists", next);
            prop_assert!(!made.contains(&next));
            // the series is "name", "name (2)" … "name (taken+1)", so the
            // next free one is exactly the one after that
            prop_assert_eq!(suffix_number(&next), Some(taken as u32 + 2),
                            "{:?} should follow {:?} in the series", next, last);
            if taken == 0 {
                prop_assert_eq!(suffix_number(&p), None, "the original carries no suffix");
            }
            // every name stays inside the limit, whatever the base name
            for m in made.iter().chain(std::iter::once(&next)) {
                let (mdir, mname) = split(m);
                prop_assert_eq!(mdir, format!("{}/", d));
                prop_assert!(mname.len() <= MAX_NAME, "{:?} is {} bytes", mname, mname.len());
                // the part and state files hang off the sidecar prefix, which
                // is what keeps a 255-byte name resumable
                prop_assert!(split(&sidecar(m)).1.len() + ".fiostate.tmp".len() <= MAX_NAME,
                             "the sidecar of {:?} leaves no room for its suffix", mname);
            }
        }

        /// `path_free` must mirror the filesystem and refuse to swallow an
        /// error: a name that cannot be stat'ed is not "free".
        #[test]
        fn path_free_mirrors_the_filesystem(name in file_name()) {
            let dir = tempfile::tempdir().expect("tempdir");
            let p = format!("{}/{}", dir.path().to_string_lossy(), name);
            prop_assert!(path_free(&p).unwrap(), "{:?} does not exist yet", p);
            std::fs::write(&p, b"z").expect("write");
            prop_assert!(!path_free(&p).unwrap(), "{:?} exists", p);
            // a path whose directory does not exist is free (the caller creates it)
            let deeper = format!("{}/nope/{}", dir.path().to_string_lossy(), name);
            prop_assert!(path_free(&deeper).unwrap_or(true));
        }
    }

    /// A name that may appear on disk: multi-byte, dotfiles, several dots, no
    /// extension, and names at the file-name limit.
    fn file_name() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("a.mov".to_string()),
            Just("a.tar.gz".to_string()),
            Just("noext".to_string()),
            Just(".hidden".to_string()),
            Just("..double".to_string()),
            Just("clip é.mov".to_string()),
            Just("🎬 take 1.mov".to_string()),
            Just(format!("{}.mov", "x".repeat(251))),
            Just(format!("{}.mov", "x".repeat(247))),
            Just("é".repeat(100)),
            Just(format!("{}", "y".repeat(255))),
            Just(format!("{}.tar.gz", "z".repeat(240))),
        ]
    }

    fn sidecar_name() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("a.mov".to_string()),
            Just(format!("{}.mov", "x".repeat(251))),
            Just(format!("{}.mov", "x".repeat(242))),
            Just(format!("{}.mov", "x".repeat(243))),
            Just("é".repeat(130)),
            Just("y".repeat(255)),
            Just(".fio-deadbeefdeadbeef".to_string()),
        ]
    }

    fn dir_choice() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("/tmp/".to_string()),
            Just("/tmp/Downloads/".to_string()),
            Just("".to_string()),
            Just("rel/path/".to_string()),
        ]
    }

    /// The ` (n)` counter in a name, for the count-up check.
    fn suffix_number(p: &str) -> Option<u32> {
        let name = split(p).1;
        let open = name.rfind(" (")?;
        let close = name[open..].find(')')? + open;
        name[open + 2..close].parse().ok()
    }

}

// ---------- Property tests: block-granular resume ----------
//
// A download is a plan (segments of `seg_size` bytes, the last one short) and a
// vector of 1 MiB blocks per segment. The models here are the obvious ones: the
// number of bytes recorded per segment, the segment length, and the offset the
// next attempt starts at. `resume_state` is the whole of the resume decision, so
// these properties cover exactly what a second attempt does.

#[cfg(test)]
mod resume_tests {
    use super::*;
    use proptest::collection::vec as pvec;
    use proptest::prelude::*;

    const BLOCK_I: i64 = BLOCK as i64;

    #[derive(Debug, Clone, Copy)]
    struct Plan {
        size: i64,
        seg: i64,
        nseg: usize,
    }

    impl Plan {
        fn seg_start(&self, i: usize) -> i64 {
            i as i64 * self.seg
        }
        fn seg_len(&self, i: usize) -> i64 {
            std::cmp::min(self.seg, self.size - self.seg_start(i))
        }
    }

    /// `nseg` segments of `seg_blocks` blocks each, the last one `last` bytes.
    fn plan(seg_blocks: usize, nseg: usize, last: i64) -> Plan {
        let seg = BLOCK_I * seg_blocks as i64;
        // The last segment is between one byte and a whole segment long, so the
        // plan always needs exactly `nseg` segments.
        let last = last.clamp(1, seg);
        Plan { size: seg * (nseg as i64 - 1) + last, seg, nseg }
    }

    /// A part file of the right size and the sidecar path beside it, as
    /// `download_file` names them.
    fn files(dir: &Path, size: i64) -> (String, String) {
        let part = dir.join("a.fiopart");
        let fh = std::fs::File::create(&part).expect("part file");
        fh.set_len(size as u64).expect("size the part file");
        (part.to_string_lossy().into_owned(), dir.join("a.fiostate").to_string_lossy().into_owned())
    }

    fn fresh_saver(p: Plan, path: &str) -> Saver {
        Saver {
            inner: Mutex::new(SaverInner {
                st: DlState { size: p.size, seg_size: p.seg, done: vec![false; p.nseg], filled: vec![0; p.nseg] },
                last_save: Instant::now(),
            }),
            path: path.to_string(),
        }
    }

    fn resumed_saver(st: DlState, path: &str) -> Saver {
        Saver { inner: Mutex::new(SaverInner { st, last_save: Instant::now() }), path: path.to_string() }
    }

    /// What the next attempt does with what the last one left behind.
    fn next_attempt(state: &str, part_exists: bool, size: i64) -> Option<(i64, usize, DlState)> {
        let previous = std::fs::read(state).ok();
        resume_state(previous.as_deref(), part_exists, size)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

        /// After an arbitrary schedule of finished blocks and finished segments,
        /// and a cancel, the next attempt starts each segment at the last finished
        /// block boundary: no byte it recorded is fetched again, and it never
        /// starts inside a block it did not record.
        #[test]
        fn a_resume_starts_each_segment_at_its_last_finished_block(
            blocks in pvec(0usize..6, 1..5),
            done in pvec(any::<bool>(), 1..5),
            last in 1i64..(BLOCK_I * 4 + 1),
        ) {
            let p = plan(4, blocks.len(), last);
            let dir = tempfile::tempdir().expect("temp dir");
            let (part, state) = files(dir.path(), p.size);
            let s = fresh_saver(p, &state);
            s.save(true);
            for i in 0..p.nseg {
                if done[i % done.len()] {
                    s.mark_done(i);
                } else {
                    s.fill(i, blocks[i % blocks.len()] as i64 * BLOCK_I);
                }
            }
            s.save(true); // the attempt ends: cancel, failure, or the app quitting
            drop(s);
            prop_assert!(std::fs::metadata(&part).is_ok(), "the part file is still there");
            let (seg, nseg, st) = next_attempt(&state, true, p.size).expect("the next attempt resumes");
            prop_assert_eq!(seg, p.seg, "the segment size the download started with");
            prop_assert_eq!(nseg, p.nseg);
            for i in 0..p.nseg {
                let len = p.seg_len(i);
                let d = done[i % done.len()];
                let recorded = if d { len } else { (blocks[i % blocks.len()] as i64 * BLOCK_I).min(len) };
                prop_assert!(st.filled[i] >= 0, "segment {} says {} bytes are on disk", i, st.filled[i]);
                prop_assert!(st.filled[i] <= len, "segment {} says {} of {} bytes", i, st.filled[i], len);
                prop_assert!(st.filled[i] >= recorded, "segment {} lost {} bytes of recorded progress", i, recorded - st.filled[i]);
                let start = p.seg_start(i) + st.filled[i];
                prop_assert!(start <= p.seg_start(i) + len, "segment {} would start past its end", i);
                if st.filled[i] < len {
                    // Everything already in the part file is on a block boundary.
                    prop_assert_eq!(start % BLOCK_I, 0, "segment {} resumes at {} (not a block boundary)", i, start);
                }
                if d {
                    prop_assert_eq!(st.filled[i], len, "a done segment has nothing left to fetch");
                }
            }
        }

        /// Three attempts in a row, each finishing more blocks: a segment's start
        /// only ever moves forward, and what a later attempt writes is exactly the
        /// bytes that are not on disk yet.
        #[test]
        fn progress_moves_forward_across_attempts(
            seg_blocks in 1usize..5,
            nseg in 1usize..5,
            per_attempt in pvec(pvec(0usize..5, 1..5), 2..4),
            last in 1i64..(BLOCK_I * 4 + 1),
        ) {
            let p = plan(seg_blocks, nseg, last);
            let dir = tempfile::tempdir().expect("temp dir");
            let (_, state) = files(dir.path(), p.size);
            let mut previous: Vec<i64> = vec![0; nseg];
            let mut carried: Option<DlState> = None;
            for (a, blocks) in per_attempt.iter().enumerate() {
                let st = match carried.take() {
                    Some(st) => st,
                    None => DlState { size: p.size, seg_size: p.seg, done: vec![false; nseg], filled: vec![0; nseg] },
                };
                let s = resumed_saver(st, &state);
                s.save(a == 0); // the first attempt writes its plan; later ones resume
                for i in 0..nseg {
                    // Only ever add to what is already there, as the engine does.
                    let have = s.filled(i);
                    let more = std::cmp::min((blocks[i % blocks.len()] as i64) * BLOCK_I, p.seg_len(i) - have).max(0);
                    s.fill(i, have + more);
                }
                s.save(true); // the attempt ends
                drop(s);
                let (_, onseg, st) = next_attempt(&state, true, p.size).expect("it resumes");
                prop_assert_eq!(onseg, nseg);
                for i in 0..nseg {
                    prop_assert!(st.filled[i] >= previous[i], "attempt {} moved segment {} back", a, i);
                    prop_assert!(st.filled[i] <= p.seg_len(i));
                    if st.filled[i] < p.seg_len(i) {
                        prop_assert_eq!(st.filled[i] % BLOCK_I, 0, "attempt {} segment {}", a, i);
                    }
                }
                previous = st.filled.clone();
                carried = Some(st);
            }
        }

        /// `filled` counts bytes on disk, so it is capped at the segment length —
        /// including the short last segment, and including a segment that is done.
        #[test]
        fn filled_is_capped_at_the_segment_length(
            seg_blocks in 1usize..4,
            nseg in 1usize..5,
            last in 1i64..(BLOCK_I * 3 + 1),
            claim in 0i64..(BLOCK_I * 20),
        ) {
            let p = plan(seg_blocks, nseg, last);
            let dir = tempfile::tempdir().expect("temp dir");
            let (_, state) = files(dir.path(), p.size);
            let s = fresh_saver(p, &state);
            let i = nseg - 1;
            s.fill(i, claim);
            prop_assert_eq!(s.filled(i), std::cmp::min(claim, p.seg_len(i)), "fill caps at the segment length");
            s.fill(i, p.seg_len(i) - 1);
            prop_assert_eq!(s.filled(i), p.seg_len(i) - 1, "fill can also lower it");
            s.fill(i, p.seg_len(i) + 1);
            prop_assert_eq!(s.filled(i), p.seg_len(i));
            // Done from a segment that is only partly on disk: a done segment is
            // full, and the next attempt must not fetch the rest of it again.
            s.fill(i, 0);
            s.mark_done(i);
            prop_assert!(s.is_done(i));
            prop_assert_eq!(s.filled(i), p.seg_len(i), "a done segment is filled to its length");
            s.save(true);
            drop(s);
            let (_, _, st) = next_attempt(&state, true, p.size).expect("it resumes");
            for k in 0..nseg {
                prop_assert!(st.filled[k] <= p.seg_len(k), "segment {}", k);
            }
        }

        /// A done segment is never fetched again: the next attempt gives it no
        /// bytes at all, even when the segment is shorter than a block.
        #[test]
        fn a_done_segment_is_never_refetched(
            seg_blocks in 1usize..4,
            nseg in 1usize..5,
            last in 1i64..(BLOCK_I * 3 + 1),
            which in pvec(any::<bool>(), 1..5),
        ) {
            let p = plan(seg_blocks, nseg, last);
            let dir = tempfile::tempdir().expect("temp dir");
            let (_, state) = files(dir.path(), p.size);
            let s = fresh_saver(p, &state);
            s.save(true);
            for i in 0..nseg {
                if which[i % which.len()] {
                    s.mark_done(i);
                }
            }
            s.save(true);
            drop(s);
            let (_, _, st) = next_attempt(&state, true, p.size).expect("it resumes");
            for i in 0..nseg {
                if which[i % which.len()] {
                    prop_assert!(st.done[i], "segment {} is still done", i);
                    prop_assert_eq!(p.seg_start(i) + st.filled[i], p.seg_start(i) + p.seg_len(i), "segment {} fetches {} bytes", i, p.seg_len(i) - st.filled[i]);
                } else {
                    prop_assert!(!st.done[i], "segment {} was not done", i);
                }
            }
        }

        /// A sidecar written before block-level progress existed has no `filled`
        /// field: it must still load, with the done segments done and the rest
        /// starting at their segment boundary.
        #[test]
        fn an_old_sidecar_without_filled_still_loads(
            seg_blocks in 1usize..4,
            nseg in 1usize..5,
            last in 1i64..(BLOCK_I * 3 + 1),
            which in pvec(any::<bool>(), 1..5),
        ) {
            let p = plan(seg_blocks, nseg, last);
            let dir = tempfile::tempdir().expect("temp dir");
            let (_, state) = files(dir.path(), p.size);
            let done: Vec<bool> = (0..nseg).map(|i| which[i % which.len()]).collect();
            let old = serde_json::json!({ "size": p.size, "seg_size": p.seg, "done": done });
            std::fs::write(&state, serde_json::to_vec(&old).unwrap()).expect("sidecar");
            let (seg, onseg, st) = next_attempt(&state, true, p.size).expect("an old sidecar still resumes");
            prop_assert_eq!(seg, p.seg);
            prop_assert_eq!(onseg, nseg);
            for i in 0..nseg {
                prop_assert_eq!(st.done[i], done[i], "segment {}", i);
                if done[i] {
                    prop_assert_eq!(st.filled[i], p.seg_len(i), "segment {}", i);
                } else {
                    prop_assert_eq!(st.filled[i], 0, "segment {} starts at its segment boundary", i);
                    prop_assert_eq!(p.seg_start(i) % BLOCK_I, 0);
                }
            }
            // And a sidecar whose `filled` is shorter than its `done` list is
            // padded, not indexed out of range.
            let short = serde_json::json!({ "size": p.size, "seg_size": p.seg, "done": done, "filled": [0] });
            std::fs::write(&state, serde_json::to_vec(&short).unwrap()).expect("sidecar");
            let (_, onseg, st) = next_attempt(&state, true, p.size).expect("it still resumes");
            prop_assert_eq!(onseg, nseg);
            prop_assert_eq!(st.filled.len(), nseg);
        }

        /// A sidecar describes one file with one layout. Anything else — another
        /// size, another segment count, a zero segment size, a missing part file,
        /// bytes that are not JSON — is ignored and the download starts fresh.
        #[test]
        fn a_sidecar_for_another_file_or_layout_is_ignored(
            seg_blocks in 1usize..4,
            nseg in 1usize..5,
            last in 1i64..(BLOCK_I * 3 + 1),
            dsize in -2i64..3,
            dlen in -1i64..2,
            part_exists in any::<bool>(),
            garbage in any::<bool>(),
        ) {
            let p = plan(seg_blocks, nseg, last);
            let dir = tempfile::tempdir().expect("temp dir");
            let (_, state) = files(dir.path(), p.size);
            let s = fresh_saver(p, &state);
            s.save(true);
            drop(s);
            let body = if garbage {
                b"not a sidecar".to_vec()
            } else {
                let mut st = DlState { size: p.size, seg_size: p.seg, done: vec![false; nseg], filled: vec![0; nseg] };
                st.size += dsize;
                let mut done = st.done.clone();
                for _ in 0..dlen.max(0) {
                    done.push(false);
                }
                for _ in 0..(-dlen).max(0) {
                    done.pop();
                }
                st.done = done;
                st.filled = vec![0; st.done.len()];
                serde_json::to_vec(&st).unwrap()
            };
            std::fs::write(&state, &body).expect("sidecar");
            let usable = !garbage && part_exists && dsize == 0 && dlen == 0;
            let got = next_attempt(&state, part_exists, p.size);
            prop_assert_eq!(got.is_some(), usable, "usable={} garbage={} part={} dsize={} dlen={}", usable, garbage, part_exists, dsize, dlen);
            if let Some((seg, onseg, st)) = got {
                prop_assert_eq!(seg, p.seg);
                prop_assert_eq!(onseg, nseg);
                prop_assert_eq!(st.done.len(), nseg);
            }
        }

        /// Whatever a sidecar claims about bytes already on disk, a resumed
        /// segment never starts outside itself: a claim of more than the segment
        /// holds must not turn into "this segment is finished" with its bytes
        /// never fetched, and a negative claim must not pull the start back into
        /// the previous segment.
        #[test]
        fn a_sidecar_cannot_claim_more_than_the_segment_holds(
            seg_blocks in 1usize..4,
            nseg in 1usize..5,
            last in 1i64..(BLOCK_I * 3 + 1),
            claims in pvec((any::<bool>(), -3i64 * BLOCK_I..6i64 * BLOCK_I), 1..5),
        ) {
            let p = plan(seg_blocks, nseg, last);
            let dir = tempfile::tempdir().expect("temp dir");
            let (_, state) = files(dir.path(), p.size);
            let mut st = DlState { size: p.size, seg_size: p.seg, done: vec![false; nseg], filled: vec![0; nseg] };
            for (i, (done, claim)) in claims.iter().enumerate() {
                if i < nseg {
                    st.done[i] = *done;
                    st.filled[i] = *claim;
                }
            }
            std::fs::write(&state, serde_json::to_vec(&st).unwrap()).expect("sidecar");
            let (_, onseg, got) = next_attempt(&state, true, p.size).expect("it resumes");
            prop_assert_eq!(onseg, nseg);
            for i in 0..nseg {
                let len = p.seg_len(i);
                prop_assert!(got.filled[i] >= 0, "segment {} starts {} bytes before itself", i, -got.filled[i]);
                prop_assert!(got.filled[i] <= len, "segment {} starts {} bytes past its end", i, got.filled[i] - len);
                prop_assert!(p.seg_start(i) + got.filled[i] <= p.seg_start(i) + len);
            }
        }

        /// The save that runs at most once a second may lose the last few blocks
        /// after a crash, but it can never record progress that was not made.
        #[test]
        fn a_throttled_save_never_invents_progress(
            nseg in 1usize..4,
            seg_blocks in 1usize..4,
            blocks in pvec(0usize..4, 1..4),
        ) {
            let p = plan(seg_blocks, nseg, BLOCK_I * 2);
            let dir = tempfile::tempdir().expect("temp dir");
            let (_, state) = files(dir.path(), p.size);
            let s = fresh_saver(p, &state);
            s.save(true);
            for i in 0..nseg {
                s.fill(i, blocks[i % blocks.len()] as i64 * BLOCK_I);
                s.save(false); // throttled: usually a no-op
            }
            let on_disk = std::fs::read(&state).ok().and_then(|b| serde_json::from_slice::<DlState>(&b).ok());
            prop_assert!(on_disk.is_some(), "the plan is on disk");
            for i in 0..nseg {
                let got = on_disk.as_ref().map(|st| st.filled[i]).unwrap_or(0);
                prop_assert!(got >= 0);
                prop_assert!(got <= s.filled(i), "the file claims more progress than was made");
                prop_assert!(got <= p.seg_len(i));
            }
        }
    }

}
