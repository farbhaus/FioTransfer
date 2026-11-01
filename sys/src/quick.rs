//! The download panel.
//!
//! "Download from Link…" in the menu bar opens a small panel
//! (`mac/quick_darwin.m`, unchanged from the Go app) that takes a link and a
//! destination and starts a download without the main window. The panel calls
//! `goQuickInspect`, `goQuickDest` and `goQuickStart` back, so those entry
//! points are defined here and forward to the app through `set_quick_hooks`.
//!
//! Threading: every C entry point in `quick_darwin.m` hops to the main queue
//! itself, so these may be called from any thread; the `goQuick*` callbacks
//! arrive on the main thread.

use std::ffi::{c_char, c_int};
use std::sync::Mutex;

#[cfg(target_os = "macos")]
use std::ffi::CString;
#[cfg(target_os = "macos")]
use crate::strings::cstr;
use crate::strings::from_c_str;

#[cfg(target_os = "macos")]
extern "C" {
    fn fioQuickShow(link: *const c_char, dir: *const c_char);
    fn fioQuickInfo(seq: c_int, state: c_int, name: *const c_char, detail: *const c_char, warn: *const c_char);
    fn fioQuickStarted(err: *const c_char);
}

/// What the panel asks the app to do, with the panel's strings already read.
///
/// They take `&str` because the pointer work is done here: a hook must not
/// hold on to them past the call.
#[derive(Clone, Copy)]
pub struct QuickHooks {
    /// Preview a link for the panel; answer with `quick_info(seq, …)`.
    pub inspect: fn(i32, &str, &str),
    /// The folder chosen in the panel becomes the download folder.
    pub dest: fn(&str),
    /// Download the link into the folder.
    pub start: fn(&str, &str, &str, bool),
}

static HOOKS: Mutex<Option<QuickHooks>> = Mutex::new(None);

/// Hands the panel's actions to the app.
pub fn set_quick_hooks(hooks: QuickHooks) {
    *HOOKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(hooks);
}

fn hooks() -> Option<QuickHooks> {
    *HOOKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The panel's link needed a preview.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "C" fn goQuickInspect(seq: c_int, link: *const c_char, pass: *const c_char) {
    if let Some(h) = hooks() {
        (h.inspect)(seq, &from_c_str(link), &from_c_str(pass));
    }
}

/// The panel's folder button changed the download folder.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "C" fn goQuickDest(dir: *const c_char) {
    if let Some(h) = hooks() {
        (h.dest)(&from_c_str(dir));
    }
}

/// The panel's Download button was pressed.
#[no_mangle]
#[allow(non_snake_case)]
pub extern "C" fn goQuickStart(link: *const c_char, pass: *const c_char, dir: *const c_char, subfolder: c_int) {
    if let Some(h) = hooks() {
        (h.start)(&from_c_str(link), &from_c_str(pass), &from_c_str(dir), subfolder != 0);
    }
}

/// Opens the panel, or brings it forward with what it holds. A panel that was
/// closed starts over with `link` (may be empty) and `dir`.
#[cfg(target_os = "macos")]
pub fn quick_show(link: &str, dir: &str) {
    let (cl, cd): (CString, CString) = (cstr(link), cstr(dir));
    unsafe { fioQuickShow(cl.as_ptr(), cd.as_ptr()) };
}

/// Opens the panel; there is no panel outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn quick_show(_link: &str, _dir: &str) {}

/// Answers `inspect(seq)`: the link's name (or a message) and its detail, with
/// `warn` in the notice color.
#[cfg(target_os = "macos")]
pub fn quick_info(seq: i32, state: i32, name: &str, detail: &str, warn: &str) {
    let (cn, cd, cw): (CString, CString, CString) = (cstr(name), cstr(detail), cstr(warn));
    unsafe { fioQuickInfo(seq, state, cn.as_ptr(), cd.as_ptr(), cw.as_ptr()) };
}

/// Answers an inspect; there is no panel outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn quick_info(_seq: i32, _state: i32, _name: &str, _detail: &str, _warn: &str) {}

/// Closes the panel, or shows why the download didn't start.
#[cfg(target_os = "macos")]
pub fn quick_started(err: &str) {
    let c = cstr(err);
    unsafe { fioQuickStarted(c.as_ptr()) };
}

/// Closes the panel; there is no panel outside macOS.
#[cfg(not(target_os = "macos"))]
pub fn quick_started(_err: &str) {}
