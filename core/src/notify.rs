//! Notifications.
//!
//! Notification Center banners, posted the way the Go app posted them — with
//! `osascript` (`display notification … with title "FioTransfer"`), so the
//! banner is attributed to Script Editor rather than to the app, exactly as
//! before. Two callers: a finished batch of transfers, and a new release.
//! Nothing here touches AppKit, so it works from any thread; the script runs on
//! a thread of its own, as Go ran it on a goroutine, and its exit status is
//! ignored.

use std::process::Command;

/// Posts a notification. `sound` is a Notification Center sound name such as
/// "Glass"; `None` posts silently.
pub fn notify(title: &str, message: &str, sound: Option<&str>) {
    let script = script_for(title, message, sound);
    let spawned = std::thread::Builder::new()
        .name("fio-notify".to_string())
        .spawn(move || {
            let _ = Command::new("/usr/bin/osascript").arg("-e").arg(&script).status();
        });
    if let Err(e) = spawned {
        eprintln!("FioTransfer: could not post a notification: {}", e);
    }
}

/// The AppleScript a notification runs.
fn script_for(title: &str, message: &str, sound: Option<&str>) -> String {
    let mut s = format!(
        "display notification \"{}\" with title \"{}\"",
        quote(message),
        quote(title)
    );
    if let Some(name) = sound.filter(|n| !n.is_empty()) {
        s.push_str(&format!(" sound name \"{}\"", quote(name)));
    }
    s
}

/// A string inside AppleScript quotes. Backslash and quote would end or break
/// the literal, so they are escaped; line breaks have no escape in AppleScript
/// and become spaces, which keeps the script one line.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_match_the_go_app() {
        // notifyFinished: "Transfers complete", or "3 transfer(s) with errors".
        assert_eq!(
            script_for("FioTransfer", "Transfers complete", Some("Glass")),
            "display notification \"Transfers complete\" with title \"FioTransfer\" sound name \"Glass\""
        );
        // notifyUpdate.
        assert_eq!(
            script_for("FioTransfer", "Version 1.4.0 is available", None),
            "display notification \"Version 1.4.0 is available\" with title \"FioTransfer\""
        );
    }

    #[test]
    fn quotes_and_line_breaks_can_not_break_the_script() {
        assert_eq!(
            script_for("FioTransfer", "say \"hi\" \\ bye", Some("")),
            "display notification \"say \\\"hi\\\" \\\\ bye\" with title \"FioTransfer\""
        );
        assert_eq!(quote("a\nb\tc"), "a b c");
    }
}
