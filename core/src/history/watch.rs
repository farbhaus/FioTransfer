//! Watch-folder rows.

use super::{now_ms, Store};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(crate) const WATCH_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS watches(
  id TEXT PRIMARY KEY, local_path TEXT, account_id TEXT, folder_id TEXT, dest_label TEXT,
  enabled INTEGER, created_at INTEGER, origin TEXT);
CREATE TABLE IF NOT EXISTS watch_files(
  watch_id TEXT, rel_path TEXT, size INTEGER, mtime INTEGER, state TEXT, attempts INTEGER,
  error TEXT, remote_id TEXT, updated_at INTEGER, PRIMARY KEY(watch_id, rel_path));
CREATE TABLE IF NOT EXISTS watch_dirs(
  watch_id TEXT, rel_dir TEXT, remote_id TEXT, PRIMARY KEY(watch_id, rel_dir));
CREATE TABLE IF NOT EXISTS watch_remote(
  watch_id TEXT, remote_id TEXT, rel_path TEXT, size INTEGER, state TEXT, attempts INTEGER,
  error TEXT, updated_at INTEGER, PRIMARY KEY(watch_id, remote_id));
";

// Watch directions.
/// Local folder → Frame.io.
pub const DIR_UPLOAD: &str = "upload";
/// Frame.io → local folder.
pub const DIR_DOWNLOAD: &str = "download";
/// Both ways; a deletion on one side isn't copied to the other.
pub const DIR_MIRROR: &str = "mirror";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Watch {
    pub id: String,
    pub local_path: String,
    pub account_id: String,
    pub folder_id: String,
    pub dest_label: String,
    pub enabled: bool,
    pub created_at: i64,
    pub direction: String,
    /// IgnoreRemote: the next Frame.io listing records what it finds as existing
    /// files instead of downloading them (set when downloading starts, unless
    /// existing files are included).
    pub ignore_remote: bool,
    /// ON_CHANGE_OVERWRITE or ON_CHANGE_KEEP.
    pub on_change: String,
    /// How this watch came to exist. Empty for one the user made; `portal:<link
    /// id>` for one the Portal reconciler owns, which is the only kind it may
    /// update or remove. A watch the user made is never touched by Portal,
    /// whatever folder it points at.
    pub origin: String,
}

impl Watch {
    pub fn uploads(&self) -> bool {
        self.direction != DIR_DOWNLOAD
    }

    pub fn downloads(&self) -> bool {
        self.direction == DIR_DOWNLOAD || self.direction == DIR_MIRROR
    }
}

/// Whether `d` names a watch direction.
pub fn valid_direction(d: &str) -> bool {
    d == DIR_UPLOAD || d == DIR_DOWNLOAD || d == DIR_MIRROR
}

// What an uploading watch does with a local file that changed after it was
// uploaded (a re-render): it uploads again either way.
/// The earlier copy on Frame.io is deleted once the new one is confirmed.
pub const ON_CHANGE_OVERWRITE: &str = "overwrite";
/// Both stay on Frame.io.
pub const ON_CHANGE_KEEP: &str = "keep";

/// Whether `c` names a rule for changed files.
pub fn valid_on_change(c: &str) -> bool {
    c == ON_CHANGE_OVERWRITE || c == ON_CHANGE_KEEP
}

// Watch file states. Local files (watch_files) are uploaded, downloaded,
// failed, ignored, canceled or queued; Frame.io files (watch_remote) are
// downloaded, waiting, failed, ignored or canceled. A local file whose size or
// mtime no longer match its uploaded, downloaded, ignored or canceled record is
// a new version: it uploads again, and the watch's OnChange rule says whether
// its earlier copy on Frame.io goes.
pub const WATCH_UPLOADED: &str = "uploaded";
/// Written by the watch from Frame.io.
pub const WATCH_DOWNLOADED: &str = "downloaded";
pub const WATCH_FAILED: &str = "failed";
/// Existed before the watch was created, or already on the other side.
pub const WATCH_IGNORED: &str = "ignored";
/// The user canceled its transfer; not retried automatically.
pub const WATCH_CANCELED: &str = "canceled";
/// Frame.io isn't serving the original yet; tried again at the next listing.
pub const WATCH_WAITING: &str = "waiting";
/// To upload again (Retry, "Transfer them"); the record stays, so it's still a
/// new version of its earlier copy.
pub const WATCH_QUEUED: &str = "queued";

#[derive(Debug, Clone, Default)]
pub struct WatchFile {
    pub rel_path: String,
    pub size: i64,
    pub mtime: i64,
    pub state: String,
    pub attempts: i64,
    pub error: String,
    pub remote_id: String,
    pub updated_at: i64,
}

/// A Frame.io file a downloading watch has dealt with, by file ID.
#[derive(Debug, Clone, Default)]
pub struct WatchRemote {
    pub remote_id: String,
    pub rel_path: String,
    pub size: i64,
    pub state: String,
    pub attempts: i64,
    pub error: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct WatchStats {
    pub uploaded: i64,
    pub downloaded: i64,
    pub failed: i64,
    pub canceled: i64,
    pub last_transfer: i64,
}

impl Store {
    pub fn add_watch(&self, w: &Watch) -> Result<(), String> {
        let conn = self.db();
        conn.execute(
            "INSERT INTO watches(id, local_path, account_id, folder_id, dest_label, enabled, created_at, direction, ignore_remote, on_change, origin)
VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                w.id,
                w.local_path,
                w.account_id,
                w.folder_id,
                w.dest_label,
                w.enabled,
                w.created_at,
                w.direction,
                w.ignore_remote,
                w.on_change,
                w.origin,
            ],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    pub fn watches(&self) -> Result<Vec<Watch>, String> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT id, local_path, account_id, folder_id, dest_label, enabled, created_at, direction, ignore_remote, on_change, origin
FROM watches ORDER BY created_at",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut out: Vec<Watch> = Vec::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            out.push(Watch {
                id: r.get::<_, Option<String>>(0).map_err(|e| e.to_string())?.unwrap_or_default(),
                local_path: r.get::<_, Option<String>>(1).map_err(|e| e.to_string())?.unwrap_or_default(),
                account_id: r.get::<_, Option<String>>(2).map_err(|e| e.to_string())?.unwrap_or_default(),
                folder_id: r.get::<_, Option<String>>(3).map_err(|e| e.to_string())?.unwrap_or_default(),
                dest_label: r.get::<_, Option<String>>(4).map_err(|e| e.to_string())?.unwrap_or_default(),
                enabled: r.get::<_, Option<bool>>(5).map_err(|e| e.to_string())?.unwrap_or(false),
                created_at: r.get::<_, Option<i64>>(6).map_err(|e| e.to_string())?.unwrap_or(0),
                direction: r.get::<_, Option<String>>(7).map_err(|e| e.to_string())?.unwrap_or_default(),
                ignore_remote: r.get::<_, Option<bool>>(8).map_err(|e| e.to_string())?.unwrap_or(false),
                on_change: r.get::<_, Option<String>>(9).map_err(|e| e.to_string())?.unwrap_or_default(),
                origin: r.get::<_, Option<String>>(10).map_err(|e| e.to_string())?.unwrap_or_default(),
            });
        }
        Ok(out)
    }

    pub fn update_watch(&self, w: &Watch) -> Result<(), String> {
        let conn = self.db();
        conn.execute(
            "UPDATE watches SET local_path=?, account_id=?, folder_id=?, dest_label=?, direction=?, ignore_remote=?, on_change=?, origin=? WHERE id=?",
            rusqlite::params![
                w.local_path,
                w.account_id,
                w.folder_id,
                w.dest_label,
                w.direction,
                w.ignore_remote,
                w.on_change,
                w.origin,
                w.id,
            ],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Clears or sets the "record what Frame.io holds as existing" flag.
    pub fn set_ignore_remote(&self, id: &str, on: bool) {
        let conn = self.db();
        let _ = conn.execute("UPDATE watches SET ignore_remote=? WHERE id=?", rusqlite::params![on, id]);
    }

    /// Forgets every local file record (used when the watched folder changes).
    pub fn reset_watch_files(&self, id: &str) {
        let conn = self.db();
        let _ = conn.execute("DELETE FROM watch_files WHERE watch_id=?", [id]);
    }

    /// Forgets every Frame.io file record (used when the Frame.io folder changes).
    pub fn reset_watch_remotes(&self, id: &str) {
        let conn = self.db();
        let _ = conn.execute("DELETE FROM watch_remote WHERE watch_id=?", [id]);
    }

    /// Lets canceled files transfer again.
    pub fn requeue_canceled(&self, id: &str) {
        let conn = self.db();
        let _ = conn.execute(
            "UPDATE watch_files SET state='queued', attempts=0, error='' WHERE watch_id=? AND state='canceled'",
            [id],
        );
        let _ = conn.execute("DELETE FROM watch_remote WHERE watch_id=? AND state='canceled'", [id]);
    }

    /// Queues these local files to upload again if their records are failed or
    /// canceled.
    pub fn requeue_watch_files(&self, id: &str, rels: &[String]) -> Result<(), String> {
        self.exec_each(
            "UPDATE watch_files SET state='queued', attempts=0, error='' WHERE watch_id=? AND rel_path=? AND state IN ('failed','canceled')",
            id,
            rels,
        )
    }

    /// Drops the failed, waiting and canceled records of these Frame.io files so
    /// the watch downloads them again.
    pub fn forget_watch_remotes(&self, id: &str, remote_ids: &[String]) -> Result<(), String> {
        self.exec_each(
            "DELETE FROM watch_remote WHERE watch_id=? AND remote_id=? AND state IN ('failed','waiting','canceled')",
            id,
            remote_ids,
        )
    }

    /// Runs `query` once per key, in one transaction.
    fn exec_each(&self, query: &str, id: &str, keys: &[String]) -> Result<(), String> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut conn = self.db();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx.prepare(query).map_err(|e| e.to_string())?;
            for k in keys {
                stmt.execute(rusqlite::params![id, k]).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn set_watch_enabled(&self, id: &str, on: bool) {
        let conn = self.db();
        let _ = conn.execute("UPDATE watches SET enabled=? WHERE id=?", rusqlite::params![on, id]);
    }

    pub fn delete_watch(&self, id: &str) {
        let conn = self.db();
        let _ = conn.execute("DELETE FROM watch_files WHERE watch_id=?", [id]);
        let _ = conn.execute("DELETE FROM watch_remote WHERE watch_id=?", [id]);
        let _ = conn.execute("DELETE FROM watch_dirs WHERE watch_id=?", [id]);
        let _ = conn.execute("DELETE FROM watches WHERE id=?", [id]);
    }

    pub fn watch_files(&self, id: &str) -> Result<HashMap<String, WatchFile>, String> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT rel_path, size, mtime, state, attempts, error, remote_id, updated_at FROM watch_files WHERE watch_id=?",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
        let mut out: HashMap<String, WatchFile> = HashMap::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let f = WatchFile {
                rel_path: r.get::<_, Option<String>>(0).map_err(|e| e.to_string())?.unwrap_or_default(),
                size: r.get::<_, Option<i64>>(1).map_err(|e| e.to_string())?.unwrap_or(0),
                mtime: r.get::<_, Option<i64>>(2).map_err(|e| e.to_string())?.unwrap_or(0),
                state: r.get::<_, Option<String>>(3).map_err(|e| e.to_string())?.unwrap_or_default(),
                attempts: r.get::<_, Option<i64>>(4).map_err(|e| e.to_string())?.unwrap_or(0),
                error: r.get::<_, Option<String>>(5).map_err(|e| e.to_string())?.unwrap_or_default(),
                remote_id: r.get::<_, Option<String>>(6).map_err(|e| e.to_string())?.unwrap_or_default(),
                updated_at: r.get::<_, Option<i64>>(7).map_err(|e| e.to_string())?.unwrap_or(0),
            };
            out.insert(f.rel_path.clone(), f);
        }
        Ok(out)
    }

    pub fn put_watch_files(&self, id: &str, files: &[WatchFile]) -> Result<(), String> {
        self.put_watch_files_verb("INSERT OR REPLACE", id, files)
    }

    /// Records files as existing, keeping any record they already have.
    pub fn baseline_watch_files(&self, id: &str, files: &[WatchFile]) -> Result<(), String> {
        self.put_watch_files_verb("INSERT OR IGNORE", id, files)
    }

    fn put_watch_files_verb(&self, verb: &str, id: &str, files: &[WatchFile]) -> Result<(), String> {
        let mut conn = self.db();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        {
            let mut stmt = tx
                .prepare(&format!("{} INTO watch_files VALUES(?,?,?,?,?,?,?,?,?)", verb))
                .map_err(|e| e.to_string())?;
            for f in files {
                let updated_at = if f.updated_at == 0 { now } else { f.updated_at };
                stmt.execute(rusqlite::params![
                    id,
                    f.rel_path,
                    f.size,
                    f.mtime,
                    f.state,
                    f.attempts,
                    f.error,
                    f.remote_id,
                    updated_at,
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn watch_remotes(&self, id: &str) -> Result<HashMap<String, WatchRemote>, String> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT remote_id, rel_path, size, state, attempts, error, updated_at FROM watch_remote WHERE watch_id=?",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([id]).map_err(|e| e.to_string())?;
        let mut out: HashMap<String, WatchRemote> = HashMap::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let f = WatchRemote {
                remote_id: r.get::<_, Option<String>>(0).map_err(|e| e.to_string())?.unwrap_or_default(),
                rel_path: r.get::<_, Option<String>>(1).map_err(|e| e.to_string())?.unwrap_or_default(),
                size: r.get::<_, Option<i64>>(2).map_err(|e| e.to_string())?.unwrap_or(0),
                state: r.get::<_, Option<String>>(3).map_err(|e| e.to_string())?.unwrap_or_default(),
                attempts: r.get::<_, Option<i64>>(4).map_err(|e| e.to_string())?.unwrap_or(0),
                error: r.get::<_, Option<String>>(5).map_err(|e| e.to_string())?.unwrap_or_default(),
                updated_at: r.get::<_, Option<i64>>(6).map_err(|e| e.to_string())?.unwrap_or(0),
            };
            out.insert(f.remote_id.clone(), f);
        }
        Ok(out)
    }

    pub fn put_watch_remotes(&self, id: &str, files: &[WatchRemote]) -> Result<(), String> {
        self.put_watch_remotes_verb("INSERT OR REPLACE", id, files)
    }

    /// Records Frame.io files as existing, keeping any record they already have.
    pub fn baseline_watch_remotes(&self, id: &str, files: &[WatchRemote]) -> Result<(), String> {
        self.put_watch_remotes_verb("INSERT OR IGNORE", id, files)
    }

    fn put_watch_remotes_verb(&self, verb: &str, id: &str, files: &[WatchRemote]) -> Result<(), String> {
        let mut conn = self.db();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        {
            let mut stmt = tx
                .prepare(&format!("{} INTO watch_remote VALUES(?,?,?,?,?,?,?,?)", verb))
                .map_err(|e| e.to_string())?;
            for f in files {
                let updated_at = if f.updated_at == 0 { now } else { f.updated_at };
                stmt.execute(rusqlite::params![
                    id,
                    f.remote_id,
                    f.rel_path,
                    f.size,
                    f.state,
                    f.attempts,
                    f.error,
                    updated_at,
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn watch_stats(&self, id: &str) -> WatchStats {
        let mut st = WatchStats::default();
        let conn = self.db();
        let local = conn.query_row(
            "SELECT COUNT(*) FILTER (WHERE state='uploaded'), COUNT(*) FILTER (WHERE state='failed'),
COUNT(*) FILTER (WHERE state='canceled'), MAX(CASE WHEN state='uploaded' THEN updated_at END)
FROM watch_files WHERE watch_id=?",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            },
        );
        let mut last_up = 0;
        if let Ok((up, failed, canceled, last)) = local {
            st.uploaded = up;
            st.failed = failed;
            st.canceled = canceled;
            last_up = last;
        }
        let remote = conn.query_row(
            "SELECT COUNT(*) FILTER (WHERE state='downloaded'), COUNT(*) FILTER (WHERE state='failed'),
COUNT(*) FILTER (WHERE state='canceled'), MAX(CASE WHEN state='downloaded' THEN updated_at END)
FROM watch_remote WHERE watch_id=?",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ))
            },
        );
        let mut last_down = 0;
        if let Ok((down, failed, canceled, last)) = remote {
            st.downloaded = down;
            st.failed += failed;
            st.canceled += canceled;
            last_down = last;
        }
        st.last_transfer = last_up.max(last_down);
        st
    }

    pub fn watch_dirs(&self, id: &str) -> HashMap<String, String> {
        let mut out: HashMap<String, String> = HashMap::new();
        let conn = self.db();
        let mut stmt = match conn.prepare("SELECT rel_dir, remote_id FROM watch_dirs WHERE watch_id=?") {
            Ok(s) => s,
            Err(_) => return out,
        };
        let mut rows = match stmt.query([id]) {
            Ok(r) => r,
            Err(_) => return out,
        };
        while let Ok(Some(r)) = rows.next() {
            let d = r.get::<_, Option<String>>(0).ok().flatten().unwrap_or_default();
            let remote = r.get::<_, Option<String>>(1).ok().flatten().unwrap_or_default();
            out.insert(d, remote);
        }
        out
    }

    pub fn put_watch_dir(&self, id: &str, rel: &str, remote: &str) {
        let conn = self.db();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO watch_dirs VALUES(?,?,?)",
            rusqlite::params![id, rel, remote],
        );
    }

    pub fn clear_watch_dirs(&self, id: &str) {
        let conn = self.db();
        let _ = conn.execute("DELETE FROM watch_dirs WHERE watch_id=?", [id]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "fio-watch-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir, &dir).unwrap(), dir)
    }

    // Go's TestWatchMigration: a database from before watch directions keeps its
    // watches, as uploads.
    #[test]
    fn watch_migration() {
        let dir = std::env::temp_dir().join(format!("fio-watch-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        {
            let db = rusqlite::Connection::open(dir.join("history.db")).unwrap();
            db.execute_batch(
                "CREATE TABLE watches(id TEXT PRIMARY KEY, local_path TEXT, account_id TEXT, folder_id TEXT, dest_label TEXT,
  enabled INTEGER, created_at INTEGER);
  INSERT INTO watches VALUES('w1', '/Volumes/Media/In', 'acct', 'folder', 'P / In', 1, 1700000000000)",
            )
            .unwrap();
        }
        let s = Store::open(&dir, &dir).unwrap();
        let list = s.watches().unwrap();
        assert_eq!(list.len(), 1);
        let w = &list[0];
        assert_eq!(w.direction, DIR_UPLOAD);
        assert!(!w.ignore_remote);
        assert!(w.uploads());
        assert!(!w.downloads());
        assert_eq!(w.local_path, "/Volumes/Media/In");
        assert_eq!(w.on_change, ON_CHANGE_OVERWRITE);

        let mut n = Watch {
            id: "w2".into(),
            local_path: "/tmp/x".into(),
            account_id: "a".into(),
            folder_id: "f".into(),
            enabled: true,
            created_at: 1_700_000_001_000,
            direction: DIR_MIRROR.into(),
            ignore_remote: true,
            on_change: ON_CHANGE_KEEP.into(),
            ..Default::default()
        };
        s.add_watch(&n).unwrap();
        n.ignore_remote = false;
        s.update_watch(&n).unwrap();
        let list = s.watches().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].direction, DIR_MIRROR);
        assert!(!list[1].ignore_remote);
        assert_eq!(list[1].on_change, ON_CHANGE_KEEP);

        s.put_watch_remotes(
            "w2",
            &[WatchRemote {
                remote_id: "r1".into(),
                rel_path: "a.mov".into(),
                size: 1,
                state: WATCH_DOWNLOADED.into(),
                ..Default::default()
            }],
        )
        .unwrap();
        // A baseline must not overwrite records.
        s.baseline_watch_remotes(
            "w2",
            &[
                WatchRemote { remote_id: "r1".into(), state: WATCH_IGNORED.into(), ..Default::default() },
                WatchRemote { remote_id: "r2".into(), state: WATCH_IGNORED.into(), ..Default::default() },
            ],
        )
        .unwrap();
        let got = s.watch_remotes("w2").unwrap();
        assert_eq!(got["r1"].state, WATCH_DOWNLOADED);
        assert_eq!(got["r2"].state, WATCH_IGNORED);
        let st = s.watch_stats("w2");
        assert_eq!(st.downloaded, 1);
        assert_ne!(st.last_transfer, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn directions_and_rules() {
        assert!(valid_direction(DIR_UPLOAD));
        assert!(valid_direction(DIR_DOWNLOAD));
        assert!(valid_direction(DIR_MIRROR));
        assert!(!valid_direction("sideways"));
        assert!(valid_on_change(ON_CHANGE_OVERWRITE));
        assert!(valid_on_change(ON_CHANGE_KEEP));
        assert!(!valid_on_change("merge"));
    }

    #[test]
    fn stats_add_both_sides() {
        let (s, dir) = temp_store();
        s.put_watch_files(
            "w",
            &[
                WatchFile { rel_path: "a".into(), state: WATCH_UPLOADED.into(), ..Default::default() },
                WatchFile { rel_path: "b".into(), state: WATCH_FAILED.into(), ..Default::default() },
            ],
        )
        .unwrap();
        s.put_watch_remotes(
            "w",
            &[
                WatchRemote { remote_id: "r1".into(), state: WATCH_FAILED.into(), ..Default::default() },
                WatchRemote { remote_id: "r2".into(), state: WATCH_CANCELED.into(), ..Default::default() },
            ],
        )
        .unwrap();
        let st = s.watch_stats("w");
        assert_eq!((st.uploaded, st.downloaded, st.failed, st.canceled), (1, 0, 2, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
