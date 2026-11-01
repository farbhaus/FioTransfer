//! FioTransfer's logic: the Frame.io V4 API, the transfer engine, watch
//! folders, history, updates, and the `App` every front end drives.
//!
//! A Rust port of the Go/Wails app of the same name: the same API, the same
//! engine behaviour, the same strings and the same thresholds.
//!
//! Nothing here draws anything and nothing here needs a window. Two things
//! drive this crate today: `egui-ui` (the app) and the root package's CLI and
//! bench (`fio`, `fio-bench`). A host supplies the platform pieces — panels, the
//! clipboard, notification banners, showing the window, quitting — through
//! [`host::Host`]; a host that implements nothing still runs every code path the
//! bridge tests exercise.

#![forbid(unsafe_code)]
// Every `unsafe` in this workspace lives in the `fiotransfer-sys` crate: the
// AppKit shims in `mac/` and the `libc` calls are declared and used there,
// behind safe functions. This crate can't write `unsafe` at all — `forbid`,
// not `deny`, so not even an `#[allow(unsafe_code)]` deeper down is possible.

pub mod api;
pub mod auth;
pub mod config;
pub mod fileio;
pub mod host;
pub mod limiter;
pub mod localcert;
pub mod login;
pub mod netx;
pub mod osopen;
pub mod notify;
pub mod portal;
pub mod secret;

pub mod history;
pub mod share;
pub mod update;
pub mod xfer;

pub mod app;
pub mod services;

pub use app::App;
pub use host::Host;

/// The version this build reports: `Cargo.toml`'s, unless a build overrides it
/// (`FIO_VERSION=0.0.0-abc1234` for a development build off a commit). One number,
/// in one place, and the release workflow reads the same one.
pub const VERSION: &str = match option_env!("FIO_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
