//! The Space screen inside a project: what each folder holds, and deleting some
//! or all of it.
//!
//! Frame.io sizes a project (`Project.storage`) but not a folder, so a folder's
//! figure is a walk: its children listed, then each subfolder walked in turn.
//! Every folder a walk lists is kept in [`FolderCache`] with its own children's
//! figures, so opening a folder the walk has been through asks Frame.io nothing,
//! and a delete takes its bytes off every folder above it without a new walk.
//!
//! The screen opens a folder ([`App::open_space_folder`]) and reads it back on
//! every frame ([`App::space_folder`]): the rows appear once the folder is
//! listed, and each subfolder's figure lands as its walk finishes. Opening
//! another folder supersedes the walk that was running; what it had counted
//! stays in the cache.
//!
//! Deleting ([`App::delete_space_items`]) touches Frame.io alone, like
//! `delete_project`: no local file is read, moved or removed. It deletes exactly
//! the items the screen showed when the delete was confirmed, by id, never "what
//! the folder holds now".

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{for_log, one_path_segment, order_space, App};
use crate::api::{ApiErr, Asset};

pub const KIND_FOLDER: &str = "folder";
pub const KIND_FILE: &str = "file";
pub const KIND_STACK: &str = "version_stack";

/// The same cap as the project walk: a cycle in Frame.io's data must not spin
/// forever.
const MAX_DEPTH: usize = 32;

/// One thing in a folder, as the Space screen lists it.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct SpaceItem {
    pub id: String,
    /// `folder`, `file` or `version_stack`, as Frame.io types it.
    pub kind: String,
    pub name: String,
    pub bytes: i64,
    pub files: i64,
    /// False for a folder the walk has not added up yet: its figures are 0 and
    /// mean nothing. Files and version stacks are counted when listed.
    pub counted: bool,
    /// Set when the walk could not finish this folder; `bytes` is what it had
    /// counted before it stopped.
    pub error: String,
    /// True while a delete of this item is on its way.
    pub deleting: bool,
    /// The watch folders whose Frame.io folder is this one or inside it, as far
    /// as the walk has seen. Deleting it stops them.
    pub watches: Vec<String>,
}

/// One folder, as [`App::space_folder`] hands it to the screen.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct SpaceFolder {
    pub items: Vec<SpaceItem>,
    /// True once the folder's children have been listed.
    pub listed: bool,
    /// True while a walk opened on this folder is adding up its subfolders.
    pub counting: bool,
    /// Why the folder could not be listed.
    pub error: String,
}

/// One thing to delete, as the screen showed it when the delete was confirmed.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SpaceTarget {
    pub id: String,
    pub kind: String,
    pub name: String,
}

/// What a delete of several items did: the names it deleted, and the ones it
/// could not with Frame.io's reason.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct SpaceDeleteReport {
    pub deleted: Vec<String>,
    pub failed: Vec<(String, String)>,
}

// ---------- the cache ----------

/// Every folder a walk has listed, by (account, folder id), with the parent it
/// was listed under so a delete can reach the folders above it.
#[derive(Debug, Default)]
pub struct FolderCache {
    entries: HashMap<(String, String), Listing>,
    failed: HashMap<(String, String), String>,
}

#[derive(Debug, Clone, Default)]
struct Listing {
    parent: Option<String>,
    items: Vec<SpaceItem>,
}

fn key(acct: &str, folder: &str) -> (String, String) {
    (acct.to_string(), folder.to_string())
}

impl FolderCache {
    pub fn listing(&self, acct: &str, folder: &str) -> Option<&[SpaceItem]> {
        self.entries.get(&key(acct, folder)).map(|l| l.items.as_slice())
    }

    pub fn error(&self, acct: &str, folder: &str) -> Option<&str> {
        self.failed.get(&key(acct, folder)).map(|e| e.as_str())
    }

    /// A fresh listing. A parent already known is kept when the caller does not
    /// know one (the screen opening a folder by id).
    pub fn put(&mut self, acct: &str, folder: &str, parent: Option<&str>, items: Vec<SpaceItem>) {
        let k = key(acct, folder);
        self.failed.remove(&k);
        let parent = parent
            .map(|p| p.to_string())
            .or_else(|| self.entries.get(&k).and_then(|l| l.parent.clone()));
        self.entries.insert(k, Listing { parent, items });
    }

    pub fn fail(&mut self, acct: &str, folder: &str, error: String) {
        self.failed.insert(key(acct, folder), error);
    }

    /// The subfolders of `folder` still to add up.
    pub fn uncounted(&self, acct: &str, folder: &str) -> Vec<String> {
        self.listing(acct, folder)
            .unwrap_or_default()
            .iter()
            .filter(|i| !i.counted)
            .map(|i| i.id.clone())
            .collect()
    }

    /// A folder's figures once every item in it is counted: bytes, files, and
    /// the first error any of them carries. `None` while anything is left.
    pub fn totals(&self, acct: &str, folder: &str) -> Option<(i64, i64, String)> {
        let items = self.listing(acct, folder)?;
        if items.iter().any(|i| !i.counted) {
            return None;
        }
        let bytes = items.iter().map(|i| i.bytes.max(0)).sum();
        let files = items.iter().map(|i| i.files.max(0)).sum();
        let error = items.iter().find(|i| !i.error.is_empty()).map(|i| i.error.clone()).unwrap_or_default();
        Some((bytes, files, error))
    }

    /// A subfolder's walk finished: its row in `folder` gets the figures.
    pub fn count(&mut self, acct: &str, folder: &str, item: &str, bytes: i64, files: i64, error: String) {
        if let Some(l) = self.entries.get_mut(&key(acct, folder)) {
            if let Some(i) = l.items.iter_mut().find(|i| i.id == item) {
                i.bytes = bytes;
                i.files = files;
                i.error = error;
                i.counted = true;
            }
        }
    }

    /// True when `inner` is `outer` or sits inside it, by the parents the walk
    /// recorded.
    pub fn within(&self, acct: &str, inner: &str, outer: &str) -> bool {
        let mut at = inner.to_string();
        for _ in 0..=MAX_DEPTH + 1 {
            if at == outer {
                return true;
            }
            match self.entries.get(&key(acct, &at)).and_then(|l| l.parent.clone()) {
                Some(p) => at = p,
                None => return false,
            }
        }
        false
    }

    /// The folders above `folder`, nearest first.
    fn ancestors(&self, acct: &str, folder: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut at = folder.to_string();
        while out.len() <= MAX_DEPTH {
            match self.entries.get(&key(acct, &at)).and_then(|l| l.parent.clone()) {
                Some(p) => {
                    out.push(p.clone());
                    at = p;
                }
                None => break,
            }
        }
        out
    }

    /// An item is gone from Frame.io: it leaves its folder's listing, its
    /// figures come off the rows of every folder above, and a folder's own
    /// listings go with it. Returns the item as it was listed.
    pub fn remove(&mut self, acct: &str, folder: &str, id: &str) -> Option<SpaceItem> {
        let listing = self.entries.get_mut(&key(acct, folder))?;
        let at = listing.items.iter().position(|i| i.id == id)?;
        let item = listing.items.remove(at);
        if item.counted {
            let mut child = folder.to_string();
            for up in self.ancestors(acct, folder) {
                if let Some(l) = self.entries.get_mut(&key(acct, &up)) {
                    if let Some(row) = l.items.iter_mut().find(|i| i.id == child) {
                        row.bytes = (row.bytes - item.bytes).max(0);
                        row.files = (row.files - item.files).max(0);
                    }
                }
                child = up;
            }
        }
        if item.kind == KIND_FOLDER {
            self.drop_subtree(acct, &item.id);
        }
        Some(item)
    }

    /// Refresh inside a folder: its listings go, and its row in every folder
    /// above is counted again (the walk reuses every sibling still cached).
    pub fn forget(&mut self, acct: &str, folder: &str) {
        let mut child = folder.to_string();
        for up in self.ancestors(acct, folder) {
            if let Some(l) = self.entries.get_mut(&key(acct, &up)) {
                if let Some(row) = l.items.iter_mut().find(|i| i.id == child) {
                    row.counted = false;
                    row.bytes = 0;
                    row.files = 0;
                    row.error.clear();
                }
            }
            child = up;
        }
        self.drop_subtree(acct, folder);
    }

    fn drop_subtree(&mut self, acct: &str, root: &str) {
        let doomed: Vec<(String, String)> = self
            .entries
            .keys()
            .filter(|(a, f)| a == acct && self.within(acct, f, root))
            .cloned()
            .collect();
        for k in doomed {
            self.entries.remove(&k);
            self.failed.remove(&k);
        }
        self.failed.remove(&key(acct, root));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.failed.clear();
    }
}

/// A folder's children as rows: files and version stacks carry their size at
/// once, folders wait for their walk.
pub fn items_of(kids: Vec<Asset>) -> Vec<SpaceItem> {
    kids.into_iter()
        .map(|k| {
            let mut item = SpaceItem { id: k.id, name: k.name, ..Default::default() };
            match k.type_.as_str() {
                KIND_FOLDER => item.kind = KIND_FOLDER.into(),
                KIND_STACK => {
                    item.kind = KIND_STACK.into();
                    item.counted = true;
                    if let Some(head) = &k.head_version {
                        item.bytes = head.file_size;
                        item.files = 1;
                    }
                }
                _ => {
                    item.kind = KIND_FILE.into();
                    item.counted = true;
                    item.bytes = k.file_size;
                    item.files = 1;
                }
            }
            item
        })
        .collect()
}

/// Why a walk stopped before its end.
enum Stop {
    /// The screen opened another folder.
    Superseded,
    Failed(String),
}

impl App {
    /// One folder as the screen draws it, from the cache: cheap, called on every
    /// frame. Biggest first once everything in it is counted; Frame.io's name
    /// order until then, so rows do not move under the pointer as sizes land.
    pub fn space_folder(&self, acct: &str, folder: &str) -> SpaceFolder {
        let watches = self.watches();
        let deleting = self.space_deleting.lock().unwrap().clone();
        let counting = matches!(
            &*self.space_counting.lock().unwrap(),
            Some((_, a, f)) if a == acct && f == folder
        );
        let cache = self.space_folders.lock().unwrap();
        let Some(items) = cache.listing(acct, folder) else {
            return SpaceFolder {
                counting,
                error: cache.error(acct, folder).unwrap_or_default().to_string(),
                ..Default::default()
            };
        };
        let mut items: Vec<SpaceItem> = items
            .iter()
            .map(|i| {
                let mut row = i.clone();
                row.deleting = deleting.contains(&key(acct, &i.id));
                if i.kind == KIND_FOLDER {
                    row.watches = watches
                        .iter()
                        .filter(|w| w.account_id == acct && cache.within(acct, &w.folder_id, &i.id))
                        .map(|w| if w.name.trim().is_empty() { w.local_path.clone() } else { w.name.clone() })
                        .collect();
                }
                row
            })
            .collect();
        if items.iter().all(|i| i.counted) {
            items.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        }
        SpaceFolder { items, listed: true, counting, error: String::new() }
    }

    /// Opens a folder: lists it unless the cache has it, then adds up each
    /// subfolder not counted yet. `parent` is the folder the screen opened it
    /// from (`None` for a project's root). Reads only.
    pub async fn open_space_folder(self: &Arc<Self>, acct: &str, folder: &str, parent: Option<&str>) {
        let seq = self.space_seq.fetch_add(1, Ordering::SeqCst) + 1;
        if !one_path_segment(acct) || !one_path_segment(folder) {
            self.space_folders.lock().unwrap().fail(acct, folder, "not a Frame.io folder id".into());
            self.state_changed();
            return;
        }
        if self.space_folders.lock().unwrap().totals(acct, folder).is_some() {
            // Counted already: nothing to walk, and no "Counting…" to flash.
            self.state_changed();
            return;
        }
        *self.space_counting.lock().unwrap() = Some((seq, acct.to_string(), folder.to_string()));
        self.state_changed();
        let cx = self.api_cx();
        let _ = self.count_tree(&cx, seq, acct, folder, parent, 0).await;
        {
            let mut counting = self.space_counting.lock().unwrap();
            if matches!(&*counting, Some((s, ..)) if *s == seq) {
                *counting = None;
            }
        }
        self.state_changed();
    }

    /// Refresh inside a project: the folder and everything under it are walked
    /// again the next time it is opened.
    pub fn forget_space_folder(&self, acct: &str, folder: &str) {
        self.space_folders.lock().unwrap().forget(acct, folder);
    }

    fn superseded(&self, seq: u64) -> bool {
        self.space_seq.load(Ordering::SeqCst) != seq
    }

    async fn count_tree(
        &self,
        cx: &CancellationToken,
        seq: u64,
        acct: &str,
        folder: &str,
        parent: Option<&str>,
        depth: usize,
    ) -> Result<(i64, i64, String), Stop> {
        if depth > MAX_DEPTH {
            return Ok((0, 0, String::new()));
        }
        if let Some(t) = self.space_folders.lock().unwrap().totals(acct, folder) {
            return Ok(t);
        }
        let listed = self.space_folders.lock().unwrap().listing(acct, folder).is_some();
        if !listed {
            if self.superseded(seq) {
                return Err(Stop::Superseded);
            }
            match self.api.children(cx, acct, folder).await {
                Ok(kids) => self.space_folders.lock().unwrap().put(acct, folder, parent, items_of(kids)),
                Err(e) => {
                    let e = e.to_string();
                    self.space_folders.lock().unwrap().fail(acct, folder, e.clone());
                    self.state_changed();
                    return Err(Stop::Failed(e));
                }
            }
            self.state_changed();
        }
        let todo = self.space_folders.lock().unwrap().uncounted(acct, folder);
        for id in todo {
            if self.superseded(seq) {
                return Err(Stop::Superseded);
            }
            let (bytes, files, error) = match Box::pin(self.count_tree(cx, seq, acct, &id, Some(folder), depth + 1)).await {
                Ok(t) => t,
                Err(Stop::Superseded) => return Err(Stop::Superseded),
                Err(Stop::Failed(e)) => (0, 0, e),
            };
            self.space_folders.lock().unwrap().count(acct, folder, &id, bytes, files, error);
            self.state_changed();
        }
        // A delete may have taken the folder while it was walked: nothing left.
        Ok(self.space_folders.lock().unwrap().totals(acct, folder).unwrap_or_default())
    }

    /// Deletes items of one folder on Frame.io: folders with everything in
    /// them, files, and version stacks (every version). Like `delete_project`,
    /// this touches Frame.io alone, and every id must be exactly one path
    /// segment or nothing is sent at all.
    ///
    /// A 404 means the item is already gone, so its row goes too. Each delete is
    /// written to the transfer log; the project's row loses what was deleted.
    pub async fn delete_space_items(
        &self,
        acct: &str,
        project: &str,
        folder: &str,
        items: &[SpaceTarget],
    ) -> Result<SpaceDeleteReport, String> {
        let bad_kind = items.iter().find(|t| ![KIND_FOLDER, KIND_FILE, KIND_STACK].contains(&t.kind.as_str()));
        let bad_id = !one_path_segment(acct)
            || !one_path_segment(folder)
            || items.iter().any(|t| !one_path_segment(&t.id));
        if bad_id || bad_kind.is_some() {
            let why = match bad_kind {
                Some(t) if !bad_id => format!("refused to delete {}: unknown kind {}", for_log(&t.id), for_log(&t.kind)),
                _ => format!(
                    "refused to delete in folder {} on account {}: an id must be exactly one path segment",
                    for_log(folder),
                    for_log(acct)
                ),
            };
            if let Some(h) = self.hist() {
                h.log(&format!("SPACE    {why} — nothing was sent, and nothing local was touched"));
            }
            return Err(why);
        }
        let cx = self.api_cx();
        {
            let mut deleting = self.space_deleting.lock().unwrap();
            for t in items {
                deleting.insert(key(acct, &t.id));
            }
        }
        self.state_changed();
        let mut report = SpaceDeleteReport::default();
        for t in items {
            let sent = match t.kind.as_str() {
                KIND_FOLDER => self.api.delete_folder(&cx, acct, &t.id).await,
                KIND_STACK => self.delete_stack(&cx, acct, &t.id).await,
                _ => self.api.delete_file(&cx, acct, &t.id).await,
            };
            let gone = match sent {
                Ok(()) => true,
                Err(e) if e.status() == Some(404) => true,
                Err(e) => {
                    if let Some(h) = self.hist() {
                        h.log(&format!(
                            "SPACE    could not delete {} {} ({}) on Frame.io: {} (nothing local was touched)",
                            t.kind,
                            for_log(&t.name),
                            for_log(&t.id),
                            e
                        ));
                    }
                    report.failed.push((t.name.clone(), e.to_string()));
                    false
                }
            };
            self.space_deleting.lock().unwrap().remove(&key(acct, &t.id));
            if gone {
                let removed = self.space_folders.lock().unwrap().remove(acct, folder, &t.id);
                if let Some(item) = removed.filter(|i| i.counted) {
                    self.take_off_project(acct, project, item.bytes, item.files);
                }
                if let Some(h) = self.hist() {
                    h.log(&format!(
                        "SPACE    deleted {} {} ({}) in project {} on Frame.io (nothing local was touched)",
                        t.kind,
                        for_log(&t.name),
                        for_log(&t.id),
                        for_log(project)
                    ));
                }
                report.deleted.push(t.name.clone());
            }
            self.state_changed();
        }
        Ok(report)
    }

    /// Frame.io has no delete for a version stack, so each version goes, which
    /// takes the stack with it.
    async fn delete_stack(&self, cx: &CancellationToken, acct: &str, stack: &str) -> Result<(), ApiErr> {
        let versions = self.api.version_stack_children(cx, acct, stack).await?;
        if let Some(v) = versions.iter().find(|v| !one_path_segment(&v.id)) {
            return Err(ApiErr::Other(format!("Frame.io listed a version with the id {}", for_log(&v.id))));
        }
        for v in versions {
            match self.api.delete_file(cx, acct, &v.id).await {
                Ok(()) => {}
                Err(e) if e.status() == Some(404) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// What was deleted inside a project comes off its row. A row Frame.io
    /// sized is Frame.io's figure minus this until the next Refresh reads it
    /// again; only a walked row has a file count to take from.
    fn take_off_project(&self, acct: &str, project: &str, bytes: i64, files: i64) {
        if let Some((_, payload)) = self.space.lock().unwrap().as_mut() {
            if let Some(row) = payload.projects.iter_mut().find(|p| p.account_id == acct && p.project_id == project) {
                row.bytes = (row.bytes - bytes).max(0);
                if row.walked {
                    row.files = (row.files - files).max(0);
                }
            }
            order_space(&mut payload.projects);
        }
    }
}

/// The ids of a delete still on its way, by (account, item id).
pub(super) type Deleting = HashSet<(String, String)>;

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str) -> SpaceItem {
        SpaceItem { id: id.into(), kind: KIND_FOLDER.into(), name: id.into(), ..Default::default() }
    }

    fn file(id: &str, bytes: i64) -> SpaceItem {
        SpaceItem { id: id.into(), kind: KIND_FILE.into(), name: id.into(), bytes, files: 1, counted: true, ..Default::default() }
    }

    /// root ─ jobs ─ a (file 100)
    ///      │      └ old ─ b (file 30)
    ///      └ c (file 5)
    fn walked() -> FolderCache {
        let mut c = FolderCache::default();
        c.put("acct", "root", None, vec![folder("jobs"), file("c", 5)]);
        c.put("acct", "jobs", Some("root"), vec![file("a", 100), folder("old")]);
        c.put("acct", "old", Some("jobs"), vec![file("b", 30)]);
        let (b, f, e) = c.totals("acct", "old").unwrap();
        c.count("acct", "jobs", "old", b, f, e);
        let (b, f, e) = c.totals("acct", "jobs").unwrap();
        c.count("acct", "root", "jobs", b, f, e);
        c
    }

    #[test]
    fn a_folder_has_totals_only_once_everything_in_it_is_counted() {
        let mut c = FolderCache::default();
        c.put("acct", "root", None, vec![folder("jobs"), file("c", 5)]);
        assert_eq!(c.totals("acct", "root"), None);
        assert_eq!(c.uncounted("acct", "root"), vec!["jobs".to_string()]);
        c.count("acct", "root", "jobs", 130, 2, String::new());
        assert_eq!(c.totals("acct", "root"), Some((135, 3, String::new())));
        assert_eq!(walked().totals("acct", "root"), Some((135, 3, String::new())));
    }

    #[test]
    fn an_error_below_reaches_the_folders_above() {
        let mut c = FolderCache::default();
        c.put("acct", "root", None, vec![folder("jobs")]);
        c.count("acct", "root", "jobs", 0, 0, "403 forbidden".into());
        assert_eq!(c.totals("acct", "root"), Some((0, 0, "403 forbidden".into())));
    }

    #[test]
    fn a_deleted_file_comes_off_every_folder_above_it() {
        let mut c = walked();
        let gone = c.remove("acct", "old", "b").expect("listed");
        assert_eq!(gone.bytes, 30);
        assert_eq!(c.listing("acct", "old").unwrap().len(), 0);
        let jobs = c.listing("acct", "root").unwrap().iter().find(|i| i.id == "jobs").unwrap().clone();
        assert_eq!((jobs.bytes, jobs.files), (100, 1));
        let old = c.listing("acct", "jobs").unwrap().iter().find(|i| i.id == "old").unwrap().clone();
        assert_eq!((old.bytes, old.files), (0, 0));
        assert_eq!(c.totals("acct", "root"), Some((105, 2, String::new())));
    }

    #[test]
    fn a_deleted_folder_takes_its_listings_with_it() {
        let mut c = walked();
        c.remove("acct", "root", "jobs").expect("listed");
        assert!(c.listing("acct", "jobs").is_none());
        assert!(c.listing("acct", "old").is_none(), "a folder inside it goes too");
        assert_eq!(c.totals("acct", "root"), Some((5, 1, String::new())));
    }

    #[test]
    fn removing_what_is_not_listed_changes_nothing() {
        let mut c = walked();
        assert!(c.remove("acct", "root", "nope").is_none());
        assert!(c.remove("other", "root", "jobs").is_none(), "another account's folder");
        assert_eq!(c.totals("acct", "root"), Some((135, 3, String::new())));
    }

    #[test]
    fn forgetting_a_folder_recounts_it_and_the_folders_above() {
        let mut c = walked();
        c.forget("acct", "old");
        assert!(c.listing("acct", "old").is_none());
        assert_eq!(c.uncounted("acct", "jobs"), vec!["old".to_string()]);
        assert_eq!(c.uncounted("acct", "root"), vec!["jobs".to_string()]);
        // The sibling stays: a recount of jobs asks only for old.
        assert!(c.listing("acct", "jobs").is_some());
    }

    #[test]
    fn within_follows_the_recorded_parents() {
        let c = walked();
        assert!(c.within("acct", "old", "root"));
        assert!(c.within("acct", "old", "jobs"));
        assert!(c.within("acct", "jobs", "jobs"));
        assert!(!c.within("acct", "jobs", "old"));
        assert!(!c.within("other", "old", "root"));
        assert!(!c.within("acct", "elsewhere", "root"));
    }

    #[test]
    fn a_parent_already_known_is_kept() {
        let mut c = walked();
        c.put("acct", "old", None, vec![file("b", 30)]);
        assert!(c.within("acct", "old", "root"));
    }

    #[test]
    fn listed_children_become_rows() {
        let kids: Vec<Asset> = serde_json::from_str(
            r#"[{"id":"f","name":"Jobs","type":"folder"},
                {"id":"m","name":"a.mov","type":"file","file_size":7},
                {"id":"s","name":"cut","type":"version_stack","head_version":{"id":"v","file_size":9}}]"#,
        )
        .unwrap();
        let rows = items_of(kids);
        assert_eq!(rows[0].kind, KIND_FOLDER);
        assert!(!rows[0].counted);
        assert_eq!((rows[1].kind.as_str(), rows[1].bytes, rows[1].counted), (KIND_FILE, 7, true));
        assert_eq!((rows[2].kind.as_str(), rows[2].bytes, rows[2].files), (KIND_STACK, 9, 1));
    }
}
