//! The menu bar item.
//!
//! `mac/tray_darwin.m` is the AppKit side, unchanged from the Go app. It calls
//! `goTrayAction` back when a menu item is chosen, so the entry point C links
//! against is defined here and forwards to the handler the app installs
//! (`set_tray_action`).
//!
//! Threading: every C entry point in `tray_darwin.m` hops to the main queue
//! itself, so these may be called from any thread; `goTrayAction` arrives on
//! the main thread.

use std::sync::Mutex;

#[cfg(target_os = "macos")]
use std::ffi::{c_int, CString};
#[cfg(target_os = "macos")]
use crate::strings::cstr;

#[cfg(target_os = "macos")]
extern "C" {
    fn fioTrayInit();
    fn fioTraySet(status: *const std::ffi::c_char, title: *const std::ffi::c_char, login: c_int, update: *const std::ffi::c_char);
    fn fioTrayJobs(json: *const std::ffi::c_char);
    fn fioSyncDock();
    fn fioActivate();
    fn fioHide();
    fn fioSetActive(on: c_int);
}

/// The app's menu handler: a tag in, nothing out.
type Action = fn(i32);

static ACTION: Mutex<Option<Action>> = Mutex::new(None);

/// Hands the menu's action handler over: `goTrayAction` calls it.
pub fn set_tray_action(action: Action) {
    *ACTION.lock().unwrap_or_else(|e| e.into_inner()) = Some(action);
}

/// Called from tray_darwin.m when a menu item is chosen.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "C" fn goTrayAction(tag: i32) {
    let action = *ACTION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = action {
        f(tag);
    }
}

/// Creates the status item and its menu.
#[cfg(target_os = "macos")]
pub fn tray_init() {
    unsafe { fioTrayInit() };
}

/// Creates the status item and its menu; nothing to do outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn tray_init() {}

/// The menu's status line, the text next to the icon, the "Open at Login"
/// state and the update item (an empty `update` shows "Check for Updates…"
/// instead).
#[cfg(target_os = "macos")]
pub fn tray_set(status: &str, title: &str, login: bool, update: &str) {
    let (cs, ct, cu): (CString, CString, CString) = (cstr(status), cstr(title), cstr(update));
    unsafe { fioTraySet(cs.as_ptr(), ct.as_ptr(), i32::from(login), cu.as_ptr()) };
}

/// The menu's status line and update item; nothing to do outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn tray_set(_status: &str, _title: &str, _login: bool, _update: &str) {}

/// The running transfers, as the JSON the menu draws.
#[cfg(target_os = "macos")]
pub fn tray_jobs(json: &str) {
    let c = cstr(json);
    unsafe { fioTrayJobs(c.as_ptr()) };
}

/// The running transfers; nothing to draw outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn tray_jobs(_json: &str) {}

/// Shows the Dock icon only while a window is on screen.
#[cfg(target_os = "macos")]
pub fn sync_dock() {
    unsafe { fioSyncDock() };
}

/// Shows the Dock icon only while a window is on screen.
#[cfg(not(target_os = "macos"))]
pub fn sync_dock() {}

/// Hides the app (its window), leaving the menu bar item.
#[cfg(target_os = "macos")]
pub fn hide() {
    unsafe { fioHide() };
}

/// Hides the app; there is no menu bar item to leave outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn hide() {}

/// Brings the app forward.
#[cfg(target_os = "macos")]
pub fn activate() {
    unsafe { fioActivate() };
}

/// Brings the app forward.
#[cfg(not(target_os = "macos"))]
pub fn activate() {}

/// Tells macOS that transfers are user-requested work (no App Nap, no idle
/// sleep) while they run.
#[cfg(target_os = "macos")]
pub fn set_active(on: bool) {
    unsafe { fioSetActive(i32::from(on)) };
}

/// Tells macOS about running transfers; nothing to tell outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn set_active(_on: bool) {}
