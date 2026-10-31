//! Records every transfer in SQLite and in a human-readable monthly log under
//! ~/Library/Logs/FioTransfer.

pub mod unfinished;
pub mod watch;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Transfer {
    pub id: String,
    pub kind: String, // upload | download
    pub title: String,
    pub source: String, // share link, or local path summary
    pub dest: String,   // local folder, or Frame.io destination label
    pub account_id: String,
    pub folder_id: String,
    pub share_id: String,
    pub status: String,
    pub error: String,
    pub total_bytes: i64,
    pub done_bytes: i64,
    pub files_total: i64,
    pub files_done: i64,
    pub files_failed: i64,
    pub created_at: i64, // unix ms
    pub started_at: i64,
    pub finished_at: i64,
    pub avg_bps: f64,
    pub peak_bps: f64,
    pub options: String,
    pub link: String,       // Frame.io link to the uploaded files
    pub share_link: String, // public share of them, made on request
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct File {
    pub transfer_id: String,
    pub idx: i64,
    pub rel_path: String,
    pub local_path: String,
    pub size: i64,
    pub status: String,
    pub error: String,
    pub note: String,
    pub remote_parent: String,
    pub remote_id: String,
    pub view_url: String,
    pub asset_id: String,
    pub finished_at: i64,
}

pub struct Store {
    pub(crate) db: Mutex<rusqlite::Connection>,
    pub(crate) log_dir: PathBuf,
    pub(crate) log_mu: Mutex<()>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS transfers(
  id TEXT PRIMARY KEY, kind TEXT, title TEXT, source TEXT, dest TEXT,
  account_id TEXT, folder_id TEXT, share_id TEXT, status TEXT, error TEXT,
  total_bytes INTEGER, done_bytes INTEGER, files_total INTEGER, files_done INTEGER, files_failed INTEGER,
  created_at INTEGER, started_at INTEGER, finished_at INTEGER, avg_bps REAL, peak_bps REAL, options TEXT);
CREATE INDEX IF NOT EXISTS transfers_created ON transfers(created_at DESC);
CREATE TABLE IF NOT EXISTS transfer_files(
  transfer_id TEXT, idx INTEGER, rel_path TEXT, local_path TEXT, size INTEGER,
  status TEXT, error TEXT, note TEXT, remote_parent TEXT, remote_id TEXT, view_url TEXT,
  asset_id TEXT, finished_at INTEGER, PRIMARY KEY(transfer_id, idx));
CREATE INDEX IF NOT EXISTS transfer_files_asset ON transfer_files(asset_id);
CREATE INDEX IF NOT EXISTS transfer_files_local ON transfer_files(local_path);
CREATE INDEX IF NOT EXISTS transfer_files_remote ON transfer_files(remote_id);
";

/// The transfers columns in `Transfer` order, shared by every query that scans
/// a whole row.
const COLS: &str = "id, kind, title, source, dest, account_id, folder_id, share_id, status, error, \
total_bytes, done_bytes, files_total, files_done, files_failed, created_at, started_at, finished_at, \
avg_bps, peak_bps, options, link, share_link";

impl Store {
    pub fn open(dir: &Path, log_dir: &Path) -> Result<Store, String> {
        // Go opens `file:<dir>/history.db?_pragma=journal_mode(WAL)&
        // _pragma=busy_timeout(10000)&_pragma=synchronous(NORMAL)` with
        // MaxOpenConns(1): one connection, WAL, and a busy wait so the watcher
        // thread and the UI never see "database is locked".
        let path = dir.join("history.db");
        let conn = rusqlite::Connection::open(&path).map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_millis(10_000)).map_err(|e| e.to_string())?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
            .map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA synchronous=NORMAL").map_err(|e| e.to_string())?;

        let mut full = String::from(SCHEMA);
        full.push_str(watch::WATCH_SCHEMA);
        full.push_str(unfinished::UNFINISHED_SCHEMA);
        if let Err(e) = conn.execute_batch(&full) {
            return Err(e.to_string());
        }
        // Columns added after the first release.
        add_column(&conn, "transfers", "link", "TEXT");
        add_column(&conn, "transfers", "share_link", "TEXT");
        add_column(&conn, "watches", "direction", "TEXT NOT NULL DEFAULT 'upload'");
        add_column(&conn, "watches", "ignore_remote", "INTEGER NOT NULL DEFAULT 0");
        add_column(&conn, "watches", "on_change", "TEXT NOT NULL DEFAULT 'overwrite'");
        // `origin` ('' for a watch the user made, `portal:<link id>` for one the
        // Portal reconciler owns) — the older default keeps every existing watch
        // a local one, which is what the reconciler treats as untouchable.
        add_column(&conn, "watches", "origin", "TEXT");
        Ok(Store { db: Mutex::new(conn), log_dir: log_dir.to_path_buf(), log_mu: Mutex::new(()) })
    }

    /// The one connection, as the Mutex guard. A panic elsewhere must not turn
    /// every later history write into a panic here.
    pub(crate) fn db(&self) -> MutexGuard<'_, rusqlite::Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn close(&self) {}

    /// Flags transfers that were running when the app last quit.
    pub fn mark_interrupted(&self) {
        let now = now_ms();
        let conn = self.db();
        let _ = conn.execute(
            "UPDATE transfer_files SET status='interrupted' WHERE status IN ('queued','running','resolving')",
            [],
        );
        let _ = conn.execute(
            "UPDATE transfers SET status='interrupted', finished_at=? WHERE status IN ('queued','running','resolving')",
            [now],
        );
    }

    pub fn start(&self, t: &Transfer, files: &[File]) -> Result<(), String> {
        let mut conn = self.db();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            &format!(
                "INSERT OR REPLACE INTO transfers({}) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                COLS
            ),
            rusqlite::params![
                t.id,
                t.kind,
                t.title,
                t.source,
                t.dest,
                t.account_id,
                t.folder_id,
                t.share_id,
                t.status,
                t.error,
                t.total_bytes,
                t.done_bytes,
                t.files_total,
                t.files_done,
                t.files_failed,
                t.created_at,
                t.started_at,
                t.finished_at,
                t.avg_bps,
                t.peak_bps,
                t.options,
                t.link,
                t.share_link,
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM transfer_files WHERE transfer_id=?", [&t.id])
            .map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare("INSERT INTO transfer_files VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .map_err(|e| e.to_string())?;
            for f in files {
                stmt.execute(rusqlite::params![
                    t.id,
                    f.idx,
                    f.rel_path,
                    f.local_path,
                    f.size,
                    f.status,
                    f.error,
                    f.note,
                    f.remote_parent,
                    f.remote_id,
                    f.view_url,
                    f.asset_id,
                    f.finished_at,
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn update_file(&self, f: &File) {
        let conn = self.db();
        let _ = conn.execute(
            "UPDATE transfer_files SET local_path=?, size=?, status=?, error=?, note=?, remote_parent=?, \
remote_id=?, view_url=?, finished_at=? WHERE transfer_id=? AND idx=?",
            rusqlite::params![
                f.local_path,
                f.size,
                f.status,
                f.error,
                f.note,
                f.remote_parent,
                f.remote_id,
                f.view_url,
                f.finished_at,
                f.transfer_id,
                f.idx,
            ],
        );
    }

    /// Saves a transfer's progress. It never clears a share link made meanwhile.
    pub fn update(&self, t: &Transfer) {
        let conn = self.db();
        let _ = conn.execute(
            "UPDATE transfers SET title=?, source=?, dest=?, status=?, error=?, total_bytes=?, \
done_bytes=?, files_total=?, files_done=?, files_failed=?, started_at=?, finished_at=?, avg_bps=?, \
peak_bps=?, share_id=?, options=?, link=?, share_link=COALESCE(NULLIF(?, ''), share_link) WHERE id=?",
            rusqlite::params![
                t.title,
                t.source,
                t.dest,
                t.status,
                t.error,
                t.total_bytes,
                t.done_bytes,
                t.files_total,
                t.files_done,
                t.files_failed,
                t.started_at,
                t.finished_at,
                t.avg_bps,
                t.peak_bps,
                t.share_id,
                t.options,
                t.link,
                t.share_link,
                t.id,
            ],
        );
    }

    pub fn set_share_link(&self, id: &str, link: &str) -> Result<(), String> {
        let conn = self.db();
        conn.execute("UPDATE transfers SET share_link=? WHERE id=?", rusqlite::params![link, id])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn get(&self, id: &str) -> Result<Option<Transfer>, String> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(&format!("SELECT {} FROM transfers WHERE id=?", COLS))
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
        match rows.next() {
            Ok(Some(r)) => Ok(Some(scan_transfer(r).map_err(|e| e.to_string())?)),
            Ok(None) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The most recent transfers, optionally filtered by text and kind.
    pub fn list(&self, query: &str, kind: &str, limit: i64) -> Result<Vec<Transfer>, String> {
        let limit = if limit <= 0 { 200 } else { limit };
        let mut where_parts: Vec<&str> = vec!["1=1"];
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let q = query.trim();
        if !q.is_empty() {
            let like = rusqlite::types::Value::Text(format!("%{}%", q));
            where_parts.push(
                "(title LIKE ? OR source LIKE ? OR dest LIKE ? OR error LIKE ? OR id IN (SELECT transfer_id FROM transfer_files WHERE rel_path LIKE ?))",
            );
            for _ in 0..5 {
                args.push(like.clone());
            }
        }
        if kind == "upload" || kind == "download" {
            where_parts.push("kind=?");
            args.push(rusqlite::types::Value::Text(kind.to_string()));
        }
        args.push(rusqlite::types::Value::Integer(limit));
        let sql = format!(
            "SELECT {} FROM transfers WHERE {} ORDER BY created_at DESC LIMIT ?",
            COLS,
            where_parts.join(" AND ")
        );
        let conn = self.db();
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut rows = stmt.query(rusqlite::params_from_iter(args)).map_err(|e| e.to_string())?;
        let mut out: Vec<Transfer> = Vec::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            out.push(scan_transfer(r).map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn files(&self, id: &str) -> Result<Vec<File>, String> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT transfer_id, idx, rel_path, local_path, size, status, error, note, remote_parent, \
remote_id, view_url, asset_id, finished_at FROM transfer_files WHERE transfer_id=? ORDER BY idx",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
        let mut out: Vec<File> = Vec::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            out.push(File {
                transfer_id: text(r, 0).map_err(|e| e.to_string())?,
                idx: num(r, 1).map_err(|e| e.to_string())?,
                rel_path: text(r, 2).map_err(|e| e.to_string())?,
                local_path: text(r, 3).map_err(|e| e.to_string())?,
                size: num(r, 4).map_err(|e| e.to_string())?,
                status: text(r, 5).map_err(|e| e.to_string())?,
                error: text(r, 6).map_err(|e| e.to_string())?,
                note: text(r, 7).map_err(|e| e.to_string())?,
                remote_parent: text(r, 8).map_err(|e| e.to_string())?,
                remote_id: text(r, 9).map_err(|e| e.to_string())?,
                view_url: text(r, 10).map_err(|e| e.to_string())?,
                asset_id: text(r, 11).map_err(|e| e.to_string())?,
                finished_at: num(r, 12).map_err(|e| e.to_string())?,
            });
        }
        Ok(out)
    }

    /// Where `asset_id` was most recently downloaded inside `dir` by a transfer
    /// other than `exclude`, or "" if never.
    pub fn asset_path(&self, asset_id: &str, dir: &str, exclude: &str) -> String {
        if asset_id.is_empty() {
            return String::new();
        }
        let conn = self.db();
        let mut stmt = match conn.prepare(
            "SELECT local_path FROM transfer_files WHERE asset_id=? AND transfer_id<>? ORDER BY rowid DESC LIMIT 50",
        ) {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        let mut rows = match stmt.query(rusqlite::params![asset_id, exclude]) {
            Ok(r) => r,
            Err(_) => return String::new(),
        };
        loop {
            match rows.next() {
                Ok(Some(r)) => {
                    let p = text(r, 0).unwrap_or_default();
                    if parent_dir(&p) == dir {
                        return p;
                    }
                }
                _ => return String::new(),
            }
        }
    }

    /// The asset last downloaded to `local_path` by a transfer other than
    /// `exclude`, or "" if history has no finished download there.
    pub fn path_owner(&self, local_path: &str, exclude: &str) -> String {
        let conn = self.db();
        conn.query_row(
            "SELECT asset_id FROM transfer_files WHERE local_path=? AND transfer_id<>? AND asset_id<>'' \
AND status IN ('done','skipped') ORDER BY rowid DESC LIMIT 1",
            rusqlite::params![local_path, exclude],
            |r| text(r, 0),
        )
        .unwrap_or_default()
    }

    /// Writes one row per file across all transfers.
    pub fn export_csv(&self, path: &Path) -> Result<(), String> {
        use std::io::Write;
        let fh = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut w = std::io::BufWriter::new(fh);
        let header: Vec<String> = [
            "transfer_id",
            "direction",
            "transfer",
            "started",
            "finished",
            "transfer_status",
            "avg_MBps",
            "source",
            "destination",
            "file",
            "local_path",
            "size_bytes",
            "file_status",
            "error",
            "frameio_url",
        ]
        .iter()
        .map(|s| csv_field(s))
        .collect();
        writeln!(w, "{}", header.join(",")).map_err(|e| e.to_string())?;

        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.kind, t.title, t.started_at, t.finished_at, t.status, t.avg_bps, t.source, t.dest,
f.rel_path, f.local_path, f.size, f.status, COALESCE(NULLIF(f.error,''), t.error), f.view_url
FROM transfers t LEFT JOIN transfer_files f ON f.transfer_id = t.id ORDER BY t.created_at DESC, f.idx",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let avg = |i: usize| -> f64 { r.get::<_, Option<f64>>(i).ok().flatten().unwrap_or(0.0) };
            let row: Vec<String> = vec![
                text(r, 0).map_err(|e| e.to_string())?,
                text(r, 1).map_err(|e| e.to_string())?,
                text(r, 2).map_err(|e| e.to_string())?,
                fmt_time(num(r, 3).map_err(|e| e.to_string())?),
                fmt_time(num(r, 4).map_err(|e| e.to_string())?),
                text(r, 5).map_err(|e| e.to_string())?,
                format!("{:.2}", avg(6) / 1e6),
                text(r, 7).map_err(|e| e.to_string())?,
                text(r, 8).map_err(|e| e.to_string())?,
                text(r, 9).map_err(|e| e.to_string())?,
                text(r, 10).map_err(|e| e.to_string())?,
                num(r, 11).map_err(|e| e.to_string())?.to_string(),
                text(r, 12).map_err(|e| e.to_string())?,
                text(r, 13).map_err(|e| e.to_string())?,
                text(r, 14).map_err(|e| e.to_string())?,
            ];
            let line: Vec<String> = row.iter().map(|s| csv_field(s)).collect();
            writeln!(w, "{}", line.join(",")).map_err(|e| e.to_string())?;
        }
        drop(rows);
        drop(stmt);
        w.flush().map_err(|e| e.to_string())
    }

    /// Appends a line to this month's transfer log.
    pub fn log(&self, line: &str) {
        use std::io::Write;
        let _g = self.log_mu.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_ms();
        let p = self.log_dir.join(format!("transfers-{}.log", fmt_month(now)));
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        // 0644 on unix: a log a human reads. Windows has no mode bits, and the
        // file inherits the ACL of the folder it is created in (see
        // `fileio::mode`).
        crate::fileio::mode(&mut opts, 0o644);
        let fh = opts.open(p);
        if let Ok(mut fh) = fh {
            let _ = writeln!(fh, "{}  {}", fmt_stamp(now), line);
        }
    }

    /// The current month's log file path.
    pub fn log_path(&self) -> String {
        self.log_dir
            .join(format!("transfers-{}.log", fmt_month(now_ms())))
            .to_string_lossy()
            .into_owned()
    }
}

/// Adds a column if an older database doesn't have it yet.
fn add_column(conn: &rusqlite::Connection, table: &str, col: &str, typ: &str) {
    let sql = format!("PRAGMA table_info({})", table);
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(_) => return,
    };
    loop {
        match rows.next() {
            Ok(Some(r)) => {
                // cid, name, type, notnull, dflt_value, pk
                if let Ok(name) = r.get::<_, String>(1) {
                    if name == col {
                        return;
                    }
                }
            }
            Ok(None) => break,
            Err(_) => return,
        }
    }
    drop(rows);
    drop(stmt);
    // The table and column names come from this file, never from user data.
    let _ = conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, col, typ));
}

fn scan_transfer(r: &rusqlite::Row<'_>) -> rusqlite::Result<Transfer> {
    Ok(Transfer {
        id: text(r, 0)?,
        kind: text(r, 1)?,
        title: text(r, 2)?,
        source: text(r, 3)?,
        dest: text(r, 4)?,
        account_id: text(r, 5)?,
        folder_id: text(r, 6)?,
        share_id: text(r, 7)?,
        status: text(r, 8)?,
        error: text(r, 9)?,
        total_bytes: num(r, 10)?,
        done_bytes: num(r, 11)?,
        files_total: num(r, 12)?,
        files_done: num(r, 13)?,
        files_failed: num(r, 14)?,
        created_at: num(r, 15)?,
        started_at: num(r, 16)?,
        finished_at: num(r, 17)?,
        avg_bps: real(r, 18)?,
        peak_bps: real(r, 19)?,
        options: text(r, 20)?,
        link: text(r, 21)?,
        share_link: text(r, 22)?,
    })
}

// Every text column of `transfers` is nullable; the Go code read them through
// sql.NullString for the same reason.
fn text(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<String> {
    Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
}

fn num(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<i64> {
    Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0))
}

fn real(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<f64> {
    Ok(r.get::<_, Option<f64>>(i)?.unwrap_or(0.0))
}

/// Go's filepath.Dir, for the folder comparison AssetPath makes.
fn parent_dir(p: &str) -> String {
    match Path::new(p).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    }
}

/// One CSV field the way Go's encoding/csv writes it.
fn csv_field(s: &str) -> String {
    let needs_quotes = !s.is_empty()
        && (s == "\\."
            || s.chars().any(|c| c == ',' || c == '"' || c == '\r' || c == '\n')
            || s.trim_start_matches('\u{feff}').chars().next().map(|c| c.is_whitespace()).unwrap_or(false));
    if !needs_quotes {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\"\""),
            '\r' | '\n' => out.push(c),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn fmt_time(ms: i64) -> String {
    if ms == 0 {
        return String::new();
    }
    fmt_stamp(ms)
}

/// Unix milliseconds, as everything in the database and the views counts time.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Local broken-down time, the way Go's time.Now() reports it (the machine's
/// zone, DST included).
fn local_tm(ms: i64) -> Option<fiotransfer_sys::platform::LocalParts> {
    fiotransfer_sys::platform::local_time(ms.div_euclid(1000))
}

/// "2006-01-02 15:04:05" for a unix-ms stamp.
fn fmt_stamp(ms: i64) -> String {
    match local_tm(ms) {
        Some(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.year, t.month, t.day, t.hour, t.minute, t.second
        ),
        None => String::new(),
    }
}

/// "2006-01", the log file's month.
fn fmt_month(ms: i64) -> String {
    match local_tm(ms) {
        Some(t) => format!("{:04}-{:02}", t.year, t.month),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quoting() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field(""), "");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
        assert_eq!(csv_field(" leading"), "\" leading\"");
        assert_eq!(csv_field("\\."), "\"\\.\"");
    }

    #[test]
    fn zero_time_prints_nothing() {
        assert_eq!(fmt_time(0), "");
        // 2023-11-14 22:13:20 UTC, in whatever zone the machine is in.
        let s = fmt_time(1_700_000_000_000);
        assert_eq!(s.len(), 19, "{}", s);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], " ");
        assert_eq!(fmt_month(1_700_000_000_000).len(), 7);
    }

    #[test]
    fn parent_dir_matches_go_filepath_dir() {
        assert_eq!(parent_dir("/tmp/a/a.mov"), "/tmp/a");
        assert_eq!(parent_dir("a.mov"), ".");
    }
}
