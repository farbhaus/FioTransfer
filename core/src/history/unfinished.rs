//! Uploads that didn't finish, and files a new upload replaced.

use super::{now_ms, Store};
use std::collections::HashMap;

pub(crate) const UNFINISHED_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS unfinished_uploads(
  file_id TEXT PRIMARY KEY, account_id TEXT, transfer_id TEXT, rel_path TEXT, pid INTEGER, run TEXT, created_at INTEGER);
CREATE TABLE IF NOT EXISTS replaced_files(
  file_id TEXT PRIMARY KEY, account_id TEXT, new_id TEXT, transfer_id TEXT, rel_path TEXT, pid INTEGER, run TEXT, created_at INTEGER);
";

/// A file created on Frame.io whose bytes haven't all been sent yet. Its owner
/// (PID and Run) is the process sending it. A row with no owner, or whose owner
/// is gone (the app quit or crashed mid-upload), is a copy to delete from
/// Frame.io.
#[derive(Debug, Clone, Default)]
pub struct Unfinished {
    pub file_id: String,
    pub account_id: String,
    pub transfer_id: String,
    pub rel_path: String,
    pub pid: i64,
    pub run: String,
    pub created_at: i64,
}

/// A Frame.io file that a new upload of the same file (NewID) replaces. It's
/// deleted once Frame.io confirms the new upload. A row with no owner, or whose
/// owner is gone (the app quit while Frame.io was confirming), is left to
/// clean_up, which checks the new upload first.
#[derive(Debug, Clone, Default)]
pub struct Replaced {
    pub file_id: String,
    pub account_id: String,
    pub new_id: String,
    pub transfer_id: String,
    pub rel_path: String,
    pub pid: i64,
    pub run: String,
    pub created_at: i64,
}

impl Store {
    /// Records an upload in progress, or changes an existing row's owner
    /// (none: delete it).
    pub fn add_unfinished(&self, u: &Unfinished) {
        let created_at = if u.created_at == 0 { now_ms() } else { u.created_at };
        let conn = self.db();
        let _ = conn.execute(
            "INSERT INTO unfinished_uploads VALUES(?,?,?,?,?,?,?)
ON CONFLICT(file_id) DO UPDATE SET pid=excluded.pid, run=excluded.run",
            rusqlite::params![u.file_id, u.account_id, u.transfer_id, u.rel_path, u.pid, u.run, created_at],
        );
    }

    pub fn unfinished_uploads(&self) -> Result<Vec<Unfinished>, String> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT file_id, account_id, transfer_id, rel_path, pid, run, created_at FROM unfinished_uploads ORDER BY created_at",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut out: Vec<Unfinished> = Vec::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            out.push(Unfinished {
                file_id: str_col(r, 0).map_err(|e| e.to_string())?,
                account_id: str_col(r, 1).map_err(|e| e.to_string())?,
                transfer_id: str_col(r, 2).map_err(|e| e.to_string())?,
                rel_path: str_col(r, 3).map_err(|e| e.to_string())?,
                pid: int_col(r, 4).map_err(|e| e.to_string())?,
                run: str_col(r, 5).map_err(|e| e.to_string())?,
                created_at: int_col(r, 6).map_err(|e| e.to_string())?,
            });
        }
        Ok(out)
    }

    /// Drops the row of an upload whose bytes all reached Frame.io.
    pub fn upload_sent(&self, file_id: &str) {
        let conn = self.db();
        let _ = conn.execute("DELETE FROM unfinished_uploads WHERE file_id=?", [file_id]);
    }

    /// Drops the rows of a file deleted from Frame.io (an unfinished upload, or a
    /// version that was replaced), and the links to it in history.
    pub fn upload_deleted(&self, file_id: &str) {
        let conn = self.db();
        let _ = conn.execute("DELETE FROM unfinished_uploads WHERE file_id=?", [file_id]);
        let _ = conn.execute("DELETE FROM replaced_files WHERE file_id=?", [file_id]);
        let _ = conn.execute(
            "UPDATE transfers SET link='' WHERE link IN (SELECT view_url FROM transfer_files WHERE remote_id=? AND view_url!='')",
            [file_id],
        );
        let _ = conn.execute(
            "UPDATE transfer_files SET remote_id='', view_url='' WHERE remote_id=?",
            [file_id],
        );
        let _ = conn.execute("UPDATE watch_files SET remote_id='' WHERE remote_id=?", [file_id]);
    }

    /// Records a file to delete once its replacement is confirmed, or changes an
    /// existing row's owner (none: clean_up's to finish).
    pub fn add_replaced(&self, r: &Replaced) {
        let created_at = if r.created_at == 0 { now_ms() } else { r.created_at };
        let conn = self.db();
        let _ = conn.execute(
            "INSERT INTO replaced_files VALUES(?,?,?,?,?,?,?,?)
ON CONFLICT(file_id) DO UPDATE SET new_id=excluded.new_id, pid=excluded.pid, run=excluded.run",
            rusqlite::params![
                r.file_id,
                r.account_id,
                r.new_id,
                r.transfer_id,
                r.rel_path,
                r.pid,
                r.run,
                created_at,
            ],
        );
    }

    pub fn replaced_files(&self) -> Result<Vec<Replaced>, String> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT file_id, account_id, new_id, transfer_id, rel_path, pid, run, created_at FROM replaced_files ORDER BY created_at",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        let mut out: Vec<Replaced> = Vec::new();
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            out.push(Replaced {
                file_id: str_col(r, 0).map_err(|e| e.to_string())?,
                account_id: str_col(r, 1).map_err(|e| e.to_string())?,
                new_id: str_col(r, 2).map_err(|e| e.to_string())?,
                transfer_id: str_col(r, 3).map_err(|e| e.to_string())?,
                rel_path: str_col(r, 4).map_err(|e| e.to_string())?,
                pid: int_col(r, 5).map_err(|e| e.to_string())?,
                run: str_col(r, 6).map_err(|e| e.to_string())?,
                created_at: int_col(r, 7).map_err(|e| e.to_string())?,
            });
        }
        Ok(out)
    }

    /// Drops the row of a file whose replacement didn't make it to Frame.io: the
    /// earlier version stays.
    pub fn keep_replaced(&self, file_id: &str) {
        let conn = self.db();
        let _ = conn.execute("DELETE FROM replaced_files WHERE file_id=?", [file_id]);
    }

    /// Returns which of these Frame.io files FioTransfer uploaded and Frame.io
    /// confirmed (none if history can't be read).
    pub fn uploaded(&self, file_ids: &[String]) -> HashMap<String, bool> {
        let mut out: HashMap<String, bool> = HashMap::new();
        let conn = self.db();
        for id in file_ids {
            if id.is_empty() {
                continue;
            }
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM transfer_files WHERE remote_id=? AND status='done'",
                    [id],
                    |r| Ok(r.get::<_, Option<i64>>(0)?.unwrap_or(0)),
                )
                .unwrap_or(0);
            if n > 0 {
                out.insert(id.clone(), true);
            }
        }
        out
    }
}

fn str_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<String> {
    Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
}

fn int_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<i64> {
    Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::File;

    // Go's TestUploaded: only a confirmed upload that's still on Frame.io counts
    // as FioTransfer's.
    #[test]
    fn uploaded_only_counts_confirmed_rows() {
        let dir = std::env::temp_dir().join(format!("fio-unfinished-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let s = Store::open(&dir, &dir).unwrap();
        let f = |idx: i64, status: &str, remote: &str| File {
            transfer_id: "t1".into(),
            idx,
            status: status.into(),
            remote_id: remote.into(),
            ..Default::default()
        };
        s.start(
            &crate::history::Transfer { id: "t1".into(), kind: "upload".into(), ..Default::default() },
            &[
                f(0, "done", "ok"),
                f(1, "failed", "failed"),
                f(2, "canceled", "canceled"),
                f(3, "done", "deleted"),
            ],
        )
        .unwrap();
        s.upload_deleted("deleted");
        let ids: Vec<String> = ["ok", "failed", "canceled", "deleted", "other", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let got = s.uploaded(&ids);
        assert_eq!(got.len(), 1, "{:?}", got);
        assert!(got.contains_key("ok"));
        // The deleted file is no longer linked from history.
        assert!(s.files("t1").unwrap().iter().all(|f| f.view_url.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
