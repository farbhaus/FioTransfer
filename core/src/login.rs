//! Open at login.
//!
//! A per-user LaunchAgent that starts the app hidden in the menu bar, written
//! the way `login_darwin.go` writes it: the same label, the same plist bytes,
//! `/usr/bin/open -g <app> --args --hidden`. Nothing here touches AppKit, so it
//! works from any thread.

use std::path::{Path, PathBuf};

/// The LaunchAgent's label, and its file name in ~/Library/LaunchAgents.
const LOGIN_LABEL: &str = "com.fiotransfer.login";

/// ~/Library/LaunchAgents/com.fiotransfer.login.plist
fn plist_path() -> PathBuf {
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from("/"),
    };
    home.join("Library").join("LaunchAgents").join(format!("{}.plist", LOGIN_LABEL))
}

/// The `.app` containing the running binary.
pub fn app_bundle_path() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| os_message(&e))?;
    // The binary is reached through symlinks (a build dir, a launcher); the
    // launch agent has to name the bundle the user sees.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let s = exe.to_string_lossy().into_owned();
    match s.find(".app/") {
        Some(i) => Ok(s[..i + 4].to_string()),
        None => Err("not running from an app bundle".to_string()),
    }
}

/// Whether the plist on disk starts this copy of the app. A plist left by
/// another copy (an older download) doesn't count.
pub fn enabled() -> bool {
    let bytes = match std::fs::read(plist_path()) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let app = match app_bundle_path() {
        Ok(a) => a,
        Err(_) => return false,
    };
    String::from_utf8_lossy(&bytes).contains(&xml_escape(&app))
}

/// Writes or removes the launch agent. Off, a missing plist is not an error.
pub fn set(on: bool) -> Result<(), String> {
    let path = plist_path();
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(path_error("remove", &path, &e)),
        };
    }
    let app = app_bundle_path()?;
    let plist = plist_xml(&app);
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return Err(path_error("mkdir", dir, &e));
        }
    }
    std::fs::write(&path, plist.as_bytes()).map_err(|e| path_error("open", &path, &e))
}

/// Whether the app was started by the launch agent (`--hidden`).
pub fn started_hidden() -> bool {
    std::env::args_os().skip(1).any(|a| a == "--hidden")
}

fn plist_xml(app: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>{}</string>
	<key>ProgramArguments</key>
	<array>
		<string>/usr/bin/open</string>
		<string>-g</string>
		<string>{}</string>
		<string>--args</string>
		<string>--hidden</string>
	</array>
	<key>RunAtLoad</key><true/>
</dict>
</plist>
"#,
        LOGIN_LABEL,
        xml_escape(app)
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// An error text like the Go app's `err.Error()`: `op path: strerror`.
fn path_error(op: &str, path: &Path, e: &std::io::Error) -> String {
    format!("{} {}: {}", op, path.display(), os_message(e))
}

/// The strerror text for an OS error, which is what Go printed.
fn os_message(e: &std::io::Error) -> String {
    if let Some(code) = e.raw_os_error() {
        // The C library's text for the code, as Go printed it.
        let text = fiotransfer_sys::platform::strerror_text(code);
        if !text.is_empty() {
            return text;
        }
    }
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_the_go_plist() {
        let want = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>com.fiotransfer.login</string>
	<key>ProgramArguments</key>
	<array>
		<string>/usr/bin/open</string>
		<string>-g</string>
		<string>/Applications/FioTransfer.app</string>
		<string>--args</string>
		<string>--hidden</string>
	</array>
	<key>RunAtLoad</key><true/>
</dict>
</plist>
"#;
        assert_eq!(plist_xml("/Applications/FioTransfer.app"), want);
    }

    #[test]
    fn app_paths_are_escaped_in_the_plist() {
        let xml = plist_xml("/Users/a & b/Apps/FioTransfer.app");
        assert!(xml.contains("<string>/Users/a &amp; b/Apps/FioTransfer.app</string>"), "{xml}");
        assert!(!xml.contains("a & b"), "the raw path is not written");
        // No plist and no bundle in a test run: a plain false, not a panic.
        let _ = enabled();
    }

    #[test]
    fn os_message_is_strerror() {
        let e = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(os_message(&e), "No such file or directory");
    }
}
