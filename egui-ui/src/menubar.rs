//! The menu bar item and the "Download from Link…" panel (macOS).
//!
//! The AppKit side is `fiotransfer_sys`'s (`sys/mac/*.m`, carried over from the
//! Go app); this connects it to the app. Menu actions arrive on the main thread
//! and go to the shell as [`Update::Menu`], with a repaint, so they are handled
//! while the window is hidden too. The panel calls the app itself on the
//! runtime, as `quick.go` did, and a loop on the runtime keeps the menu, the
//! Dock icon and the App Nap guard current, as `trayLoop` did. Outside macOS the
//! `fiotransfer_sys` calls do nothing.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use std::sync::mpsc::Sender;

use fiotransfer_core::app::{App, UpdateStatus};
use fiotransfer_core::xfer::{DownloadOptions, JobView, LinkInfo, Snapshot, Status};
use fiotransfer_sys::quick::{self, QuickHooks};
use fiotransfer_sys::tray;

use crate::model::{fmt_bytes, fmt_dur};
use crate::shell::Update;

/// The menu's tags (`tray_darwin.m`'s `fioAdd` and `fioAppMenuAdd` calls).
pub const OPEN: i32 = 1;
pub const LOGIN: i32 = 2;
pub const QUIT: i32 = 3;
/// The app was activated with no window on screen (the Dock, Finder).
pub const REOPEN: i32 = 4;
pub const UPDATE: i32 = 5;
pub const CHECK: i32 = 6;
pub const LINK: i32 = 7;

/// The panel's answer states (`qShowInfo` in `quick_darwin.m`).
const QUICK_OK: i32 = 1;
const QUICK_PASS: i32 = 2;
const QUICK_ERROR: i32 = 3;

/// At most this many transfers are rows in the menu; the rest are "N more".
const JOBS_MAX: usize = 5;

struct Bridge {
    app: Arc<App>,
    rt: tokio::runtime::Handle,
    tx: Mutex<Sender<Update>>,
    ctx: egui::Context,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

/// Creates the menu bar item and connects it and the panel to the app. Once.
pub fn install(app: Arc<App>, rt: tokio::runtime::Handle, tx: Sender<Update>, ctx: egui::Context) {
    let bridge = Bridge { app: app.clone(), rt: rt.clone(), tx: Mutex::new(tx), ctx };
    if BRIDGE.set(bridge).is_err() {
        return;
    }
    tray::set_tray_action(on_action);
    quick::set_quick_hooks(QuickHooks { inspect, dest, start });
    tray::tray_init();
    if cfg!(target_os = "macos") {
        rt.spawn(menu_loop(app));
    }
}

/// Opens the panel with `link` (may be empty) and `dir`, or brings it forward.
pub fn show_panel(link: &str, dir: &str) {
    quick::quick_show(link, dir);
}

fn on_action(tag: i32) {
    if let Some(b) = BRIDGE.get() {
        let _ = b.tx.lock().unwrap_or_else(|e| e.into_inner()).send(Update::Menu(tag));
        b.ctx.request_repaint();
    }
}

fn inspect(seq: i32, link: &str, pass: &str) {
    let Some(b) = BRIDGE.get() else { return };
    let (app, link, pass) = (b.app.clone(), link.to_string(), pass.to_string());
    b.rt.spawn(async move {
        let (state, name, detail, warn) = panel_answer(&app.inspect_link(&link, &pass).await);
        quick::quick_info(seq, state, &name, &detail, &warn);
    });
}

/// The folder chosen in the panel becomes the download folder, as choosing one
/// in the window does.
fn dest(dir: &str) {
    let Some(b) = BRIDGE.get() else { return };
    let mut s = b.app.settings();
    s.download_dir = dir.to_string();
    if b.app.save_settings(s).is_ok() {
        b.ctx.request_repaint();
    }
}

fn start(link: &str, pass: &str, dir: &str, subfolder: bool) {
    let Some(b) = BRIDGE.get() else { return };
    let _rt = b.rt.enter();
    let o = DownloadOptions {
        link: link.to_string(),
        passphrase: pass.to_string(),
        dest_dir: dir.to_string(),
        subfolder,
    };
    let err = b.app.start_download(o).err().unwrap_or_default();
    quick::quick_started(&err);
    b.ctx.request_repaint();
}

/// What the panel shows for a link: state, name (or the message), file count
/// and size, and how many files can't be downloaded.
fn panel_answer(li: &LinkInfo) -> (i32, String, String, String) {
    if li.needs_passphrase {
        return (QUICK_PASS, li.error.clone(), String::new(), String::new());
    }
    if !li.error.is_empty() {
        return (QUICK_ERROR, li.error.clone(), String::new(), String::new());
    }
    let name = if li.name.is_empty() { "Shared files".to_string() } else { li.name.clone() };
    let files = if li.files == 1 { "file" } else { "files" };
    let warn = if li.unavailable > 0 { format!("{} unavailable", li.unavailable) } else { String::new() };
    (QUICK_OK, name, format!("{} {} · {}", li.files, files, fmt_bytes(li.total_bytes)), warn)
}

async fn menu_loop(app: Arc<App>) {
    let mut t = tokio::time::interval(Duration::from_secs(1));
    let mut shown = String::new(); // the transfers in the menu
    let mut login = false;
    for n in 0u64.. {
        t.tick().await;
        if n % 30 == 0 {
            // The login item can also be removed in System Settings.
            login = fiotransfer_core::login::enabled();
        }
        tray::sync_dock();
        let (up, down, active) = app.mgr().map(|m| m.activity()).unwrap_or((0.0, 0.0, 0));
        let (status, title) = status_line(up, down, active);
        tray::tray_set(&status, &title, login, &update_title(&app.update_status()));
        let jobs = if active > 0 { menu_jobs(&app.snapshot()) } else { r#"{"jobs":[],"more":0}"#.to_string() };
        if jobs != shown {
            tray::tray_jobs(&jobs);
            shown = jobs;
        }
        tray::set_active(active > 0);
    }
}

/// The menu's status line and the text next to the icon.
fn status_line(up: f64, down: f64, active: usize) -> (String, String) {
    if active == 0 {
        return ("Idle".to_string(), String::new());
    }
    let mut parts = Vec::new();
    if up > 0.0 {
        parts.push(format!("↑ {}", speed(up)));
    }
    if down > 0.0 {
        parts.push(format!("↓ {}", speed(down)));
    }
    let title = parts.join("  ");
    let mut status = format!("{} active", active);
    if !title.is_empty() {
        status = format!("{} · {}", status, title);
    }
    (status, title)
}

/// "Update to X…" while an update is available, else "" (Check for Updates…).
fn update_title(u: &UpdateStatus) -> String {
    if u.available && !u.latest.is_empty() {
        format!("Update to {}…", u.latest.trim_start_matches('v'))
    } else {
        String::new()
    }
}

fn speed(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{:.1} GB/s", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.0} MB/s", bps / 1e6)
    } else {
        format!("{:.0} KB/s", bps / 1e3)
    }
}

/// Running transfers for the menu, those moving data first, as the JSON
/// `fioTrayJobs` reads: `{"jobs": […], "more": n}`.
fn menu_jobs(s: &Snapshot) -> String {
    let rank = |st: Status| match st {
        Status::Running => Some(0),
        Status::Resolving => Some(1),
        Status::Queued => Some(2),
        _ => None,
    };
    let mut active: Vec<&JobView> = s.jobs.iter().filter(|j| rank(j.status).is_some()).collect();
    active.sort_by_key(|j| rank(j.status));
    let jobs: Vec<serde_json::Value> = active.iter().take(JOBS_MAX).map(|j| menu_row(j)).collect();
    serde_json::json!({ "jobs": jobs, "more": active.len().saturating_sub(JOBS_MAX) }).to_string()
}

/// One transfer, worded like the window's list.
fn menu_row(j: &JobView) -> serde_json::Value {
    let up = j.kind == "upload";
    let name = match (j.title.is_empty(), up) {
        (false, _) => j.title.clone(),
        (true, true) => "Upload".to_string(),
        (true, false) => "Download".to_string(),
    };
    let (mut progress, mut pct) = (-1.0, String::new());
    let detail = if j.status == Status::Queued {
        "Queued".to_string()
    } else if j.status == Status::Resolving || j.total <= 0 {
        if j.phase.is_empty() { "Preparing".to_string() } else { j.phase.clone() }
    } else {
        progress = (j.done as f64 / j.total as f64).min(1.0);
        pct = format!("{}%", (progress * 100.0) as i64);
        if up && j.done >= j.total {
            "Verifying".to_string() // Frame.io is assembling the parts
        } else {
            let mut parts = vec![format!("{} of {}", fmt_bytes(j.done), fmt_bytes(j.total))];
            if j.speed > 0.0 {
                parts.push(speed(j.speed));
            }
            if j.eta > 0.0 {
                parts.push(format!("{} left", fmt_dur(j.eta)));
            }
            parts.join(" · ")
        }
    };
    serde_json::json!({ "up": up, "name": name, "progress": progress, "pct": pct, "detail": detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: &str, status: Status, done: i64, total: i64) -> JobView {
        JobView { kind: kind.into(), title: "clip.mov".into(), status, done, total, ..Default::default() }
    }

    #[test]
    fn the_status_line_reads_like_the_go_apps() {
        assert_eq!(status_line(0.0, 0.0, 0), ("Idle".to_string(), String::new()));
        assert_eq!(
            status_line(116e6, 2.5e9, 2),
            ("2 active · ↑ 116 MB/s  ↓ 2.5 GB/s".to_string(), "↑ 116 MB/s  ↓ 2.5 GB/s".to_string())
        );
        assert_eq!(status_line(0.0, 0.0, 1).0, "1 active");
    }

    #[test]
    fn rows_put_moving_transfers_first_and_count_the_rest() {
        let mut s = Snapshot::default();
        s.jobs.push(job("download", Status::Queued, 0, 0));
        for _ in 0..5 {
            s.jobs.push(job("upload", Status::Running, 2_500_000, 5_000_000));
        }
        s.jobs.push(job("upload", Status::Done, 100, 100));
        let v: serde_json::Value = serde_json::from_str(&menu_jobs(&s)).unwrap();
        assert_eq!(v["jobs"].as_array().unwrap().len(), 5);
        assert_eq!(v["more"], 1, "the queued one doesn't fit");
        assert_eq!(v["jobs"][0]["pct"], "50%");
        assert_eq!(v["jobs"][0]["detail"], "2.50 MB of 5.00 MB");
    }

    #[test]
    fn a_row_says_queued_preparing_or_verifying() {
        assert_eq!(menu_row(&job("download", Status::Queued, 0, 0))["detail"], "Queued");
        assert_eq!(menu_row(&job("download", Status::Resolving, 0, 0))["detail"], "Preparing");
        let v = menu_row(&job("upload", Status::Running, 100, 100));
        assert_eq!((v["detail"].as_str(), v["pct"].as_str()), (Some("Verifying"), Some("100%")));
    }

    #[test]
    fn the_panel_words_a_link_like_quick_go() {
        let li = LinkInfo { name: String::new(), files: 1, total_bytes: 2_000_000, unavailable: 1, ..Default::default() };
        assert_eq!(
            panel_answer(&li),
            (QUICK_OK, "Shared files".into(), "1 file · 2.00 MB".into(), "1 unavailable".into())
        );
        let li = LinkInfo { needs_passphrase: true, error: "passphrase required".into(), ..Default::default() };
        assert_eq!(panel_answer(&li).0, QUICK_PASS);
        let li = LinkInfo { error: "link expired".into(), ..Default::default() };
        assert_eq!(panel_answer(&li), (QUICK_ERROR, "link expired".into(), String::new(), String::new()));
    }

    #[test]
    fn the_update_item_names_the_version() {
        let u = UpdateStatus { available: true, latest: "v1.7.0".into(), ..Default::default() };
        assert_eq!(update_title(&u), "Update to 1.7.0…");
        assert_eq!(update_title(&UpdateStatus::default()), "");
    }

    #[test]
    fn tags_match_tray_darwin_m() {
        assert_eq!((OPEN, LOGIN, QUIT, REOPEN, UPDATE, CHECK, LINK), (1, 2, 3, 4, 5, 6, 7));
    }
}
