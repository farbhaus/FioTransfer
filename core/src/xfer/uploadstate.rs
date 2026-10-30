
//! What an interrupted upload left behind, so it can be continued instead of
//! started again.
//!
//! Downloads have resumed for a while (`.fiopart` / `.fiostate`); uploads did
//! not, because the parts Frame.io hands out belong to one file id — lose the id
//! and the bytes already sent are unreachable. This keeps that id, the part
//! layout, the part URLs and the list of parts already on Frame.io, in the app's
//! own directory (not next to the media, so a watched folder doesn't see it),
//! and lets the next attempt at the same file continue from where it stopped
//! while the presigned URLs are still good.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How long a saved upload is worth resuming: presigned URLs do not last
/// forever, and a stale ledger would mean a 403 on the first resumed part.
pub fn resume_window() -> std::time::Duration {
    std::time::Duration::from_secs(
        std::env::var("FIO_UPLOAD_RESUME_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60 * 60),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartState {
    pub off: i64,
    pub size: i64,
    pub url: String,
}

/// A resumable upload: everything needed to keep going.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadState {
    pub source: String,
    pub file_id: String,
    pub account_id: String,
    pub folder_id: String,
    pub name: String,
    pub size: i64,
    pub mtime_ms: i64,
    pub created_at: i64,
    /// The page Frame.io shows for the file once it is complete.
    #[serde(default)]
    pub view_url: String,
    pub parts: Vec<PartState>,
}

fn uploads_dir() -> PathBuf {
    let d = crate::config::dir().join("uploads");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// A stable name for a source path, so the ledger can be found from the file
/// alone (no index, nothing to keep in step).
fn key(source: &str) -> String {
    use sha2::Digest;
    let sum = sha2::Sha256::digest(source.as_bytes());
    sum.iter().take(12).map(|b| format!("{:02x}", b)).collect()
}

pub struct Ledger {
    header: PathBuf,
    done: PathBuf,
    source: String,
    /// The finished-parts file, kept open: one append per part, not one
    /// open/close per part (which cost a measurable slice of a fast upload).
    done_fh: std::sync::Mutex<Option<std::fs::File>>,
}

impl Ledger {
    pub fn for_source(source: &str) -> Ledger {
        Ledger::in_dir(&uploads_dir(), source)
    }

    /// A ledger rooted somewhere else, so a test never writes into the user's
    /// own profile (and never sees what a previous test left there).
    pub fn in_dir(dir: &Path, source: &str) -> Ledger {
        let base = dir.join(key(source));
        let mut header = base.clone().into_os_string();
        header.push(".json");
        let mut done = base.into_os_string();
        done.push(".done");
        Ledger {
            header: PathBuf::from(header),
            done: PathBuf::from(done),
            source: source.to_string(),
            done_fh: std::sync::Mutex::new(None),
        }
    }

    pub fn exists(&self) -> bool {
        self.header.exists()
    }

    pub fn header_path(&self) -> &Path {
        &self.header
    }

    /// Writes the header and forgets the parts of any earlier attempt.
    pub fn save(&self, st: &UploadState) -> std::io::Result<()> {
        let body = serde_json::to_vec(st)?;
        let tmp = self.header.with_extension("json.tmp");
        {
            let mut fh = std::fs::File::create(&tmp)?;
            fh.write_all(&body)?;
            // No fsync: a process crash still finds it in the page cache, and a
            // sync per file measurably slows a fast upload down.
        }
        std::fs::rename(&tmp, &self.header)?;
        *self.done_fh.lock().unwrap() = None;
        let _ = std::fs::remove_file(&self.done);
        Ok(())
    }

    /// The saved upload, if it is for this file in this shape, and still fresh.
    /// Returns the state and, per part, whether it is already on Frame.io.
    pub fn load(&self, size: i64, mtime_ms: i64, now_ms: i64) -> Option<(UploadState, Vec<bool>)> {
        let body = std::fs::read(&self.header).ok()?;
        let st: UploadState = serde_json::from_slice(&body).ok()?;
        if st.source != self.source || st.size != size || st.mtime_ms != mtime_ms {
            return None; // the file changed: its part URLs no longer describe it
        }
        if !fresh(&st, now_ms) {
            return None; // the presigned URLs are likely dead
        }
        let mut done = vec![false; st.parts.len()];
        if let Ok(text) = std::fs::read_to_string(&self.done) {
            for line in text.lines() {
                if let Ok(i) = line.trim().parse::<usize>() {
                    if i < done.len() {
                        done[i] = true;
                    }
                }
            }
        }
        Some((st, done))
    }

    /// Records that part `i` is on Frame.io. One short line per part, appended:
    /// cheap enough to do per part, and replayable after a crash.
    pub fn mark_done(&self, i: usize) {
        let mut g = self.done_fh.lock().unwrap();
        if g.is_none() {
            *g = std::fs::OpenOptions::new().create(true).append(true).open(&self.done).ok();
            // No fsync: losing the last few lines only means resending a few
            // parts after a crash, and paying for a sync per part would slow
            // every upload down for it.
        }
        if let Some(fh) = g.as_mut() {
            let _ = writeln!(fh, "{}", i);
        }
    }

    pub fn remove(&self) {
        *self.done_fh.lock().unwrap() = None;
        let _ = std::fs::remove_file(&self.header);
        let _ = std::fs::remove_file(&self.done);
    }
}

pub fn fresh(st: &UploadState, now_ms: i64) -> bool {
    // `created_at` and `now_ms` are milliseconds (see now_ms()), so the window
    // has to be compared in milliseconds too: as_secs() here made an hour-long
    // window expire after 3.6 seconds.
    let age = now_ms - st.created_at;
    age >= 0 && (age as u64) <= resume_window().as_millis() as u64
}

/// Whether some resumable upload still holds this Frame.io file id, so the
/// housekeeping must not delete that file.
pub fn keeps_file(file_id: &str, now_ms: i64) -> bool {
    keeps_file_in(&uploads_dir(), file_id, now_ms)
}

/// `keeps_file` for a ledger directory other than the app's own.
pub fn keeps_file_in(dir: &Path, file_id: &str, now_ms: i64) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        if let Ok(body) = std::fs::read(&p) {
            if let Ok(st) = serde_json::from_slice::<UploadState>(&body) {
                if st.file_id == file_id && fresh(&st, now_ms) {
                    return true;
                }
            }
        }
    }
    false
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn mtime_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_source(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("fio-src-{}-{}", std::process::id(), tag));
        std::fs::write(&p, b"x").unwrap();
        p.to_string_lossy().into_owned()
    }

    fn state(source: &str, size: i64, mtime_ms: i64, now: i64) -> UploadState {
        state_id(source, size, mtime_ms, now, "file-1")
    }

    fn state_id(source: &str, size: i64, mtime_ms: i64, now: i64, id: &str) -> UploadState {
        UploadState {
            source: source.to_string(),
            file_id: id.to_string(),
            account_id: "acct".into(),
            folder_id: "folder".into(),
            name: "a.bin".into(),
            size,
            mtime_ms,
            created_at: now,
            view_url: "http://view/1".into(),
            parts: vec![
                PartState { off: 0, size: 4, url: "http://x/0".into() },
                PartState { off: 4, size: 4, url: "http://x/1".into() },
            ],
        }
    }

    #[test]
    fn a_saved_upload_comes_back_with_the_parts_that_finished() {
        let src = tmp_source("round");
        let l = Ledger::for_source(&src);
        let now = now_ms();
        let id = format!("file-round-{}", std::process::id());
        l.save(&state_id(&src, 8, 1234, now, &id)).unwrap();
        l.mark_done(1);
        let (st, done) = l.load(8, 1234, now).expect("a resumable upload");
        assert_eq!(st.file_id, id);
        assert_eq!(done, vec![false, true]);
        assert!(keeps_file(&id, now), "the housekeeping must leave that file alone");
        l.remove();
        assert!(!l.exists());
        assert!(!keeps_file(&id, now));
        let _ = std::fs::remove_file(&src);
    }

    #[test]
    fn a_changed_file_or_a_stale_upload_is_not_resumed() {
        let src = tmp_source("stale");
        let l = Ledger::for_source(&src);
        let now = now_ms();
        l.save(&state(&src, 8, 1234, now)).unwrap();
        assert!(l.load(9, 1234, now).is_none(), "size changed");
        assert!(l.load(8, 9999, now).is_none(), "mtime changed");
        assert!(l.load(8, 1234, now + 24 * 3600 * 1000).is_none(), "too old");
        l.remove();
        let _ = std::fs::remove_file(&src);
    }

    #[test]
    fn saving_again_forgets_the_earlier_parts() {
        let src = tmp_source("again");
        let l = Ledger::for_source(&src);
        let now = now_ms();
        l.save(&state(&src, 8, 1234, now)).unwrap();
        l.mark_done(0);
        l.mark_done(1);
        l.save(&state(&src, 8, 1234, now)).unwrap();
        let (_, done) = l.load(8, 1234, now).unwrap();
        assert_eq!(done, vec![false, false], "a fresh attempt starts with nothing done");
        l.remove();
        let _ = std::fs::remove_file(&src);
    }

    // Regression: `fresh` compared a millisecond age against a window expressed
    // in seconds, so a ledger stopped being resumable 3.6 seconds after it was
    // written instead of an hour later. An interrupted upload was therefore
    // always restarted from zero, and clean_up deleted its copy on Frame.io.
    #[test]
    fn a_ledger_saved_a_minute_ago_still_resumes() {
        let dir = std::env::temp_dir().join(format!("fio-resume-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = "/media/window.mov".to_string();
        let l = Ledger::in_dir(&dir, &src);
        let now = now_ms();
        l.save(&state(&src, 8, 1234, now)).unwrap();
        let window_ms = resume_window().as_millis() as i64;
        assert!(resume_window().as_secs() >= 60, "the default window is an hour, not seconds");
        assert!(l.load(8, 1234, now + 60_000).is_some(), "one minute is inside the window");
        assert!(l.load(8, 1234, now + window_ms).is_some(), "the window is inclusive");
        assert!(l.load(8, 1234, now + window_ms + 1_000).is_none(), "past the window");
        assert!(l.load(8, 1234, now - 1_000).is_none(), "a stamp in the future is not fresh");
        l.remove();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_file_with_the_same_shape_has_its_own_ledger() {
        let a = tmp_source("a");
        let b = tmp_source("b");
        let la = Ledger::for_source(&a);
        let lb = Ledger::for_source(&b);
        assert_ne!(la.header_path(), lb.header_path());
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }
}
