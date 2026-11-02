//! The macOS Keychain, through `/usr/bin/security`, as the Go app's go-keyring
//! did.
//!
//! An item `security` writes lists `security` as the app that may read it. So
//! this app reads and writes its items without a "wants to use your
//! confidential information" dialog, whatever its own signature is: the Go
//! app's session carries over on the update, and an ad-hoc signed update never
//! asks for the login password again. (Reading through the Security framework
//! made every new build ask.)
//!
//! Values are stored the way go-keyring stored them, `go-keyring-base64:` +
//! base64, so `security` prints them back unchanged.
//!
//! Only macOS has this keychain. Elsewhere every call answers "no keychain", and
//! `auth` falls back to the 0600 file under `HOME` — the same fallback the app
//! already took when the keychain was unavailable, so the Linux and Windows
//! builds of the app keep their sessions in that file.

pub const SERVICE: &str = "FioTransfer";

#[cfg(target_os = "macos")]
const TOOL: &str = "/usr/bin/security";

/// go-keyring's wrapping of a stored value.
#[cfg(target_os = "macos")]
const PREFIX: &str = "go-keyring-base64:";

/// `security -i` reads lines of at most this many bytes (go-keyring's
/// `ErrSetDataTooBig`).
#[cfg(target_os = "macos")]
const MAX_LINE: usize = 4096;

/// Tests and headless runs set `FIO_NO_KEYCHAIN=1`: the keychain is then neither
/// read (which can raise a "wants access" dialog and would sign a test in with the
/// real account) nor written (so `auth` falls back to the 0600 file, which lives
/// under `HOME` and is therefore redirectable to a temporary directory).
///
/// Only macOS reads it: the other platforms answer "no keychain" without asking.
#[cfg(target_os = "macos")]
fn disabled() -> bool {
    std::env::var_os("FIO_NO_KEYCHAIN").is_some()
}

#[cfg(target_os = "macos")]
pub fn get(key: &str) -> Option<String> {
    if disabled() {
        return None;
    }
    get_with(TOOL, key)
}

/// There is no keychain here: the caller falls back to its file.
#[cfg(not(target_os = "macos"))]
pub fn get(_key: &str) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
pub fn set(key: &str, value: &str) -> Result<(), String> {
    if disabled() {
        return Err("keychain disabled".to_string());
    }
    set_with(TOOL, key, value)
}

/// There is no keychain here: the caller falls back to its file.
#[cfg(not(target_os = "macos"))]
pub fn set(_key: &str, _value: &str) -> Result<(), String> {
    Err("no keychain on this platform".to_string())
}

#[cfg(target_os = "macos")]
pub fn delete(key: &str) {
    if disabled() {
        return;
    }
    delete_with(TOOL, key);
}

#[cfg(not(target_os = "macos"))]
pub fn delete(_key: &str) {}

/// `security find-generic-password -w`: the value, or `None` when there is no
/// such item or it can't be read.
#[cfg(target_os = "macos")]
fn get_with(tool: &str, key: &str) -> Option<String> {
    let out = std::process::Command::new(tool)
        .args(["find-generic-password", "-s", SERVICE, "-a", key, "-w"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(decode(text.strip_suffix('\n').unwrap_or(&text)))
}

/// `security -i` with the command on stdin, so the secret never shows in the
/// process list. Read back afterwards: a write that didn't land is an error, so
/// the caller keeps its file instead of losing the value.
#[cfg(target_os = "macos")]
fn set_with(tool: &str, key: &str, value: &str) -> Result<(), String> {
    use std::io::Write;
    let line = format!(
        "add-generic-password -U -s {} -a {} -w {}\n",
        quote(SERVICE),
        quote(key),
        quote(&encode(value))
    );
    if line.len() > MAX_LINE {
        return Err("the value is too long for the keychain".to_string());
    }
    let mut child = std::process::Command::new(tool)
        .arg("-i")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    if get_with(tool, key).as_deref() != Some(value) {
        return Err("the keychain didn't keep the value".to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn delete_with(tool: &str, key: &str) {
    let _ = std::process::Command::new(tool)
        .args(["delete-generic-password", "-s", SERVICE, "-a", key])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(target_os = "macos")]
fn encode(value: &str) -> String {
    use base64::Engine;
    format!("{PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(value))
}

/// The value as stored: go-keyring's wrapping undone, or the text as it is (an
/// item an earlier build of this app wrote through the Security framework).
#[cfg(target_os = "macos")]
fn decode(stored: &str) -> String {
    use base64::Engine;
    stored
        .strip_prefix(PREFIX)
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b.trim()).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| stored.to_string())
}

/// One argument for `security -i`, which splits its lines the way a shell does.
#[cfg(target_os = "macos")]
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for `/usr/bin/security` that keeps items as files next to
    /// itself, taking the same arguments (and `-i` lines). The tests never
    /// touch a real keychain.
    fn fake_tool(dir: &std::path::Path, script_tail: &str) -> String {
        let path = dir.join("security");
        let script = format!(
            r#"#!/bin/sh
store="$(dirname "$0")/store"; mkdir -p "$store"
run() {{
  cmd="$1"; shift; svc=""; acct=""; pw=""
  while [ $# -gt 0 ]; do
    case "$1" in
      -s) svc="$2"; shift 2 ;;
      -a) acct="$2"; shift 2 ;;
      -w) if [ "$cmd" = find-generic-password ]; then shift; else pw="$2"; shift 2; fi ;;
      *) shift ;;
    esac
  done
  f="$store/$svc.$acct"
  case "$cmd" in
    find-generic-password) [ -f "$f" ] || exit 44; cat "$f"; echo ;;
    add-generic-password) {script_tail} ;;
    delete-generic-password) rm -f "$f" ;;
  esac
}}
if [ "$1" = "-i" ]; then
  while IFS= read -r line; do eval "run $line"; done
else
  run "$@"
fi
"#
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn keeping_tool(dir: &std::path::Path) -> String {
        fake_tool(dir, r#"printf '%s' "$pw" > "$f""#)
    }

    fn stored(dir: &std::path::Path, key: &str) -> Option<String> {
        std::fs::read_to_string(dir.join("store").join(format!("{SERVICE}.{key}"))).ok()
    }

    #[test]
    fn a_value_goes_in_as_go_keyring_wrote_it_and_comes_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let tool = keeping_tool(dir.path());
        let session = r#"{"access_token":"a.b.c","refresh_token":"r'1\"","expiry":1793570607}"#;
        set_with(&tool, "adobe-ims", session).expect("written");
        let raw = stored(dir.path(), "adobe-ims").expect("an item");
        assert!(raw.starts_with(PREFIX), "{raw}");
        assert!(!raw.contains("refresh_token"), "stored wrapped, as go-keyring did");
        assert_eq!(get_with(&tool, "adobe-ims").as_deref(), Some(session));
        // Other names, other items.
        set_with(&tool, "portal_token", "tok-Ünïcode").unwrap();
        assert_eq!(get_with(&tool, "portal_token").as_deref(), Some("tok-Ünïcode"));
        assert_eq!(get_with(&tool, "adobe-ims").as_deref(), Some(session));
    }

    /// The Go app's item, exactly as go-keyring left it, reads as its JSON; an
    /// item an earlier build of this app wrote unwrapped reads as it is.
    #[test]
    fn the_go_apps_item_and_an_unwrapped_one_both_read() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let tool = keeping_tool(dir.path());
        std::fs::create_dir_all(dir.path().join("store")).unwrap();
        let go = r#"{"access_token":"x","refresh_token":"y","expiry":"2026-10-31T18:20:00+01:00"}"#;
        let wrapped = format!("go-keyring-base64:{}", base64::engine::general_purpose::STANDARD.encode(go));
        std::fs::write(dir.path().join("store").join(format!("{SERVICE}.adobe-ims")), wrapped).unwrap();
        assert_eq!(get_with(&tool, "adobe-ims").as_deref(), Some(go));
        std::fs::write(dir.path().join("store").join(format!("{SERVICE}.portal_token")), "plain").unwrap();
        assert_eq!(get_with(&tool, "portal_token").as_deref(), Some("plain"));
    }

    #[test]
    fn a_missing_item_is_none_and_delete_removes_one() {
        let dir = tempfile::tempdir().unwrap();
        let tool = keeping_tool(dir.path());
        assert_eq!(get_with(&tool, "adobe-ims"), None);
        set_with(&tool, "adobe-ims", "v").unwrap();
        delete_with(&tool, "adobe-ims");
        assert_eq!(get_with(&tool, "adobe-ims"), None);
        delete_with(&tool, "adobe-ims"); // already gone: nothing to do
    }

    /// A write that didn't land must not look like one: the caller would drop
    /// its file and lose the session.
    #[test]
    fn a_write_that_did_not_land_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let tool = fake_tool(dir.path(), ":");
        assert!(set_with(&tool, "adobe-ims", "v").is_err());
        let other = tempfile::tempdir().unwrap();
        let failing = fake_tool(other.path(), "exit 1");
        assert!(set_with(&failing, "adobe-ims", "v").is_err());
    }

    #[test]
    fn a_value_too_long_for_security_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let tool = keeping_tool(dir.path());
        assert!(set_with(&tool, "adobe-ims", &"x".repeat(4000)).is_err());
        assert_eq!(stored(dir.path(), "adobe-ims"), None);
        // A session the size Frame.io hands out fits.
        assert!(set_with(&tool, "adobe-ims", &"x".repeat(2500)).is_ok());
    }

    #[test]
    fn quoting_survives_a_shell_split() {
        assert_eq!(quote("FioTransfer"), "'FioTransfer'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
