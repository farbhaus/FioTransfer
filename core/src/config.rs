
//! User settings persisted under ~/Library/Application Support/FioTransfer.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

/// One level of a Frame.io path: account, workspace, project or folder.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Crumb {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Adobe Developer Console "OAuth Single Page App" credential client ID.
    pub client_id: String,
    /// Port for the sign-in redirect (https://localhost:<port>/callback).
    pub redirect_port: i32,
    /// Max parallel HTTP streams across all transfers.
    pub streams: i32,
    /// Download segment size in MiB.
    pub segment_mb: i32,
    /// Default folder for downloads.
    pub download_dir: String,
    /// Write a transfer log next to the media (on by default).
    pub no_folder_logs: bool,
    /// Ask GitHub for new releases (on by default).
    pub no_update_check: bool,
    /// The last release a notification was shown for.
    pub update_notified: String,

    /// Last upload destination, remembered for convenience.
    pub last_account_id: String,
    pub last_folder_id: String,
    pub last_folder_label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_folder_path: Vec<Crumb>,

    // ---- Portal ----
    // Every field is `#[serde(default)]`: a settings.json written before Portal
    // existed has none of them, and this struct has no field-level defaults, so
    // without it the whole file would be rejected and every setting lost.
    //
    // The bearer token is NOT here: it is a secret, and `portal` keeps it in the
    // Keychain the way `auth` keeps the Adobe session.
    /// Portal's base URL, e.g. https://portal.example.com. Empty: not set up.
    #[serde(default)]
    pub portal_url: String,
    /// Where the project folders live on this Mac, one per synced link.
    /// Portal's own destination paths are not used.
    #[serde(default = "default_portal_root")]
    pub portal_root: String,
    /// The folder in each project that the link's uploads come down into, when
    /// Portal doesn't name it.
    #[serde(default = "default_portal_in_folder")]
    pub portal_in_folder: String,
    /// The folder in each project whose files go back up, when Portal doesn't
    /// name it.
    #[serde(default = "default_portal_out_folder")]
    pub portal_out_folder: String,
    /// Reconcile watch folders from Portal in the background (off by default).
    #[serde(default)]
    pub portal_enabled: bool,
    /// What a mirrored watch does with a local file that changed: overwrite or
    /// keep its earlier copy on Frame.io.
    #[serde(default = "default_portal_on_change")]
    pub portal_on_change: String,
    /// Seconds between Portal reconciles.
    #[serde(default = "default_portal_every")]
    pub portal_every_secs: i64,
    /// The admin email the last Portal sign-in used. A setting, not a secret:
    /// it is what the sign-in field is prefilled with next time. The password
    /// is never a setting, and the session cookie goes to the Keychain.
    #[serde(default)]
    pub portal_email: String,
}

/// Default for `Settings::portal_root`: ~/Portal.
pub const PORTAL_ROOT_DIR: &str = "Portal";
/// Defaults for `Settings::portal_in_folder` / `portal_out_folder`.
pub const PORTAL_IN_FOLDER: &str = "IN";
pub const PORTAL_OUT_FOLDER: &str = "OUT";
/// Default for `Settings::portal_every_secs`.
pub const PORTAL_EVERY_SECS: i64 = 30;
/// The shortest interval a poller may be asked for.
pub const PORTAL_MIN_EVERY_SECS: i64 = 5;
/// Default for `Settings::portal_on_change` (the watch row's own default too).
pub const PORTAL_ON_CHANGE: &str = crate::history::watch::ON_CHANGE_OVERWRITE;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            client_id: String::new(),
            redirect_port: 8765,
            streams: 16,
            segment_mb: 64,
            download_dir: home_dir().join("Downloads").to_string_lossy().into_owned(),
            no_folder_logs: false,
            no_update_check: false,
            update_notified: String::new(),
            last_account_id: String::new(),
            last_folder_id: String::new(),
            last_folder_label: String::new(),
            last_folder_path: Vec::new(),
            portal_url: String::new(),
            portal_root: default_portal_root(),
            portal_in_folder: default_portal_in_folder(),
            portal_out_folder: default_portal_out_folder(),
            portal_enabled: false,
            portal_on_change: PORTAL_ON_CHANGE.to_string(),
            portal_every_secs: PORTAL_EVERY_SECS,
            portal_email: String::new(),
        }
    }
}

impl Settings {
    pub fn folder_logs(&self) -> bool {
        !self.no_folder_logs
    }

    /// The local root Portal's project folders are mirrored under, with a
    /// leading `~` expanded: `~/Portal` by default.
    pub fn portal_root_path(&self) -> PathBuf {
        let root = if self.portal_root.trim().is_empty() { default_portal_root() } else { self.portal_root.clone() };
        expand_home(root.trim())
    }

    /// What a mirrored watch does with a file that changed: `overwrite` unless
    /// the setting is one of the watch's own rules.
    pub fn portal_change_rule(&self) -> String {
        match self.portal_on_change.as_str() {
            crate::history::watch::ON_CHANGE_KEEP => crate::history::watch::ON_CHANGE_KEEP.to_string(),
            _ => crate::history::watch::ON_CHANGE_OVERWRITE.to_string(),
        }
    }

    /// Seconds between reconciles, clamped to what the poller can honour.
    pub fn portal_interval(&self) -> std::time::Duration {
        // `normalize` has already clamped this; the clamp is repeated here because
        // `portal_interval` is public and is called on settings that may not have
        // been through it (a test, a caller that built the struct by hand).
        let secs = if self.portal_every_secs <= 0 {
            PORTAL_EVERY_SECS
        } else {
            self.portal_every_secs.clamp(PORTAL_MIN_EVERY_SECS, 86_400)
        };
        std::time::Duration::from_secs(secs as u64)
    }

    fn normalize(&mut self) {
        let d = Settings::default();
        if self.redirect_port <= 0 {
            self.redirect_port = d.redirect_port;
        }
        if self.streams <= 0 {
            self.streams = d.streams;
        }
        if self.streams > 128 {
            self.streams = 128;
        }
        if self.segment_mb < 8 {
            self.segment_mb = d.segment_mb;
        }
        if self.segment_mb > 512 {
            self.segment_mb = 512;
        }
        if self.download_dir.is_empty() {
            self.download_dir = d.download_dir;
        }
        // Portal: an empty root means "not configured", and the filesystem root
        // is never a mirror. The URL is stored without a trailing slash so it
        // joins to a path with exactly one.
        self.portal_url = self.portal_url.trim().trim_end_matches('/').to_string();
        if self.portal_root.trim().is_empty() || self.portal_root.trim() == "/" {
            self.portal_root = d.portal_root;
        }
        self.portal_in_folder = self.portal_in_folder.trim().to_string();
        if self.portal_in_folder.is_empty() {
            self.portal_in_folder = d.portal_in_folder;
        }
        self.portal_out_folder = self.portal_out_folder.trim().to_string();
        if self.portal_out_folder.is_empty() {
            self.portal_out_folder = d.portal_out_folder;
        }
        if self.portal_on_change != crate::history::watch::ON_CHANGE_KEEP {
            self.portal_on_change = PORTAL_ON_CHANGE.to_string();
        }
        // Below the floor: clamp up to it, rather than quietly substituting the
        // default. Somebody who writes `1` wants the fastest poller allowed, not
        // thirty seconds — the old rule made a one-second setting slower than a
        // five-second one. Zero and negatives are not a request at all (a missing
        // key already takes the default), so those keep the default.
        self.portal_every_secs = if self.portal_every_secs <= 0 {
            PORTAL_EVERY_SECS
        } else {
            self.portal_every_secs.clamp(PORTAL_MIN_EVERY_SECS, 86_400)
        };
        // An email address is not a secret, but a password is never written
        // here at all: this field is trimmed and nothing else.
        self.portal_email = self.portal_email.trim().to_string();
    }
}

/// `overwrite` unless a settings file says otherwise.
fn default_portal_on_change() -> String {
    PORTAL_ON_CHANGE.to_string()
}

/// The poll interval a settings file gets when it does not say.
fn default_portal_every() -> i64 {
    PORTAL_EVERY_SECS
}

fn default_portal_in_folder() -> String {
    PORTAL_IN_FOLDER.to_string()
}

fn default_portal_out_folder() -> String {
    PORTAL_OUT_FOLDER.to_string()
}

/// ~/Portal, where the synced links' folders go.
pub fn default_portal_root() -> String {
    home_dir().join(PORTAL_ROOT_DIR).to_string_lossy().into_owned()
}

/// `~` and `~/x` are the user's home; anything else is taken as given.
fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() => home_dir(),
        Some(rest) => match rest.strip_prefix('/') {
            Some(r) => home_dir().join(r),
            None => PathBuf::from(p),
        },
        None => PathBuf::from(p),
    }
}

/// The user's home folder: `HOME`, or on Windows `USERPROFILE`
/// (`C:\Users\<name>` — Windows sets no `HOME`), and `/` where neither is set.
pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h);
    }
    if cfg!(windows) {
        if let Some(h) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(h);
        }
    }
    PathBuf::from("/")
}

/// The app's own folder, created on demand: `~/Library/Application
/// Support/FioTransfer`, and on Windows `%APPDATA%\FioTransfer`.
///
/// That folder is the per-user roaming one, and its ACL is what keeps the token
/// files in it to this account — that is the protection Windows has instead of
/// the 0600 mode the same files are created with on unix (see `fileio::mode`).
pub fn dir() -> PathBuf {
    let dir = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("FioTransfer")
    } else {
        home_dir().join("Library").join("Application Support").join("FioTransfer")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Where the human-readable transfer log lives: `~/Library/Logs/FioTransfer`, and
/// on Windows `%LOCALAPPDATA%\FioTransfer\Logs` — local, not roaming: a log
/// belongs to the machine it was written on.
pub fn log_dir() -> PathBuf {
    let dir = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Local"))
            .join("FioTransfer")
            .join("Logs")
    } else {
        home_dir().join("Library").join("Logs").join("FioTransfer")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn path() -> PathBuf {
    dir().join("settings.json")
}

static MU: Mutex<()> = Mutex::new(());

pub fn load() -> Settings {
    let _g = MU.lock().unwrap();
    load_unlocked()
}

fn load_unlocked() -> Settings {
    let mut s = Settings::default();
    if let Ok(b) = std::fs::read(path()) {
        if let Ok(v) = serde_json::from_slice::<Settings>(&b) {
            s = v;
        }
    }
    s.normalize();
    s
}

fn save(s: &Settings) -> std::io::Result<()> {
    let mut s = s.clone();
    s.normalize();
    let b = serde_json::to_vec_pretty(&s).unwrap_or_default();
    let p = path();
    let tmp = PathBuf::from(format!("{}.tmp", p.display()));
    std::fs::write(&tmp, &b)?;
    std::fs::rename(&tmp, &p)
}

/// Loads, mutates and saves settings under one lock, so concurrent updates
/// can't drop each other's changes.
pub fn update<F: FnOnce(&mut Settings)>(f: F) -> std::io::Result<Settings> {
    let _g = MU.lock().unwrap();
    let mut s = load_unlocked();
    f(&mut s);
    s.normalize();
    save(&s)?;
    Ok(s)
}

pub const ERR_NO_CLIENT_ID: &str = "no Adobe Client ID set";

#[cfg(test)]
mod tests {
    use super::*;

    /// A setting below the floor asks for the fastest poller allowed. It used to
    /// mean the default — so `1` polled every 30 s and `5` every 5 s, which is the
    /// opposite of what anybody writing `1` meant.
    #[test]
    fn a_short_interval_clamps_up_to_the_floor_not_to_the_default() {
        let mut s = Settings::default();
        s.portal_every_secs = 1;
        s.normalize();
        assert_eq!(s.portal_every_secs, PORTAL_MIN_EVERY_SECS);
        assert_eq!(s.portal_interval(), std::time::Duration::from_secs(5));

        // Not a request at all: keep the default.
        let mut z = Settings::default();
        z.portal_every_secs = 0;
        z.normalize();
        assert_eq!(z.portal_every_secs, PORTAL_EVERY_SECS);

        // Above the ceiling: the ceiling.
        let mut big = Settings::default();
        big.portal_every_secs = 10_000_000;
        big.normalize();
        assert_eq!(big.portal_every_secs, 86_400);

        // And a sane value is left alone.
        let mut ok = Settings::default();
        ok.portal_every_secs = 45;
        ok.normalize();
        assert_eq!(ok.portal_every_secs, 45);
        assert_eq!(ok.portal_interval(), std::time::Duration::from_secs(45));
    }

    /// `portal_interval` is public, so it clamps on its own too.
    #[test]
    fn the_interval_itself_is_clamped() {
        let mut s = Settings::default();
        s.portal_every_secs = 2;
        assert_eq!(s.portal_interval(), std::time::Duration::from_secs(5));
        s.portal_every_secs = -1;
        assert_eq!(s.portal_interval(), std::time::Duration::from_secs(PORTAL_EVERY_SECS as u64));
    }
}
