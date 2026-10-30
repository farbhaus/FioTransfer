//! Public Frame.io share links to what a transfer moved.
//!
//! A direct port of Go's `internal/xfer/sharelink.go`.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use super::manager::{kind_tag, INSPECT_TTL};
use super::Manager;
use crate::history;

/// How many assets Frame.io takes when creating a share.
pub const MAX_SHARE_ASSETS: usize = 100;

pub const ERR_NOT_STARTED: &str = "some files haven't started uploading yet; try again shortly";

/// Go's `shareMu`: creating a share is serialized so two clicks can't make two
/// shares. It is a tokio mutex because the critical section awaits, and the
/// future must stay `Send`.
static SHARE_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A resolved and listed share link.
#[derive(Debug, Clone, Default)]
pub struct ShareListing {
    /// Link and passphrase as given.
    pub key: String,
    pub link: crate::share::Link,
    /// Passphrase in use (given, or remembered in the Keychain).
    pub pass: String,
    pub info: crate::share::Info,
    pub files: Vec<crate::share::File>,
    pub at: Option<Instant>,
}

impl ShareListing {
    /// The link's kind ("share", "review", "smash", …).
    pub fn kind(&self) -> &str {
        &self.link.kind
    }

    /// What the download folder is named after when the share has no name.
    pub fn label(&self) -> String {
        self.link.label()
    }

    /// Every byte the listing names.
    pub fn total_bytes(&self) -> i64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Files the listing could not resolve to a download URL.
    pub fn unavailable(&self) -> usize {
        self.files.iter().filter(|f| !f.problem.is_empty()).count()
    }

    /// Whether a download may still reuse this listing instead of listing again.
    pub fn fresh(&self) -> bool {
        self.at.map(|t| t.elapsed() <= INSPECT_TTL).unwrap_or(false)
    }
}

/// The parts of a history record's options the share needs.
#[derive(serde::Deserialize, Default)]
struct ShareOptions {
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    watch: String,
}

impl Manager {
    /// Returns a public Frame.io share link to what a transfer moved, creating
    /// the share the first time and keeping it in history. The share holds
    /// exactly that: each uploaded folder and loose file, or a watch-folder
    /// batch's files, never the rest of the destination folder.
    pub async fn share_link(&self, cx: &CancellationToken, id: &str) -> Result<String, String> {
        let _gate = SHARE_GATE.lock().await;
        let t = self.hist.get(id)?.ok_or_else(|| "no such transfer".to_string())?;
        if !t.share_link.is_empty() {
            return Ok(t.share_link.clone());
        }
        let opts: ShareOptions = serde_json::from_str(&t.options).unwrap_or_default();
        let watch = !opts.watch.is_empty() || (opts.paths.len() == 1 && t.source == opts.paths[0]);
        if t.kind == "download" && !watch {
            return Err("only files in your Frame.io account can be shared".to_string());
        }

        let j = self.job(id);
        let files: Vec<history::File> = match &j {
            // Running jobs only persist a file once it's finished.
            Some(j) => j.files().iter().map(|f| j.file_record(f)).collect(),
            None => self.hist.files(id)?,
        };
        let assets = self.share_assets(cx, &t.account_id, &t.kind, watch, &files).await?;
        if assets.is_empty() {
            if let Some(j) = &j {
                if !j.status().terminal() {
                    return Err(ERR_NOT_STARTED.to_string());
                }
            }
            return Err("none of these files made it to Frame.io".to_string());
        }
        if assets.len() > MAX_SHARE_ASSETS {
            return Err(format!(
                "a share holds up to {} items, this has {}; upload them in a folder",
                MAX_SHARE_ASSETS,
                assets.len()
            ));
        }

        let dest = self.api.folder(cx, &t.account_id, &t.folder_id).await.map_err(|e| e.to_string())?;
        let sh = self
            .api
            .create_share(cx, &t.account_id, &dest.project_id, t.title.trim_start_matches("Retry: "), &assets)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(j) = &j {
            *j.share_link.lock().unwrap() = sh.short_url.clone();
        }
        self.hist.set_share_link(id, &sh.short_url)?;
        self.hist.log(&format!(
            "{} share    [{}] {} — {} items",
            kind_tag(&t.kind),
            id,
            sh.short_url,
            assets.len()
        ));
        Ok(sh.short_url)
    }

    /// Lists the Frame.io IDs a transfer's share holds. A watch batch shares its
    /// files, since its folders also hold earlier batches. Otherwise each
    /// top-level folder is shared whole, found by walking up from the shallowest
    /// file inside it.
    async fn share_assets(
        &self,
        cx: &CancellationToken,
        acct: &str,
        kind: &str,
        watch: bool,
        files: &[history::File],
    ) -> Result<Vec<String>, String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // The shallowest file of each top-level folder, by index into `files`.
        let mut tops: HashMap<String, usize> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (i, f) in files.iter().enumerate() {
            match f.status.as_str() {
                "failed" | "canceled" | "skipped" | "interrupted" => continue,
                _ => {}
            }
            let id = if kind == "download" { f.asset_id.clone() } else { f.remote_id.clone() };
            let (top, nested) = match f.rel_path.split_once('/') {
                Some((t, _)) => (t.to_string(), true),
                None => (f.rel_path.clone(), false),
            };
            if watch || !nested {
                if id.is_empty() {
                    return Err(ERR_NOT_STARTED.to_string());
                }
                if seen.insert(id.clone()) {
                    out.push(id);
                }
                continue;
            }
            if f.remote_parent.is_empty() {
                continue;
            }
            let depth = f.rel_path.matches('/').count();
            match tops.get(&top) {
                None => {
                    order.push(top.clone());
                    tops.insert(top, i);
                }
                Some(cur) => {
                    if depth < files[*cur].rel_path.matches('/').count() {
                        tops.insert(top, i);
                    }
                }
            }
        }
        for top in order {
            let Some(idx) = tops.get(&top).copied() else { continue };
            let f = &files[idx];
            let mut id = f.remote_parent.clone();
            let mut depth = f.rel_path.matches('/').count();
            while depth > 1 {
                let a = self.api.folder(cx, acct, &id).await.map_err(|e| e.to_string())?;
                id = a.parent_id;
                depth -= 1;
            }
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        Ok(out)
    }
}
