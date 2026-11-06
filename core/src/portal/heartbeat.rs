//! Telling Portal this machine is alive, and what it is keeping in step.
//!
//! `POST /api/live-work/heartbeat` is the one thing FioTransfer *writes* to
//! Portal. It carries no files and changes nothing here or there: it is the
//! "connected clients" row on Portal's Live Work page — this machine, its
//! version and platform, and one lane per project folder the reconciler has
//! actually put in place.
//!
//! Three rules decide everything in this module:
//!
//! 1. **The lanes are what is on disk, not what Portal asked for.** A lane is a
//!    watch row whose `origin` is `portal:…` — the reconciler's own records — so
//!    a link Portal asked for and we could not make (a marker that is not a
//!    folder name, a folder somebody else already watches) has no lane, and a
//!    lane never describes a folder that does not exist. A watch the user made
//!    has no `portal:` origin and is never reported: it is none of Portal's
//!    business what else this Mac is syncing.
//! 2. **It is telemetry, so it cannot break anything.** Every failure — no
//!    address, no credential, Portal unreachable, a 500 — is a debug line and
//!    nothing else. It never becomes part of a [`crate::portal::agent::Report`],
//!    never a transfer-log line, never an event for the front end, and never a
//!    reason for a reconcile to fail. `App::portal_reconcile` drops the answer
//!    on the floor.
//! 3. **It is cheap.** The same [`PortalClient`] and the same backoff as the
//!    links read, one POST of a few hundred bytes, no new dependency (the uuid
//!    is sixteen random bytes from `rand`, which is already here).
//!
//! When it goes: after every reconcile that read Portal, which is the poller's
//! every tick whether or not the watch list changed — so "last seen" stays
//! honest on a quiet machine — and once [`STARTUP_DELAY`] after startup, so a
//! freshly launched app shows up before the first interval is up. Never when
//! Portal is switched off, has no address, or has no credential.
//!
//! The payload, which is the contract Portal's side is built against:
//!
//! ```json
//! { "client_id": "0f0c1c1e-…-…", "name": "studio-mac", "app_version": "<the crate version>",
//!   "platform": "macos-aarch64",
//!   "lanes": [ { "link_id": "L1", "project": "20261028_YML-XMAS-Luc",
//!                "local": "1_DRIVE", "direction": "download",
//!                "folder_id": "F…",
//!                "path": "/Users/…/Portal/20261028_YML-XMAS-Luc/1_DRIVE" } ] }
//! ```
//!
//! Portal answers `204` and says nothing else. Anything else is a failure, and
//! a failure is silent.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::agent::{link_of, EXPORTS_SUFFIX};
use super::PortalClient;
use crate::app::App;
use crate::history::watch::Watch;

/// The endpoint this module posts to. Portal's own route, documented here and
/// nowhere invented.
pub const PATH: &str = "/api/live-work/heartbeat";

/// How long after startup the first heartbeat goes. Short enough that a Portal
/// page opened just after launching shows this machine, long enough that the
/// history store and the watch list are open and the reconciler has had its
/// chance to make the folders the payload names.
pub const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// One folder this machine is keeping in step, as Portal sees it.
///
/// One lane per watch Portal owns: a `sync` link has two (its drive folder and
/// its exports folder), a plain link has one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    /// Portal's link id. Both of a `sync` link's lanes carry the same id — the
    /// `#exports` that tells the two watches apart is ours, not Portal's — and
    /// `direction` and `local` are what separates them.
    pub link_id: String,
    /// The project folder: the link's `.turbosort` marker, which is the folder
    /// name under `Portal`. Read back from where the watch actually is, so a
    /// renamed root cannot make it disagree with the disk.
    pub project: String,
    /// The lane's own folder name inside the project: `1_DRIVE`, `0_EXPORTS`,
    /// or the project folder itself for a plain mirrored link.
    pub local: String,
    /// `download`, `upload` or `mirror` — the watch's own direction, unchanged.
    pub direction: String,
    /// The Frame.io folder the lane moves files to or from.
    pub folder_id: String,
    /// The absolute local folder. This is the watch row's own `local_path`.
    pub path: String,
}

/// The whole body of one heartbeat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// This machine's uuid v4, from the settings: made once, kept across
    /// restarts, so Portal's list does not grow a row per launch.
    pub client_id: String,
    /// The machine's hostname.
    pub name: String,
    /// The app's own version, from the crate, so a release cannot disagree with
    /// what it sends.
    pub app_version: String,
    /// `os-arch`, the way the crate's build sees itself: `macos-aarch64`,
    /// `windows-x86_64`, `linux-x86_64`.
    pub platform: String,
    /// One per watch Portal owns. Empty is a normal answer: a machine with
    /// Portal on and nothing reconciled yet is still alive.
    #[serde(default)]
    pub lanes: Vec<Lane>,
}

/// What one attempt did, for the debug line and for the tests. Nothing here
/// reaches the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attempt {
    /// Portal answered the POST with a success.
    pub sent: bool,
    /// How many lanes the payload carried (0 when none was built).
    pub lanes: usize,
    /// Why nothing was sent or why it failed, and nothing else. Never shown,
    /// never logged where a person would read it.
    pub note: String,
}

/// The platform string: `std`'s own names, joined. `macos-aarch64` on an Apple
/// silicon Mac.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The lanes, from the watch rows the reconciler owns.
///
/// `root` is where the project folders live (`Settings::portal_root_path`): the
/// first segment of a watch's path below it is the project, which is exactly the
/// marker the reconciler named the folder after. A watch outside the root — the
/// root setting was changed after the watch was made — falls back to its
/// containing folder's name, because a lane still has to name a project.
///
/// Sorted by path so one state always produces one payload, byte for byte.
pub fn lanes(watches: &[Watch], root: &Path) -> Vec<Lane> {
    let mut out: Vec<Lane> = watches
        .iter()
        .filter_map(|w| {
            let link = link_of(&w.origin)?;
            // `portal:L1#exports` is the exports lane of link `L1`: the suffix is
            // this app's, so it is not part of the id Portal is told.
            let link_id = link.strip_suffix(EXPORTS_SUFFIX).unwrap_or(link);
            if link_id.is_empty() {
                return None;
            }
            let path = Path::new(&w.local_path);
            Some(Lane {
                link_id: link_id.to_string(),
                project: project_of(path, root),
                local: path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                direction: w.direction.clone(),
                folder_id: w.folder_id.clone(),
                path: w.local_path.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The project a lane sits in: the first segment below `root`, else the folder
/// the lane is in.
fn project_of(path: &Path, root: &Path) -> String {
    if let Ok(rest) = path.strip_prefix(root) {
        if let Some(first) = rest.components().next() {
            let s = first.as_os_str().to_string_lossy().into_owned();
            if !s.is_empty() {
                return s;
            }
        }
    }
    path.parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A fresh uuid v4 in the canonical `8-4-4-4-12` shape.
///
/// Sixteen random bytes from `rand`, which this crate already depends on: a
/// uuid crate for one identifier would be a dependency carried by every build
/// for nothing. Version and variant bits are set as RFC 4122 says, so what
/// Portal's side reads is a version 4 uuid and not a random hex string.
pub fn new_client_id() -> String {
    use rand::RngExt;
    let mut b = [0u8; 16];
    rand::rng().fill(&mut b[..]);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    let hex: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// This machine's id: the stored one, or a new one written to the settings.
///
/// It is written with [`crate::config::update`] so it survives a restart, and
/// kept in memory even when that write fails — a read-only settings file must
/// not turn this machine into a new client on every send.
pub fn client_id(app: &App) -> String {
    let known = app.settings().portal_client_id.trim().to_string();
    if !known.is_empty() {
        return known;
    }
    let id = new_client_id();
    // This run's id first, so two sends in one session are one client even if
    // the file below cannot be written …
    app.settings.lock().unwrap().portal_client_id = id.clone();
    // … and then the file, so the next launch is the same client. Only this one
    // field is taken from the write: the settings in memory are the app's, and
    // replacing them with the file's copy would be a second, silent reload.
    if let Err(e) = crate::config::update(|s| s.portal_client_id = id.clone()) {
        debug(&format!("the client id could not be saved ({}): this run uses {}", e, id));
    }
    id
}

/// One heartbeat, sent now — if Portal is on, has an address and has a
/// credential. `why` is for the debug line only (`startup`, `reconcile`).
///
/// The caller drops the [`Attempt`]: nothing about a heartbeat is a feature that
/// may fail loudly.
pub async fn send(app: &App, why: &str) -> Attempt {
    let s = app.settings();
    if !s.portal_enabled {
        return Attempt { note: "Portal is switched off".to_string(), ..Default::default() };
    }
    if s.portal_url.trim().is_empty() {
        return Attempt { note: "no Portal address is set".to_string(), ..Default::default() };
    }
    // The same client the reconciler reads with: a bearer token or Portal's own
    // session cookie, and `NoUrl` / `NoToken` when there is neither.
    let client = match PortalClient::from_settings(&s) {
        Ok(c) => c,
        Err(e) => return Attempt { note: e.to_string(), ..Default::default() },
    };
    // Without the history store there is no watch list to describe, and a
    // heartbeat that claims "no lanes" would be a lie. Skip it.
    let Some(hist) = app.hist() else {
        return Attempt { note: "history isn't open".to_string(), ..Default::default() };
    };
    let payload = Heartbeat {
        client_id: client_id(app),
        name: fiotransfer_sys::platform::hostname(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: platform(),
        lanes: lanes(&hist.watches().unwrap_or_default(), &s.portal_root_path()),
    };
    let body = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => return Attempt { note: format!("the payload would not serialize: {}", e), ..Default::default() },
    };
    let count = payload.lanes.len();
    match client.post_json(PATH, &body).await {
        Ok(_) => {
            debug(&format!("{}: sent {} lane(s) for {}", why, count, payload.client_id));
            Attempt { sent: true, lanes: count, note: String::new() }
        }
        Err(e) => {
            // Silent, as promised: the next tick tries again.
            debug(&format!("{}: not sent ({} lane(s)): {}", why, count, e));
            Attempt { sent: false, lanes: count, note: e.to_string() }
        }
    }
}

/// A debug line, behind the same `FIO_TRACE` switch the transfer engine's trace
/// prints use. Nothing a user reads: not the transfer log, not the status line,
/// not an event.
fn debug(line: &str) {
    if std::env::var("FIO_TRACE").is_ok() {
        eprintln!("[portal heartbeat] {}", line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::history::watch::{DIR_DOWNLOAD, DIR_MIRROR, DIR_UPLOAD};

    fn watch(origin: &str, path: &str, direction: &str, folder: &str) -> Watch {
        Watch {
            id: format!("w-{}", path),
            local_path: path.to_string(),
            account_id: "A1".to_string(),
            folder_id: folder.to_string(),
            dest_label: "Link".to_string(),
            enabled: true,
            direction: direction.to_string(),
            on_change: "overwrite".to_string(),
            origin: origin.to_string(),
            ..Default::default()
        }
    }

    /// The case the reconciler makes for a `sync` link — two one-way lanes —
    /// plus a plain mirrored link, plus a watch the user made themselves.
    fn watches() -> Vec<Watch> {
        let root = "/Users/x/Portal";
        vec![
            watch(
                "portal:L1",
                &format!("{}/20261028_YML-XMAS-Luc/1_DRIVE", root),
                DIR_DOWNLOAD,
                "FD",
            ),
            watch(
                "portal:L1#exports",
                &format!("{}/20261028_YML-XMAS-Luc/0_EXPORTS", root),
                DIR_UPLOAD,
                "FX",
            ),
            watch("portal:L2", &format!("{}/Rakete", root), DIR_MIRROR, "F2"),
            // The user's own: no `portal:` origin, so not Portal's business.
            watch("", "/Users/x/Movies/own-project", DIR_MIRROR, "F9"),
        ]
    }

    #[test]
    fn a_lane_per_portal_watch_and_none_for_the_users_own() {
        let root = Path::new("/Users/x/Portal");
        let got = lanes(&watches(), root);
        assert_eq!(got.len(), 3, "one lane per Portal watch: {:?}", got.iter().map(|l| &l.path).collect::<Vec<_>>());
        assert!(
            got.iter().all(|l| !l.path.contains("own-project")),
            "a watch the user made is never reported: {:?}",
            got
        );

        // The drive lane: the project is the marker, the local is the folder.
        assert_eq!(got[0].path, "/Users/x/Portal/20261028_YML-XMAS-Luc/0_EXPORTS");
        assert_eq!(got[0].project, "20261028_YML-XMAS-Luc");
        assert_eq!(got[0].local, "0_EXPORTS");
        assert_eq!(got[0].direction, DIR_UPLOAD, "the exports lane only ever goes up");
        assert_eq!(got[0].folder_id, "FX");
        assert_eq!(got[0].link_id, "L1", "the #exports suffix is ours, not Portal's");

        assert_eq!(got[1].path, "/Users/x/Portal/20261028_YML-XMAS-Luc/1_DRIVE");
        assert_eq!(got[1].project, "20261028_YML-XMAS-Luc");
        assert_eq!(got[1].local, "1_DRIVE");
        assert_eq!(got[1].direction, DIR_DOWNLOAD);
        assert_eq!(got[1].folder_id, "FD");
        assert_eq!(got[1].link_id, "L1", "one link, two lanes: direction and local tell them apart");

        // The plain link: one mirrored watch on the marker folder itself.
        assert_eq!(got[2].path, "/Users/x/Portal/Rakete");
        assert_eq!(got[2].project, "Rakete");
        assert_eq!(got[2].local, "Rakete");
        assert_eq!(got[2].direction, DIR_MIRROR);
        assert_eq!(got[2].link_id, "L2");
    }

    #[test]
    fn every_lane_path_is_the_watch_row_and_absolute() {
        let root = Path::new("/Users/x/Portal");
        let ws = watches();
        let got = lanes(&ws, root);
        for l in &got {
            assert!(l.path.starts_with('/'), "{} is not absolute", l.path);
            assert!(ws.iter().any(|w| w.local_path == l.path), "{} is not a watch row", l.path);
        }
    }

    #[test]
    fn a_watch_outside_the_root_still_names_a_project() {
        // The root setting changed after the watch was made: the first segment
        // below the current root does not exist, so the containing folder is the
        // answer rather than an empty project.
        let ws = vec![watch("portal:L7", "/Volumes/Edit/Portal/Other/1_DRIVE", DIR_DOWNLOAD, "F7")];
        let got = lanes(&ws, Path::new("/Users/x/Portal"));
        assert_eq!(got[0].project, "Other");
        assert_eq!(got[0].local, "1_DRIVE");
        assert_eq!(got[0].link_id, "L7");
    }

    #[test]
    fn an_origin_without_a_link_id_is_not_a_lane() {
        let ws = vec![watch("portal:", "/Users/x/Portal/X", DIR_MIRROR, "F")];
        assert!(lanes(&ws, Path::new("/Users/x/Portal")).is_empty());
    }

    #[test]
    fn the_payload_is_the_contract() {
        let root = "/Users/x/Portal";
        let p = Heartbeat {
            client_id: "0f0c1c1e-0000-4000-8000-000000000000".to_string(),
            name: "studio-mac".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            platform: platform(),
            lanes: lanes(&watches(), Path::new(root)),
        };
        let v: serde_json::Value = serde_json::to_value(&p).expect("it serializes");
        assert_eq!(v["app_version"], env!("CARGO_PKG_VERSION"), "the version is the crate's own");
        assert_eq!(v["platform"], format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH));
        assert_eq!(v["name"], "studio-mac");
        let lane = &v["lanes"][1];
        for key in ["link_id", "project", "local", "direction", "folder_id", "path"] {
            assert!(lane.get(key).is_some(), "lane has no {}", key);
        }
        assert_eq!(lane["link_id"], "L1");
        assert_eq!(lane["project"], "20261028_YML-XMAS-Luc");
        assert_eq!(lane["local"], "1_DRIVE");
        assert_eq!(lane["direction"], "download");
        assert_eq!(lane["folder_id"], "FD");
        assert_eq!(lane["path"], "/Users/x/Portal/20261028_YML-XMAS-Luc/1_DRIVE");
    }

    /// The shape Portal's side parses: a uuid v4, canonical form, different every
    /// time. Checked as a shape here — no uuid crate, so no `Uuid::parse_str`.
    #[test]
    fn a_new_client_id_is_a_uuid_v4() {
        let a = new_client_id();
        let b = new_client_id();
        assert_ne!(a, b, "two ids are two random draws");
        assert_eq!(a.len(), 36, "{}", a);
        let groups: Vec<&str> = a.split('-').collect();
        assert_eq!(groups.iter().map(|g| g.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12], "{}", a);
        assert!(
            a.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
            "{} is not hex and dashes",
            a
        );
        assert_eq!(&a[14..15], "4", "version 4: {}", a);
        assert!(
            ['8', '9', 'a', 'b'].contains(&a.chars().nth(19).unwrap()),
            "RFC 4122 variant: {}",
            a
        );
    }

    /// Portal switched off: nothing is sent, and nothing is written either — a
    /// machine that never turns Portal on has no client id to keep.
    #[test]
    fn a_disabled_portal_sends_nothing_and_mints_no_id() {
        let mut s = Settings::default();
        s.portal_enabled = false;
        s.portal_url = "http://127.0.0.1:1".to_string();
        let app = App::new(s);
        let a = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async { send(&app, "test").await });
        assert!(!a.sent);
        assert!(a.note.contains("switched off"), "{}", a.note);
        assert_eq!(app.settings().portal_client_id, "", "no id was made");
    }

    /// On, but with no address: nothing is sent and no request is made — the
    /// check happens before the credential is even read. (`tests/heartbeat.rs`
    /// covers the other refusals against a stub Portal: no credential, a 500
    /// and a broken connection.)
    #[test]
    fn no_address_sends_nothing() {
        let mut s = Settings::default();
        s.portal_enabled = true;
        s.portal_url = "  ".to_string();
        let app = App::new(s);
        let a = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async { send(&app, "test").await });
        assert!(!a.sent);
        assert_eq!(a.lanes, 0);
        assert!(a.note.contains("address"), "{}", a.note);
        assert_eq!(app.settings().portal_client_id, "", "no id was made either");
    }
}
