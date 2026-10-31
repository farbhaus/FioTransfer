//! The reconciler: Portal's `.turbosort` markers become watch folders.
//!
//! Portal publishes upload/download links and writes a `.turbosort` marker into
//! a link's Frame.io folder. The marker names the `Live_Work` folder the project
//! belongs to. For every link that carries one, this module keeps exactly one
//! watch folder: local `portal_root/<marker>` <-> the link's Frame.io folder,
//! both ways (`mirror`).
//!
//! A link whose `folder` carries a `sync` object ([`crate::portal::Sync`]) is
//! **two one-way folders** instead of one mirror, both under the same marker
//! folder and both owned by that link:
//!
//! * `portal_root/<marker>/<drive.local>` ← the link's (or `drive`'s) Frame.io
//!   folder: `download`, the folder the client sends into;
//! * `portal_root/<marker>/<exports.local>` → `exports.folder_id`: `upload`, the
//!   folder we make things in. Only when Portal names it.
//!
//! The direction is what keeps deletions from propagating: a `download` watch
//! never uploads and an `upload` watch never downloads, and nothing in this
//! module or in the watch engine deletes a file because its opposite number
//! disappeared. A mapping Portal stops asking for is **unwatched, not deleted**,
//! exactly as a cleared marker has always been.
//!
//! What it never does:
//!
//! * touch Portal (only `GET /api/upload-links` is called; no file goes there);
//! * adopt Portal's `destination_path` / `path_template` — the local root is a
//!   FioTransfer setting, and the folder name is the marker, one path segment;
//! * touch a watch the user made. Only a watch whose `origin` is
//!   `portal:<link id>` may be updated or removed, and only while that link is
//!   live and still asks for a folder;
//! * delete anything local. The root and the folders under it are created, and
//!   nothing else on disk is created, moved or removed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::app::App;
use crate::history::watch::{Watch, DIR_DOWNLOAD, DIR_MIRROR, DIR_UPLOAD};
use crate::portal::{PortalClient, SyncFolder, UploadLink, SYNC_ADD_ONLY};

/// The origin a Portal-owned watch carries: `portal:<link id>`.
///
/// That is the drive side of a `sync` link, and the only watch a link has when
/// Portal sends no `sync` at all.
pub fn origin_of(link_id: &str) -> String {
    format!("portal:{}", link_id)
}

/// The origin of a `sync` link's exports watch: `portal:<link id>#exports`.
///
/// A different origin from the drive watch's, on purpose: the reconciler removes
/// a watch only when its own origin is no longer wanted, so clearing the exports
/// folder in Portal removes that one watch and leaves the drive watch alone. The
/// `#` cannot appear in a Portal link id that reaches this module (see `plan`),
/// so the two origins of one link and of two links never collide.
pub fn exports_origin_of(link_id: &str) -> String {
    format!("portal:{}#exports", link_id)
}

/// The suffix that marks an exports origin. `link_of` returns the link id with
/// this still on it — the origin is what identifies a watch, and
/// [`exports_origin_of`] is how a caller asks for the other one.
pub const EXPORTS_SUFFIX: &str = "#exports";

/// The link id of a Portal-owned watch, or `None` for a watch the user made.
///
/// Both of a `sync` link's watches answer here: `Some("L1")` for the drive watch
/// (`portal:L1`) and `Some("L1#exports")` for the exports one (`portal:L1#exports`),
/// which is all this is asked — "is this Portal's watch, or the user's?".
pub fn link_of(origin: &str) -> Option<&str> {
    origin.strip_prefix("portal:")
}

/// The longest marker accepted. A single path segment on any of the three
/// platforms is 255 bytes; a longer one is not a folder name.
pub const MAX_MARKER: usize = 255;

/// One link that produced no watch, and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Skip {
    pub link: String,
    pub marker: String,
    pub reason: String,
}

/// What one reconcile did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// When it ran (unix ms).
    pub at: i64,
    /// How many rows `GET /api/upload-links` answered with.
    pub links_seen: usize,
    /// Watches created or brought back in line. A `sync` link can account for
    /// two of these, one per mapping.
    pub ensured: usize,
    /// Portal-owned watches removed: the link is gone or off, the marker is
    /// cleared, a `sync` mapping was withdrawn (only that mapping's watch — a
    /// cleared exports folder leaves the drive watch alone), or a duplicate of
    /// another link's folder.
    pub removed: usize,
    /// Links that could not be acted on. An empty list is the normal case.
    pub skipped: Vec<Skip>,
    /// What a dry run *would* create or bring back in line, as absolute local
    /// paths — one per watch, so a `sync` link can show two. Empty after a real
    /// reconcile: it acted instead of planning.
    #[serde(default)]
    pub planned: Vec<String>,
    /// Why nothing was done, when the whole run failed (no token, no URL,
    /// Portal unreachable, history closed). Nothing is changed when this is set.
    pub error: String,
    /// What Portal said that this build read differently from how it was
    /// written: an unknown `sync` mode (read as `add_only`), or a `sync` side
    /// Portal named without a Frame.io folder. Not refusals — the watch was made
    /// — so they are not `skipped`; they say what was done instead.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// What the front end shows: the settings, whether a token is stored, and the
/// last reconcile.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PortalStatus {
    pub enabled: bool,
    /// A credential is stored — a bearer token or a signed-in session. The
    /// credential itself is never returned.
    pub signed_in: bool,
    /// A sign-in is half done: Portal took the password and is waiting for a
    /// one-time code. Not a credential and not `signed_in` — the code finishes
    /// the same sign-in, and there is nothing to send until it does.
    pub needs_code: bool,
    /// The admin the stored session belongs to, when one is stored. Empty for a
    /// token, which names no account: Portal's token path picks the only active
    /// admin there is.
    pub email: String,
    pub url: String,
    pub root: String,
    pub running: bool,
    pub last_at: i64,
    pub links_seen: usize,
    pub ensured: usize,
    pub removed: usize,
    pub skipped: usize,
    pub error: String,
}

/// Whether a `.turbosort` value is exactly one path segment.
///
/// This is the safety check the whole feature rests on: the marker is joined to
/// the local root, so `/`, `\`, `%` (an escape that could reach another route),
/// `.`, `..`, an empty value or a control character must never get through. A
/// marker of `..` would otherwise point the watch at the root's parent, and an
/// absolute one at the whole disk.
/// The one rule: exactly one path segment, whatever the value is called.
fn check_segment(x: &str, what: &str) -> Result<(), String> {
    if x.is_empty() {
        return Err(format!("{} is empty", what));
    }
    if x == "." || x == ".." {
        return Err(format!("{} {:?} is not a folder name", what, x));
    }
    if x.len() > MAX_MARKER {
        return Err(format!("{} is longer than {} bytes", what, MAX_MARKER));
    }
    if let Some(c) = x.chars().find(|c| *c == '/' || *c == '\\' || *c == '%' || c.is_control()) {
        return Err(format!("{} {:?} contains {:?}: one path segment only", what, x, c));
    }
    Ok(())
}

/// The marker, checked as one path segment.
pub fn check_marker(x: &str) -> Result<(), String> {
    check_segment(x, "the turbosort marker")
}

/// A `sync` mapping's `local`, checked by the same rule and for the same reason:
/// it is joined to `portal_root/<marker>`, so it must not be able to leave it.
pub fn check_local(x: &str) -> Result<(), String> {
    check_segment(x, "the local folder")
}

/// One watch Portal wants.
#[derive(Debug, Clone)]
struct Target {
    link: String,
    marker: String,
    local: String,
    acct: String,
    folder: String,
    label: String,
    rule: String,
    /// `mirror` for a link with no `sync`, `download` for its drive folder and
    /// `upload` for its exports folder. This is the only thing that decides
    /// which way files travel, and it is never widened: an `upload` watch does
    /// not download, so nothing it sees on Frame.io can delete a local file.
    direction: String,
    /// What the watch is recorded as: `portal:<id>` or `portal:<id>#exports`.
    origin: String,
}

/// One link's identity, as `plan` needs it while it works through the link's
/// one or two mappings.
struct LinkCtx<'a> {
    id: &'a str,
    marker: &'a str,
    /// `portal_root/<marker>`: the only folder a mapping may sit in.
    base: &'a Path,
    acct: &'a str,
    rule: &'a str,
}

/// Reads Portal, then applies what it says. A failure to read changes nothing.
pub async fn reconcile(app: &App) -> Report {
    let s = app.settings();
    let mut rep = Report { at: crate::xfer::now_ms(), ..Default::default() };
    if app.hist().is_none() {
        rep.error = "history isn't open".to_string();
        return rep;
    }
    if app.wat().is_none() {
        rep.error = "watch folders aren't running".to_string();
        return rep;
    }
    let client = match PortalClient::from_settings(&s) {
        Ok(c) => c,
        Err(e) => {
            rep.error = e.to_string();
            return rep;
        }
    };
    let links = match client.upload_links().await {
        Ok(l) => l,
        Err(e) => {
            rep.error = e.to_string();
            return rep;
        }
    };
    rep.links_seen = links.len();
    apply(app, &links, &mut rep);
    rep
}


/// What a reconcile would do, without doing any of it.
///
/// Same credential, same request, same plan — then it stops. Nothing is created,
/// nothing is removed, no watch is touched. This exists because the first time it
/// runs against a real library the folders it makes are somebody's actual
/// projects, and "show me first" is a reasonable thing to ask for.
pub async fn preview(app: &App) -> Report {
    let s = app.settings();
    let mut rep = Report { at: crate::xfer::now_ms(), ..Default::default() };
    let client = match PortalClient::from_settings(&s) {
        Ok(c) => c,
        Err(e) => {
            rep.error = e.to_string();
            return rep;
        }
    };
    let links = match client.upload_links().await {
        Ok(l) => l,
        Err(e) => {
            rep.error = e.to_string();
            return rep;
        }
    };
    rep.links_seen = links.len();
    let root = s.portal_root_path();
    let targets = plan(&links, &root, &s.portal_change_rule(), &mut rep);
    rep.ensured = targets.len();
    rep.planned = targets.iter().map(|t| t.local.clone()).collect();
    // Removals are knowable too: Portal-owned watches whose link is not in today's
    // list. Reading the database is not changing it.
    if let Some(hist) = app.hist() {
        let wanted: BTreeSet<String> = targets.iter().map(|t| t.origin.clone()).collect();
        rep.removed = hist
            .watches()
            .unwrap_or_default()
            .iter()
            .filter(|w| link_of(&w.origin).is_some() && !wanted.contains(&w.origin))
            .count();
    }
    rep
}

/// The local side: create the root, then add, edit and remove the watches Portal
/// asks for. Synchronous, so a caller can run it twice and compare.
fn apply(app: &App, links: &[UploadLink], rep: &mut Report) {
    let s = app.settings();
    let (Some(hist), Some(wat)) = (app.hist(), app.wat()) else {
        rep.error = "watch folders aren't running".to_string();
        return;
    };
    let root = s.portal_root_path();
    if let Err(e) = std::fs::create_dir_all(&root) {
        rep.error = format!("can't create {}: {}", root.display(), e);
        return;
    }
    let targets = plan(links, &root, &s.portal_change_rule(), rep);
    let wanted: BTreeSet<String> = targets.iter().map(|t| t.origin.clone()).collect();

    // First: Portal-owned watches that Portal no longer asks for. A user's watch
    // has no `portal:` origin, so it is never in this list.
    for w in hist.watches().unwrap_or_default().iter().filter(|w| link_of(&w.origin).is_some()) {
        if !wanted.contains(&w.origin) {
            wat.remove(&w.id);
            rep.removed += 1;
            hist.log(&format!(
                "PORTAL   removed  [{}] {} ({} no longer asks for it)",
                w.id, w.local_path, w.origin
            ));
        }
    }

    for t in &targets {
        let origin = t.origin.clone();
        // Read back each time: an earlier target may have moved or removed a
        // watch, and Portal owns every one of these.
        let rows = hist.watches().unwrap_or_default();

        // A Portal watch sitting on this folder under another link's origin is
        // stale (its own link wants some other folder, or is gone): Portal owns
        // it, so it goes. Its link's own turn — before or after this one —
        // makes the watch that is right for it.
        if let Some(other) = rows
            .iter()
            .find(|w| w.local_path == t.local && w.origin != origin && link_of(&w.origin).is_some())
        {
            wat.remove(&other.id);
            rep.removed += 1;
            hist.log(&format!("PORTAL   moved    [{}] {} (taken over by {})", other.id, other.local_path, t.link));
        }
        // Several rows under one origin cannot happen; if one is there, the
        // extras go. Each mapping has its own origin, so this is per watch, not
        // per link: the exports watch is never an "extra" of the drive one.
        let mine: Vec<Watch> = rows.iter().filter(|w| w.origin == origin).cloned().collect();
        for extra in mine.iter().skip(1) {
            wat.remove(&extra.id);
            rep.removed += 1;
        }

        // The watch has to have a folder to point at; `add` and `update` both
        // refuse one that isn't there. Only a directory under the root is ever
        // created here, never a file, and nothing local is ever removed.
        if let Err(e) = std::fs::create_dir_all(&t.local) {
            rep.skipped.push(Skip {
                link: t.link.clone(),
                marker: t.marker.clone(),
                reason: format!("can't create {}: {}", t.local, e),
            });
            continue;
        }

        match mine.first() {
            Some(w) => {
                if matches(w, t) {
                    continue; // already right: a second run changes nothing
                }
                match wat.update(&w.id, &t.local, &t.acct, &t.folder, &t.label, &t.direction, &t.rule, false) {
                    Ok(()) => {
                        rep.ensured += 1;
                        hist.log(&format!(
                            "PORTAL   edited   [{}] {} {} {} ({}, {})",
                            w.id, t.local, arrow(&t.direction), t.label, t.marker, t.direction
                        ));
                    }
                    Err(e) => rep.skipped.push(Skip {
                        link: t.link.clone(),
                        marker: t.marker.clone(),
                        reason: e,
                    }),
                }
            }
            None => {
                // A watch the user made in that folder is untouchable, whatever
                // Portal says the folder should hold.
                if rows.iter().any(|w| w.local_path == t.local && link_of(&w.origin).is_none()) {
                    rep.skipped.push(Skip {
                        link: t.link.clone(),
                        marker: t.marker.clone(),
                        reason: format!("{} is already watched by a watch you made", t.local),
                    });
                    continue;
                }
                match wat.add(&t.local, &t.acct, &t.folder, &t.label, &t.direction, &t.rule, false) {
                    Ok(()) => {
                        // The watch is the watcher's now; it has to carry the
                        // origin that says Portal may take it away again.
                        adopt_origin(&hist, &wat, &t.local, &origin);
                        rep.ensured += 1;
                        hist.log(&format!(
                            "PORTAL   added    {} {} {} ({}, {})",
                            t.local, arrow(&t.direction), t.label, t.marker, t.direction
                        ));
                    }
                    Err(e) => rep.skipped.push(Skip {
                        link: t.link.clone(),
                        marker: t.marker.clone(),
                        reason: e,
                    }),
                }
            }
        }
    }
}

/// Stamps the watch just made in `local` with `origin`, so the next reconcile
/// finds it instead of making a second one and the one after that may remove it.
/// `Watcher::add` does not take an origin (a user's watch has none), so it is
/// recorded after the fact, through the watcher, which keeps its own copy.
/// A failure is logged rather than ignored: without the origin the next
/// reconcile would read the watch as the user's own and leave it alone.
fn adopt_origin(hist: &crate::history::Store, wat: &crate::xfer::watch::Watcher, local: &str, origin: &str) {
    let done = hist
        .watches()
        .map(|rows| {
            rows.iter()
                .find(|w| w.local_path == local && w.origin.is_empty())
                .map(|w| wat.set_origin(&w.id, origin))
                .is_some()
        })
        .unwrap_or(false);
    if !done {
        hist.log(&format!("PORTAL   could not record {} as Portal's watch for {}", local, origin));
    }
}

/// Whether a watch already is what Portal wants for this link's mapping.
///
/// `enabled` is deliberately not compared: a user who paused a Portal watch
/// meant it, and a reconcile that re-enabled it every 30 s would undo that.
/// `direction` is compared, and it is the point: a mapping that says `download`
/// is not served by a mirror, because a mirror would send local files back.
fn matches(w: &Watch, t: &Target) -> bool {
    w.local_path == t.local
        && w.account_id == t.acct
        && w.folder_id == t.folder
        && w.dest_label == t.label
        && w.direction == t.direction
        && w.on_change == t.rule
}

/// Which way a watch moves files, for the log line.
fn arrow(direction: &str) -> &'static str {
    match direction {
        DIR_UPLOAD => "->",
        DIR_DOWNLOAD => "<-",
        _ => "<->",
    }
}

/// Turns Portal's rows into the watches they call for, reporting (not acting on)
/// the ones that cannot be.
///
/// A link with no `sync` plans one mirrored watch at `root/<marker>`, exactly as
/// before. A link with one plans up to two: the drive folder `download`, the
/// exports folder `upload`, each under `root/<marker>/`, each with its own
/// origin so that either can be withdrawn without touching the other.
fn plan(links: &[UploadLink], root: &Path, rule: &str, rep: &mut Report) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    // Sorted by id so two runs over the same rows decide the same way.
    let mut by_id: BTreeMap<String, &UploadLink> = BTreeMap::new();
    for l in links {
        let id = l.id.trim().to_string();
        if id.is_empty() {
            rep.skipped.push(Skip {
                link: String::new(),
                marker: l.marker(),
                reason: "the link has no id, so there is nothing to own a watch by".to_string(),
            });
            continue;
        }
        by_id.entry(id).or_insert(l);
    }

    for (id, l) in by_id {
        let marker = l.marker();
        let skip = |reason: String| Skip { link: id.clone(), marker: marker.clone(), reason };
        if marker.is_empty() {
            // Portal is not asking for a watch: no target, and any watch this
            // link used to own is removed by `apply`.
            continue;
        }
        if l.is_disabled() {
            rep.skipped.push(skip("the link is not live".to_string()));
            continue;
        }
        if let Err(e) = check_marker(&marker) {
            rep.skipped.push(skip(e));
            continue;
        }
        let (acct, folder) = (l.acct(), l.dest_folder());
        let base: PathBuf = root.join(&marker);
        // Belt and braces: the join must stay directly under the root. A marker
        // that slipped past `check_marker` would otherwise be able to leave it.
        if base.parent() != Some(root) {
            rep.skipped.push(skip(format!("{} is not directly under {}", base.display(), root.display())));
            continue;
        }
        // A marker folder belongs to one link, `sync` or not: the lower id wins
        // it, run after run. Its mappings live inside it, so a second link on
        // the same marker could otherwise have its mirror walk the first link's
        // one-way folders. Claimed only once the link has been cleared to make
        // something, so a broken link never takes the folder from a good one.
        let base_s = base.to_string_lossy().into_owned();
        let label = l.label();

        let Some(sync) = l.folder.sync.as_ref() else {
            // No `sync`: today's link, today's watch, untouched.
            if acct.is_empty() || folder.is_empty() {
                rep.skipped.push(skip(
                    "the link names no Frame.io account (or folder): Portal's /api/upload-links row seen here has no account_id"
                        .to_string(),
                ));
                continue;
            }
            if !claimed.insert(base_s.clone()) {
                rep.skipped.push(skip(format!("{} is another link's folder already", base_s)));
                continue;
            }
            out.push(Target {
                link: id.clone(),
                marker,
                local: base_s,
                acct,
                folder,
                label,
                rule: rule.to_string(),
                direction: DIR_MIRROR.to_string(),
                origin: origin_of(&id),
            });
            continue;
        };

        // Two one-way folders. Every side needs the account the watches are made
        // in, and there is no sensible default for that.
        if acct.is_empty() {
            rep.skipped.push(skip(
                "the link names no Frame.io account: Portal's /api/upload-links row seen here has no account_id"
                    .to_string(),
            ));
            continue;
        }
        if !claimed.insert(base_s.clone()) {
            rep.skipped.push(skip(format!("{} is another link's folder already", base_s)));
            continue;
        }
        if !sync.is_known_mode() {
            // Not a refusal: the contract's own mode is what this build does, so
            // an unknown one is read as it and said so.
            rep.notes.push(format!(
                "link {}: the sync mode {:?} is not one this build knows: read as {}",
                id,
                sync.mode.trim(),
                SYNC_ADD_ONLY
            ));
        }
        // The exports origin is `portal:<id>#exports`, so an id that already
        // holds a `#` could name another link's watch. Such a link is refused
        // rather than given an origin that might be somebody else's, and it says
        // which part of it was the problem.
        if sync.exports.is_some() && id.contains('#') {
            rep.skipped.push(skip(format!(
                "the link id {:?} contains '#', which the exports watch's origin is built from",
                id
            )));
            continue;
        }
        let ctx = LinkCtx { id: &id, marker: &marker, base: &base, acct: &acct, rule };
        let drive_origin = origin_of(&id);

        // The drive side: the folder the client sends into. Portal may name its
        // own folder; without one it is the link's own folder.
        let mut drive_local: Option<String> = None;
        let mut drive_folder = String::new();
        match sync.drive.as_ref() {
            Some(d) => {
                let f = if d.folder_id.is_empty() { folder.clone() } else { d.folder_id.clone() };
                match mapping(&ctx, &mut claimed, "drive", d, &f, DIR_DOWNLOAD, drive_origin.clone(), label.clone()) {
                    Ok(t) => {
                        // The names as Portal wrote them, so "both locals are
                        // the same folder" is decided on what Portal said and
                        // not on what it happened to resolve to.
                        drive_local = Some(d.local.clone());
                        drive_folder = t.folder.clone();
                        out.push(t);
                    }
                    Err(e) => rep.skipped.push(skip(e)),
                }
            }
            None => rep.notes.push(format!(
                "link {}: sync names no drive folder, so nothing is downloaded for it",
                id
            )),
        }

        // The exports side: the folder we make things in. Only when Portal names
        // one, and never in the drive folder's place.
        if let Some(x) = sync.exports.as_ref() {
            if x.folder_id.is_empty() {
                rep.skipped.push(skip(
                    "the sync exports folder names no folder_id: the link's own folder is already the drive's"
                        .to_string(),
                ));
                continue;
            }
            if drive_local.as_deref() == Some(x.local.as_str()) {
                rep.skipped.push(skip(format!(
                    "the exports folder {:?} is the drive folder: one folder cannot be both one-way",
                    x.local
                )));
                continue;
            }
            if x.folder_id == drive_folder && !drive_folder.is_empty() {
                rep.skipped.push(skip(format!(
                    "the exports folder is Frame.io folder {} already, the drive's: one folder cannot be both one-way",
                    x.folder_id
                )));
                continue;
            }
            let x_label = format!("{} ({})", label, x.local);
            match mapping(
                &ctx,
                &mut claimed,
                "exports",
                x,
                &x.folder_id,
                DIR_UPLOAD,
                exports_origin_of(&id),
                x_label,
            ) {
                Ok(t) => out.push(t),
                Err(e) => rep.skipped.push(skip(e)),
            }
        }
    }
    out
}

/// One side of a `sync` mapping, as the watch it calls for.
///
/// The local name is checked by the same rule the marker gets, and the folder it
/// makes must stay under `root/<marker>/` — the marker folder, not the root, is
/// as far as a mapping may reach. A refusal is returned as the sentence the
/// report carries; nothing is created for it.
#[allow(clippy::too_many_arguments)]
fn mapping(
    ctx: &LinkCtx<'_>,
    claimed: &mut BTreeSet<String>,
    side: &str,
    m: &SyncFolder,
    folder: &str,
    direction: &str,
    origin: String,
    label: String,
) -> Result<Target, String> {
    if let Err(e) = check_local(&m.local) {
        return Err(format!("the sync {} folder: {}", side, e));
    }
    let local = ctx.base.join(&m.local);
    // Belt and braces, as for the marker: the mapping must stay inside the
    // marker's folder.
    if local.parent() != Some(ctx.base) {
        return Err(format!("{} is not directly under {}", local.display(), ctx.base.display()));
    }
    if folder.is_empty() {
        return Err(format!("the sync {} folder names no Frame.io folder", side));
    }
    let local = local.to_string_lossy().into_owned();
    if !claimed.insert(local.clone()) {
        return Err(format!("{} is another mapping's folder already", local));
    }
    Ok(Target {
        link: ctx.id.to_string(),
        marker: ctx.marker.to_string(),
        local,
        acct: ctx.acct.to_string(),
        folder: folder.to_string(),
        label,
        rule: ctx.rule.to_string(),
        direction: direction.to_string(),
        origin,
    })
}

/// Starts the poller: every `Settings::portal_every_secs`, while Portal is on.
///
/// It reads the settings each time round, so a changed interval or a portal that
/// was switched off takes effect within one interval. One reconcile at a time is
/// `App::portal_reconcile`'s job, not this loop's.
pub fn spawn(app: Arc<App>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(app.settings().portal_interval()).await;
            let s = app.settings();
            if !s.portal_enabled || s.portal_url.trim().is_empty() {
                continue;
            }
            let _ = app.portal_reconcile().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::watch::{ON_CHANGE_KEEP, ON_CHANGE_OVERWRITE};

    #[test]
    fn a_marker_is_exactly_one_plain_segment() {
        for good in ["Rakete", "projekt-42", "Ümlaut", ".hidden", "a b", "x.y"] {
            assert!(check_marker(good).is_ok(), "{:?} is a folder name", good);
        }
        for bad in ["", ".", "..", "/", "a/b", "..%2f..", "a\\b", "a%b", "a\nb", "a\u{0}b", "a\tb"] {
            assert!(check_marker(bad).is_err(), "{:?} must be refused", bad);
        }
        assert!(check_marker(&"x".repeat(MAX_MARKER + 1)).is_err());
        assert!(check_marker(&"x".repeat(MAX_MARKER)).is_ok());
    }

    #[test]
    fn origin_round_trips() {
        assert_eq!(origin_of("L1"), "portal:L1");
        assert_eq!(link_of("portal:L1"), Some("L1"));
        assert_eq!(link_of(""), None);
        assert_eq!(link_of("user"), None);
        assert_eq!(link_of("Portal:L1"), None, "the prefix is exact");
    }

    fn link(id: &str, marker: &str) -> UploadLink {
        UploadLink {
            id: id.to_string(),
            name: format!("Link {}", id),
            target_folder_id: format!("F{}", id),
            target_account_id: "A1".to_string(),
            turbosort: marker.to_string(),
            ..Default::default()
        }
    }

    fn plan_of(links: &[UploadLink]) -> (Vec<Target>, Report) {
        let mut rep = Report::default();
        let root = Path::new("/root/Live_Work");
        let t = plan(links, root, ON_CHANGE_OVERWRITE, &mut rep);
        (t, rep)
    }

    /// One side of a `sync` object, as Portal writes it.
    fn side(local: &str, folder: &str) -> Option<SyncFolder> {
        Some(SyncFolder { local: local.to_string(), folder_id: folder.to_string() })
    }

    /// A link with a `sync` object inside its `folder`, the way Portal sends one.
    fn sync_link(id: &str, marker: &str, drive: Option<SyncFolder>, exports: Option<SyncFolder>) -> UploadLink {
        with_mode(id, marker, drive, exports, crate::portal::SYNC_ADD_ONLY)
    }

    fn with_mode(
        id: &str,
        marker: &str,
        drive: Option<SyncFolder>,
        exports: Option<SyncFolder>,
        mode: &str,
    ) -> UploadLink {
        let mut l = link(id, marker);
        l.folder = crate::portal::Folder {
            sync: Some(crate::portal::Sync { mode: mode.to_string(), drive, exports }),
            ..Default::default()
        };
        l
    }

    fn origins(t: &[Target]) -> Vec<String> {
        t.iter().map(|x| x.origin.clone()).collect()
    }

    fn locals(t: &[Target]) -> Vec<String> {
        t.iter().map(|x| x.local.clone()).collect()
    }

    #[test]
    fn a_valid_link_plans_one_mirrored_watch_under_the_root() {
        let (t, rep) = plan_of(&[link("L1", "Rakete")]);
        assert_eq!(t.len(), 1);
        assert!(rep.skipped.is_empty());
        assert_eq!(t[0].local, "/root/Live_Work/Rakete");
        assert_eq!(t[0].acct, "A1");
        assert_eq!(t[0].folder, "FL1");
        assert_eq!(t[0].label, "Link L1");
        assert_eq!(t[0].rule, ON_CHANGE_OVERWRITE);
    }

    #[test]
    fn an_empty_or_broken_marker_plans_nothing_and_says_why() {
        let (t, rep) = plan_of(&[link("L1", ""), link("L2", ".."), link("L3", "a/b"), link("L4", "ok")]);
        assert_eq!(t.len(), 1, "only the good marker plans a watch");
        assert_eq!(t[0].link, "L4");
        assert_eq!(rep.skipped.len(), 2, "an empty marker is not a skip: Portal simply asked for nothing");
        assert_eq!(rep.skipped.iter().map(|s| s.link.as_str()).collect::<Vec<_>>(), vec!["L2", "L3"]);
    }

    #[test]
    fn two_links_naming_one_folder_plan_one_watch() {
        let (t, rep) = plan_of(&[link("L1", "Same"), link("L2", "Same")]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].link, "L1", "the lower id wins, run after run");
        assert_eq!(rep.skipped.len(), 1);
        assert!(rep.skipped[0].reason.contains("another link's folder"));
    }

    #[test]
    fn a_link_without_an_account_plans_nothing() {
        let mut l = link("L1", "X");
        l.target_account_id.clear();
        let (t, rep) = plan_of(&[l]);
        assert!(t.is_empty());
        assert!(rep.skipped[0].reason.contains("no Frame.io account"));
    }

    #[test]
    fn a_link_that_is_off_plans_nothing() {
        let mut l = link("L1", "X");
        l.status = "archived".to_string();
        let (t, _) = plan_of(&[l]);
        assert!(t.is_empty());
    }

    #[test]
    fn the_change_rule_is_the_one_asked_for() {
        let mut rep = Report::default();
        let t = plan(&[link("L1", "X")], Path::new("/r"), ON_CHANGE_KEEP, &mut rep);
        assert_eq!(t[0].rule, ON_CHANGE_KEEP);
        assert_eq!(ON_CHANGE_KEEP, "keep");
    }

    #[test]
    fn a_watch_matches_only_when_every_field_is_right() {
        let t = Target {
            link: "L1".into(),
            marker: "X".into(),
            local: "/r/X".into(),
            acct: "A".into(),
            folder: "F".into(),
            label: "N".into(),
            rule: ON_CHANGE_OVERWRITE.into(),
            direction: DIR_MIRROR.into(),
            origin: origin_of("L1"),
        };
        let mut w = Watch {
            local_path: "/r/X".into(),
            account_id: "A".into(),
            folder_id: "F".into(),
            dest_label: "N".into(),
            direction: DIR_MIRROR.into(),
            on_change: ON_CHANGE_OVERWRITE.into(),
            ..Default::default()
        };
        assert!(matches(&w, &t));
        w.enabled = false;
        assert!(matches(&w, &t), "a paused watch is not edited back on");
        w.direction = crate::history::watch::DIR_UPLOAD.into();
        assert!(!matches(&w, &t));
        w.direction = DIR_MIRROR.into();
        w.on_change = ON_CHANGE_KEEP.into();
        assert!(!matches(&w, &t));
    }

    // ---------- a link that is two one-way folders ----------

    #[test]
    fn a_sync_link_with_a_drive_folder_downloads_into_it() {
        let (t, rep) = plan_of(&[sync_link("L1", "Rakete", side("1_DRIVE", ""), None)]);
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        assert_eq!(t.len(), 1);
        assert_eq!(locals(&t), vec!["/root/Live_Work/Rakete/1_DRIVE"]);
        assert_eq!(t[0].direction, DIR_DOWNLOAD, "the drive folder only ever comes down");
        assert_eq!(t[0].folder, "FL1", "no folder_id on the drive side: the link's own folder");
        assert_eq!(t[0].acct, "A1");
        assert_eq!(t[0].rule, ON_CHANGE_OVERWRITE);
        assert_eq!(t[0].origin, "portal:L1");
        assert_eq!(link_of(&t[0].origin), Some("L1"));
        assert!(rep.notes.is_empty(), "a plain add_only link says nothing: {:?}", rep.notes);
    }

    #[test]
    fn a_sync_link_with_exports_plans_two_watches_in_opposite_directions() {
        let l = sync_link("L1", "Rakete", side("1_DRIVE", "FD"), side("0_EXPORTS", "FX"));
        let (t, rep) = plan_of(&[l]);
        assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
        assert_eq!(locals(&t), vec!["/root/Live_Work/Rakete/1_DRIVE", "/root/Live_Work/Rakete/0_EXPORTS"]);
        assert_eq!(t[0].direction, DIR_DOWNLOAD);
        assert_eq!(t[0].folder, "FD");
        assert_eq!(t[1].direction, DIR_UPLOAD, "the exports folder only ever goes up");
        assert_eq!(t[1].folder, "FX");
        assert_eq!(origins(&t), vec![origin_of("L1"), exports_origin_of("L1")]);
        assert_eq!(link_of(&t[1].origin), Some("L1#exports"), "the exports watch is Portal's too");
        assert_ne!(t[0].origin, t[1].origin, "one origin each: either can be withdrawn alone");
        assert_eq!(t[1].label, "Link L1 (0_EXPORTS)", "two watches of one link are told apart on screen");
    }

    #[test]
    fn a_local_that_is_not_one_segment_refuses_that_mapping_and_no_other() {
        for bad in ["", ".", "..", "a/b", "a\\b", "a%b", "a\nb"] {
            let l = sync_link("L1", "Rakete", side(bad, ""), side("0_EXPORTS", "FX"));
            let (t, rep) = plan_of(&[l]);
            assert_eq!(locals(&t), vec!["/root/Live_Work/Rakete/0_EXPORTS"], "{:?} made a drive watch", bad);
            assert_eq!(rep.skipped.len(), 1, "{:?} said nothing: {:?}", bad, rep.skipped);
            assert!(rep.skipped[0].reason.contains("sync drive folder"), "{}", rep.skipped[0].reason);
            assert_eq!(t[0].direction, DIR_UPLOAD);
        }
        // …and the exports side is checked by the same rule.
        let l = sync_link("L1", "Rakete", side("1_DRIVE", ""), side("../out", "FX"));
        let (t, rep) = plan_of(&[l]);
        assert_eq!(locals(&t), vec!["/root/Live_Work/Rakete/1_DRIVE"]);
        assert!(rep.skipped[0].reason.contains("sync exports folder"), "{}", rep.skipped[0].reason);
    }

    #[test]
    fn one_local_twice_refuses_the_second_mapping() {
        let l = sync_link("L1", "Rakete", side("SAME", "FD"), side("SAME", "FX"));
        let (t, rep) = plan_of(&[l]);
        assert_eq!(locals(&t), vec!["/root/Live_Work/Rakete/SAME"]);
        assert_eq!(t[0].direction, DIR_DOWNLOAD);
        assert!(rep.skipped[0].reason.contains("is the drive folder"), "{}", rep.skipped[0].reason);
    }

    #[test]
    fn one_frame_io_folder_cannot_be_read_and_written_at_once() {
        let l = sync_link("L1", "Rakete", side("1_DRIVE", "F"), side("0_EXPORTS", "F"));
        let (t, rep) = plan_of(&[l]);
        assert_eq!(locals(&t), vec!["/root/Live_Work/Rakete/1_DRIVE"]);
        assert!(rep.skipped[0].reason.contains("one folder cannot be both one-way"), "{}", rep.skipped[0].reason);
    }

    #[test]
    fn the_exports_side_needs_a_folder_of_its_own() {
        let l = sync_link("L1", "Rakete", side("1_DRIVE", ""), side("0_EXPORTS", ""));
        let (t, rep) = plan_of(&[l]);
        assert_eq!(t.len(), 1, "the link's own folder is the drive's: exports has nothing to name");
        assert!(rep.skipped[0].reason.contains("names no folder_id"), "{}", rep.skipped[0].reason);
    }

    #[test]
    fn an_unknown_mode_is_read_as_add_only_and_noted() {
        let l = with_mode("L1", "Rakete", side("1_DRIVE", ""), side("0_EXPORTS", "FX"), "two_way");
        let (t, rep) = plan_of(&[l]);
        assert_eq!(t.len(), 2, "an unknown mode is not a refusal: it is read as add_only");
        assert_eq!(rep.notes.len(), 1);
        assert!(rep.notes[0].contains("two_way") && rep.notes[0].contains("add_only"), "{}", rep.notes[0]);
    }

    #[test]
    fn an_unstated_mode_is_add_only_and_says_nothing() {
        let l = with_mode("L1", "Rakete", side("1_DRIVE", ""), None, "  ");
        let (t, rep) = plan_of(&[l]);
        assert_eq!(t.len(), 1);
        assert!(rep.notes.is_empty(), "{:?}", rep.notes);
    }

    #[test]
    fn a_sync_link_without_a_drive_side_says_so_and_makes_only_what_it_can() {
        let l = sync_link("L1", "Rakete", None, side("0_EXPORTS", "FX"));
        let (t, rep) = plan_of(&[l]);
        assert_eq!(locals(&t), vec!["/root/Live_Work/Rakete/0_EXPORTS"]);
        assert_eq!(t[0].direction, DIR_UPLOAD);
        assert!(rep.notes.iter().any(|n| n.contains("no drive folder")), "{:?}", rep.notes);
    }

    #[test]
    fn a_sync_link_needs_an_account_like_any_other() {
        let mut l = sync_link("L1", "Rakete", side("1_DRIVE", ""), side("0_EXPORTS", "FX"));
        l.target_account_id.clear();
        let (t, rep) = plan_of(&[l]);
        assert!(t.is_empty());
        assert!(rep.skipped[0].reason.contains("no Frame.io account"), "{}", rep.skipped[0].reason);
    }

    #[test]
    fn a_plain_link_and_a_sync_link_cannot_share_one_marker_folder() {
        let (t, rep) = plan_of(&[
            sync_link("L1", "Same", side("1_DRIVE", ""), None),
            link("L2", "Same"),
        ]);
        assert_eq!(origins(&t), vec![origin_of("L1")], "the lower id owns the marker folder");
        assert!(rep.skipped[0].reason.contains("another link's folder"), "{}", rep.skipped[0].reason);
    }

    #[test]
    fn a_link_id_holding_a_hash_cannot_have_an_exports_watch() {
        // `portal:<id>#exports` is built from the id, so an id that already holds
        // a `#` could name another link's watch.
        let l = sync_link("L1#exports", "Rakete", side("1_DRIVE", ""), side("0_EXPORTS", "FX"));
        let (t, rep) = plan_of(&[l]);
        assert!(t.is_empty());
        assert!(rep.skipped[0].reason.contains('#'), "{}", rep.skipped[0].reason);
    }

    #[test]
    fn the_exports_origin_is_a_second_origin_for_the_same_link() {
        assert_eq!(exports_origin_of("L1"), "portal:L1#exports");
        assert_eq!(origin_of("L1"), "portal:L1");
        assert!(exports_origin_of("L1").ends_with(EXPORTS_SUFFIX));
        assert_ne!(origin_of("L1"), exports_origin_of("L1"));
        assert_eq!(link_of(&exports_origin_of("L1")), Some("L1#exports"));
        assert_eq!(link_of("portal:L1"), Some("L1"));
        assert_eq!(link_of("user"), None);
    }

    #[test]
    fn the_local_folder_rule_is_the_markers_rule() {
        for good in ["1_DRIVE", "0_EXPORTS", "projekt-42", "Ümlaut", ".hidden", "a b", "x.y"] {
            assert!(check_local(good).is_ok(), "{:?} is a folder name", good);
        }
        for bad in ["", ".", "..", "/", "a/b", "..%2f..", "a\\b", "a%b", "a\nb", "a\u{0}b", "a\tb"] {
            assert!(check_local(bad).is_err(), "{:?} must be refused", bad);
        }
        assert!(check_local(&"x".repeat(MAX_MARKER + 1)).is_err());
        assert!(check_local(&"x".repeat(MAX_MARKER)).is_ok());
    }
}
