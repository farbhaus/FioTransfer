//! FioTransfer's C boundary.
//!
//! Everything the app cannot write without `unsafe` lives here, and only here:
//! the `extern "C"` declarations for the AppKit shims in `mac/` (the menu bar
//! item, the download panel and Finder's services, compiled by `build.rs` on
//! macOS) and the handful of `libc` calls the engine needs — the hostname, the
//! machine's time zone, a mount's flags, a write permission, a `setsid()` that
//! outlives the app, `strerror`, `isatty` and the process' SIGINT disposition.
//! The app crate and `fiotransfer-core` are `#![forbid(unsafe_code)]`, so this is
//! the one crate in the workspace allowed to write it.
//!
//! Everything public in this crate is a safe function: `&str` and `String` go in
//! and out, and the `CString`/`CStr` work stays inside. The macOS-only pieces are
//! gated on the target; the same functions exist elsewhere and answer "no", so
//! the crate still type-checks on a platform with no `libc` to call.

#![allow(unsafe_code)] // the workspace's C boundary: the shim declarations, the pointer reads they need, and the libc calls

pub mod fs;
pub mod platform;
pub mod quick;
pub mod services;
pub mod strings;
pub mod tray;

pub use strings::{cstr, from_c_str};
