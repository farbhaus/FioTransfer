//! What the logic needs from whatever is showing it.
//!
//! The app in [`crate::app`] is the Go app's `App` moved across unchanged: the
//! same methods, the same order of calls, the same strings. What it cannot
//! carry with it is the platform: a file panel, a question with two buttons, the
//! clipboard, an error alert, the notification banner, "show the window",
//! "quit". In the Wails-compatible build those were AppKit calls through
//! `fiotransfer-sys`; here they are one trait, and each front end answers them
//! its own way.
//!
//! Every method has a default, and the default is what a host with no window
//! can honestly do: no files chosen, no folder chosen, no question answered,
//! an empty clipboard. That is the same answer `mac/dialogs.m` gave a caller
//! before the app's event loop was running — cancelled rather than waited for —
//! so the bridge tests see the shapes they always saw.

use serde_json::Value;

/// The platform, as the app asks for it.
pub trait Host: Send + Sync + 'static {
    /// An event for the UI (`state-changed`, `update`, `service`, …), with the
    /// payload the webview front end received.
    fn emit(&self, _event: &str, _payload: Value) {}

    /// The app wants its window in front of the user.
    fn show_window(&self) {}

    /// The app is finished (after an update installed, or the user asked).
    fn quit(&self) {}

    /// Asks for files to upload; empty when cancelled.
    fn open_files(&self, _title: &str) -> Vec<String> {
        Vec::new()
    }

    /// Asks for one folder; `None` when cancelled. `default_dir` opens the panel
    /// there, and `can_create` lets the panel make a new folder.
    fn open_dir(&self, _title: &str, _default_dir: &str, _can_create: bool) -> Option<String> {
        None
    }

    /// Asks where to write a file; `None` when cancelled.
    fn save_file(&self, _title: &str, _default_name: &str) -> Option<String> {
        None
    }

    /// Asks a question with two answers: true when `ok` was pressed. `cancel` is
    /// the default button, as it was in the Go app.
    fn question(&self, _title: &str, _message: &str, _ok: &str, _cancel: &str) -> bool {
        false
    }

    /// Shows an error, with an OK button.
    fn error(&self, _title: &str, _message: &str) {}

    /// The clipboard as plain text; empty when it holds no text.
    fn clipboard_text(&self) -> String {
        String::new()
    }

    /// Replaces the clipboard with plain text.
    fn clipboard_set_text(&self, _text: &str) {}
}
