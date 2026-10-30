//! Asking a signed update feed for new releases, and installing one over this
//! copy.
//!
//! The feed is one `latest.json` published beside the release's files, and every
//! download it names is verified with minisign against [`PUB_KEY`] before
//! anything on disk is touched: `cargo-packager-updater` does the check, the
//! download and that verification. A release must therefore carry the macOS zip
//! and a signature for exactly that file. Nothing is taken from the GitHub API
//! on trust any more.
//!
//! The app's own words for the states are unchanged (`idle`, `checking`,
//! `downloading`, `restarting`, plus the `progress`, `error` and `blocker`
//! fields the Settings card and the CLI read).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cargo_packager_updater::{Error as UpdaterError, Update, Updater, UpdaterBuilder};
use tokio_util::sync::CancellationToken;

/// Where the newest release's feed lives: a release asset, so it always
/// describes the newest release that has finished uploading.
pub const ENDPOINT: &str =
    "https://github.com/chaos-dotcom/FioTransfer/releases/latest/download/latest.json";

/// The public half of the key the release workflow signs update assets with.
///
/// It comes from `cargo packager signer generate`, which writes two files: `key`
/// (the private key, base64) and `key.pub` (the public one). This is the content
/// of `key.pub`, verbatim; the private key is the release workflow's
/// `CARGO_PACKAGER_SIGN_PRIVATE_KEY` and is never in this repository.
///
/// ROTATING THIS KEY STRANDS EVERY INSTALLED COPY. A build accepts only a
/// signature made by this exact key, so the first release signed with a new one
/// is refused by every copy already installed, and each of those has to be
/// replaced by hand (download and drag across). If it ever has to change, ship a
/// release signed with the old key first, then start signing with the new one.
/// Treat this value as write-once.
pub const PUB_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDZGRTZEQ0E0Q0Q1NjM5MjUKUldRbE9WYk5wTnptYjhlZ1pEck52Y3VvaHY1M0drUE1tUDhXT2dyUHdyUWNNbDBXdFZCY3N4bmoK";

/// The app's bundle identifier; an update must have the same one.
pub const BUNDLE_ID: &str = "com.farbhaus.fiotransfer";

const WORK_PREFIX: &str = ".FioTransfer-update-";
/// The app caps a check at 30s as well; this is the crate's own bound, so the
/// blocking request ends even when nobody is waiting for it any more.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// A transfer-sized download, bounded so a stalled connection can't last for
/// ever.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How far a download has come, as a fraction of the whole (0.0..=1.0). The
/// app draws it as "Downloading… N%".
pub type Progress = Arc<dyn Fn(f64) + Send + Sync>;

/// A published release, as the feed describes it.
#[derive(Debug, Clone)]
pub struct Release {
    /// The feed's `version`, "1.0.3". This is what decides "newer".
    pub version: String,
    /// Where the update is downloaded from (the feed's `url` for this platform).
    pub url: String,
    /// The crate's handle for it: download what it names, verified.
    update: Update,
}

/// One update feed, and the key every download it names must carry.
#[derive(Clone)]
pub struct Source {
    pub endpoint: String,
    pub pub_key: String,
    pub user_agent: String,
}

impl Source {
    /// The published feed, verified against the key burnt into this build.
    pub fn new(user_agent: &str) -> Self {
        Source {
            endpoint: ENDPOINT.to_string(),
            pub_key: PUB_KEY.to_string(),
            user_agent: user_agent.to_string(),
        }
    }

    /// The same, pointing somewhere else. Tests serve a feed from a socket; the
    /// app never does.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// The same, with another key. A build with no key can verify nothing, so it
    /// refuses to check at all.
    pub fn with_pub_key(mut self, pub_key: impl Into<String>) -> Self {
        self.pub_key = pub_key.into();
        self
    }

    /// The crate's updater for this build: this version, the feed, the key, and
    /// the one platform this copy runs on.
    fn updater(&self, timeout: Duration) -> Result<Updater, String> {
        if self.pub_key.trim().is_empty() {
            return Err("no update signing key is configured".to_string());
        }
        let version = match parse_semver(crate::VERSION) {
            Some(v) => v,
            None => return Err(format!("{} isn't a released version", crate::VERSION)),
        };
        let target = cargo_packager_updater::target()
            .ok_or_else(|| "this platform has no update target".to_string())?;
        let url = self
            .endpoint
            .parse()
            .map_err(|e| format!("the update endpoint isn't a URL: {}", e))?;
        let config = cargo_packager_updater::Config {
            endpoints: vec![url],
            ..Default::default()
        };
        let builder = UpdaterBuilder::new(version, config)
            // A feed may describe this one platform (url/signature at the top) or
            // a `platforms` map; the key is `macos-aarch64` either way.
            .target(target)
            .pub_key(self.pub_key.clone())
            .header("User-Agent", self.user_agent.clone())
            .map_err(|e| e.to_string())?
            .timeout(timeout);
        // The crate would ask `current_exe()` itself, and that refuses a path
        // holding a symlink (a target folder under /tmp, or an app reached
        // through one). This app swaps the bundle itself, so the crate only ever
        // needs this value as a placeholder.
        let builder = match std::env::current_exe() {
            Ok(exe) => builder.executable_path(exe),
            Err(_) => builder,
        };
        builder.build().map_err(|e| e.to_string())
    }

    /// Returns the release the feed announces, or None when it announces nothing
    /// newer than this build.
    pub async fn latest(&self, cx: &CancellationToken) -> Result<Option<Release>, String> {
        let updater = self.updater(CHECK_TIMEOUT)?;
        // The crate's client is blocking, so it runs off the runtime. The app's
        // own 30s timeout and cancellation can stop waiting for it, and the
        // crate's timeout ends the request itself.
        let work = tokio::task::spawn_blocking(move || updater.check());
        let out = tokio::select! {
            j = work => j.map_err(|e| format!("the update check stopped: {}", e))?,
            _ = cx.cancelled() => return Err("context canceled".to_string()),
        };
        match out {
            Ok(Some(update)) => Ok(Some(Release {
                version: update.version.clone(),
                url: update.download_url.to_string(),
                update,
            })),
            // Younger or the same version, or the feed said "204 No Content".
            Ok(None) => Ok(None),
            // No release has published a feed yet: that is "nothing newer", not a
            // failure to show the user.
            Err(UpdaterError::ReleaseNotFound) => Ok(None),
            Err(UpdaterError::Serialization(e)) => {
                Err(format!("the update feed isn't valid JSON: {}", e))
            }
            // The crate reads the body with reqwest's own JSON decoder, so a feed
            // that isn't JSON arrives here rather than above.
            Err(UpdaterError::Reqwest(e)) if e.is_decode() => {
                Err(format!("the update feed isn't valid JSON: {}", e))
            }
            Err(UpdaterError::TargetNotFound(t)) => {
                Err(format!("the update feed has no build for {}", t))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Downloads `rel`, checks it, and swaps it in for the app at `app`. The
    /// running copy keeps working from its new place, the returned folder, which
    /// is to be deleted once it has quit. Nothing changes if any step fails.
    pub async fn install(
        &self,
        cx: &CancellationToken,
        rel: &Release,
        app: &Path,
        progress: Progress,
    ) -> Result<String, String> {
        let app_dir = parent(app).ok_or_else(|| "not running from an app bundle".to_string())?;
        // Same volume, so the swap is two renames.
        let work = mkdtemp(&app_dir, WORK_PREFIX)?;
        match self.install_in(cx, rel, app, &work, progress).await {
            Ok(()) => Ok(work.to_string_lossy().into_owned()),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                Err(e)
            }
        }
    }

    async fn install_in(
        &self,
        cx: &CancellationToken,
        rel: &Release,
        app: &Path,
        work: &Path,
        progress: Progress,
    ) -> Result<(), String> {
        let bytes = download(cx, rel, progress).await?;
        let zip = work.join(zip_name(rel));
        std::fs::write(&zip, &bytes).map_err(|e| format!("saving the update: {}", e))?;

        let new = work.join("new");
        let out = std::process::Command::new("/usr/bin/ditto")
            .arg("-x")
            .arg("-k")
            .arg(&zip)
            .arg(&new)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let mut msg = String::from_utf8_lossy(&out.stdout).to_string();
            msg.push_str(&String::from_utf8_lossy(&out.stderr));
            return Err(format!("unpacking the update: {}", msg.trim()));
        }
        let fresh = new.join("FioTransfer.app");
        check(&fresh, &rel.version, app)?;
        // A quarantined app can't be launched by the helper below.
        let _ = std::process::Command::new("/usr/bin/xattr")
            .arg("-dr")
            .arg("com.apple.quarantine")
            .arg(&fresh)
            .output();

        let prev = work.join("previous.app");
        std::fs::rename(app, &prev).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(&fresh, app) {
            let _ = std::fs::rename(&prev, app);
            return Err(e.to_string());
        }
        Ok(())
    }
}

/// Downloads `rel` and checks its signature. The bytes come back only if they
/// are exactly what the feed's signature was made for; a tampered file, a
/// signature from another key, or a signature that isn't one at all is refused
/// here, before anything is unpacked.
async fn download(
    cx: &CancellationToken,
    rel: &Release,
    progress: Progress,
) -> Result<Vec<u8>, String> {
    if cx.is_cancelled() {
        return Err("context canceled".to_string());
    }
    // The crate hands the update the timeout its check ran with (short, for a
    // feed request); a release asset takes longer than that to arrive.
    let mut update = rel.update.clone();
    update.timeout = Some(DOWNLOAD_TIMEOUT);
    let stop = cx.clone();
    let work = tokio::task::spawn_blocking(move || {
        // A blocking request can't be interrupted between chunks, so a cancelled
        // context stops updating the bar and the bytes are thrown away below.
        let canceled = Arc::new(AtomicBool::new(false));
        let done = std::cell::Cell::new(0u64);
        let bytes = update.download_extended(
            |chunk, total| {
                if canceled.load(Ordering::Relaxed) {
                    return;
                }
                if stop.is_cancelled() {
                    canceled.store(true, Ordering::Relaxed);
                    return;
                }
                done.set(done.get() + chunk as u64);
                if let Some(total) = total.filter(|t| *t > 0) {
                    progress(done.get() as f64 / total as f64);
                }
            },
            || {},
        );
        (bytes, canceled.load(Ordering::Relaxed))
    });
    let (bytes, canceled) = work
        .await
        .map_err(|e| format!("the update download stopped: {}", e))?;
    if canceled {
        return Err("context canceled".to_string());
    }
    let bytes = bytes.map_err(download_error)?;
    if bytes.is_empty() {
        return Err("the update file is empty".to_string());
    }
    Ok(bytes)
}

/// The crate's download errors, said in this app's words. The two minisign
/// refusals are the interesting ones: they mean the bytes on the wire are not
/// the bytes the release signed.
fn download_error(e: UpdaterError) -> String {
    match e {
        // The bytes don't match the signature, or the signature isn't this
        // key's: either way nothing is unpacked.
        UpdaterError::Minisign(m) => format!("the update isn't signed as published: {}", m),
        UpdaterError::SignatureUtf8(_) | UpdaterError::Base64(_) => {
            "the update's signature can't be read".to_string()
        }
        UpdaterError::Network(m) => format!("downloading the update: {}", m),
        other => format!("downloading the update: {}", other),
    }
}

/// The feed's `version` and this build's are compared as semver, so anything the
/// crate can't read isn't a version at all.
fn parse_semver(v: &str) -> Option<cargo_packager_updater::semver::Version> {
    cargo_packager_updater::semver::Version::parse(v.trim_start_matches('v')).ok()
}

/// Whether `a` is a newer version than `b` ("1.0.10" > "1.0.9"). A leading "v"
/// and a "-suffix" are ignored; a version that doesn't parse (a development
/// build) is never newer or older than anything.
pub fn newer(a: &str, b: &str) -> bool {
    let (x, y) = match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => (x, y),
        _ => return false,
    };
    for i in 0..x.len().max(y.len()) {
        let p = x.get(i).copied().unwrap_or(0);
        let q = y.get(i).copied().unwrap_or(0);
        if p != q {
            return p > q;
        }
    }
    false
}

/// Whether this build is a released version (development builds don't update).
pub fn released(v: &str) -> bool {
    parse_version(v).is_some()
}

fn parse_version(v: &str) -> Option<Vec<i64>> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let v = v.split('-').next().unwrap_or("");
    if v.is_empty() {
        return None;
    }
    let mut out: Vec<i64> = Vec::new();
    for p in v.split('.') {
        let n: i64 = p.parse().ok()?;
        if n < 0 {
            return None;
        }
        out.push(n);
    }
    Some(out)
}

/// The .app containing the running binary.
pub fn bundle_path(exe: &Path) -> Result<PathBuf, String> {
    let exe: PathBuf = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    // .app/Contents/MacOS/<exe>
    let app = match exe.parent().and_then(Path::parent).and_then(Path::parent) {
        Some(a) => a.to_path_buf(),
        None => return Err("not running from an app bundle".to_string()),
    };
    let in_macos = exe
        .parent()
        .and_then(Path::file_name)
        .map(|n| n == "MacOS")
        .unwrap_or(false);
    if app.extension().map(|e| e != "app").unwrap_or(true) || !in_macos {
        return Err("not running from an app bundle".to_string());
    }
    if app.to_string_lossy().contains("/AppTranslocation/") {
        return Err("move FioTransfer to Applications to update it here".to_string());
    }
    Ok(app)
}

/// Checks that the folder holding `app` can take a new copy.
pub fn can_replace(app: &Path) -> Result<(), String> {
    let dir = parent(app).unwrap_or_else(|| PathBuf::from("/"));
    // W_OK == 2, as Go's syscall.Access(dir, 2) asks (a path with an interior
    // NUL can't be asked about, and is not writable).
    if !fiotransfer_sys::platform::is_writable(&dir.to_string_lossy()) {
        return Err(format!("no permission to replace the app in {}", dir.display()));
    }
    Ok(())
}

/// Removes what earlier updates left next to `app`.
pub fn cleanup(app: &Path) {
    let dir = match parent(app) {
        Some(d) => d,
        None => return,
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(WORK_PREFIX) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Starts a helper that waits (up to two minutes) for process `pid` to exit,
/// deletes `old` (the folder install returned) and opens `app`.
pub fn relaunch(pid: u32, app: &Path, old: &str) -> Result<(), String> {
    let base = Path::new(old).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !base.starts_with(WORK_PREFIX) {
        return Err(format!("not an update folder: {}", old));
    }
    const SCRIPT: &str = r#"i=0
while kill -0 "$1" 2>/dev/null; do
  i=$((i + 1)); [ "$i" -gt 600 ] && exit 0
  sleep 0.2
done
case "$3" in */.FioTransfer-update-*) rm -rf "$3" ;; esac
open "$2""#;
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(SCRIPT)
        .arg("fiotransfer-relaunch")
        .arg(pid.to_string())
        .arg(app)
        .arg(old)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Outlives the app: its own session, so quitting the app doesn't kill it.
    fiotransfer_sys::platform::pre_exec_detach(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // Reap the helper so the app doesn't leave a zombie behind while it quits.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// ---------- the checks Install makes on the unpacked bundle ----------

/// Makes sure `fresh` is an intact FioTransfer of the expected version, signed
/// by the same team as the running copy when that one has a team.
fn check(fresh: &Path, version: &str, running: &Path) -> Result<(), String> {
    if std::fs::metadata(fresh).is_err() {
        return Err("the update doesn't contain FioTransfer.app".to_string());
    }
    let out = std::process::Command::new("/usr/bin/codesign")
        .arg("--verify")
        .arg("--deep")
        .arg("--strict")
        .arg(fresh)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let mut msg = String::from_utf8_lossy(&out.stdout).to_string();
        msg.push_str(&String::from_utf8_lossy(&out.stderr));
        return Err(format!("the update isn't signed for macOS: {}", msg.trim()));
    }
    let plist = fresh.join("Contents").join("Info.plist");
    let id = plist_value(&plist, "CFBundleIdentifier");
    if id != BUNDLE_ID {
        return Err(format!("the update is \"{}\", not FioTransfer", id));
    }
    let v = plist_value(&plist, "CFBundleShortVersionString");
    if v != version {
        return Err(format!("the update is version \"{}\", expected {}", v, version));
    }
    let team = team_id(running);
    if !team.is_empty() && team_id(fresh) != team {
        return Err("the update isn't signed by the same developer".to_string());
    }
    Ok(())
}

fn plist_value(plist: &Path, key: &str) -> String {
    match std::process::Command::new("/usr/bin/plutil")
        .arg("-extract")
        .arg(key)
        .arg("raw")
        .arg("-o")
        .arg("-")
        .arg(plist)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// The signing team of an app, "" when it's ad-hoc signed.
fn team_id(app: &Path) -> String {
    let out = match std::process::Command::new("/usr/bin/codesign").arg("-dv").arg(app).output() {
        Ok(o) => o,
        Err(_) => return String::new(),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    for l in text.lines() {
        if let Some(t) = l.strip_prefix("TeamIdentifier=") {
            if t != "not set" {
                return t.trim().to_string();
            }
        }
    }
    String::new()
}

// ---------- small helpers ----------

fn parent(p: &Path) -> Option<PathBuf> {
    p.parent().filter(|d| !d.as_os_str().is_empty()).map(|d| d.to_path_buf())
}

/// The zip asset's file name: the last segment of the URL the feed gives, and
/// only a fallback when that URL doesn't name one.
fn zip_name(rel: &Release) -> String {
    if let Some(last) = rel.url.rsplit('/').next() {
        if last.ends_with(".zip") {
            return last.to_string();
        }
    }
    format!("FioTransfer-{}-macOS.zip", rel.version)
}


/// Go's os.MkdirTemp(dir, prefix): a fresh 0700 folder next to the app.
fn mkdtemp(dir: &Path, prefix: &str) -> Result<PathBuf, String> {
    let mut last = String::new();
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    for i in 0..100u64 {
        let n = seed ^ (i.wrapping_mul(0x9e37_79b9_7f4a_7c15)) ^ ((std::process::id() as u64) << 32);
        let p = dir.join(format!("{}{:012x}", prefix, n & 0xffff_ffff_ffff));
        let mut builder = std::fs::DirBuilder::new();
        // 0700 on unix; on Windows the folder inherits its parent's ACL (see
        // `fileio::dir_mode`).
        crate::fileio::dir_mode(&mut builder, 0o700);
        match builder.create(&p) {
            Ok(()) => return Ok(p),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    // Go's TestNewer.
    #[test]
    fn versions_compare() {
        for (a, b, want) in [
            ("1.0.3", "1.0.2", true),
            ("v1.0.10", "1.0.9", true),
            ("1.1", "1.0.9", true),
            ("1.0.2", "1.0.2", false),
            ("1.0.2", "1.0.3", false),
            ("1.0.2", "1.0.2.0", false),
            ("2.0.0", "0.0.0-abc1234", true), // CI builds of main
            ("1.0.3", "dev", false),
            ("dev", "1.0.0", false),
            ("1.0.x", "1.0.0", false),
        ] {
            assert_eq!(newer(a, b), want, "newer({:?}, {:?})", a, b);
        }
    }

    // "Newer" is now the feed's semver against this build; the app's own helper
    // must not disagree with it on anything semver can read.
    #[test]
    fn the_crate_and_this_module_agree_on_newer() {
        let this = crate::VERSION;
        let have = parse_semver(this).expect("this build's version is semver");
        for feed in ["1.5.1", "1.6.0", "2.0.0", "1.5.0", "1.4.9", "0.0.1", "1.5.0-rc1"] {
            let offer = parse_semver(feed).expect(feed);
            assert_eq!(newer(feed, this), offer > have, "feed {} against {}", feed, this);
        }
        assert!(parse_semver("dev").is_none());
    }

    // The key in this file is what every installed copy verifies against; a
    // truncated or re-wrapped one would refuse every release for ever.
    #[test]
    fn the_burnt_in_key_is_a_minisign_public_key() {
        use base64::Engine as _;
        let text = base64::engine::general_purpose::STANDARD
            .decode(PUB_KEY.trim())
            .expect("the published key is base64");
        let text = String::from_utf8(text).expect("the published key is text");
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("untrusted comment: minisign public key"), "{:?}", lines[0]);
        let raw = base64::engine::general_purpose::STANDARD
            .decode(lines[1])
            .expect("the key line is base64");
        assert_eq!(raw.len(), 42, "a minisign public key is 42 bytes");
        assert!(ENDPOINT.ends_with("/releases/latest/download/latest.json"), "{}", ENDPOINT);
    }

    // Go's TestBundlePath.
    #[test]
    fn bundle_paths() {
        let p = bundle_path(Path::new("/Applications/FioTransfer.app/Contents/MacOS/FioTransfer"));
        assert_eq!(p.unwrap(), PathBuf::from("/Applications/FioTransfer.app"));
        assert!(bundle_path(Path::new(
            "/private/var/folders/x/T/AppTranslocation/ABC/d/FioTransfer.app/Contents/MacOS/FioTransfer"
        ))
        .is_err());
        assert!(bundle_path(Path::new("/Users/x/go/bin/fio")).is_err());
    }

    // Go's TestRelaunchOnlyRemovesUpdateFolders: the helper deletes the folder
    // it's given, so only an update's own folder is accepted.
    #[test]
    fn relaunch_only_removes_update_folders() {
        for old in ["/Applications", "/Users/x", "", "/Applications/FioTransfer.app"] {
            assert!(relaunch(std::process::id(), Path::new("/Applications/FioTransfer.app"), old).is_err());
        }
    }

    #[test]
    fn released_versions() {
        assert!(released("1.0.3"));
        assert!(released("v1.0.3"));
        assert!(!released("dev"));
        assert!(!released(""));
        assert!(!released("1.x"));
    }

    // ---------- a feed on a socket, and nothing else ----------
    //
    // No case here ever asks GitHub anything: a stub on 127.0.0.1 answers one
    // request per connection and closes.

    struct Stub {
        base: String,
        hits: Arc<AtomicUsize>,
    }

    impl Stub {
        /// Binds a port now, so the feed can name its own asset URL.
        fn bind() -> (TcpListener, String) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind a stub feed");
            let base = format!("http://{}", listener.local_addr().unwrap());
            (listener, base)
        }

        /// Serves `routes` — `(path, body)` — until the test process ends;
        /// anything else answers 404.
        fn play(listener: TcpListener, routes: Vec<(String, Vec<u8>)>) -> Stub {
            let base = format!("http://{}", listener.local_addr().unwrap());
            let hits = Arc::new(AtomicUsize::new(0));
            let seen = Arc::clone(&hits);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    // Read the head one byte at a time: it ends at the blank line,
                    // and the stub never waits for a body.
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => break,
                        }
                    }
                    let head = String::from_utf8_lossy(&head).to_string();
                    let path = head.split(' ').nth(1).unwrap_or("/").to_string();
                    seen.fetch_add(1, Ordering::Relaxed);
                    let route = routes.iter().find(|(p, _)| *p == path).map(|(_, b)| b.clone());
                    let (status, body) = match route {
                        Some(b) => ("200 OK", b),
                        None => ("404 Not Found", b"not found".to_vec()),
                    };
                    let mut out = format!(
                        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        status,
                        body.len()
                    )
                    .into_bytes();
                    out.extend_from_slice(&body);
                    let _ = stream.write_all(&out);
                    let _ = stream.flush();
                }
            });
            Stub { base, hits }
        }

        fn serve(routes: Vec<(String, Vec<u8>)>) -> Stub {
            let (listener, _) = Stub::bind();
            Stub::play(listener, routes)
        }

        /// A source that asks this stub for its feed.
        fn source(&self) -> Source {
            Source::new("FioTransfer/test").with_endpoint(format!("{}/latest.json", self.base))
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::Relaxed)
        }
    }

    /// The one-platform shape a release publishes: `url`, `signature` and
    /// `format` at the top level.
    fn feed_json(base: &str, version: &str, signature: &str) -> String {
        serde_json::json!({
            "version": version,
            "pub_date": "2026-01-01T00:00:00Z",
            "notes": "a test feed",
            "url": format!("{}/download", base),
            "signature": signature,
            "format": "app",
        })
        .to_string()
    }

    /// A stub serving `feed` at /latest.json and `asset` at /download, and a
    /// source pointed at it.
    fn a_feed(version: &str, signature: &str, asset: Vec<u8>) -> (Source, Stub) {
        let (listener, base) = Stub::bind();
        let routes = vec![
            ("/latest.json".to_string(), feed_json(&base, version, signature).into_bytes()),
            ("/download".to_string(), asset),
        ];
        let stub = Stub::play(listener, routes);
        (stub.source(), stub)
    }

    // ---------- the checks, without a signature ----------

    #[tokio::test]
    async fn a_newer_version_in_the_feed_is_reported() {
        let (src, stub) = a_feed("9.9.9", "unused-by-a-check", b"not needed".to_vec());
        let rel = src.latest(&CancellationToken::new()).await.expect("a check").expect("an update");
        assert_eq!(rel.version, "9.9.9");
        assert_eq!(rel.url, format!("{}/download", stub.base));
    }

    #[tokio::test]
    async fn an_equal_or_older_version_is_not_reported() {
        for version in [crate::VERSION, "1.4.9", "0.0.1"] {
            let (src, _stub) = a_feed(version, "unused-by-a-check", b"not needed".to_vec());
            let got = src.latest(&CancellationToken::new()).await.expect("a check");
            assert!(got.is_none(), "the feed's {} came back as an update", version);
        }
    }

    #[tokio::test]
    async fn malformed_json_is_refused() {
        let stub = Stub::serve(vec![(
            "/latest.json".to_string(),
            br#"{"version": "9.9.9", "url": "#.to_vec(),
        )]);
        let err = stub.source().latest(&CancellationToken::new()).await.expect_err("a broken feed");
        assert!(err.contains("isn't valid JSON"), "{}", err);
    }

    #[tokio::test]
    async fn a_feed_without_a_signature_is_refused() {
        let body = serde_json::json!({
            "version": "9.9.9",
            "url": "http://127.0.0.1:1/download",
            "format": "app",
        })
        .to_string();
        let stub = Stub::serve(vec![("/latest.json".to_string(), body.into_bytes())]);
        let err = stub.source().latest(&CancellationToken::new()).await.expect_err("no signature");
        assert!(err.contains("signature"), "{}", err);
    }

    #[tokio::test]
    async fn the_public_key_is_required() {
        let (src, stub) = a_feed("9.9.9", "unused-by-a-check", b"not needed".to_vec());
        let err = src
            .with_pub_key("")
            .latest(&CancellationToken::new())
            .await
            .expect_err("a build with no key");
        assert!(err.contains("signing key"), "{}", err);
        assert_eq!(stub.hits(), 0, "a build with no key must not ask the feed anything");
    }

    #[tokio::test]
    async fn a_missing_feed_reads_as_no_update() {
        // Nothing published yet: 404, and the card says "up to date", not
        // "couldn't check".
        let stub = Stub::serve(vec![]);
        let got = stub.source().latest(&CancellationToken::new()).await.expect("a check");
        assert!(got.is_none());
    }

    // ---------- download and signature ----------

    #[tokio::test]
    async fn a_tampered_download_is_refused() {
        let Some(signer) = Signer::ready() else { return };
        let signed = b"the update this signature was made for".to_vec();
        let served = b"a different file, but the same URL".to_vec();
        let signature = signer.sign_bytes(&signed).expect("sign");
        let (src, _stub) = a_feed("9.9.9", &signature, served);
        let src = src.with_pub_key(signer.pub_key.clone());
        let rel = src.latest(&CancellationToken::new()).await.expect("a check").expect("an update");
        let err = download(&CancellationToken::new(), &rel, Arc::new(|_p| {}))
            .await
            .expect_err("a tampered download");
        assert!(err.contains("isn't signed as published"), "{}", err);
    }

    #[tokio::test]
    async fn a_signature_from_another_key_is_refused() {
        let Some(signer) = Signer::ready() else { return };
        let Some(other) = Signer::ready() else { return };
        let asset = b"the update".to_vec();
        let signature = other.sign_bytes(&asset).expect("sign");
        let (src, _stub) = a_feed("9.9.9", &signature, asset);
        let src = src.with_pub_key(signer.pub_key.clone());
        let rel = src.latest(&CancellationToken::new()).await.expect("a check").expect("an update");
        let err = download(&CancellationToken::new(), &rel, Arc::new(|_p| {}))
            .await
            .expect_err("a foreign signature");
        assert!(err.contains("isn't signed as published"), "{}", err);
        assert!(err.contains("different key"), "{}", err);
    }

    #[tokio::test]
    async fn a_signature_that_is_not_one_is_refused() {
        let (src, _stub) = a_feed("9.9.9", "%%%% not a signature %%%%", b"the update".to_vec());
        let rel = src.latest(&CancellationToken::new()).await.expect("a check").expect("an update");
        let err = download(&CancellationToken::new(), &rel, Arc::new(|_p| {}))
            .await
            .expect_err("garbage for a signature");
        assert!(err.contains("signature can't be read") || err.contains("isn't a signature"), "{}", err);
    }

    #[tokio::test]
    async fn a_signed_download_comes_back_whole() {
        let Some(signer) = Signer::ready() else { return };
        let asset: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let signature = signer.sign_bytes(&asset).expect("sign");
        let (src, _stub) = a_feed("9.9.9", &signature, asset.clone());
        let src = src.with_pub_key(signer.pub_key.clone());
        let rel = src.latest(&CancellationToken::new()).await.expect("a check").expect("an update");
        let seen = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&seen);
        let bytes = download(
            &CancellationToken::new(),
            &rel,
            Arc::new(move |_p| {
                counted.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .await
        .expect("a signed download");
        assert_eq!(bytes, asset);
    }

    #[tokio::test]
    async fn a_cancelled_download_never_starts() {
        let (src, _stub) = a_feed("9.9.9", "unused", b"the update".to_vec());
        let rel = src
            .latest(&CancellationToken::new())
            .await
            .expect("a check")
            .expect("an update");
        let cx = CancellationToken::new();
        cx.cancel();
        let err = download(&cx, &rel, Arc::new(|_p| {})).await.expect_err("a cancelled download");
        assert_eq!(err, "context canceled");
    }

    // ---------- signing, with the real tool ----------
    //
    // `cargo packager signer` makes the key and signs the asset, exactly as the
    // release workflow does. When it isn't on PATH these cases print why and
    // pass: a test that needs a signature can't invent one.

    struct Signer {
        dir: tempfile::TempDir,
        private: PathBuf,
        /// The content of `key.pub`: what the feed's signature is checked
        /// against.
        pub_key: String,
    }

    impl Signer {
        fn ready() -> Option<Signer> {
            let help = std::process::Command::new("cargo")
                .args(["packager", "signer", "--help"])
                .output();
            match help {
                Ok(o) if o.status.success() => {}
                _ => {
                    println!("skipping: `cargo packager signer` is not on PATH, so no key can be made");
                    return None;
                }
            }
            let dir = tempfile::tempdir().expect("a temporary folder for the key");
            let private = dir.path().join("key");
            let out = std::process::Command::new("cargo")
                .args(["packager", "signer", "generate", "-f", "--ci", "--path"])
                .arg(&private)
                .output()
                .expect("run the signer");
            if !out.status.success() {
                println!(
                    "skipping: `cargo packager signer generate` failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                return None;
            }
            let pub_key =
                std::fs::read_to_string(dir.path().join("key.pub")).expect("the public key file");
            Some(Signer { dir, private, pub_key: pub_key.trim().to_string() })
        }

        /// What the feed's `signature` field carries for `bytes`: the content of
        /// the `.sig` file the tool writes beside the asset.
        fn sign_bytes(&self, bytes: &[u8]) -> Option<String> {
            let file = self.dir.path().join("asset.bin");
            std::fs::write(&file, bytes).ok()?;
            self.sign(&file)
        }

        fn sign(&self, file: &Path) -> Option<String> {
            let out = std::process::Command::new("cargo")
                .args(["packager", "signer", "sign", "-k"])
                .arg(&self.private)
                .arg(file)
                .output()
                .ok()?;
            if !out.status.success() {
                println!(
                    "skipping: `cargo packager signer sign` failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                return None;
            }
            let sig = std::fs::read_to_string(format!("{}.sig", file.display())).ok()?;
            Some(sig.trim().to_string())
        }
    }

    // ---------- the whole thing, over a real bundle ----------

    /// A minimal .app with this app's bundle id, ad-hoc signed, so the checks
    /// `install` makes on it (codesign, bundle id, version) can pass.
    #[cfg(target_os = "macos")]
    fn make_app(app: &Path, version: &str, marker: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let macos = app.join("Contents/MacOS");
        let res = app.join("Contents/Resources");
        std::fs::create_dir_all(&macos).expect("Contents/MacOS");
        std::fs::create_dir_all(&res).expect("Contents/Resources");
        std::fs::write(res.join("marker.txt"), marker).expect("the marker");
        let exe = macos.join("FioTransfer");
        std::fs::write(&exe, "#!/bin/sh\nexit 0\n").expect("the executable");
        let mut perms = std::fs::metadata(&exe).expect("the executable's mode").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe, perms).expect("chmod the executable");
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>{}</string>
  <key>CFBundleExecutable</key><string>FioTransfer</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{}</string>
</dict>
</plist>
"#,
                BUNDLE_ID, version
            ),
        )
        .expect("the Info.plist");
        let out = std::process::Command::new("/usr/bin/codesign")
            .arg("--force")
            .arg("--deep")
            .arg("-s")
            .arg("-")
            .arg(app)
            .output()
            .expect("codesign the bundle");
        assert!(
            out.status.success(),
            "codesign: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    #[cfg(target_os = "macos")]
    fn zip_app(app: &Path, zip: &Path) {
        let out = std::process::Command::new("/usr/bin/ditto")
            .arg("-c")
            .arg("-k")
            .arg("--keepParent")
            .arg(app)
            .arg(zip)
            .output()
            .expect("zip the bundle");
        assert!(out.status.success(), "ditto: {}", String::from_utf8_lossy(&out.stderr).trim());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_signed_zip_replaces_the_app() {
        let Some(signer) = Signer::ready() else { return };
        let tmp = tempfile::tempdir().expect("a temporary folder");
        let app = tmp.path().join("Applications/FioTransfer.app");
        make_app(&app, crate::VERSION, "the copy that is running");
        let fresh = tmp.path().join("release/FioTransfer.app");
        make_app(&fresh, "9.9.9", "the released copy");
        let zip = tmp.path().join("FioTransfer-9.9.9-macOS.zip");
        zip_app(&fresh, &zip);
        let signature = signer.sign(&zip).expect("sign the release");
        let (src, _stub) = a_feed("9.9.9", &signature, std::fs::read(&zip).expect("the zip"));
        let src = src.with_pub_key(signer.pub_key.clone());
        let cx = CancellationToken::new();
        let rel = src.latest(&cx).await.expect("a check").expect("an update");
        let work = src.install(&cx, &rel, &app, Arc::new(|_p| {})).await.expect("install");

        let marker = std::fs::read_to_string(app.join("Contents/Resources/marker.txt")).expect("the marker");
        assert_eq!(marker, "the released copy", "the new bundle is in place");
        let version = plist_value(&app.join("Contents/Info.plist"), "CFBundleShortVersionString");
        assert_eq!(version, "9.9.9");
        let work = Path::new(&work);
        assert!(work.join("previous.app").is_dir(), "the old copy is kept until the app quits");
        assert!(
            std::fs::read_to_string(work.join("previous.app/Contents/Resources/marker.txt"))
                .expect("the old marker")
                == "the copy that is running"
        );
        let _ = std::fs::remove_dir_all(work);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_bundle_of_another_version_is_refused_and_nothing_moves() {
        let Some(signer) = Signer::ready() else { return };
        let tmp = tempfile::tempdir().expect("a temporary folder");
        let app = tmp.path().join("Applications/FioTransfer.app");
        make_app(&app, crate::VERSION, "the copy that is running");
        // The feed says 9.9.9; the bundle inside says 9.9.8.
        let fresh = tmp.path().join("release/FioTransfer.app");
        make_app(&fresh, "9.9.8", "the wrong copy");
        let zip = tmp.path().join("FioTransfer-9.9.9-macOS.zip");
        zip_app(&fresh, &zip);
        let signature = signer.sign(&zip).expect("sign the release");
        let (src, _stub) = a_feed("9.9.9", &signature, std::fs::read(&zip).expect("the zip"));
        let src = src.with_pub_key(signer.pub_key.clone());
        let cx = CancellationToken::new();
        let rel = src.latest(&cx).await.expect("a check").expect("an update");
        let err = src
            .install(&cx, &rel, &app, Arc::new(|_p| {}))
            .await
            .expect_err("a bundle of another version");
        assert!(err.contains("expected 9.9.9"), "{}", err);
        let marker = std::fs::read_to_string(app.join("Contents/Resources/marker.txt")).expect("the marker");
        assert_eq!(marker, "the copy that is running", "nothing was swapped in");
    }
    /// A typo in [`PUB_KEY`] is invisible at compile time and fatal at runtime: every
    /// download fails "signature verification failed" and nobody can ever update. It
    /// shipped that way once. This verifies a fixture signed by the release key, so
    /// the constant is checked against the key rather than against itself.
    #[test]
    fn the_embedded_public_key_verifies_a_signature_made_by_the_release_key() {
        use base64::Engine as _;
        let dec = |s: &str| {
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(s.trim()).expect("base64"))
                .expect("utf8")
        };
        let key = minisign_verify::PublicKey::decode(&dec(PUB_KEY)).expect("PUB_KEY is a minisign key");
        let sig_b64 = include_str!("../../tests/data/updater-keycheck.txt.sig");
        let sig = minisign_verify::Signature::decode(&dec(sig_b64)).expect("signature decodes");
        let data = include_bytes!("../../tests/data/updater-keycheck.txt");
        key.verify(data, &sig, true)
            .expect("PUB_KEY must be the public half of the release signing key");
    }

}
