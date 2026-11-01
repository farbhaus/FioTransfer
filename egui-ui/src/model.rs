//! What a screen needs in order to draw, in the terms the views speak.
//!
//! The data types are the core's, not copies: a `JobView`, a `FileView`, a
//! `Snapshot`, the `Settings` and the signed-in `User` are what
//! `fiotransfer_core::xfer` and `fiotransfer_core::config` serialise, and the
//! same structs the app's own methods answer with. There is one model now — a
//! mismatch between the two front ends' idea of a job is not possible.
//!
//! One exception, and it is a build one: `fiotransfer-core` cannot compile for
//! `wasm32` (tokio, rustls, rusqlite), so the browser capture gets the same five
//! shapes from [`crate::model_browser`] instead. The re-export below is the whole
//! of the difference; everything else in this file is one file for both targets.
//!
//! What stays here is what only a screen has an opinion about: which of the five
//! screens is showing, the toast stack, the composer's text, and [`Request`] —
//! what a screen asks the app to do.

use serde::Deserialize;

// Two sets of data shapes, one per target. The native build draws what the
// engine produces, so it uses the core's own structs — there is one model, not a
// copy that can drift. The browser capture (`src/wasm.rs`) has no engine to ask:
// `fiotransfer-core` is tokio, rustls and rusqlite underneath and does not build
// for `wasm32`, so it keeps the same five shapes in `model_browser.rs`, field for
// field.
#[cfg(not(target_arch = "wasm32"))]
pub use fiotransfer_core::api::User;
#[cfg(not(target_arch = "wasm32"))]
pub use fiotransfer_core::config::Settings;
#[cfg(not(target_arch = "wasm32"))]
pub use fiotransfer_core::xfer::{FileView, JobView, Snapshot, Status};

#[cfg(target_arch = "wasm32")]
pub use crate::model_browser::*;

/// Which of the five screens is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Transfers,
    Watch,
    History,
    Space,
    Settings,
}

impl View {
    pub fn label(&self) -> &'static str {
        match self {
            View::Transfers => "Transfers",
            View::Watch => "Watch Folders",
            View::History => "History",
            View::Space => "Space",
            View::Settings => "Settings",
        }
    }
    /// `View::ALL` is the sidebar's order, which is `index.html`'s: Space sits
    /// between History and Settings.
    pub const ALL: [View; 5] = [
        View::Transfers,
        View::Watch,
        View::History,
        View::Space,
        View::Settings,
    ];
}

// ---------- what a screen asks for ----------

/// One thing a screen asked the app to do this frame.
///
/// A view is a pure renderer over [`UiState`]: it never opens a panel, never
/// talks to Frame.io and never touches a file. It pushes a `Request` instead,
/// and the shell (`crate::shell`) hands it to the real `App` — the same call the
/// webview front end made through `window.go.main.App[...]`, named here in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// A file or folder panel; the answer comes back as new state.
    PickFiles,
    PickFolder,
    PickWatchFolder,
    PickDownloadDir,

    /// Upload these paths to the remembered destination (`StartUpload`).
    StartUpload(Vec<String>),
    /// What `InspectLink` says about a typed link; the answer fills
    /// `UiState::link_info`.
    InspectLink { link: String, passphrase: String },
    /// The composer's Download button (`StartDownload`).
    StartDownload { link: String, passphrase: String, dest_dir: String, subfolder: bool },
    Cancel(String),
    Retry(String),
    /// `ClearFinished`.
    ClearFinished,
    /// The job's public share link; the shell copies it and says so (`ShareLink`).
    ShareLink(String),
    /// Show a path in the file manager (`Reveal`).
    Reveal(String),

    /// Ask the history store again (`History(query, kind, 500)`).
    History { query: String, kind: String },
    /// Write the history to a CSV the user picks (`ExportHistory`).
    ExportCsv,
    /// Open the log folder (`OpenLogs`).
    OpenLogs,

    /// Paste the clipboard into the composer's link field (`ClipboardText`,
    /// then `InspectLink`).
    Paste,
    /// Open a URL in the browser (`OpenURL`).
    OpenUrl(String),
    /// Put text on the clipboard.
    CopyText(String),

    /// `SaveSettings` (and `SetUploadDestination` when the destination is what
    /// changed).
    SaveSettings,
    SetOpenAtLogin(bool),
    Login,
    Logout,
    /// `CheckForUpdate` / `InstallUpdate`.
    CheckUpdate,
    InstallUpdate,

    /// Ask for the watch list again (`Watches`).
    Watches,
    /// Watch `local`, moving files to or from the Frame.io folder `folder` in
    /// account `acct` (`AddWatch`). This is the editor's Save in create mode: the
    /// folder is one the user typed or chose, the two ids are pasted, and every
    /// field is the dialog's own — no settings are read behind its back.
    AddWatch {
        local: String,
        acct: String,
        folder: String,
        label: String,
        dir: String,
        on_change: String,
        include_existing: bool,
    },
    SetWatchEnabled { id: String, on: bool },
    RemoveWatch(String),
    /// Change a watch's folder, destination, direction and rule for changed
    /// files (`UpdateWatch`). The editor's Save in edit mode.
    UpdateWatch {
        id: String,
        local: String,
        acct: String,
        folder: String,
        label: String,
        dir: String,
        on_change: String,
        include_existing: bool,
    },
    RequeueCanceled(String),

    /// Open the destination picker (`openDestAt`). An upload opens it at the
    /// remembered destination, `at` one of its levels: `-1` the project list,
    /// `0` the project, `1…` its folders, `None` the folder itself. The watch
    /// editor opens it at the editor's folder.
    OpenDest { mode: DestMode, at: Option<i32> },
    /// List the picker's top level (`ListAccounts` … `ListFolders`). `skip` goes
    /// on into a level with a single row when it's an account or a workspace.
    DestLoad { skip: bool },
    /// Search the account for the picker's query (`Search`).
    DestSearch,
    /// Open the picker where a search hit is (`PathTo`).
    DestGoTo(DestNode),
    /// Make a folder in the picker's level and go into it (`CreateFolder`).
    DestNewFolder(String),
    /// The picker's "Upload here": remember the folder (`SetUploadDestination`),
    /// then upload what was waiting for it.
    DestChoose,

    /// Walk every account, workspace and project again (`RefreshSpace`).
    RefreshSpace,
    /// Delete one project on Frame.io (`DeleteProject(account, project)`).
    /// `name` is for the toast; the two ids are what the call takes.
    DeleteProject { account_id: String, project_id: String, name: String },
    /// List a folder of an opened project and add up its subfolders
    /// (`App::open_space_folder`). `parent` is `None` for the project's root.
    OpenSpaceFolder { account_id: String, folder_id: String, parent: Option<String> },
    /// Refresh inside a project: walk the folder again, then open it.
    RefreshSpaceFolder { account_id: String, folder_id: String, parent: Option<String> },
    /// Delete items of one folder on Frame.io (`App::delete_space_items`),
    /// exactly the ones the confirmation listed. `what` names them in the toast.
    DeleteSpaceItems {
        account_id: String,
        project_id: String,
        folder_id: String,
        items: Vec<SpaceTarget>,
        what: String,
    },

    /// Settings' Portal section: the server URL, already checked for its scheme
    /// by the row that commits it (`App::set_portal`).
    SetPortalUrl(String),
    /// The local folder Portal's markers are mirrored under
    /// (`Settings::portal_root`).
    SetPortalRoot(String),
    /// Store the bearer token in the keychain. The token is a [`Secret`], so no
    /// `{:?}` of this enum can print it, and it is never a setting.
    SetPortalToken(Secret),
    /// Forget the stored bearer token.
    ForgetPortalToken,
    /// Sign in to Portal itself with the admin's email and password
    /// (`App::portal_sign_in`). The password is a [`Secret`], so no `{:?}` of
    /// this enum can print it, and it is never a setting: the app posts it,
    /// keeps the cookie Portal answers with and hands the new status back.
    SignInPortal { email: String, password: Secret },
    /// The six digits Portal asked for, finishing the sign-in the password
    /// started (`App::portal_sign_in_totp`). A [`Secret`] like the password: no
    /// `{:?}` of this enum prints it, and it is never a setting.
    SignInPortalTotp { code: Secret },
    /// Forget the stored session cookie (`App::portal_sign_out`). A bearer
    /// token is a different credential under a different key, and stays.
    SignOutPortal,
    /// Turn Portal's background poller on or off (`Settings::portal_enabled`).
    SetPortalEnabled(bool),
    /// Reconcile the watch folders with Portal now (`App::portal_reconcile`).
    PortalReconcile,

    /// Read everything again (the first frame, and after an event).
    Refresh,
    /// The user closed the window (the shell asks before quitting).
    Quit,
}

// ---------- everything a screen draws from ----------

#[derive(Debug, Clone, Default)]
pub struct UiState {
    pub view: View,
    pub logged_in: bool,
    pub user: Option<User>,
    pub settings: Settings,
    pub snapshot: Snapshot,
    pub history: Vec<HistoryRow>,
    pub watches: Vec<WatchRow>,
    /// The watch folder editor, while it is open: the draft a pencil or Add
    /// opened, or `None` when no dialog is up. The screen owns it, so nothing
    /// the app reads on a tick can close a dialog under the user's hands.
    pub watch_editor: Option<WatchEditor>,
    /// The destination picker, while it's open (over any screen).
    pub dest_picker: Option<DestPicker>,
    /// What the last Frame.io storage reading found, one row per project.
    pub space: Vec<SpaceRow>,
    /// One row per account: how much it uses against its limit, for the Space
    /// screen's headline. Empty until `Space` has answered.
    pub space_accounts: Vec<SpaceAccount>,
    /// True while `RefreshSpace` is walking: the Space screen says so instead of
    /// pretending the figures are current.
    pub space_walking: bool,
    /// The project the Space screen is opened into, or `None` for the project
    /// list.
    pub space_open: Option<SpaceOpen>,
    /// The folder `space_open` shows, as the app's cache has it.
    pub space_folder: SpaceFolderView,
    pub toasts: Vec<Toast>,
    /// Frame.io destination the composer will upload to.
    pub destination: String,
    /// The running build's version, for Settings' "Version" row.
    pub version: String,
    /// Whether the app is set to start at login (Settings' checkbox).
    pub open_at_login: bool,
    /// Where the app keeps its log, for "App log" + "Show in Finder".
    pub log_dir: String,
    /// The user's home, so paths can be shown the way `shortPath` does (`~/Movies`).
    pub home: String,
    /// Update state, for the "Check now" row and the sidebar's update card.
    pub update: UpdateState,
    /// What the user has typed in the composer's link field.
    pub link: String,
    /// The Settings screen's Portal section: the last status, the fields being
    /// typed, and the reason a field's last commit was refused.
    pub portal: PortalUi,
    /// The composer's passphrase field, shown only when a link needs one.
    pub passphrase: String,
    /// "New folder" under the link field (`#subfolder`).
    pub new_folder: bool,
    /// The destination chosen for this download, overriding `settings.download_dir`.
    pub download_dir: String,
    /// What `InspectLink` said about the typed link: the preview line, the
    /// passphrase prompt and whether Download is live all come from it.
    pub link_info: Option<LinkInfo>,
    /// The crumbs of the remembered upload destination, outermost first.
    pub dest_path: Vec<Crumb>,
    /// The expanded job whose files are listed, and its files.
    pub expanded: Option<String>,
    pub files: Vec<FileView>,
    /// What the screens asked the app to do since the last frame; the shell
    /// drains it before drawing.
    pub requests: Vec<Request>,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryRow {
    pub title: String,
    pub kind: String,
    pub status: String,
    pub when: String,
    pub files: String,
    pub size: String,
}

/// One level of a Frame.io path: account, workspace, project or folder.
#[derive(Debug, Clone, Default)]
pub struct Crumb {
    pub kind: String,
    pub name: String,
}

// ---------- the destination picker ----------

/// A row of the destination picker: an account, workspace, project or folder
/// (the core's `Node`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DestNode {
    pub id: String,
    pub name: String,
    /// account | workspace | project | folder
    pub kind: String,
    /// Where a project or folder keeps its files (a project's root folder).
    pub folder_id: String,
    /// A search hit's project.
    pub where_: String,
    /// A search hit from anywhere in the account rather than a row of this level.
    pub remote: bool,
}

/// What the picker's Choose does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestMode {
    /// Remember the folder as the upload destination, then upload `pending`.
    Upload { pending: Vec<String> },
    /// Put the folder in the watch editor.
    Watch,
}

/// The destination picker (`#dest-modal`): a level at a time from the accounts
/// down, with a search over the level and the whole account.
#[derive(Debug, Clone)]
pub struct DestPicker {
    pub mode: DestMode,
    /// The levels opened: account, workspace, project, then folders.
    pub stack: Vec<DestNode>,
    /// The deepest path shown: the destination the picker opened at, or where
    /// the user came back up from. Past the open level, its crumbs stay on
    /// screen (dimmed) and lead back down.
    pub trail: Vec<DestNode>,
    /// The rows of the top level; `None` while they load.
    pub items: Option<Vec<DestNode>>,
    pub error: String,
    pub query: String,
    /// What the account-wide search found for `query`; `None` while it runs.
    pub hits: Option<Vec<DestNode>>,
    pub hits_error: String,
    /// When `query` last changed (egui time), so the search waits for a pause.
    pub typed_at: f64,
    /// The search for this `query` has been asked for.
    pub searched: bool,
    /// The keyboard's row, and whether the keyboard moved it.
    pub sel: usize,
    pub kbd: bool,
    /// The list brings `sel` into view on its next frame. Once: after that the
    /// wheel moves the list.
    pub scroll_sel: bool,
    /// The New folder name field, while it's open.
    pub new_folder: Option<String>,
    /// Bumped for each level (`seq`) and each search (`qseq`) asked for: only
    /// the newest answer lands.
    pub seq: u64,
    pub qseq: u64,
    /// The search field takes the keyboard when the picker opens.
    pub focus_query: bool,
}

impl DestPicker {
    pub fn new(mode: DestMode) -> DestPicker {
        DestPicker {
            mode,
            stack: Vec::new(),
            trail: Vec::new(),
            items: None,
            error: String::new(),
            query: String::new(),
            hits: None,
            hits_error: String::new(),
            typed_at: 0.0,
            searched: false,
            sel: 0,
            kbd: false,
            scroll_sel: false,
            new_folder: None,
            seq: 0,
            qseq: 0,
            focus_query: true,
        }
    }

    pub fn top(&self) -> Option<&DestNode> {
        self.stack.last()
    }

    /// The trail's levels below the open one, when the open level is on it.
    pub fn forward(&self) -> &[DestNode] {
        let n = self.stack.len();
        let on_trail = self.trail.len() > n && self.trail.iter().zip(&self.stack).all(|(a, b)| a.id == b.id);
        if on_trail { &self.trail[n..] } else { &[] }
    }

    /// Going up to `depth`: what was open stays as the trail back down, unless
    /// it's already on a deeper one.
    pub fn go_up(&mut self, depth: usize) {
        if self.forward().is_empty() {
            self.trail = self.stack.clone();
        }
        self.stack.truncate(depth);
    }

    /// A project or a folder is open: files can go here.
    pub fn in_folder(&self) -> bool {
        self.top().is_some_and(|t| t.kind == "project" || t.kind == "folder")
    }

    /// The destination's name: the project and the folders under it.
    pub fn label(&self) -> String {
        let from = self.stack.iter().position(|n| n.kind == "project").unwrap_or(self.stack.len());
        self.stack[from..].iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join(" / ")
    }

    /// The account a search looks in: the open one, else the remembered one.
    pub fn search_account(&self, settings: &Settings) -> String {
        match self.stack.iter().find(|n| n.kind == "account") {
            Some(a) => a.id.clone(),
            None => settings.last_account_id.clone(),
        }
    }

    /// Empties the search, dropping any answer still on its way.
    pub fn clear_query(&mut self) {
        self.query.clear();
        self.hits = None;
        self.hits_error.clear();
        self.searched = false;
        self.sel = 0;
        self.kbd = false;
        self.qseq += 1;
    }

    /// The rows the list shows, in order: this level's matches, then the
    /// account's hits that aren't among them (`D.rows`).
    pub fn rows(&self) -> (Vec<DestNode>, Vec<DestNode>) {
        let q = self.query.trim().to_lowercase();
        let here: Vec<DestNode> = self
            .items
            .iter()
            .flatten()
            .filter(|n| q.is_empty() || n.name.to_lowercase().contains(&q))
            .cloned()
            .collect();
        let there: Vec<DestNode> = match (&self.hits, q.is_empty()) {
            (Some(hits), false) => hits.iter().filter(|h| !here.iter().any(|n| n.id == h.id)).cloned().collect(),
            _ => Vec::new(),
        };
        (here, there)
    }
}

/// Which crumbs of a path fit in `width`, given each crumb's width and the
/// separator's and ellipsis's: all of them, else the first, an ellipsis
/// (`None`) and as many of the last as fit. The last crumb — the folder the
/// path ends at — is the one never dropped.
pub fn fit_crumbs(widths: &[f32], sep: f32, ellipsis: f32, width: f32) -> Vec<Option<usize>> {
    let n = widths.len();
    let total: f32 = widths.iter().sum::<f32>() + sep * n.saturating_sub(1) as f32;
    if total <= width || n <= 1 {
        return (0..n).map(Some).collect();
    }
    for tail in (1..n.saturating_sub(1)).rev() {
        let kept: f32 = widths[n - tail..].iter().sum();
        let w = widths[0] + ellipsis + kept + sep * (tail + 1) as f32;
        if w <= width {
            let mut out = vec![Some(0), None];
            out.extend((n - tail..n).map(Some));
            return out;
        }
    }
    vec![None, Some(n - 1)]
}

/// `-1` the project list, `0` the project, `1…` its folders, `None` all of it:
/// how much of a path the picker opens at (`openDestAt`).
pub fn cut_path(mut path: Vec<DestNode>, at: Option<i32>) -> Vec<DestNode> {
    let (Some(at), Some(project)) = (at, path.iter().position(|n| n.kind == "project")) else {
        return path;
    };
    let keep = (project as i32 + 1 + at).max(0) as usize;
    path.truncate(keep.min(path.len()));
    path
}

/// The `InspectLink` payload the composer's preview is built from.
#[derive(Debug, Clone, Default)]
pub struct LinkInfo {
    pub name: String,
    pub files: i64,
    pub total_bytes: i64,
    pub unavailable: i64,
    pub error: String,
    pub needs_passphrase: bool,
}

impl LinkInfo {
    /// The app's answer, as the composer wants it (`fiotransfer_core::xfer::LinkInfo`).
    ///
    /// Native only: the browser capture has no app to ask, so it has no such
    /// conversion and no `fiotransfer_core` to name.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_core(li: fiotransfer_core::xfer::LinkInfo) -> LinkInfo {
        LinkInfo {
            name: li.name,
            files: li.files,
            total_bytes: li.total_bytes,
            unavailable: li.unavailable,
            error: li.error,
            needs_passphrase: li.needs_passphrase,
        }
    }
}

/// What Settings and the update card need to know about a release.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    pub current: String,
    pub latest: String,
    pub available: bool,
    /// idle | checking | downloading | restarting
    pub state: String,
    pub progress: f32,
    pub error: String,
    /// Why this copy cannot update itself ("development build", …).
    pub blocker: String,
}

impl UpdateState {
    /// The app's `UpdateStatus`, as the views want it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_core(st: &fiotransfer_core::app::UpdateStatus) -> UpdateState {
        UpdateState {
            current: st.current.clone(),
            latest: st.latest.clone(),
            available: st.available,
            state: st.state.clone(),
            progress: st.progress as f32,
            error: st.error.clone(),
            blocker: st.blocker.clone(),
        }
    }
}

/// What the Settings screen's Portal section knows: the last status the app
/// gave (`App::portal_status`), the fields the user is typing in, and the reason
/// a commit was refused. Neither secret is here: the access token is only a
/// draft the field masks plus a [`Secret`] on its way to the keychain, and the
/// sign-in password is only a masked draft the shell empties, plus the [`Secret`]
/// one request carries.
#[derive(Debug, Clone, Default)]
pub struct PortalUi {
    pub status: PortalView,
    /// The URL being typed, or `None` while the field shows
    /// `settings.portal_url`. A refused value stays here with its reason under
    /// the field, so the stored setting is never the one that was rejected.
    pub url_draft: Option<String>,
    /// The local folder being typed, or `None` while the field shows
    /// `settings.portal_root`.
    pub root_draft: Option<String>,
    /// The token being typed. It never becomes a setting: Save hands it to the
    /// keychain and the shell empties this.
    pub token_draft: String,
    /// Why the last commit of the URL / the folder was refused.
    pub url_error: Option<String>,
    pub root_error: Option<String>,
    /// The admin email being typed into "Sign in instead". Empty means "show
    /// `settings.portal_email`", which is the address the last sign-in used.
    /// An email is not a secret, so this one is a plain draft and may stay
    /// after a failure.
    pub email_draft: String,
    /// The password being typed. Cleared by the shell the moment the sign-in
    /// answers, whichever way it went: nothing here keeps a password, not even
    /// a refused one.
    pub password_draft: String,
    /// Why Portal refused the last sign-in, in the app's own words.
    pub signin_error: Option<String>,
    /// The one-time code being typed, while Portal waits for one. Cleared by the
    /// shell the moment the code answers, whichever way it went: a code is not
    /// kept, not even a refused one.
    pub code_draft: String,
    /// Why Portal refused the last one-time code, in the app's own words — which
    /// say whether the code was wrong or the sign-in must start again.
    pub code_error: Option<String>,
    /// A sign-in is in flight: the button waits rather than posting twice.
    pub signing_in: bool,
}

/// A secret on its way out of a Settings field: the Portal bearer token (to the
/// keychain) or the password of a Portal sign-in (to `App::portal_sign_in` and
/// nowhere else).
///
/// `Debug` prints nothing of it: a secret must not be logged, and a request enum
/// that holds one would otherwise print it the first time somebody wrote
/// `{:?}` on a `Request`. The one place it is read is the shell's call.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(secret: impl Into<String>) -> Secret {
        Secret(secret.into())
    }

    /// The secret itself.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether there is a token to store at all.
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// What Portal's status line needs, as `App::portal_status` reports it.
///
/// A copy, not the core's `PortalStatus`: the browser capture has no core to ask
/// (the same reason [`UpdateState`] exists), and `now_ms` is added so the
/// relative time is a pure function of the status rather than of the clock at
/// draw time.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PortalView {
    /// The background poller is on (`Settings::portal_enabled`).
    pub enabled: bool,
    /// A credential is stored — a bearer token or a signed-in session. The
    /// credential itself is never part of a status.
    pub signed_in: bool,
    /// The admin the stored session belongs to. Empty for a bearer token, which
    /// names no account, and empty when nobody is signed in.
    pub email: String,
    /// A sign-in is half done: Portal took the password and is waiting for a
    /// six-digit code. Not a credential, and not `signed_in`.
    pub needs_code: bool,
    pub url: String,
    /// The local root, resolved to an absolute path.
    pub root: String,
    /// A reconcile is running right now.
    pub running: bool,
    /// When the last reconcile ran (unix ms); 0 means it has never run.
    pub last_at: i64,
    /// The last reconcile's numbers.
    pub links_seen: usize,
    pub ensured: usize,
    pub removed: usize,
    pub skipped: usize,
    /// Why the last reconcile did nothing, when it did nothing.
    pub error: String,
    /// When this reading was taken (unix ms), so "2 min ago" is decided by the
    /// shell that read the status rather than by the frame that draws it.
    pub now_ms: i64,
}

impl PortalView {
    /// A URL and a credential are both set: the state the reconcile button needs.
    /// A session counts as much as a bearer token.
    pub fn configured(&self) -> bool {
        !self.url.trim().is_empty() && self.signed_in
    }

    /// The app's `PortalStatus`, as the screen wants it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_core(st: &fiotransfer_core::portal::agent::PortalStatus) -> PortalView {
        PortalView {
            enabled: st.enabled,
            signed_in: st.signed_in,
            needs_code: st.needs_code,
            email: st.email.clone(),
            url: st.url.clone(),
            root: st.root.clone(),
            running: st.running,
            last_at: st.last_at,
            links_seen: st.links_seen,
            ensured: st.ensured,
            removed: st.removed,
            skipped: st.skipped,
            error: st.error.clone(),
            now_ms: 0,
        }
    }
}

/// One project's footprint on Frame.io, as `App::Space` reports it.
///
/// The field names are the payload's, minus `workspace_id`; `root_folder_id` is
/// where the screen opens the project. `bytes` is what Frame.io reported for the project, or what the walk
/// added up when it reported nothing; `walked` says which, and `files` is only
/// counted by a walk (0 means "not counted", drawn as a dash). `error` is set
/// when a walk could not finish (permissions, a folder deleted mid-walk).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SpaceRow {
    pub account_id: String,
    pub account_name: String,
    pub workspace_name: String,
    pub project_id: String,
    pub name: String,
    pub root_folder_id: String,
    pub bytes: i64,
    pub files: i64,
    /// True when `bytes` came from walking the project's folders, false when
    /// Frame.io reported it. The row says which.
    pub walked: bool,
    pub error: String,
}

/// The Space screen opened into a project: where it is, and what is ticked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceOpen {
    pub account_id: String,
    pub account_name: String,
    pub workspace_name: String,
    pub project_id: String,
    /// The folders opened, the project's root first (named after the project).
    pub trail: Vec<SpaceCrumb>,
    /// Ticked rows of the folder shown, by id. Cleared when the folder changes.
    pub selected: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceCrumb {
    pub id: String,
    pub name: String,
}

impl SpaceOpen {
    /// The folder shown: the last crumb.
    pub fn folder(&self) -> &SpaceCrumb {
        self.trail.last().expect("an opened project has its root in the trail")
    }

    /// The folder above the one shown, `None` at the project's root.
    pub fn parent(&self) -> Option<&SpaceCrumb> {
        self.trail.len().checked_sub(2).and_then(|i| self.trail.get(i))
    }

    pub fn at_root(&self) -> bool {
        self.trail.len() <= 1
    }

    /// Go into a subfolder of the one shown.
    pub fn enter(&mut self, id: &str, name: &str) {
        self.trail.push(SpaceCrumb { id: id.into(), name: name.into() });
        self.selected.clear();
    }

    /// Go back up to crumb `index` (0 is the project's root).
    pub fn cut(&mut self, index: usize) {
        self.trail.truncate(index + 1);
        self.selected.clear();
    }

    /// The request that lists and counts the folder shown.
    pub fn open_request(&self) -> Request {
        Request::OpenSpaceFolder {
            account_id: self.account_id.clone(),
            folder_id: self.folder().id.clone(),
            parent: self.parent().map(|p| p.id.clone()),
        }
    }
}

/// One folder of an opened project, as `App::space_folder` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceFolderView {
    pub items: Vec<SpaceItemRow>,
    pub listed: bool,
    pub counting: bool,
    pub error: String,
}

/// One thing in a folder: a subfolder, a file or a version stack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceItemRow {
    pub id: String,
    /// `folder`, `file` or `version_stack`.
    pub kind: String,
    pub name: String,
    pub bytes: i64,
    pub files: i64,
    /// False for a folder the walk has not added up yet.
    pub counted: bool,
    pub error: String,
    pub deleting: bool,
    /// Watch folders whose Frame.io folder is this one or inside it.
    pub watches: Vec<String>,
}

impl SpaceItemRow {
    pub fn is_folder(&self) -> bool {
        self.kind == "folder"
    }

    pub fn target(&self) -> SpaceTarget {
        SpaceTarget { id: self.id.clone(), kind: self.kind.clone(), name: self.name.clone() }
    }
}

/// One thing a delete takes, as the confirmation showed it: the core's
/// `SpaceTarget`, field for field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceTarget {
    pub id: String,
    pub kind: String,
    pub name: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<SpaceTarget> for fiotransfer_core::app::SpaceTarget {
    fn from(t: SpaceTarget) -> Self {
        fiotransfer_core::app::SpaceTarget { id: t.id, kind: t.kind, name: t.name }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<fiotransfer_core::app::SpaceFolder> for SpaceFolderView {
    fn from(f: fiotransfer_core::app::SpaceFolder) -> SpaceFolderView {
        SpaceFolderView {
            items: f
                .items
                .into_iter()
                .map(|i| SpaceItemRow {
                    id: i.id,
                    kind: i.kind,
                    name: i.name,
                    bytes: i.bytes,
                    files: i.files,
                    counted: i.counted,
                    error: i.error,
                    deleting: i.deleting,
                    watches: i.watches,
                })
                .collect(),
            listed: f.listed,
            counting: f.counting,
            error: f.error,
        }
    }
}

/// One account's storage, as `Account.storage_limit` and `Account.storage_usage`
/// report it. `limit: None` is Frame.io's `null`, which means "no limit".
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SpaceAccount {
    pub account_id: String,
    pub name: String,
    pub limit: Option<i64>,
    pub used: Option<i64>,
}

/// The `Space` payload: the project rows to chart and the per-account headline
/// above them. `App::Space` answers this object, not a bare list of projects.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SpacePayload {
    pub projects: Vec<SpaceRow>,
    pub accounts: Vec<SpaceAccount>,
}

impl SpaceRow {
    /// This project's part of `total`, 0.0–1.0. A total of zero is no share
    /// rather than a divide by zero.
    pub fn share(&self, total: i64) -> f32 {
        if total <= 0 || self.bytes <= 0 {
            0.0
        } else {
            (self.bytes as f64 / total as f64) as f32
        }
    }

    /// True when the last walk could not finish, so the figures are not whole.
    pub fn incomplete(&self) -> bool {
        !self.error.trim().is_empty()
    }
}

/// The core's `SpaceProject`, as this screen wants it: `workspace_id`, which the
/// screen never names, is dropped.
///
/// Native only: the browser capture is given its rows by `src/fixture.rs`.
#[cfg(not(target_arch = "wasm32"))]
impl From<fiotransfer_core::app::SpaceProject> for SpaceRow {
    fn from(p: fiotransfer_core::app::SpaceProject) -> SpaceRow {
        SpaceRow {
            account_id: p.account_id,
            account_name: p.account_name,
            workspace_name: p.workspace_name,
            project_id: p.project_id,
            name: p.name,
            root_folder_id: p.root_folder_id,
            bytes: p.bytes,
            files: p.files,
            walked: p.walked,
            error: p.error,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<fiotransfer_core::app::SpaceAccount> for SpaceAccount {
    fn from(a: fiotransfer_core::app::SpaceAccount) -> SpaceAccount {
        SpaceAccount {
            account_id: a.account_id,
            name: a.name,
            limit: a.limit,
            used: a.used,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<fiotransfer_core::app::SpacePayload> for SpacePayload {
    fn from(p: fiotransfer_core::app::SpacePayload) -> SpacePayload {
        SpacePayload {
            projects: p.projects.into_iter().map(SpaceRow::from).collect(),
            accounts: p.accounts.into_iter().map(SpaceAccount::from).collect(),
        }
    }
}

/// Which sync lane of a duplex one watch drives.
///
/// A Portal-managed project is drawn as one job — a **Smart In/Out Tray** — made
/// of up to two sync lanes. The inbound lane is the download watch on the
/// client's folder (its origin is `portal:<link id>`); the outbound lane is the
/// upload watch on the exports folder (origin `portal:<link id>#exports`). A
/// watch the user made belongs to no duplex — it keeps a row of its own — and
/// its lane is `None`.
///
/// The lane follows the watch record's `direction`, not the folder's name: a
/// project whose client folder is called anything at all still has its inbound
/// lane on the download watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lane {
    /// A watch the user made: no duplex, no lane.
    #[default]
    None,
    /// The download watch: the client's folder, fed from Frame.io.
    Inbound,
    /// The upload watch: the exports folder, sent to Frame.io.
    Outbound,
}

/// The prefix and suffix of a Portal-owned watch's `origin`, as
/// `fiotransfer_core::portal::agent` writes them. Spelled here because this file
/// also builds for `wasm32`, where the core (and so `portal::agent::link_of`) is
/// not compiled in; `the_origin_parser_agrees_with_the_portal_agent` holds the
/// two to the same answer on the native build.
const PORTAL_PREFIX: &str = "portal:";
const EXPORTS_SUFFIX: &str = "#exports";

/// A watch's duplex — the Portal link id — and the sync lane it drives, from the
/// record's `origin` and `direction`.
///
/// `portal:L1` is the inbound lane of duplex `L1`; `portal:L1#exports` is its
/// outbound lane. A watch the user made (`origin` empty) answers
/// `(None, Lane::None)`, and so does an origin this screen cannot read — an
/// unreadable origin leaves its row drawing exactly as it did before there were
/// lanes, rather than inventing a job.
pub fn lane_of(origin: &str, direction: &str) -> (Option<String>, Lane) {
    let Some(link) = origin.strip_prefix(PORTAL_PREFIX) else {
        return (None, Lane::None);
    };
    let (id, exports) = match link.strip_suffix(EXPORTS_SUFFIX) {
        Some(rest) => (rest, true),
        None => (link, false),
    };
    // Nothing before the suffix, or a `#` anywhere else: not an origin the
    // reconciler writes (`plan` refuses a link id that holds one), so no duplex.
    if id.is_empty() || id.contains('#') {
        return (None, Lane::None);
    }
    let lane = match direction {
        "download" => Lane::Inbound,
        "upload" => Lane::Outbound,
        // A direction with no lane of its own — a mirror Portal watch, or one
        // this build does not know — takes the side its origin names.
        _ if exports => Lane::Outbound,
        _ => Lane::Inbound,
    };
    (Some(id.to_string()), lane)
}

/// One row of the Watch Folders screen, from the app's `WatchView`.
#[derive(Debug, Clone, Default)]
pub struct WatchRow {
    /// The app's watch id (`SetWatchEnabled`, `RemoveWatch`, …).
    pub id: String,
    pub local: String,
    pub label: String,
    pub direction: String,
    pub enabled: bool,
    pub pending: i64,
    /// True when Portal owns this watch: its record's `origin` is
    /// `portal:<link id>`, so the Portal reconciler edits it, and recreates it
    /// when it is removed here. A watch the user made has an empty origin, and
    /// this is false.
    pub from_portal: bool,
    /// The duplex this watch is a lane of: the Portal link id, without the
    /// `#exports` suffix, so both lanes of one link answer the same id. `None`
    /// for a watch the user made, and for an origin this screen cannot read.
    pub duplex_id: Option<String>,
    /// Which sync lane of [`WatchRow::duplex_id`] this watch drives.
    pub lane: Lane,
    /// The Frame.io account the record names. The row draws the label, not the
    /// id; the editor opens with it, because `add_watch` and `update_watch` take
    /// it on every call.
    pub account_id: String,
    /// The Frame.io folder the record names, for the same reason.
    pub folder_id: String,
    /// `overwrite` or `keep`: what happens to a local file that changes after it
    /// has been uploaded. Read by the core on every call, so the editor shows it.
    pub on_change: String,
}

impl WatchRow {
    /// The app's `WatchView`, as the Watch Folders screen wants it.
    ///
    /// Native only, for the same reason as [`LinkInfo::from_core`]: the browser
    /// capture is given its rows by `src/fixture.rs`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_core(w: &fiotransfer_core::xfer::watch::WatchView) -> WatchRow {
        let (duplex_id, lane) = lane_of(&w.origin, &w.direction);
        WatchRow {
            id: w.id.clone(),
            local: w.local_path.clone(),
            label: if w.dest_label.is_empty() { w.name.clone() } else { w.dest_label.clone() },
            direction: w.direction.clone(),
            enabled: w.enabled,
            pending: w.pending,
            // The record's own `origin`, through the reconciler's own reading of
            // it: only `portal:<link id>` makes Portal the owner, and an empty
            // origin (a watch the user made) is not Portal's.
            from_portal: fiotransfer_core::portal::agent::link_of(&w.origin).is_some(),
            duplex_id,
            lane,
            account_id: w.account_id.clone(),
            folder_id: w.folder_id.clone(),
            on_change: w.on_change.clone(),
        }
    }
}

/// One block of the Watch Folders screen: a duplex and its lanes, or a single
/// watch the user made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchBlock {
    /// The duplex this block is, by link id; `None` for a watch the user made,
    /// which is a block of one and is drawn on its own row.
    pub duplex_id: Option<String>,
    /// Where the block's rows sit in [`UiState::watches`], inbound lane first and
    /// then outbound — the order the lanes are drawn in.
    pub rows: Vec<usize>,
}

/// The rows of the Watch Folders screen, grouped for drawing: one block per
/// duplex, one block per watch the user made.
///
/// A duplex's lanes are put together in one block wherever its first lane sat,
/// so a lane that appears before its sibling does not split the job and a record
/// order of `out, in` still draws inbound first.
///
/// The blocks are then ordered: the watches the user made first, in the order
/// the app lists them (a user's own folder keeps the place it had), then one
/// block per duplex, by label — the Frame.io destination the duplex names — with
/// a stable sort, so two duplexes that share a label keep the app's order and
/// the same list always draws the same way.
///
/// The indices point into the slice they were made from, so the screen addresses
/// its actions (`RemoveWatch`, `SetWatchEnabled`) by the row that owns them.
pub fn watch_blocks(rows: &[WatchRow]) -> Vec<WatchBlock> {
    let mut blocks: Vec<WatchBlock> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match &row.duplex_id {
            Some(id) => match blocks.iter_mut().find(|b| b.duplex_id.as_ref() == Some(id)) {
                Some(block) => block.rows.push(index),
                None => blocks.push(WatchBlock { duplex_id: Some(id.clone()), rows: vec![index] }),
            },
            None => blocks.push(WatchBlock { duplex_id: None, rows: vec![index] }),
        }
    }
    for block in &mut blocks {
        // Inbound before outbound, whatever order the records were created in.
        block.rows.sort_by_key(|&index| match rows[index].lane {
            Lane::Inbound => 0,
            Lane::Outbound => 1,
            Lane::None => 2,
        });
    }
    // A watch the user made keeps the place the app gave it — its own rows are
    // not reordered at all — and the jobs follow them in label order. `sort_by` is
    // stable, so two jobs that share a label keep the app's order too.
    let (mut own, mut jobs): (Vec<WatchBlock>, Vec<WatchBlock>) =
        blocks.into_iter().partition(|block| block.duplex_id.is_none());
    jobs.sort_by(|a, b| {
        rows[a.rows[0]].label.to_lowercase().cmp(&rows[b.rows[0]].label.to_lowercase())
    });
    own.extend(jobs);
    own
}

/// The same rows, in the order [`watch_blocks`] draws them.
///
/// The screen groups by `duplex_id` itself; this is for the shell, so the list
/// the app hands over is already the list that is drawn and a reader of
/// `UiState::watches` sees what is on screen. The rows move, nothing else: an
/// app watch id still addresses its own row.
pub fn in_block_order(rows: Vec<WatchRow>) -> Vec<WatchRow> {
    let blocks = watch_blocks(&rows);
    let mut rest: Vec<Option<WatchRow>> = rows.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(rest.len());
    for block in blocks {
        for index in block.rows {
            if let Some(row) = rest[index].take() {
                out.push(row);
            }
        }
    }
    // The grouping reaches every row; a row it somehow missed is listed after
    // the rest rather than dropped.
    out.extend(rest.into_iter().flatten());
    out
}

// ---------- the watch folder editor ----------

/// The three directions the core understands (`valid_direction` in
/// `fiotransfer_core::history::watch`), spelled as its records spell them.
pub const DIRECTION_UPLOAD: &str = "upload";
pub const DIRECTION_DOWNLOAD: &str = "download";
pub const DIRECTION_MIRROR: &str = "mirror";
/// The two rules for a local file that changes after it was uploaded
/// (`valid_on_change`).
pub const ON_CHANGE_OVERWRITE: &str = "overwrite";
pub const ON_CHANGE_KEEP: &str = "keep";

/// The watch folder editor's draft: one watch's fields, as they are being typed.
///
/// It lives on [`UiState`] rather than in egui's own store because the shell
/// fills one field of it: "Choose…" answers through the folder panel, which runs
/// on the blocking pool, and its answer is put back into [`WatchEditor::local`]
/// when it lands.
///
/// Nothing is written to the app while the dialog is open: [`WatchEditor::request`]
/// is the only way out that asks for anything, so closing the dialog — Cancel,
/// Escape or a click on the backdrop — leaves the watch exactly as it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchEditor {
    /// The watch this dialog edits, by the app's id; `None` while it creates one.
    pub id: Option<String>,
    /// The local folder the watch reads (upload, mirror) or writes (download).
    pub local: String,
    /// The Frame.io account id, pasted — there is no browser for it here.
    pub account_id: String,
    /// The Frame.io folder id, pasted.
    pub folder_id: String,
    /// The destination's name on screen. Empty means the folder's own name.
    pub label: String,
    /// [`DIRECTION_UPLOAD`], [`DIRECTION_DOWNLOAD`] or [`DIRECTION_MIRROR`].
    pub direction: String,
    /// [`ON_CHANGE_OVERWRITE`] or [`ON_CHANGE_KEEP`].
    pub on_change: String,
    /// The core's `include_existing`: files already in the folder are taken too.
    /// The core reads it on every call, and only a side that is new uses it.
    pub include_existing: bool,
    /// A watch Portal owns. The dialog shows its values and offers no Save: the
    /// next reconcile would overwrite whatever an edit wrote, so the sentence it
    /// carries points at Portal instead.
    pub read_only: bool,
}

impl WatchEditor {
    /// The dialog for an existing watch, opened with the values the row carries.
    pub fn editing(row: &WatchRow) -> WatchEditor {
        WatchEditor {
            id: Some(row.id.clone()),
            local: row.local.clone(),
            account_id: row.account_id.clone(),
            folder_id: row.folder_id.clone(),
            label: row.label.clone(),
            direction: if row.direction.is_empty() {
                DIRECTION_UPLOAD.to_string()
            } else {
                row.direction.clone()
            },
            on_change: if row.on_change.is_empty() {
                ON_CHANGE_OVERWRITE.to_string()
            } else {
                row.on_change.clone()
            },
            include_existing: false,
            read_only: row.from_portal,
        }
    }

    /// The dialog for a new watch. The destination starts at the one the app
    /// remembers (`Settings::last_*`), which is what the webview's Add flow
    /// watched with; the local folder is the one thing it cannot know.
    pub fn creating(settings: &Settings) -> WatchEditor {
        WatchEditor {
            id: None,
            local: String::new(),
            account_id: settings.last_account_id.clone(),
            folder_id: settings.last_folder_id.clone(),
            label: settings.last_folder_label.clone(),
            direction: DIRECTION_UPLOAD.to_string(),
            on_change: ON_CHANGE_OVERWRITE.to_string(),
            include_existing: false,
            read_only: false,
        }
    }

    /// Why Save is off, in one sentence, or `None` when every field is usable.
    ///
    /// A read-only dialog has no Save to hold back, so it has no reason either.
    /// The folder is checked for being an absolute path, not for existing: a
    /// view does not touch the disk, and the core's `add`/`update` answer
    /// `not a folder: …` for a path that is not there.
    pub fn reason(&self) -> Option<&'static str> {
        if self.read_only {
            return None;
        }
        let local = self.local.trim();
        if local.is_empty() {
            return Some("Choose the folder to watch first.");
        }
        if !local.starts_with('/') {
            return Some("The folder has to be an absolute path, starting at /.");
        }
        if self.account_id.trim().is_empty() || self.folder_id.trim().is_empty() {
            return Some("Choose the Frame.io folder.");
        }
        None
    }

    /// What Save asks the app for: `UpdateWatch` for a watch that exists,
    /// `AddWatch` for one that does not — the same seven values either way, with
    /// the ids trimmed and the label filled in if it was left empty.
    pub fn request(&self) -> Request {
        let label = self.label_or_folder();
        match &self.id {
            Some(id) => Request::UpdateWatch {
                id: id.clone(),
                local: self.local.trim().to_string(),
                acct: self.account_id.trim().to_string(),
                folder: self.folder_id.trim().to_string(),
                label,
                dir: self.direction.clone(),
                on_change: self.on_change.clone(),
                include_existing: self.include_existing,
            },
            None => Request::AddWatch {
                local: self.local.trim().to_string(),
                acct: self.account_id.trim().to_string(),
                folder: self.folder_id.trim().to_string(),
                label,
                dir: self.direction.clone(),
                on_change: self.on_change.clone(),
                include_existing: self.include_existing,
            },
        }
    }

    /// The label to save: what was typed, or the folder's own name when the
    /// field was left empty — the default the field's hint shows.
    pub fn label_or_folder(&self) -> String {
        let label = self.label.trim();
        if !label.is_empty() {
            return label.to_string();
        }
        let local = self.local.trim().trim_end_matches('/');
        match local.rsplit('/').next() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => local.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Ok,
    Err,
    Info,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub seconds: f32,
}

impl UiState {
    /// Take a `Space` payload: the rows the chart draws and the accounts the
    /// headline names. Both lists are replaced together, so a refresh that
    /// finds no projects does not leave last week's accounts on screen.
    pub fn apply_space(&mut self, payload: SpacePayload) {
        self.space = payload.projects;
        self.space_accounts = payload.accounts;
    }

    pub fn active_jobs(&self) -> usize {
        self.snapshot.jobs.iter().filter(|j| !j.terminal()).count()
    }
    pub fn push_toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push(Toast { text: text.into(), kind, seconds: 3.5 });
    }
    /// Asks the shell for something.
    pub fn ask(&mut self, request: Request) {
        self.requests.push(request);
    }
}

/// Bytes as the webview front end words them (`fmtBytes` in app.js).
pub fn fmt_bytes(n: i64) -> String {
    if n <= 0 {
        return "0 B".into();
    }
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 || v >= 100.0 {
        format!("{:.0} {}", v, units[i])
    } else if v >= 10.0 {
        format!("{:.1} {}", v, units[i])
    } else {
        format!("{:.2} {}", v, units[i])
    }
}

/// Seconds as the webview front end words them (`fmtDur` in app.js).
pub fn fmt_dur(sec: f64) -> String {
    let s = sec.round() as i64;
    let (h, m) = (s / 3600, s % 3600 / 60);
    if h > 0 {
        format!("{}h {:02}m", h, m)
    } else if m > 0 {
        format!("{}m {:02}s", m, s % 60)
    } else {
        format!("{}s", s)
    }
}

pub fn fmt_speed(bps: f64) -> String {
    if bps <= 0.0 {
        "—".to_string()
    } else {
        format!("{}/s", fmt_bytes(bps as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_from_the_app_parses() {
        let json = r#"{"jobs":[{"id":"a","kind":"upload","title":"clip.mov","status":"running",
            "total":100,"done":25,"speed":10.0,"files_done":1,"files_total":4}],
            "speed":10.0,"active_streams":3,"stream_limit":16}"#;
        let s: Snapshot = serde_json::from_str(json).expect("snapshot parses");
        assert_eq!(s.jobs.len(), 1);
        assert_eq!(s.jobs[0].progress(), Some(0.25));
        assert!(s.jobs[0].is_upload());
        assert!(!s.jobs[0].terminal());
        assert_eq!(s.stream_limit, 16);
    }

    #[test]
    fn byte_and_duration_words_match_the_webview_ui() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(999), "999 B");
        assert_eq!(fmt_bytes(1000), "1.00 KB");
        assert_eq!(fmt_bytes(1_500_000), "1.50 MB");
        assert_eq!(fmt_dur(65.0), "1m 05s");
        assert_eq!(fmt_speed(0.0), "—");
    }

    #[test]
    fn a_space_row_from_the_app_parses_and_knows_its_share() {
        // The payload `App::Space` returns, with the field the screen does not
        // need (`workspace_id`) still in it.
        let json = r#"{"account_id":"acct-1","account_name":"Farbhaus",
            "workspace_id":"ws-1","workspace_name":"Shows","project_id":"proj-1",
            "name":"Nightfall","root_folder_id":"root-1","bytes":750,"files":3,
            "error":""}"#;
        let row: SpaceRow = serde_json::from_str(json).expect("space row parses");
        assert_eq!(row.account_name, "Farbhaus");
        assert_eq!(row.workspace_name, "Shows");
        assert_eq!(row.project_id, "proj-1");
        assert_eq!(row.name, "Nightfall");
        assert_eq!(row.root_folder_id, "root-1");
        assert_eq!((row.bytes, row.files), (750, 3));
        assert!(!row.incomplete());
        assert_eq!(row.share(3000), 0.25);
        // A zero total is no share, not a divide by zero.
        assert_eq!(row.share(0), 0.0);
        assert_eq!(SpaceRow { bytes: 0, ..row.clone() }.share(3000), 0.0);

        let failed = SpaceRow {
            error: "403 forbidden".into(),
            ..row
        };
        assert!(failed.incomplete());
    }

    /// The `Space` payload is `{ projects, accounts }`, and the account figures
    /// are nullable: `storage_limit: null` is "no limit", not a limit of 0.
    #[test]
    fn the_space_payload_parses_projects_and_accounts() {
        let json = r#"{
            "projects":[
                {"account_id":"acct-1","project_id":"proj-1","name":"Nightfall",
                 "bytes":750,"files":3,"walked":false,"error":""}
            ],
            "accounts":[
                {"account_id":"acct-1","name":"Farbhaus","limit":2000,"used":750},
                {"account_id":"acct-2","name":"Archive","limit":null,"used":640},
                {"account_id":"acct-3","name":"Quiet","limit":500,"used":null}
            ]
        }"#;
        let payload: SpacePayload = serde_json::from_str(json).expect("space payload parses");
        assert_eq!(payload.projects.len(), 1);
        assert_eq!(payload.projects[0].name, "Nightfall");
        assert!(!payload.projects[0].walked, "a reported size was not walked");
        assert_eq!(payload.accounts.len(), 3);
        assert_eq!(payload.accounts[0].limit, Some(2000));
        assert_eq!(payload.accounts[1].limit, None, "null is no limit");
        assert_eq!(payload.accounts[1].used, Some(640));
        assert_eq!(payload.accounts[2].used, None, "a null usage is not reported");

        let mut state = UiState::default();
        state.apply_space(payload.clone());
        assert_eq!(state.space.len(), 1);
        assert_eq!(state.space_accounts.len(), 3);

        // A bare list is not this payload. serde reads a sequence into a struct
        // as positional fields, so an empty one is the empty payload rather than
        // an error — no rows, no accounts, nothing drawn.
        let bare: SpacePayload = serde_json::from_str("[]").expect("an empty sequence parses");
        assert!(bare.projects.is_empty() && bare.accounts.is_empty());
        // Both lists may be absent, and then both are empty.
        let empty: SpacePayload = serde_json::from_str("{}").expect("an empty payload parses");
        assert!(empty.projects.is_empty() && empty.accounts.is_empty());
    }

    /// The core's own payload converts into the rows the screen draws, minus the
    /// two fields the screen never names.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_cores_space_payload_becomes_the_screens_rows() {
        let core = fiotransfer_core::app::SpacePayload {
            projects: vec![fiotransfer_core::app::SpaceProject {
                account_id: "acct-1".into(),
                account_name: "Farbhaus".into(),
                workspace_id: "ws-1".into(),
                workspace_name: "Shows".into(),
                project_id: "proj-1".into(),
                name: "Nightfall".into(),
                root_folder_id: "root-1".into(),
                bytes: 750,
                files: 3,
                walked: true,
                error: String::new(),
            }],
            accounts: vec![fiotransfer_core::app::SpaceAccount {
                account_id: "acct-1".into(),
                name: "Farbhaus".into(),
                limit: None,
                used: Some(750),
            }],
        };
        let ours: SpacePayload = core.into();
        assert_eq!(ours.projects.len(), 1);
        assert_eq!(ours.projects[0].project_id, "proj-1");
        assert_eq!(ours.projects[0].bytes, 750);
        assert!(ours.projects[0].walked);
        assert_eq!(ours.accounts[0].limit, None);
        assert_eq!(ours.accounts[0].used, Some(750));
        // The two dropped fields have no home on this side.
        assert_eq!(ours.projects[0].workspace_name, "Shows");
        assert_eq!(ours.projects[0].name, "Nightfall");
        assert_eq!(ours.projects[0].files, 3);
    }

    // ---------- a duplex, its sync lanes, and where they sit on the screen ----------

    /// The record's `origin` says which duplex a watch is a lane of, and its
    /// `direction` says which lane. Both are read from the record: the folder's
    /// name plays no part.
    #[test]
    fn an_origin_and_a_direction_name_a_duplex_and_a_lane() {
        // The inbound lane: the drive watch, a download, `portal:<link id>`.
        assert_eq!(
            lane_of("portal:L1", "download"),
            (Some("L1".to_string()), Lane::Inbound)
        );
        // The outbound lane: the exports watch, an upload, `#exports` on the end.
        assert_eq!(
            lane_of("portal:L1#exports", "upload"),
            (Some("L1".to_string()), Lane::Outbound)
        );
        // Both lanes answer the same duplex, which is what groups them.
        assert_eq!(lane_of("portal:L1", "download").0, lane_of("portal:L1#exports", "upload").0);

        // The direction decides, not the suffix: a link whose drive watch pushes
        // (a mirror) still draws on the side its record says.
        assert_eq!(lane_of("portal:L1", "upload"), (Some("L1".to_string()), Lane::Outbound));
        assert_eq!(lane_of("portal:L1#exports", "download"), (Some("L1".to_string()), Lane::Inbound));
        // A direction with no lane of its own takes its origin's side.
        assert_eq!(lane_of("portal:L1", "mirror"), (Some("L1".to_string()), Lane::Inbound));
        assert_eq!(lane_of("portal:L1#exports", "mirror"), (Some("L1".to_string()), Lane::Outbound));
        assert_eq!(lane_of("portal:L1", ""), (Some("L1".to_string()), Lane::Inbound));

        // A watch the user made: no origin, so no duplex and no lane.
        assert_eq!(lane_of("", "upload"), (None, Lane::None));
        assert_eq!(lane_of("", "download"), (None, Lane::None));
        assert_eq!(lane_of("user", "upload"), (None, Lane::None));

        // Malformed: a prefix with nothing after it, a `#` that is not the
        // exports suffix, the suffix with nothing before it, and the prefix in
        // the wrong case. None of them invents a job.
        assert_eq!(lane_of("portal:", "download"), (None, Lane::None));
        assert_eq!(lane_of("portal:#exports", "upload"), (None, Lane::None));
        assert_eq!(lane_of("portal:L1#bogus", "download"), (None, Lane::None));
        assert_eq!(lane_of("portal:L1#exports#exports", "upload"), (None, Lane::None));
        assert_eq!(lane_of("Portal:L1", "download"), (None, Lane::None), "the prefix is exact");
        assert_eq!(lane_of(" portal:L1", "download"), (None, Lane::None));
    }

    /// The parser here and the reconciler's must read the same origin the same
    /// way, or a watch would be Portal's on one screen and nobody's on the next.
    /// The constants are spelled out in this file for the browser build, which has
    /// no core; this is what keeps the two spellings honest.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_origin_parser_agrees_with_the_portal_agent() {
        use fiotransfer_core::portal::agent::{
            EXPORTS_SUFFIX as CORE_SUFFIX, exports_origin_of, link_of, origin_of,
        };
        assert_eq!(EXPORTS_SUFFIX, CORE_SUFFIX, "the suffix this file spells is the core's");
        assert_eq!(PORTAL_PREFIX, "portal:", "and so is the prefix");
        assert_eq!(origin_of("L1").strip_prefix(PORTAL_PREFIX), link_of(&origin_of("L1")));
        assert_eq!(
            exports_origin_of("L1").strip_prefix(PORTAL_PREFIX),
            link_of(&exports_origin_of("L1"))
        );
        // `link_of` keeps the suffix on, which is what it is for: the origin is
        // what identifies a watch. This file strips it to find the duplex.
        assert_eq!(link_of(&exports_origin_of("L1")), Some("L1#exports"));
        assert_eq!(lane_of(&origin_of("L1"), "download").0, Some("L1".to_string()));
        assert_eq!(lane_of(&exports_origin_of("L1"), "upload").0, Some("L1".to_string()));
        assert_eq!(lane_of("", "download"), (link_of("").map(str::to_string), Lane::None));
    }

    /// Two lanes of one link are one block, in inbound-then-outbound order,
    /// wherever the records sat in the list; a watch the user made is a block of
    /// its own; and the blocks are drawn in label order.
    #[test]
    fn the_rows_group_by_duplex_and_sort_by_label() {
        fn portal(id: &str, label: &str, origin: &str, direction: &str) -> WatchRow {
            let (duplex_id, lane) = lane_of(origin, direction);
            WatchRow {
                id: id.to_string(),
                label: label.to_string(),
                direction: direction.to_string(),
                duplex_id,
                lane,
                ..WatchRow::default()
            }
        }
        fn user(id: &str, label: &str) -> WatchRow {
            WatchRow { id: id.to_string(), label: label.to_string(), ..WatchRow::default() }
        }

        // The exports watch first and the drive watch third: the two lanes are
        // still one block, and the lane order is not the record order.
        let rows = vec![
            portal("out", "LUC", "portal:LUC#exports", "upload"),
            user("mine", "Rushes 2026"),
            portal("in", "LUC", "portal:LUC", "download"),
            portal("b-out", "Archive", "portal:ARCH#exports", "upload"),
            portal("b-in", "Archive", "portal:ARCH", "download"),
        ];
        let blocks = watch_blocks(&rows);
        assert_eq!(blocks.len(), 3, "two duplexes and one watch of the user's: {blocks:?}");
        // The user's own folder first, where the app had it; then the two jobs
        // by label, Archive before LUC.
        assert_eq!(blocks[0].duplex_id, None);
        assert_eq!(blocks[0].rows, vec![1]);
        assert_eq!(blocks[1].duplex_id.as_deref(), Some("ARCH"));
        assert_eq!(blocks[1].rows, vec![4, 3], "inbound first, then outbound");
        assert_eq!(blocks[2].duplex_id.as_deref(), Some("LUC"));
        assert_eq!(blocks[2].rows, vec![2, 0]);

        // The same list grouped twice is the same list: nothing depends on the
        // order the records arrived in beyond the tie-break, which is stable.
        assert_eq!(watch_blocks(&rows), blocks);

        // A duplex with one lane is one block of one row.
        let one = vec![portal("in", "Solo", "portal:SOLO", "download")];
        assert_eq!(watch_blocks(&one), vec![WatchBlock { duplex_id: Some("SOLO".into()), rows: vec![0] }]);

        // Nothing to group is no blocks, not a block with no rows.
        assert!(watch_blocks(&[]).is_empty());
    }

    // ---------- the destination picker ----------

    fn node(kind: &str, name: &str) -> DestNode {
        DestNode { id: format!("{kind}:{name}"), name: name.into(), kind: kind.into(), folder_id: format!("f:{name}"), ..Default::default() }
    }

    fn path() -> Vec<DestNode> {
        vec![node("account", "Farbhaus"), node("workspace", "Main"), node("project", "Nightfall"), node("folder", "Ep 1"), node("folder", "Renders")]
    }

    #[test]
    fn the_picker_opens_at_the_level_clicked() {
        let names = |p: Vec<DestNode>| p.into_iter().map(|n| n.name).collect::<Vec<_>>();
        assert_eq!(names(cut_path(path(), None)).len(), 5, "the destination itself");
        assert_eq!(names(cut_path(path(), Some(-1))), ["Farbhaus", "Main"], "the project list");
        assert_eq!(names(cut_path(path(), Some(0))), ["Farbhaus", "Main", "Nightfall"]);
        assert_eq!(names(cut_path(path(), Some(1))), ["Farbhaus", "Main", "Nightfall", "Ep 1"]);
        assert_eq!(names(cut_path(path(), Some(9))).len(), 5, "never past the end");
        assert_eq!(cut_path(Vec::new(), Some(0)), Vec::new());
    }

    #[test]
    fn the_label_starts_at_the_project() {
        let mut p = DestPicker::new(DestMode::Watch);
        p.stack = path();
        assert_eq!(p.label(), "Nightfall / Ep 1 / Renders");
        assert!(p.in_folder());
        p.stack.truncate(2);
        assert_eq!(p.label(), "");
        assert!(!p.in_folder(), "a workspace holds projects, not files");
    }

    #[test]
    fn rows_filter_the_level_and_add_hits_from_elsewhere() {
        let mut p = DestPicker::new(DestMode::Watch);
        p.items = Some(vec![node("folder", "Renders"), node("folder", "Audio"), node("folder", "Rushes")]);
        let (here, there) = p.rows();
        assert_eq!((here.len(), there.len()), (3, 0));
        p.query = "r".into();
        p.hits = Some(vec![node("folder", "Rushes"), DestNode { remote: true, ..node("folder", "Reels") }]);
        let (here, there) = p.rows();
        assert_eq!(here.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["Renders", "Rushes"]);
        assert_eq!(there.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["Reels"], "a hit already listed here isn't repeated");
    }

    #[test]
    fn the_trail_leads_back_down_from_where_the_picker_opened() {
        let mut p = DestPicker::new(DestMode::Watch);
        p.trail = path();
        p.stack = cut_path(path(), Some(0));
        let names = |n: &[DestNode]| n.iter().map(|n| n.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(p.forward()), ["Ep 1", "Renders"], "the rest of the destination");
        p.stack.push(node("folder", "Sound"));
        assert!(p.forward().is_empty(), "off the trail");

        // Going up from somewhere else leaves a way back to it.
        p.go_up(3);
        assert_eq!(names(p.forward()), ["Sound"]);
        // Going further up along the same trail keeps it whole.
        p.go_up(2);
        assert_eq!(names(p.forward()), ["Nightfall", "Sound"]);
    }

    #[test]
    fn crumbs_that_dont_fit_lose_the_middle_never_the_end() {
        let w = [100.0, 80.0, 60.0, 40.0];
        assert_eq!(fit_crumbs(&w, 10.0, 15.0, 1000.0), [Some(0), Some(1), Some(2), Some(3)]);
        // 100 + 15 + 60 + 40 + 3 seps = 245
        assert_eq!(fit_crumbs(&w, 10.0, 15.0, 250.0), [Some(0), None, Some(2), Some(3)]);
        assert_eq!(fit_crumbs(&w, 10.0, 15.0, 180.0), [Some(0), None, Some(3)]);
        assert_eq!(fit_crumbs(&w, 10.0, 15.0, 50.0), [None, Some(3)]);
        assert_eq!(fit_crumbs(&[300.0], 10.0, 15.0, 50.0), [Some(0)]);
        assert!(fit_crumbs(&[], 10.0, 15.0, 50.0).is_empty());
    }

    #[test]
    fn a_search_looks_in_the_open_account_or_the_remembered_one() {
        let mut settings = Settings::default();
        settings.last_account_id = "acct-remembered".into();
        let mut p = DestPicker::new(DestMode::Watch);
        assert_eq!(p.search_account(&settings), "acct-remembered");
        p.stack = path();
        assert_eq!(p.search_account(&settings), "account:Farbhaus");
        let qseq = p.qseq;
        p.query = "abc".into();
        p.clear_query();
        assert!(p.query.is_empty() && p.qseq > qseq, "a cleared query drops the answer on its way");
    }

    // ---------- the watch folder editor ----------

    fn row_for_editor() -> WatchRow {
        let (duplex_id, lane) = lane_of("portal:L1", "download");
        WatchRow {
            id: "w-1".to_string(),
            local: "/Users/chaos/Live_Work/LUC/1_DRIVE".to_string(),
            label: "LUC".to_string(),
            direction: "download".to_string(),
            enabled: true,
            pending: 0,
            from_portal: true,
            duplex_id,
            lane,
            account_id: "acct-1".to_string(),
            folder_id: "fld-1".to_string(),
            on_change: "keep".to_string(),
        }
    }

    /// The editor opens on the values the row carries — every one of them, since
    /// a field it invented would be saved over the record.
    #[test]
    fn the_editor_opens_on_the_rows_values() {
        let editor = WatchEditor::editing(&row_for_editor());
        assert_eq!(editor.id.as_deref(), Some("w-1"));
        assert_eq!(editor.local, "/Users/chaos/Live_Work/LUC/1_DRIVE");
        assert_eq!(editor.account_id, "acct-1");
        assert_eq!(editor.folder_id, "fld-1");
        assert_eq!(editor.label, "LUC");
        assert_eq!(editor.direction, DIRECTION_DOWNLOAD);
        assert_eq!(editor.on_change, ON_CHANGE_KEEP);
        assert!(!editor.include_existing, "the checkbox starts off, as the webview's did");
        // A watch Portal owns is the read-only dialog: no edit here survives the
        // next reconcile.
        assert!(editor.read_only);
        assert!(editor.reason().is_none(), "a read-only dialog has no Save to hold back");
    }

    /// A watch the user made is editable, and a record with no direction or rule
    /// in it still opens on something the core accepts.
    #[test]
    fn an_empty_record_opens_on_the_defaults_the_core_accepts() {
        let mut row = row_for_editor();
        row.from_portal = false;
        row.direction = String::new();
        row.on_change = String::new();
        let editor = WatchEditor::editing(&row);
        assert!(!editor.read_only);
        assert_eq!(editor.direction, DIRECTION_UPLOAD);
        assert_eq!(editor.on_change, ON_CHANGE_OVERWRITE);
    }

    /// Save is off until the folder is an absolute path and both ids are there,
    /// and each refusal says which one it is.
    #[test]
    fn the_reason_a_save_is_off_names_the_field_to_fix() {
        let mut editor = WatchEditor { id: Some("w-1".to_string()), ..WatchEditor::default() };
        assert_eq!(editor.reason(), Some("Choose the folder to watch first."));
        editor.local = "Movies/Rushes".to_string();
        assert_eq!(
            editor.reason(),
            Some("The folder has to be an absolute path, starting at /.")
        );
        editor.local = "/Users/chaos/Movies/Rushes".to_string();
        assert_eq!(editor.reason(), Some("Choose the Frame.io folder."));
        editor.account_id = "acct-1".to_string();
        assert_eq!(editor.reason(), Some("Choose the Frame.io folder."));
        editor.folder_id = "fld-1".to_string();
        assert_eq!(editor.reason(), None);
        // The label is never a reason: empty means the folder's own name.
        assert_eq!(editor.label, "");
        assert_eq!(editor.label_or_folder(), "Rushes");
        editor.label = "  Rushes 2026 ".to_string();
        assert_eq!(editor.label_or_folder(), "Rushes 2026");
    }

    /// Save asks for the right call: an update for a watch that exists, an add
    /// for one that does not, with the ids trimmed and the label filled in.
    #[test]
    fn the_editor_asks_for_an_update_or_an_add_by_what_it_was_opened_on() {
        let editor = WatchEditor {
            local: " /Users/chaos/Movies/Rushes ".to_string(),
            account_id: " acct-1 ".to_string(),
            folder_id: "fld-1 ".to_string(),
            direction: DIRECTION_MIRROR.to_string(),
            on_change: ON_CHANGE_KEEP.to_string(),
            include_existing: true,
            ..WatchEditor::default()
        };
        // No id: a new watch.
        assert_eq!(
            editor.request(),
            Request::AddWatch {
                local: "/Users/chaos/Movies/Rushes".to_string(),
                acct: "acct-1".to_string(),
                folder: "fld-1".to_string(),
                label: "Rushes".to_string(),
                dir: DIRECTION_MIRROR.to_string(),
                on_change: ON_CHANGE_KEEP.to_string(),
                include_existing: true,
            }
        );
        // An id: the same seven values, for the watch it names.
        let editor = WatchEditor { id: Some("w-1".to_string()), ..editor };
        assert_eq!(
            editor.request(),
            Request::UpdateWatch {
                id: "w-1".to_string(),
                local: "/Users/chaos/Movies/Rushes".to_string(),
                acct: "acct-1".to_string(),
                folder: "fld-1".to_string(),
                label: "Rushes".to_string(),
                dir: DIRECTION_MIRROR.to_string(),
                on_change: ON_CHANGE_KEEP.to_string(),
                include_existing: true,
            }
        );
    }
}
