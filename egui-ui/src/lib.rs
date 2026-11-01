//! FioTransfer's front end, rebuilt with egui.
//!
//! This is the app: the five screens and the shell around them, over
//! `fiotransfer-core`'s `App`. `model.rs` holds what a screen needs to draw and
//! what a screen asks for; a view only draws and pushes
//! [`model::Request`]s, and [`shell::Shell`] (native only) answers them by calling
//! the engine, so no screen ever talks to Frame.io, a panel or a file itself.

#![forbid(unsafe_code)]

pub mod app;
pub mod fixture;
pub mod icons;
pub mod model;
pub mod theme;
pub mod views;

/// The two halves of the native app that the screens never see: the shell that
/// drives `fiotransfer_core::App` ([`shell`]) and the desktop it asks for its
/// panels and its clipboard ([`platform`]). Native only — the browser capture has
/// no app behind it, which is also why `fiotransfer-core` is a native-only
/// dependency in `Cargo.toml`.
#[cfg(not(target_arch = "wasm32"))]
pub mod menubar;
#[cfg(not(target_arch = "wasm32"))]
pub mod platform;
#[cfg(not(target_arch = "wasm32"))]
pub mod shell;

/// The five data shapes the browser capture draws instead of the core's own
/// (tokio, rustls and rusqlite do not build for `wasm32`). `model` re-exports
/// them, so every screen is one file for both targets.
#[cfg(target_arch = "wasm32")]
pub mod model_browser;

/// The browser entry point. Wasm only: the native build has `main.rs`, and this
/// module is the only thing here that knows a `<canvas>` exists.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use shell::Shell;
pub use app::FioApp;

/// The window the webview front end had, so the screens still open at the same
/// size and `tests/snapshots.rs` renders what the pixel comparison measured.
pub const WINDOW_WIDTH: f32 = 1180.0;
pub const WINDOW_HEIGHT: f32 = 780.0;
pub const MIN_WIDTH: f32 = 860.0;
pub const MIN_HEIGHT: f32 = 560.0;
