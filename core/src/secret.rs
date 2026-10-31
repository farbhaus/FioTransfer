//! The macOS Keychain (the Go app uses github.com/zalando/go-keyring).
//!
//! Only macOS has this keychain. Elsewhere every call answers "no keychain", and
//! `auth` falls back to the 0600 file under `HOME` — the same fallback the app
//! already took when the keychain was unavailable, so the Linux and Windows
//! builds of the app keep their sessions in that file.

pub const SERVICE: &str = "FioTransfer";

#[cfg(target_os = "macos")]
use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

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
    match get_generic_password(SERVICE, key) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(_) => None,
    }
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
    set_generic_password(SERVICE, key, value.as_bytes()).map_err(|e| e.to_string())
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
    let _ = delete_generic_password(SERVICE, key);
}

#[cfg(not(target_os = "macos"))]
pub fn delete(_key: &str) {}
