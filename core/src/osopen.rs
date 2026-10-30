//! Handing a file, a folder or a URL to the desktop.
//!
//! The macOS app ran `/usr/bin/open`; the same two things exist on the other
//! platforms under other names, so they are named once here. The strings the app
//! shows do not change: a failure is still the spawn error's text.

use std::process::Command;

/// Opens a URL, a file or a folder with whatever the desktop associates with it.
#[cfg(target_os = "macos")]
pub fn open(target: &str) -> std::io::Result<()> {
    Command::new("/usr/bin/open").arg(target).spawn().map(|_| ())
}

/// Opens a URL, a file or a folder with whatever the desktop associates with it.
#[cfg(target_os = "windows")]
pub fn open(target: &str) -> std::io::Result<()> {
    // `start` is a cmd built-in; the empty string is the window title, so a
    // quoted URL is not taken for one.
    Command::new("cmd").args(["/C", "start", ""]).arg(target).spawn().map(|_| ())
}

/// Opens a URL, a file or a folder with whatever the desktop associates with it.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn open(target: &str) -> std::io::Result<()> {
    Command::new("xdg-open").arg(target).spawn().map(|_| ())
}

/// Shows a file or folder in the desktop's file manager, selected if it can.
#[cfg(target_os = "macos")]
pub fn reveal(path: &str) -> std::io::Result<()> {
    Command::new("/usr/bin/open").arg("-R").arg(path).spawn().map(|_| ())
}

/// Shows a file or folder in the desktop's file manager, selected if it can.
#[cfg(target_os = "windows")]
pub fn reveal(path: &str) -> std::io::Result<()> {
    Command::new("explorer").arg(format!("/select,{}", path)).spawn().map(|_| ())
}

/// Shows a file or folder in the desktop's file manager. No file manager selects
/// from the command line here, so the folder the file is in is opened.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn reveal(path: &str) -> std::io::Result<()> {
    let dir = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    Command::new("xdg-open").arg(dir).spawn().map(|_| ())
}
