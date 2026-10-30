//! FioTransfer — the CLI and bench host, over `fiotransfer-core`.
//!
//! The app is `fiotransfer-egui`: one binary, the real engine, no webview. This
//! package keeps what a terminal needs — `fio` (upload, download, watch, history,
//! export) and `fio-bench` (the transfer benchmarks) — plus the JSON bridge the
//! webview front end used to call, which is now only a contract test
//! ([`app::BridgeHost`], `tests/prop_bridge.rs`).
//!
//! Everything else is re-exported from the core, so `fiotransfer::xfer::…` and
//! `fiotransfer::history::…` still name the same modules they always did.

#![forbid(unsafe_code)]
// Every `unsafe` in this workspace lives in the `fiotransfer-sys` crate: the
// `libc` calls the platform needs. This crate can't write `unsafe` at all —
// `forbid`, not `deny`, so not even an `#[allow(unsafe_code)]` deeper down is
// possible.

pub mod app;

pub use fiotransfer_core::{
    api, auth, config, history, host, limiter, localcert, login, netx, notify, portal, secret,
    services, share, update, xfer, App, Host, VERSION,
};
