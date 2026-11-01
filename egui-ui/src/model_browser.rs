//! The five shapes the browser capture draws, for wasm32.
//!
//! `fiotransfer-core` cannot build for `wasm32-unknown-unknown`: it is tokio,
//! rustls and rusqlite underneath, and none of those compile there. The browser
//! build (`src/wasm.rs`) is a *renderer* of the fixture, not the app: it has no
//! engine, no API and no disk. What it does have is five screens that read
//! `UiState`, so it needs the types those screens read:
//!
//! * [`JobView`], [`FileView`], [`Snapshot`] — what the Transfers screen draws;
//! * [`Settings`] and [`User`] — what Settings and the sidebar show;
//! * [`Status`] — how a job or file is spelled.
//!
//! Every field has the same name and type as the core's struct of the same name
//! (`core/src/xfer/mod.rs`, `core/src/config.rs`, `core/src/api.rs`), so the
//! fixture, the views and the two front ends agree on what a job is. The
//! `#[serde(default)]` on each struct is what makes a snapshot written by hand
//! (`--snapshot`, the fixture) parse on the native build too.
//!
//! `model.rs` re-exports these with `pub use crate::model_browser::*` under
//! `#[cfg(target_arch = "wasm32")]`, and the core's own types everywhere else.

#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};

// ---------- Status ----------

/// How a job or a file is doing. The core's `xfer::Status`, same variants, same
/// spelling on the wire (lowercase).
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
    /// The word the stylesheet's `status.<x>` classes use.
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

    /// Done, one way or another: nothing more will happen to it.
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Status::Done | Status::Partial | Status::Failed | Status::Canceled | Status::Skipped
        )
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------- Jobs, files, snapshots ----------

/// One file in a transfer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

/// One transfer: a set of files moved to or from Frame.io.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

/// Everything the Transfers screen and the sidebar's counters read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub jobs: Vec<JobView>,
    pub speed: f64,
    pub active_streams: i64,
    pub stream_limit: i64,
}

// ---------- Settings and the signed-in user ----------

/// One level of a Frame.io path, as `settings.json` stores it (`config::Crumb`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Crumb {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub folder_id: String,
}

/// The user's settings (`config::Settings`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub client_id: String,
    pub redirect_port: i32,
    pub streams: i32,
    pub segment_mb: i32,
    pub download_dir: String,
    pub no_folder_logs: bool,
    pub no_update_check: bool,
    pub update_notified: String,
    pub last_account_id: String,
    pub last_folder_id: String,
    pub last_folder_label: String,
    pub last_folder_path: Vec<Crumb>,
    // ---- Portal ----
    // The bearer token is not here: it lives in the keychain, as on the native
    // build. Every field is `#[serde(default)]` above, so an older payload reads.
    pub portal_url: String,
    pub portal_root: String,
    pub portal_in_folder: String,
    pub portal_out_folder: String,
    pub portal_enabled: bool,
    pub portal_on_change: String,
    pub portal_every_secs: i64,
    /// The admin email the last Portal sign-in used: a setting, not a secret,
    /// so the "Sign in instead" field is prefilled with it.
    pub portal_email: String,
}

impl Default for Settings {
    /// The core's defaults, so the browser capture shows the numbers a first run
    /// shows: 16 streams, 64 MB segments and `~/Downloads`.
    fn default() -> Settings {
        let home = match std::env::var_os("HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => std::path::PathBuf::from("/"),
        };
        Settings {
            client_id: String::new(),
            redirect_port: 8765,
            streams: 16,
            segment_mb: 64,
            download_dir: home.join("Downloads").to_string_lossy().into_owned(),
            no_folder_logs: false,
            no_update_check: false,
            update_notified: String::new(),
            last_account_id: String::new(),
            last_folder_id: String::new(),
            last_folder_label: String::new(),
            last_folder_path: Vec::new(),
            portal_url: String::new(),
            portal_root: home.join("Portal").to_string_lossy().into_owned(),
            portal_in_folder: "IN".to_string(),
            portal_out_folder: "OUT".to_string(),
            portal_enabled: false,
            portal_on_change: "overwrite".to_string(),
            portal_every_secs: 30,
            portal_email: String::new(),
        }
    }
}

/// Who is signed in (`api::User`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_from_the_app_parses() {
        let json = r#"{"jobs":[{"id":"a","kind":"upload","title":"clip.mov","status":"running",
            "total":100,"done":25,"speed":10.0,"files_done":1,"files_total":4}],
            "speed":10.0,"active_streams":3,"stream_limit":16}"#;
        let s: Snapshot = serde_json::from_str(json).expect("snapshot parses");
        assert_eq!(s.jobs.len(), 1);
        assert_eq!(s.jobs[0].status, Status::Running);
        assert_eq!(s.jobs[0].status.as_str(), "running");
        assert_eq!(s.jobs[0].progress(), Some(0.25));
        assert!(s.jobs[0].is_upload());
        assert!(!s.jobs[0].terminal());
    }

    #[test]
    fn every_status_is_spelled_and_ends() {
        let all = [
            (Status::Queued, "queued", false),
            (Status::Resolving, "resolving", false),
            (Status::Running, "running", false),
            (Status::Done, "done", true),
            (Status::Partial, "partial", true),
            (Status::Failed, "failed", true),
            (Status::Canceled, "canceled", true),
            (Status::Skipped, "skipped", true),
        ];
        for (status, word, ends) in all {
            assert_eq!(status.as_str(), word);
            assert_eq!(status.terminal(), ends, "{word}");
            assert_eq!(status.to_string(), word);
        }
    }

    #[test]
    fn settings_and_a_user_have_the_cores_defaults() {
        let s = Settings::default();
        assert_eq!(s.streams, 16);
        assert_eq!(s.segment_mb, 64);
        assert_eq!(s.redirect_port, 8765);
        assert!(s.download_dir.ends_with("Downloads"));
        assert!(s.last_folder_path.is_empty());
        let u = User::default();
        assert!(u.name.is_empty() && u.email.is_empty());
    }
}
