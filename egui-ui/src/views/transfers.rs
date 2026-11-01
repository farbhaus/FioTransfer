//! The Transfers screen, drawn the way `frontend/dist` draws it.
//!
//! One model, two front ends: this file reads [`UiState`] and nothing else, and
//! it speaks the webview front end's words — `style.css` for the tokens and the
//! `.jobs-grid` column maths, `app.js` for every status string and action label,
//! `index.html` for the header row and the empty state.
//!
//! The screen is drawn in the same order the DOM draws it: a 64 px head (title,
//! destination chip, entry buttons), then the job list — one 56 px row per job,
//! newest first, with an optional error line, an expandable file list, and the
//! empty state when there are no jobs.
//!
//! Two deliberate choices, both reported to the caller of this worker:
//!
//! * The colour tokens are mirrored in [`tokens`] instead of read from
//!   `crate::theme`. `theme.rs` is owned by another worker and is still a
//!   placeholder; mirroring the stylesheet keeps this screen correct either way.
//!   When `theme.rs` lands, `tokens` can be replaced by `theme` one constant at
//!   a time without touching a single drawing call.
//! * Text is painted with the painter (not `ui.label`) so that a row's single
//!   accessible node is the row itself, labelled with the job title. That is
//!   what a screen reader should read, and it makes the row clickable in tests.
//!
//! Nothing here talks to Frame.io. A click becomes one [`Action`]; [`report`]
//! turns it into one [`Request`] on `state.requests`, and the shell
//! (`crate::shell`) hands that request to the real `App` — the same call the
//! webview front end made through `window.go.main.App[...]`. The destination
//! chip opens the destination picker (`crate::views::dest`).

use std::sync::Arc;

use egui::{
    Align, Button, Checkbox, Color32, CornerRadius, FontId, Frame, Galley, Id, Label, Layout, Margin,
    Painter, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, TextWrapMode, Ui,
    UiBuilder, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::model::{
    DestMode, FileView, JobView, LinkInfo, Request, UiState, View, fit_crumbs, fmt_bytes, fmt_dur,
    fmt_speed,
};
use crate::model::Status;

// ---------- the design tokens ----------

/// Every colour, radius and size on this screen comes from [`crate::theme`],
/// which reads `frontend/dist/style.css` and fails its own tests if a value
/// drifts from the design. Nothing here re-invents a colour.
use crate::theme::{self, size, space};

// ---------- the measurements, from `style.css` ----------

/// `.page-head { height: 64px }`
const HEAD_H: f32 = size::HEAD_H;
/// `.list { margin-top: 20px }`
const LIST_GAP: f32 = space::HUGE;
/// `.list-head { height: 32px }`
const LIST_HEAD_H: f32 = size::LIST_HEAD_H;
/// `.row-main { min-height: 56px }`, `.row { padding: 0 12px }`
const ROW_H: f32 = size::ROW_H;
const ROW_PAD_X: f32 = space::ROW_X;
/// `.error-line`, `.row-detail { padding-left: 42px }`: the kind icon and its gap.
const ROW_INDENT: f32 = size::KIND_ICON + space::XL;
/// `.frow { height: 30px }`, `.files { max-height: 300px; border: 1px }`
const FILE_ROW_H: f32 = size::FILE_ROW_H;
const FILES_MAX_H: f32 = size::FILES_MAX_H;
/// `.frow` grid ends with a bottom border on all but the last row.
const FILES_BORDER: f32 = 2.0;
/// `.files` caps the rendered rows; the rest is one "+ N more files" line.
const FILES_SHOWN: usize = 400;

/// The type sizes, from `theme::font` (`body`, `.sub`, `.status`, `.list-head`).
const T_TITLE: f32 = theme::font::TITLE;
const T_SECTION: f32 = theme::font::SECTION;
const T_BODY: f32 = theme::font::BODY;
const T_NOTE: f32 = theme::font::NOTE;
const T_DETAIL: f32 = theme::font::DETAIL;
const T_CAPTION: f32 = theme::font::CAPTION;

// ---------- the column maths (`style.css`, `.jobs-grid`) ----------

/// `gap: 14px` on `.jobs-grid` and `.frow`.
pub const GRID_GAP: f32 = space::XXL;
const COL_SIZE: f32 = 124.0;
const COL_SPEED: f32 = 80.0;
const COL_STATUS: f32 = 120.0;
const COL_ACTIONS: f32 = 120.0;
/// `.progress` keeps a workable track: the CSS floor is `minmax(80px, 1fr)`.
const PROGRESS_MIN: f32 = 80.0;
/// The media queries fire on the viewport (1099 / 999); the list lives inside the
/// 224 px sidebar, so the same breakpoints are those widths minus the sidebar.
const DROP_SPEED_BELOW: f32 = 1099.0 - 224.0;
const DROP_SIZE_BELOW: f32 = 999.0 - 224.0;

/// One row's six columns, already resolved to pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Columns {
    pub name: f32,
    pub progress: f32,
    pub size: f32,
    pub speed: f32,
    pub status: f32,
    pub actions: f32,
    pub show_size: bool,
    pub show_speed: bool,
}

impl Columns {
    /// The widths of the visible columns, in drawing order.
    pub fn visible(&self) -> Vec<f32> {
        let mut v = vec![self.name, self.progress];
        if self.show_size {
            v.push(self.size);
        }
        if self.show_speed {
            v.push(self.speed);
        }
        v.push(self.status);
        v.push(self.actions);
        v
    }
    /// Total width taken, as a check that the grid fits exactly.
    pub fn total(&self) -> f32 {
        let v = self.visible();
        v.iter().sum::<f32>() + GRID_GAP * (v.len().saturating_sub(1)) as f32
    }
}

/// Resolve `.jobs-grid` for a row of `avail` pixels, honouring the same
/// "narrow windows drop the least important columns" rule as the stylesheet.
pub fn columns(avail: f32) -> Columns {
    let avail = avail.max(0.0);
    let show_speed = avail >= DROP_SPEED_BELOW;
    let show_size = avail >= DROP_SIZE_BELOW;
    let size = if show_size { COL_SIZE } else { 0.0 };
    let speed = if show_speed { COL_SPEED } else { 0.0 };
    let fixed = size + speed + COL_STATUS + COL_ACTIONS;
    // Four columns always show (name, progress, status, actions), so three gaps;
    // `Size` and `Speed` bring one gap each when the window is wide enough.
    let gaps = GRID_GAP * (3 + show_size as u8 as usize + show_speed as u8 as usize) as f32;
    let free = (avail - fixed - gaps).max(0.0);
    // `minmax(0, 1.6fr) minmax(80px, 1fr)`
    let mut progress = free / 2.6;
    if progress < PROGRESS_MIN {
        progress = PROGRESS_MIN.min(free);
    }
    let name = (free - progress).max(0.0);
    Columns { name, progress, size, speed, status: COL_STATUS, actions: COL_ACTIONS, show_size, show_speed }
}

/// `.frow`: `minmax(0, 1fr) 80px 110px 150px`, gap 14.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileColumns {
    pub name: f32,
    pub size: f32,
    pub track: f32,
    pub status: f32,
}

impl FileColumns {
    /// The widths in drawing order.
    pub fn visible(&self) -> Vec<f32> {
        vec![self.name, self.size, self.track, self.status]
    }
    /// Total width taken, gaps included.
    pub fn total(&self) -> f32 {
        self.visible().iter().sum::<f32>() + GRID_GAP * 3.0
    }
}

pub fn file_columns(avail: f32) -> FileColumns {
    let fixed = 80.0 + 110.0 + 150.0 + 3.0 * GRID_GAP;
    FileColumns {
        name: (avail.max(0.0) - fixed).max(0.0),
        size: 80.0,
        track: 110.0,
        status: 150.0,
    }
}

// ---------- the words (`app.js` and `model.rs`) ----------

/// `statusLabel()` in `app.js`.
pub fn status_label(status: &str, kind: &str) -> String {
    let s = match status {
        "queued" => "Queued",
        "resolving" => "Preparing",
        "running" => match kind {
            "upload" => "Uploading",
            "download" => "Downloading",
            _ => "In progress",
        },
        "done" => "Completed",
        "partial" => "Completed with errors",
        "failed" => "Failed",
        "canceled" => "Canceled",
        "interrupted" => "Interrupted",
        "skipped" => "Skipped",
        other => return other.to_string(),
    };
    s.to_string()
}

/// `shortLink()` in `app.js`: the scheme and a trailing slash are noise.
pub fn short_link(link: &str) -> String {
    let l = link.strip_prefix("https://").or_else(|| link.strip_prefix("http://")).unwrap_or(link);
    l.strip_suffix('/').unwrap_or(l).to_string()
}

/// `—` where the webview front end shows `—` (its `fmtDur(0)` is `—`).
fn dur_or_dash(secs: f64) -> String {
    if secs > 0.0 { fmt_dur(secs) } else { "—".to_string() }
}

/// The `.sub` line: where the transfer goes, plus the file count.
///
/// The webview front end shows `j.phase` here while a job is not terminal; it
/// shows the file count in the status cell instead. This build keeps the phase
/// in the sub line *and* appends the "N of M files" count the brief asks for, so
/// both front ends say the same things.
pub fn subtitle(job: &JobView) -> String {
    let base = if phase_is_showing(job) {
        job.phase.clone()
    } else if job.is_upload() {
        if job.dest.is_empty() { "To —".to_string() } else { format!("To {}", job.dest) }
    } else {
        format!("{} → {}", short_link(&job.source), job.dest)
    };
    if job.files_total > 0 {
        format!("{} · {}", base, file_count(job))
    } else {
        base
    }
}

/// The `N of M files` wording the brief asks for.
pub fn file_count(job: &JobView) -> String {
    if job.files_total == 1 {
        format!("{} of 1 file", job.files_done)
    } else {
        format!("{} of {} files", job.files_done, job.files_total)
    }
}

/// The status pill's text, exactly as `updateJob()` builds it.
pub fn status_text(job: &JobView) -> String {
    let terminal = job.terminal();
    let preparing = matches!(job.status, Status::Resolving | Status::Queued);
    let mut text = status_label(job.status.as_str(), &job.kind);
    if !terminal && job.eta > 0.0 && !preparing {
        text = format!("{} left", fmt_dur(job.eta));
    }
    if !terminal && job.kind == "upload" && job.total > 0 && job.done >= job.total {
        text = "Verifying".to_string();
    }
    if job.status == Status::Partial {
        text = format!("{} of {} failed", job.files_failed, job.files_total);
    }
    if job.status == Status::Done {
        let files = if job.files_total == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", job.files_total)
        };
        text = format!("{} · {}", files, dur_or_dash(job.elapsed));
    }
    text
}

/// The size cell: `done / total` while it runs, `total` when it is over.
pub fn size_text(job: &JobView) -> String {
    if job.total <= 0 {
        return "—".to_string();
    }
    if job.terminal() {
        fmt_bytes(job.total)
    } else {
        format!("{} / {}", fmt_bytes(job.done), fmt_bytes(job.total))
    }
}

/// The speed cell: the live rate while running, the average once it is over.
pub fn speed_text(job: &JobView) -> String {
    if job.terminal() { fmt_speed(job.avg_speed) } else { fmt_speed(job.speed) }
}

/// The `pct` next to the bar; empty while the size is unknown, as in `app.js`.
pub fn percent_text(job: &JobView) -> String {
    let preparing = matches!(job.status, Status::Resolving | Status::Queued);
    if preparing {
        return String::new();
    }
    match job.progress() {
        Some(p) => format!("{}%", (p * 100.0).floor() as i64),
        None => {
            if job.status == Status::Done { "100%".to_string() } else { String::new() }
        }
    }
}

/// The bar's fraction: `None` when the size is not known yet.
pub fn bar_fraction(job: &JobView) -> Option<f32> {
    match job.progress() {
        Some(p) => Some(p),
        None if job.status == Status::Done => Some(1.0),
        None => None,
    }
}

/// The pill's dot colour, from `.status.<status> .dot`.
pub fn status_dot_color(status: &str) -> Color32 {
    theme::status_color(status)
}

/// The halo a running dot carries (`box-shadow: 0 0 0 3px var(--brand-soft)`).
pub fn status_halo(status: &str) -> Option<Color32> {
    theme::status_halo(status)
}

/// The pill's text colour: `.status { color: var(--text-2) }` with
/// `.status.running { color: var(--text) }`.
pub fn status_text_color(status: &str) -> Color32 {
    theme::status_text(status)
}

/// The bar's fill, from `.row.<status> .track i`.
pub fn track_fill(status: &str) -> Color32 {
    theme::progress_color(status)
}

/// The track colour while a job's size is unknown: `--raised` with no fill.
pub fn track_fill_fraction(job: &JobView) -> Option<f32> {
    if job.status == Status::Resolving { None } else { bar_fraction(job) }
}

/// One file row's status word (`fileRows()` in `app.js`).
pub fn file_status_text(file: &FileView, kind: &str) -> String {
    let mut text = match file.status.as_str() {
        "failed" => {
            if file.error.is_empty() { "Failed".to_string() } else { file.error.clone() }
        }
        "skipped" => {
            if file.note.is_empty() { "Skipped".to_string() } else { file.note.clone() }
        }
        "done" | "running" if !file.note.is_empty() => file.note.clone(),
        other => status_label(other, kind),
    };
    let terminal = file.status.terminal();
    if file.retries > 0 && !terminal {
        let word = if file.retries == 1 { "retry" } else { "retries" };
        text = format!("{} · {} {}", text, file.retries, word);
    }
    text
}

/// `.frow .fst` colour.
pub fn file_status_color(status: &str) -> Color32 {
    theme::file_status_text(status)
}

/// `.frow .track i` colour.
pub fn file_track_fill(status: &str) -> Color32 {
    match status {
        "failed" => theme::ERROR,
        "done" | "skipped" => theme::TEXT_4,
        _ => theme::BRAND,
    }
}

/// A file row's own fraction.
pub fn file_fraction(file: &FileView) -> f32 {
    if file.size <= 0 {
        return if file.status == Status::Done { 1.0 } else { 0.0 };
    }
    (file.done as f32 / file.size as f32).clamp(0.0, 1.0)
}

/// `fileRows()` orders by state first (running, failed, queued, canceled, done,
/// skipped) and keeps the host's order inside each state.
pub fn file_rank(status: &str) -> u8 {
    match status {
        "running" => 0,
        "failed" => 1,
        "queued" => 2,
        "canceled" => 3,
        "done" | "skipped" => 4,
        _ => 5,
    }
}

/// The files a job shows, in the webview front end's order.
pub fn ordered_files(files: &[FileView]) -> Vec<&FileView> {
    let mut v: Vec<&FileView> = files.iter().collect();
    v.sort_by_key(|f| file_rank(f.status.as_str())); // stable: keeps the host's order
    v
}

/// The jobs the list shows, newest first.
pub fn ordered_jobs(jobs: &[JobView]) -> Vec<&JobView> {
    let mut v: Vec<&JobView> = jobs.iter().collect();
    v.sort_by_key(|j| std::cmp::Reverse(j.started_at)); // stable: ties keep the host's order
    v
}

/// A job's title, falling back the way `updateJob()` does.
pub fn job_title(job: &JobView) -> String {
    if !job.title.is_empty() {
        return job.title.clone();
    }
    if job.is_upload() { "Upload".to_string() } else { "Download".to_string() }
}

/// Which row actions a job shows. `(cancel, retry, copy_link, reveal, files)`,
/// matching the `hidden` toggles in `updateJob()`.
pub fn actions_for(job: &JobView) -> Actions {
    let terminal = job.terminal();
    Actions {
        cancel: !terminal,
        retry: terminal && job.status != Status::Done,
        copy_link: !job.link.is_empty(),
        open: !job.link.is_empty(),
        reveal: job.kind == "download" && !job.dest.is_empty(),
        files: job.files_total > 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actions {
    pub cancel: bool,
    pub retry: bool,
    pub copy_link: bool,
    /// `.a-open`, "Open in Frame.io": a finished transfer has a share page.
    pub open: bool,
    pub reveal: bool,
    pub files: bool,
}

/// What a click on the screen asks for. The screen reports one per frame, and
/// [`report`] turns it into the [`Request`] (or the view state) it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show or hide this job's files.
    Toggle(String),
    /// `Cancel` on the row (`call('Cancel', id)`).
    Cancel(String),
    /// `Retry` on the row.
    Retry(String),
    /// `Copy share link`: the app fetches the job's public link, copies it and
    /// says so (`copyShare(id)` → `ShareLink`).
    Share(String),
    /// `Show in Finder` (`.a-reveal`): the job's destination, found in the
    /// snapshot by this id.
    Reveal(String),
    /// Open a Frame.io page in the browser (`.a-open`, "Open in Frame.io").
    Open(String),
    /// The composer's Download button: start the download of the typed link.
    StartDownload { link: String, passphrase: String, subfolder: bool },
    /// `.head-actions`' "Clear finished".
    ClearFinished,
    /// `.comp-body`'s "Files…" (`PickFiles`): a panel, then the upload starts.
    PickFiles,
    /// `.comp-body`'s "Folder…" (`PickFolder`).
    PickFolder,
    /// The link field's `Paste` (`ClipboardText`, then `InspectLink`).
    Paste,
    /// The download folder button (`PickDownloadDir`).
    PickDownloadDir,
    /// `.dest-pick`: open the destination picker at the level clicked — `-1` the
    /// project glyph (the project list), `0…` a crumb, `None` the rest of it.
    ChooseDestination(Option<i32>),
    /// `#cta-signin` in the composer (`login()`).
    SignIn,
    /// `#cta-setup`: the webview shows Settings, where the client id is typed.
    SetUp,
}

/// `j.phase && !terminal` in `updateJob()`: a phase is only news while it runs.
pub fn phase_is_showing(job: &JobView) -> bool {
    !job.phase.is_empty() && !job.terminal()
}

// ---------- the screen ----------

/// Draw the Transfers screen into `ui`.
///
/// The shell already provides `.view { padding: 0 28px 32px }` (see
/// `theme::main_frame`) and the scroll container, so this draws the three blocks
/// `index.html` draws — `.page-head`, `.composer`, `.list` — and nothing else.
pub fn show(ui: &mut Ui, state: &mut UiState) {
    let mut composer = Composer::load(ui, state);
    ui.scope_builder(UiBuilder::new().layout(Layout::top_down(Align::Min)), |ui| {
        // Rows are separated by their own 1 px border, not by egui's item spacing.
        ui.spacing_mut().item_spacing.y = 0.0;
        head(ui, state);
        if let Some(action) = composer_block(ui, state, &mut composer) {
            report(state, action);
        }
        if composer.wants_paste {
            composer.wants_paste = false;
            report(state, Action::Paste);
        }
        ui.add_space(LIST_GAP);
        list(ui, state);
    });
    composer.store(ui, state);
}

/// What a typed link, a passphrase and the "New folder" box hold.
///
/// The buffers live in egui's own memory — egui's text widgets edit a `&mut
/// String`, and a `UiState` field cannot be split-borrowed across a frame — but
/// `UiState::link`, `UiState::passphrase` and `UiState::new_folder` are the
/// record of them: [`Composer::store`] writes the buffers back every frame, and
/// [`Composer::load`] adopts the state when this window has no buffer yet or when
/// the shell wrote it (that is how `Paste` fills the field). The shell reads what
/// the user typed from `UiState`, never from here.
#[derive(Debug, Clone, Default)]
pub struct Composer {
    /// The text of `#link`.
    pub link: String,
    /// The text of `#passphrase`, shown only when a link needs one.
    pub passphrase: String,
    /// `#subfolder`: download into a folder named after the share.
    pub new_folder: bool,
    /// `#passphrase` is `hidden` until `InspectLink` says `needs_passphrase`.
    pub needs_passphrase: bool,
    /// The one-line summary `.link-preview` shows once a link is understood.
    pub preview: String,
    /// The preview is a warning (`--notice`) rather than an error (`--error-text`).
    pub preview_warn: bool,
    /// The preview is an error (`--error-text`).
    pub preview_error: bool,
    /// `Paste` was pressed: the screen has no clipboard to read, so it asks the
    /// shell for it (`Request::Paste`) and the ask is made once, then cleared.
    pub wants_paste: bool,
    /// The link this window last gave to `UiState`: a state link that differs
    /// from it came from the shell, and is newer than the buffer.
    pub seen_link: String,
}

impl Composer {
    fn id() -> Id {
        Id::new("fiotransfer-transfers-composer")
    }
    fn load(ui: &Ui, state: &UiState) -> Self {
        let stored = ui.data(|d| d.get_temp::<Composer>(Self::id()));
        let first_frame = stored.is_none();
        let mut composer = stored.unwrap_or_default();
        if first_frame {
            // No buffer in this window yet: `UiState` is the record of it.
            composer.adopt_link_and_fields(state);
        } else if composer.seen_link != state.link {
            // The shell wrote the link (Paste): it is newer than the buffer.
            composer.link = state.link.clone();
            composer.seen_link = state.link.clone();
        }
        composer.adopt_link_info(state);
        composer
    }
    fn store(&self, ui: &Ui, state: &mut UiState) {
        state.link = self.link.clone();
        state.passphrase = self.passphrase.clone();
        state.new_folder = self.new_folder;
        let mut saved = self.clone();
        saved.seen_link = self.link.clone();
        ui.data_mut(|d| d.insert_temp(Self::id(), saved));
    }
    fn adopt_link_and_fields(&mut self, state: &UiState) {
        self.link = state.link.clone();
        self.passphrase = state.passphrase.clone();
        self.new_folder = state.new_folder;
        self.seen_link = state.link.clone();
    }
    /// The preview line, the passphrase field and Download's liveness all come
    /// from what `InspectLink` said (`ui.link_info`), when the shell has it.
    fn adopt_link_info(&mut self, state: &UiState) {
        match &state.link_info {
            Some(info) if info.needs_passphrase => {
                // The passphrase is the news, and the app's reason with it.
                self.needs_passphrase = true;
                self.preview = info.error.clone();
                self.preview_warn = true;
                self.preview_error = false;
            }
            Some(info) if !info.error.is_empty() => {
                self.needs_passphrase = false;
                self.preview = info.error.clone();
                self.preview_warn = false;
                self.preview_error = true;
            }
            Some(info) => {
                self.needs_passphrase = false;
                self.preview = preview_line(info);
                self.preview_warn = info.unavailable > 0;
                self.preview_error = false;
            }
            None => {
                self.needs_passphrase = false;
                self.preview.clear();
                self.preview_warn = false;
                self.preview_error = false;
            }
        }
    }
    /// `updateDownloadBtn()`: a download needs a link that inspection did not
    /// reject — a rejected link that wants a passphrase is still downloadable.
    fn can_download(&self) -> bool {
        !self.link.trim().is_empty() && !(self.preview_error && !self.needs_passphrase)
    }
}

/// `setPreview()`'s good case, as one line: name, file count, size, and any
/// files the app could not offer.
fn preview_line(info: &LinkInfo) -> String {
    let name = if info.name.is_empty() { "Shared files".to_string() } else { info.name.clone() };
    let files =
        if info.files == 1 { "1 file".to_string() } else { format!("{} files", info.files) };
    let mut line = format!("{} · {} · {}", name, files, fmt_bytes(info.total_bytes));
    if info.unavailable > 0 {
        line = format!("{} · {} unavailable", line, info.unavailable);
    }
    line
}

// ---------- `.page-head` ----------

/// `.page-head`: the title, and `.head-actions` pinned right with "Clear finished".
fn head(ui: &mut Ui, state: &mut UiState) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, HEAD_H), Sense::hover());
    let painter = ui.painter().clone();

    painter.text(
        pos2(rect.left(), rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Transfers",
        FontId::proportional(T_TITLE),
        theme::TEXT,
    );

    // `$('#clear-btn').classList.toggle('hidden', !jobs.some(terminal))`
    if state.snapshot.jobs.iter().any(|job| job.terminal()) {
        let label = "Clear finished";
        let w = text_width(ui, label, T_BODY) + 24.0;
        let button_rect = Rect::from_center_size(
            pos2(rect.right() - w / 2.0, rect.center().y),
            vec2(w, size::CONTROL_H),
        );
        if subtle_button(ui, button_rect, label).clicked() {
            report(state, Action::ClearFinished);
        }
    }
    // A `put` at an absolute rect rewinds the cursor to that rect, so the head
    // states its own height at the end.
    ui.advance_cursor_after_rect(rect);
}

/// `.btn` (and `.btn.primary`): 32 px tall, 12 px padding, 6 px radius, with the
/// stylesheet's hover fill (`.btn:hover` goes `--raised`, `.btn.primary:hover`
/// goes `--brand-hover`) and its disabled opacity of 0.4 (`.btn:disabled`).
fn button(ui: &mut Ui, rect: Rect, label: &str, primary: bool, enabled: bool) -> egui::Response {
    let hovered = enabled && ui.rect_contains_pointer(rect);
    let (fill, stroke, color) = if primary {
        let fill = if hovered { theme::BRAND_HOVER } else { theme::BRAND };
        (fill, Stroke::NONE, theme::WHITE)
    } else {
        let fill = if hovered { theme::RAISED } else { theme::CARD };
        (fill, theme::stroke_border_2(), theme::TEXT_BODY)
    };
    let dim = if enabled { 1.0 } else { 0.4 };
    ui.scope(|ui| {
        tight(ui);
        ui.put(
            rect,
            Button::new(RichText::new(label).size(T_BODY).color(color.gamma_multiply(dim)))
                .fill(fill.gamma_multiply(dim))
                .stroke(stroke)
                .corner_radius(theme::R_S)
                .min_size(rect.size())
                .wrap_mode(TextWrapMode::Truncate),
        )
    })
    .inner
}

/// A widget I size myself takes exactly the rect it is given: the shell's style
/// sets `button_padding: [12, 8]` and `interact_size: [60, 32]`, which would
/// otherwise grow every `put` past its rect.
fn tight(ui: &mut Ui) {
    ui.spacing_mut().button_padding = Vec2::ZERO;
    ui.spacing_mut().interact_size = Vec2::ZERO;
}

/// `.btn.subtle`: no fill, no border, `--text-2`, a `--hover` wash under the pointer.
fn subtle_button(ui: &mut Ui, rect: Rect, label: &str) -> egui::Response {
    let hovered = ui.rect_contains_pointer(rect);
    let fill = if hovered { theme::HOVER } else { Color32::TRANSPARENT };
    let color = if hovered { theme::TEXT } else { theme::TEXT_2 };
    ui.scope(|ui| {
        tight(ui);
        ui.put(
            rect,
            Button::new(RichText::new(label).size(T_BODY).color(color))
                .fill(fill)
                .stroke(Stroke::NONE)
                .corner_radius(theme::R_S)
                .min_size(rect.size())
                .wrap_mode(TextWrapMode::Truncate),
        )
    })
    .inner
}

// ---------- `.composer` ----------

/// `.composer`: a `--panel` card with an Upload row, a Download row and a foot.
fn composer_block(ui: &mut Ui, state: &mut UiState, composer: &mut Composer) -> Option<Action> {
    let frame = Frame::new()
        .fill(theme::PANEL)
        .stroke(theme::stroke_border())
        .corner_radius(theme::R_L)
        .inner_margin(Margin::ZERO);
    let inner = frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.set_width(ui.available_width());
        let mut action = comp_row(ui, "Upload", |ui| upload_body(ui, state));
        row_rule(ui);
        if let Some(next) = comp_row(ui, "Download", |ui| download_body(ui, state, composer)) {
            action = Some(next);
        }
        if let Some(next) = comp_foot(ui, composer) {
            action = Some(next);
        }
        action
    });
    inner.inner
}

/// `.comp-row`: a 72 px `--text-3` label, then the body, 12 px of vertical padding.
fn comp_row(ui: &mut Ui, label: &str, body: impl FnOnce(&mut Ui) -> Option<Action>) -> Option<Action> {
    Frame::new()
        .inner_margin(Margin {
            left: space::XXL as i8,
            right: space::XXL as i8,
            top: space::XL as i8,
            bottom: space::XL as i8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), size::CONTROL_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = space::XXXL; // `.comp-row { gap: 16px }`
                    let (label_rect, _) =
                        ui.allocate_exact_size(vec2(size::COMP_LABEL_W, size::CONTROL_H), Sense::hover());
                    let painter = ui.painter().clone();
                    line_left(
                        ui,
                        &painter,
                        label,
                        T_DETAIL,
                        theme::TEXT_3,
                        label_rect.left(),
                        label_rect.center().y,
                        label_rect.width(),
                    );
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), size::CONTROL_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = space::M; // `.comp-body { gap: 8px }`
                            body(ui)
                        },
                    )
                    .inner
                },
            )
            .inner
        })
        .inner
}

/// `.comp-row + .comp-row { border-top: 1px solid var(--border) }`.
fn row_rule(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::BORDER);
}

/// The Upload row's body: `.dest-pick`, "Files…", "Folder…" — or the sign-in CTA.
///
/// Laid out from both edges with explicit rects: the picker takes what is left,
/// so rounding can never push a button past the card's edge.
fn upload_body(ui: &mut Ui, state: &mut UiState) -> Option<Action> {
    let left = ui.cursor().min;
    let right = left.x + ui.available_width();
    let height = size::CONTROL_H;

    if !state.logged_in {
        let set_up = !state.settings.client_id.is_empty();
        let (notice, label, primary, ask) = if set_up {
            ("Not signed in", "Sign in", true, Action::SignIn)
        } else {
            // `$('#cta-setup').onclick = () => showView('settings')`: the client
            // id is typed on the Settings screen, so that is where this goes.
            ("Not set up", "Set up", false, Action::SetUp)
        };
        let w = text_width(ui, label, T_BODY) + 24.0;
        let btn = Rect::from_min_size(pos2(right - w, left.y), vec2(w, height));
        let notice_w = (btn.left() - space::M - left.x).max(0.0);
        let painter = ui.painter().clone();
        line_left(ui, &painter, notice, T_NOTE, theme::TEXT_2, left.x, left.y + height / 2.0, notice_w);
        let mut action = None;
        if button(ui, btn, label, primary, true).clicked() {
            action = Some(ask);
        }
        ui.advance_cursor_after_rect(Rect::from_min_max(left, pos2(right, left.y + height)));
        return action;
    }

    let files = "Files…";
    let folder = "Folder…";
    let files_w = text_width(ui, files, T_BODY) + 24.0;
    let folder_w = text_width(ui, folder, T_BODY) + 24.0;
    let folder_rect = Rect::from_min_size(pos2(right - folder_w, left.y), vec2(folder_w, height));
    let files_rect = Rect::from_min_size(
        pos2(folder_rect.left() - space::M - files_w, left.y),
        vec2(files_w, height),
    );
    let pick_rect = Rect::from_min_max(
        left,
        pos2((files_rect.left() - space::M).max(left.x + 120.0), left.y + height),
    );

    let mut action = dest_pick(ui, pick_rect, state);
    if button(ui, files_rect, files, false, true).clicked() {
        action = Some(Action::PickFiles);
    }
    if button(ui, folder_rect, folder, false, true).clicked() {
        action = Some(Action::PickFolder);
    }
    ui.advance_cursor_after_rect(Rect::from_min_max(left, pos2(right, left.y + height)));
    action
}

/// `.dest-pick`: the project glyph, the destination's crumbs, a chevron.
fn dest_pick(ui: &mut Ui, rect: Rect, state: &UiState) -> Option<Action> {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, theme::R_S, theme::SURFACE);
    painter.rect_stroke(rect, theme::R_S, theme::stroke_border_2(), StrokeKind::Inside);

    let icon = Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(16.0, 16.0));
    draw_project(&painter, icon, theme::TEXT_3);

    let chevron = Rect::from_center_size(pos2(rect.right() - 13.0, rect.center().y), vec2(10.0, 10.0));
    draw_chevron(&painter, chevron, theme::TEXT_3, false);

    // `.dest-pick .path`: segments, `/` separators, the last one brightest.
    let area = Rect::from_min_max(
        pos2(icon.right() + space::M, rect.top()),
        pos2(chevron.left() - space::S, rect.bottom()),
    );
    let crumbs: Vec<&str> = state
        .destination
        .split(" / ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    // Where each crumb is, so a click opens the picker at that level.
    let mut hit: Vec<(Rect, i32)> = Vec::new();
    if !crumbs.is_empty() {
        hit.push((icon.expand(4.0), -1));
    }
    if crumbs.is_empty() {
        line_left(
            ui,
            &painter,
            "Choose destination",
            T_NOTE,
            theme::TEXT_4,
            area.left(),
            rect.center().y,
            area.width(),
        );
    } else {
        let painter = painter.with_clip_rect(area.intersect(painter.clip_rect()));
        let last = crumbs.len() - 1;
        let (sep, dots) = (" / ", "…");
        let (sep_w, dots_w) = (text_width(ui, sep, T_NOTE), text_width(ui, dots, T_NOTE));
        let widths: Vec<f32> = crumbs.iter().map(|crumb| text_width(ui, crumb, T_NOTE)).collect();
        // Too long for the chip: the middle goes, so the folder it ends at shows.
        let shown = fit_crumbs(&widths, sep_w, dots_w, area.width());
        let mut x = area.left();
        for (k, item) in shown.iter().enumerate() {
            if x >= area.right() {
                break;
            }
            if k > 0 {
                line_left(ui, &painter, sep, T_NOTE, theme::TEXT_4, x, rect.center().y, area.right() - x);
                x += sep_w;
            }
            let Some(index) = *item else {
                line_left(ui, &painter, dots, T_NOTE, theme::TEXT_4, x, rect.center().y, area.right() - x);
                x += dots_w;
                continue;
            };
            let color = if index == last { theme::TEXT } else { theme::TEXT_2 };
            let w = widths[index];
            line_left(ui, &painter, crumbs[index], T_NOTE, color, x, rect.center().y, area.right() - x);
            hit.push((Rect::from_min_max(pos2(x, rect.top()), pos2((x + w).min(area.right()), rect.bottom())), index as i32));
            x += w;
        }
    }

    let title = if crumbs.is_empty() {
        "Choose destination".to_string()
    } else {
        crumbs.join(" / ")
    };
    let resp = ui.interact(rect, ui.make_persistent_id("transfers-dest-pick"), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &title));
    let resp = resp.on_hover_text(title);
    if resp.clicked() {
        let at = resp
            .interact_pointer_pos()
            .and_then(|p| hit.iter().find(|(r, _)| r.contains(p)).map(|(_, level)| *level));
        return Some(Action::ChooseDestination(at));
    }
    None
}

/// The Download row's body: the link field with Paste, the folder button, Download.
fn download_body(ui: &mut Ui, state: &UiState, composer: &mut Composer) -> Option<Action> {
    let left = ui.cursor().min;
    let right = left.x + ui.available_width();
    let height = size::CONTROL_H;

    let download = "Download";
    let download_w = text_width(ui, download, T_BODY) + 24.0;
    let dir_label = download_dir_label(state);
    let dir_w = (text_width(ui, &dir_label, T_BODY) + 16.0 + 7.0 + 24.0).min(220.0);
    let pass_w: f32 = 170.0;

    let mut x = right;
    x -= download_w;
    let dl_rect = Rect::from_min_size(pos2(x, left.y), vec2(download_w, height));
    x -= space::M + dir_w;
    let dir_rect = Rect::from_min_size(pos2(x, left.y), vec2(dir_w, height));
    let pass_rect = if composer.needs_passphrase {
        x -= space::M + pass_w.min((x - space::M - left.x - 160.0).max(0.0));
        Some(Rect::from_min_size(pos2(x, left.y), vec2(pass_w, height)))
    } else {
        None
    };
    let field_rect = Rect::from_min_max(
        left,
        pos2((x - space::M).max(left.x + 160.0), left.y + height),
    );

    let mut action = None;
    let field = link_field(ui, field_rect, composer);
    if field.changed() {
        // Typing a new link clears the stale preview (`#link`'s `input` handler).
        composer.preview.clear();
        composer.preview_warn = false;
    }

    if let Some(rect) = pass_rect {
        ui.scope(|ui| {
            input_style(ui);
            ui.put(
                rect,
                TextEdit::singleline(&mut composer.passphrase)
                    .hint_text("Passphrase")
                    .password(true)
                    .font(FontId::proportional(T_BODY))
                    .margin(Margin { left: 10, right: 10, top: 6, bottom: 6 })
                    .desired_width(rect.width() - 2.0),
            );
        });
    }

    // `.btn` with a folder glyph and the download folder's own name (`#dl-dest`).
    if dir_button(ui, dir_rect, &dir_label, state).clicked() {
        action = Some(Action::PickDownloadDir);
    }

    let enabled = composer.can_download();
    if button(ui, dl_rect, download, true, enabled).clicked() && enabled {
        // `startDownload()`: the app starts the transfer, the fields empty.
        let link = composer.link.trim().to_string();
        let passphrase = composer.passphrase.clone();
        let subfolder = composer.new_folder;
        composer.link.clear();
        composer.passphrase.clear();
        composer.needs_passphrase = false;
        action = Some(Action::StartDownload { link, passphrase, subfolder });
    }
    ui.advance_cursor_after_rect(Rect::from_min_max(left, pos2(right, left.y + height)));
    action
}

/// The input look of `.input-wrap input` and `input:focus`.
fn input_style(ui: &mut Ui) {
    let visuals = &mut ui.style_mut().visuals;
    visuals.extreme_bg_color = theme::SURFACE;
    visuals.widgets.inactive.bg_stroke = theme::stroke_border_2();
    visuals.widgets.hovered.bg_stroke = theme::hairline(theme::BORDER_3);
    visuals.widgets.active.bg_stroke = theme::stroke_brand();
}

/// `.input-wrap`: the link field, its link glyph, and the `Paste` button inside it.
fn link_field(ui: &mut Ui, rect: Rect, composer: &mut Composer) -> egui::Response {
    let field_id = Id::new("fiotransfer-transfers-link");
    let width = rect.width();
    let response = ui.scope(|ui| {
        input_style(ui);
        ui.put(
            rect,
            TextEdit::singleline(&mut composer.link)
                .id(field_id)
                .hint_text("Frame.io, Smash, WeTransfer, Dropbox, Google Drive or Portal link")
                .font(FontId::proportional(T_BODY))
                .margin(Margin { left: 32, right: 64, top: 6, bottom: 6 })
                .desired_width(width),
        )
    });
    let rect = response.response.rect;
    let painter = ui.painter().clone();
    let icon = Rect::from_center_size(pos2(rect.left() + 17.0, rect.center().y), vec2(16.0, 16.0));
    draw_link(&painter, icon, theme::TEXT_3);

    // `.input-btn { position: absolute; right: 4px; height: 24px }`
    let paste = "Paste";
    let w = text_width(ui, paste, T_DETAIL) + 16.0;
    let paste_rect = Rect::from_center_size(
        pos2(rect.right() - 4.0 - w / 2.0, rect.center().y),
        vec2(w, size::NESTED_BTN_H),
    );
    let hovered = ui.rect_contains_pointer(paste_rect);
    let fill = if hovered { theme::INPUT_BTN_HOVER } else { theme::RAISED };
    let color = if hovered { theme::TEXT } else { theme::TEXT_2 };
    let resp = ui
        .scope(|ui| {
            tight(ui);
            ui.put(
                paste_rect,
                Button::new(RichText::new(paste).size(T_DETAIL).color(color))
                    .fill(fill)
                    .stroke(Stroke::NONE)
                    .corner_radius(theme::R_MICRO)
                    .min_size(paste_rect.size()),
            )
        })
        .inner
        .on_hover_text("Paste from clipboard");
    if resp.clicked() {
        // egui has no clipboard *read* API: the button puts the caret in the field
        // so a real paste (⌘V) lands there, and the shell is asked for the text
        // (`Request::Paste`), which adopts `UiState::link` back into this buffer.
        ui.memory_mut(|m| m.request_focus(field_id));
        composer.wants_paste = true;
    }
    response.inner
}

/// `.btn` with the folder glyph and the folder's name; `title="Save to <dir>"`.
fn dir_button(ui: &mut Ui, rect: Rect, label: &str, state: &UiState) -> egui::Response {
    let hovered = ui.rect_contains_pointer(rect);
    let fill = if hovered { theme::RAISED } else { theme::CARD };
    let resp = ui
        .scope(|ui| {
            tight(ui);
            ui.put(
                rect,
                Button::new(RichText::new(label).size(T_BODY).color(theme::TEXT_BODY))
                    .fill(fill)
                    .stroke(theme::stroke_border_2())
                    .corner_radius(theme::R_S)
                    .min_size(rect.size())
                    .wrap_mode(TextWrapMode::Truncate),
            )
        })
        .inner;
    // The glyph is painted over the button's own left padding.
    let painter = ui.painter().clone();
    let icon = Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), vec2(16.0, 16.0));
    draw_folder(&painter, icon, theme::TEXT_2);
    let save_to = download_dir(state);
    resp.on_hover_text(if save_to.is_empty() { "Save to Downloads".to_string() } else { format!("Save to {save_to}") })
}

/// `S.dlDest || s.download_dir`: the folder this download will be written to —
/// the one chosen for it when the shell has recorded one, else the setting.
pub fn download_dir(state: &UiState) -> String {
    if !state.download_dir.is_empty() {
        return state.download_dir.clone();
    }
    state.settings.download_dir.clone()
}

/// `baseName(S.dlDest || s.download_dir)` — `#dl-dest-label` shows the folder's name.
pub fn download_dir_label(state: &UiState) -> String {
    let dir = download_dir(state);
    let dir = dir.trim_end_matches('/');
    match dir.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "Downloads".to_string(),
    }
}

/// `.comp-foot`: the link preview on the left, the "New folder" box on the right.
fn comp_foot(ui: &mut Ui, composer: &mut Composer) -> Option<Action> {
    Frame::new()
        .inner_margin(Margin {
            left: size::COMP_FOOT_INDENT as i8,
            right: space::XXL as i8,
            top: 0,
            bottom: space::XL as i8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), size::COMP_FOOT_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = space::XXXL;
                    let check_w = text_width(ui, "New folder", T_NOTE) + size::CHECK + space::M;
                    let preview_w = (ui.available_width() - check_w - space::XXXL).max(0.0);
                    let (rect, _) = ui.allocate_exact_size(vec2(preview_w, size::COMP_FOOT_H), Sense::hover());
                    if !composer.preview.is_empty() {
                        let painter = ui.painter().clone();
                        // `.link-preview .err` is `--error-text`, `.warn` is `--notice`.
                        let color = if composer.preview_error {
                            theme::ERROR_TEXT
                        } else if composer.preview_warn {
                            theme::NOTICE
                        } else {
                            theme::TEXT_2
                        };
                        line_left(ui, &painter, &composer.preview, T_NOTE, color, rect.left(), rect.center().y, rect.width());
                    }
                    ui.scope(|ui| {
                        ui.spacing_mut().interact_size = Vec2::ZERO;
                        ui.spacing_mut().icon_width = size::CHECK;
                        ui.spacing_mut().icon_spacing = space::M;
                        ui.add(Checkbox::new(
                            &mut composer.new_folder,
                            RichText::new("New folder").size(T_NOTE).color(theme::TEXT_2),
                        ));
                    });
                    None
                },
            )
            .inner
        })
        .inner
}

// ---------- the list ----------

fn list(ui: &mut Ui, state: &mut UiState) {
    let jobs = ordered_jobs(&state.snapshot.jobs);
    if jobs.is_empty() {
        empty_state(ui);
        return;
    }

    list_head(ui);

    let expanded = state.expanded.clone();
    let files = &state.files;
    let mut action = None;
    for job in &jobs {
        let is_open = expanded.as_deref() == Some(job.id.as_str());
        if let Some(a) = row(ui, job, is_open, files) {
            action = Some(a);
        }
    }
    if let Some(a) = action {
        report(state, a);
    }
}

/// `.list-head.jobs-grid`, over exactly the columns the rows use.
fn list_head(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, LIST_HEAD_H), Sense::hover());
    let painter = ui.painter().clone();
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, theme::BORDER));

    let inner = rect.shrink2(vec2(ROW_PAD_X, 0.0));
    let cols = columns(inner.width());
    let cy = rect.center().y;
    let mut x = inner.left();
    for (w, text, right_aligned) in [
        (cols.name, "Name", false),
        (cols.progress, "Progress", false),
        (cols.size, "Size", true),
        (cols.speed, "Speed", true),
        (cols.status, "Status", false),
        (cols.actions, "", false),
    ] {
        if w <= 0.0 {
            continue; // a column the window dropped takes its gap with it
        }
        if right_aligned {
            line_right(ui, &painter, text, T_CAPTION, theme::TEXT_3, x + w, cy, w);
        } else {
            line_left(ui, &painter, text, T_CAPTION, theme::TEXT_3, x, cy, w);
        }
        x += w + GRID_GAP;
    }
}

/// One job: the 56 px row, its error line, and its files when it is open.
fn row(ui: &mut Ui, job: &JobView, is_open: bool, files: &[FileView]) -> Option<Action> {
    let width = ui.available_width();
    let (row_rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
    let title = job_title(job);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &title));

    let painter = ui.painter().clone();
    if resp.hovered() {
        painter.rect_filled(row_rect, CornerRadius::ZERO, theme::ROW_HOVER);
    }
    painter.hline(row_rect.x_range(), row_rect.bottom() - 0.5, Stroke::new(1.0_f32, theme::BORDER));

    let inner = row_rect.shrink2(vec2(ROW_PAD_X, 0.0));
    let cols = columns(inner.width());
    let cy = row_rect.center().y;
    let mut x = inner.left();

    // Name: the kind icon, the title, and the sub line.
    let name_rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(cols.name, ROW_H));
    name_cell(ui, &painter, name_rect, job, &title);
    x += cols.name + GRID_GAP;

    // Progress: a bar and its percent — or the phase, while the size is unknown.
    let progress_rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(cols.progress, ROW_H));
    progress_cell(ui, &painter, progress_rect, job);
    x += cols.progress + GRID_GAP;

    if cols.show_size {
        let rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(cols.size, ROW_H));
        line_right(ui, &painter, &size_text(job), T_NOTE, theme::TEXT_2, rect.right(), cy, rect.width());
        x += cols.size + GRID_GAP;
    }
    if cols.show_speed {
        let rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(cols.speed, ROW_H));
        let color = if job.terminal() { theme::TEXT_2 } else { theme::TEXT };
        line_right(ui, &painter, &speed_text(job), T_NOTE, color, rect.right(), cy, rect.width());
        x += cols.speed + GRID_GAP;
    }

    let status_rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(cols.status, ROW_H));
    pill(ui, &painter, status_rect, job);
    x += cols.status + GRID_GAP;

    let actions_rect = Rect::from_min_size(pos2(x, row_rect.top()), vec2(cols.actions, ROW_H));
    let clicked_action = actions_cell(ui, actions_rect, job, is_open);

    // The error line, indented to the title, like `.error-line`.
    let mut below = row_rect.bottom();
    let error_h = if job.status == Status::Failed && !job.error.is_empty() {
        let h = 22.0;
        line_left(
            ui,
            &painter,
            &job.error,
            12.0,
            theme::ERROR_TEXT,
            inner.left() + ROW_INDENT,
            below + h / 2.0,
            (inner.width() - ROW_INDENT).max(0.0),
        );
        h
    } else {
        0.0
    };
    below += error_h;

    // `.row-detail`: the files, when the row is open.
    let detail_h = if is_open { files_box(ui, &painter, inner, below, job, files) } else { 0.0 };

    let block = Rect::from_min_size(row_rect.min, vec2(width, ROW_H + error_h + detail_h));
    ui.advance_cursor_after_rect(block);

    if let Some(a) = clicked_action {
        return Some(a);
    }
    if resp.clicked() {
        return Some(Action::Toggle(job.id.clone()));
    }
    None
}

fn name_cell(ui: &Ui, painter: &Painter, rect: Rect, job: &JobView, title: &str) {
    let icon = Rect::from_center_size(pos2(rect.left() + 15.0, rect.center().y), vec2(30.0, 30.0));
    painter.rect_filled(icon, theme::R_S, theme::CARD);
    painter.rect_stroke(
        icon,
        theme::R_S,
        Stroke::new(1.0_f32, theme::BORDER),
        StrokeKind::Inside,
    );
    draw_arrow(painter, icon.center(), job.is_upload(), theme::TEXT_2);

    let text_left = icon.right() + 12.0;
    let text_w = (rect.right() - text_left).max(0.0);
    let sub = subtitle(job);
    let title_galley = truncated(ui, title, T_BODY, theme::TEXT, text_w);
    let sub_galley = truncated(ui, &sub, T_DETAIL, theme::TEXT_3, text_w);
    let title_h = title_galley.size().y;
    let total = title_h + 1.0 + sub_galley.size().y;
    let mut y = (rect.center().y - total / 2.0).round();
    painter.galley(pos2(text_left, y), title_galley, theme::TEXT);
    y += title_h + 1.0;
    painter.galley(pos2(text_left, y), sub_galley, theme::TEXT_3);
}

fn progress_cell(ui: &mut Ui, painter: &Painter, rect: Rect, job: &JobView) {
    // A resolving job says what it is doing instead of pretending to have a bar.
    if job.status == Status::Resolving {
        let text = if job.phase.is_empty() {
            status_label("resolving", &job.kind)
        } else {
            job.phase.clone()
        };
        line_left(ui, painter, &text, T_DETAIL, theme::TEXT_2, rect.left(), rect.center().y, rect.width());
        let id = ui.make_persistent_id(("transfers-phase", &job.id));
        ui.interact(rect, id, Sense::hover())
            .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
        return;
    }
    let pct_w = 38.0;
    let track_w = (rect.width() - pct_w - 10.0).max(0.0);
    let track = Rect::from_center_size(
        pos2(rect.left() + track_w / 2.0, rect.center().y),
        vec2(track_w, size::TRACK_H),
    );
    track_bar(painter, track, track_fill_fraction(job), track_fill(job.status.as_str()));
    let pct = percent_text(job);
    if !pct.is_empty() {
        line_right(ui, painter, &pct, T_DETAIL, theme::TEXT_2, rect.right(), rect.center().y, pct_w);
    }
}

/// `.track` + `.track i`: a 4 px rounded track with a rounded fill.
fn track_bar(painter: &Painter, rect: Rect, fraction: Option<f32>, fill: Color32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let radius = theme::R_TRACK;
    painter.rect_filled(rect, radius, theme::RAISED);
    if let Some(f) = fraction {
        let w = rect.width() * f.clamp(0.0, 1.0);
        if w >= 1.0 {
            painter.rect_filled(Rect::from_min_size(rect.min, vec2(w, rect.height())), radius, fill);
        }
    }
}

/// `.status`: a dot and its word, coloured by state.
fn pill(ui: &mut Ui, painter: &Painter, rect: Rect, job: &JobView) {
    let text = status_text(job);
    let dot = pos2(rect.left() + 3.5, rect.center().y);
    if let Some(halo) = status_halo(job.status.as_str()) {
        painter.circle_stroke(dot, 5.0, Stroke::new(3.0_f32, halo));
    }
    painter.circle_filled(dot, 3.5, status_dot_color(job.status.as_str()));
    line_left(
        ui,
        painter,
        &text,
        T_NOTE,
        status_text_color(job.status.as_str()),
        rect.left() + 14.0,
        rect.center().y,
        (rect.width() - 14.0).max(0.0),
    );
    // The state is the row's most useful sentence: let a screen reader read it.
    let text_rect = Rect::from_min_size(
        pos2(rect.left() + 14.0, rect.center().y - 9.0),
        vec2((rect.width() - 14.0).max(0.0), 18.0),
    );
    let id = ui.make_persistent_id(("transfers-status", &job.id));
    ui.interact(text_rect, id, Sense::hover())
        .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
}

/// The row actions, laid out from the right edge like `.actions`: 28 px icon
/// buttons with the stylesheet's hover wash and the webview's own `title` words.
fn actions_cell(ui: &mut Ui, rect: Rect, job: &JobView, is_open: bool) -> Option<Action> {
    let shown = actions_for(job);
    // Drawn from the right edge inwards, so this order is the DOM's `.actions`
    // order read backwards: files, cancel, retry, reveal, copy, open.
    let mut icons: Vec<(Glyph, &str, Action)> = Vec::new();
    if shown.files {
        let label = if is_open { "Hide files" } else { "Show files" };
        icons.push((Glyph::ChevronUp(!is_open), label, Action::Toggle(job.id.clone())));
    }
    if shown.cancel {
        icons.push((Glyph::Cancel, "Cancel", Action::Cancel(job.id.clone())));
    }
    if shown.retry {
        icons.push((Glyph::Retry, "Retry", Action::Retry(job.id.clone())));
    }
    if shown.reveal {
        icons.push((Glyph::Reveal, "Show in Finder", Action::Reveal(job.id.clone())));
    }
    if shown.copy_link {
        icons.push((Glyph::Copy, "Copy share link", Action::Share(job.id.clone())));
    }
    if shown.open {
        icons.push((Glyph::Open, "Open in Frame.io", Action::Open(job.link.clone())));
    }

    let mut right = rect.right();
    let mut action = None;
    for (glyph, title, act) in icons {
        let size = size::ICON_BTN;
        if right - size < rect.left() {
            break; // no room left: the least important action goes, as flex would
        }
        let btn = Rect::from_min_size(pos2(right - size, rect.center().y - size / 2.0), vec2(size, size));
        right -= size + space::XXS;
        let id = ui.make_persistent_id(("transfers-action", &job.id, title));
        if icon_button(ui, id, btn, glyph, title).clicked() {
            action = Some(act);
        }
    }
    action
}

/// The glyphs the row actions use, each the paths of one `I.*` SVG in `app.js`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Open,
    Copy,
    Reveal,
    Retry,
    Cancel,
    /// The `.a-files` chevron: `true` when it points down (closed).
    ChevronUp(bool),
}

/// `.icon-btn`: 28 px, no frame, `--text-3`, a `--hover` wash and `--text` on hover.
fn icon_button(ui: &mut Ui, id: Id, rect: Rect, glyph: Glyph, title: &str) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, title));
    let painter = ui.painter().clone();
    if resp.hovered() || resp.is_pointer_button_down_on() {
        painter.rect_filled(rect, theme::R_S, theme::HOVER);
    }
    let color = if resp.hovered() { theme::TEXT } else { theme::TEXT_3 };
    let area = Rect::from_center_size(rect.center(), vec2(16.0, 16.0));
    match glyph {
        Glyph::Open => draw_open(&painter, area, color),
        Glyph::Copy => draw_copy(&painter, area, color),
        Glyph::Reveal => draw_folder(&painter, area, color),
        Glyph::Retry => draw_retry(&painter, area, color),
        Glyph::Cancel => draw_cancel(&painter, area, color),
        Glyph::ChevronUp(down) => draw_chevron(&painter, area, color, !down),
    }
    resp.on_hover_text(title.to_string())
}

// ---------- the empty state and the files ----------

/// `.empty`: a 44 px card with the transfer glyph, then "No transfers".
fn empty_state(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 200.0), Sense::hover());
    ui.scope_builder(
        UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center)),
        |ui| {
            ui.add_space(72.0); // `.empty { padding: 72px 0 40px }`
            let (icon, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
            let painter = ui.painter().clone();
            painter.rect_filled(icon, theme::R_EMPTY, theme::CARD);
            painter.rect_stroke(
                icon,
                theme::R_EMPTY,
                Stroke::new(1.0_f32, theme::BORDER),
                StrokeKind::Inside,
            );
            draw_sync_glyph(&painter, icon, theme::TEXT_3);
            ui.add_space(14.0); // `.empty-icon { margin: 0 auto 14px }`
            ui.add(Label::new(RichText::new("No transfers").size(T_SECTION).color(theme::TEXT)));
        },
    );
}

/// `.row-detail .files`: one 30 px row per file, in a 300 px scroll box.
/// Returns the height it took, so the caller can advance its cursor.
fn files_box(
    ui: &mut Ui,
    painter: &Painter,
    inner: Rect,
    top: f32,
    job: &JobView,
    files: &[FileView],
) -> f32 {
    let _ = painter;
    let left = inner.left() + ROW_INDENT;
    let width = (inner.width() - ROW_INDENT).max(0.0);
    let ordered = ordered_files(files);
    let shown = ordered.len().min(FILES_SHOWN);
    let content_h = (shown.max(1) as f32) * FILE_ROW_H + FILES_BORDER;
    let box_h = content_h.min(FILES_MAX_H);
    let extra_h = if ordered.len() > shown { 24.0 } else { 0.0 };
    let detail_h = box_h + extra_h + 12.0; // `.row-detail { padding: 0 0 12px 42px }`

    let box_rect = Rect::from_min_size(pos2(left, top), vec2(width, box_h));
    let frame = Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(theme::R)
        .inner_margin(Margin::ZERO);
    ui.scope_builder(UiBuilder::new().max_rect(box_rect), |ui| {
        frame.show(ui, |ui| {
            ui.set_width((width - FILES_BORDER).max(0.0));
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if ordered.is_empty() {
                ui.allocate_ui_with_layout(
                    vec2((width - FILES_BORDER).max(0.0), FILE_ROW_H),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.add_space(ROW_PAD_X);
                        ui.add(
                            Label::new(RichText::new("No files").size(T_DETAIL).color(theme::TEXT_3))
                                .truncate(),
                        );
                    },
                );
                return;
            }
            ScrollArea::vertical()
                .id_salt("transfers-files")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, file) in ordered.iter().take(shown).enumerate() {
                        file_row(ui, file, &job.kind, index + 1 == shown);
                    }
                });
        });
    });

    if extra_h > 0.0 {
        painter.text(
            pos2(left + ROW_PAD_X, top + box_h + extra_h / 2.0),
            egui::Align2::LEFT_CENTER,
            format!("+ {} more files", ordered.len() - shown),
            FontId::proportional(T_DETAIL),
            theme::TEXT_3,
        );
    }
    detail_h
}

/// `.frow`: the rel path, the size, a 3 px bar, and the state.
fn file_row(ui: &mut Ui, file: &FileView, kind: &str, last: bool) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, FILE_ROW_H), Sense::hover());
    let painter = ui.painter().clone();
    if !last {
        painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0_f32, theme::BORDER));
    }

    let inner = rect.shrink2(vec2(ROW_PAD_X, 0.0));
    let cols = file_columns(inner.width());
    let cy = rect.center().y;
    let mut x = inner.left();

    let name = if file.rel_path.is_empty() { file.name.clone() } else { file.rel_path.clone() };
    if cols.name > 4.0 {
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(Rect::from_min_size(pos2(x, rect.top()), vec2(cols.name, FILE_ROW_H)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.add(
                    Label::new(RichText::new(&name).size(T_DETAIL).color(theme::TEXT_BODY)).truncate(),
                );
            },
        );
    }
    x += cols.name + GRID_GAP;

    line_right(ui, &painter, &fmt_bytes(file.size), T_DETAIL, theme::TEXT_3, x + cols.size, cy, cols.size);
    x += cols.size + GRID_GAP;

    let track = Rect::from_center_size(pos2(x + cols.track / 2.0, cy), vec2(cols.track, 3.0));
    track_bar(&painter, track, Some(file_fraction(file)), file_track_fill(file.status.as_str()));
    x += cols.track + GRID_GAP;

    let status = file_status_text(file, kind);
    let status_w = (rect.right() - ROW_PAD_X - x).max(0.0);
    line_left(ui, &painter, &status, T_DETAIL, file_status_color(file.status.as_str()), x, cy, status_w);
    let id = ui.make_persistent_id(("transfers-file-status", &file.rel_path, &file.name));
    ui.interact(Rect::from_min_size(pos2(x, cy - 8.0), vec2(status_w, 16.0)), id, Sense::hover())
        .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &status));
}

// ---------- reporting a click ----------

fn report(state: &mut UiState, action: Action) {
    match action {
        // The two clicks that are the screen's own state and nothing else.
        Action::Toggle(id) => {
            state.expanded =
                if state.expanded.as_deref() == Some(id.as_str()) { None } else { Some(id) };
        }
        Action::SetUp => state.view = View::Settings,

        // Everything else is one thing the app does (`fiotransfer_core::App`).
        Action::Cancel(id) => state.ask(Request::Cancel(id)),
        Action::Retry(id) => state.ask(Request::Retry(id)),
        Action::Share(id) => state.ask(Request::ShareLink(id)),
        Action::Reveal(id) => {
            // `.a-reveal` reveals the job's destination: `call('Reveal', el.dataset.dest)`.
            let dest = state
                .snapshot
                .jobs
                .iter()
                .find(|job| job.id == id)
                .map(|job| job.dest.clone())
                .filter(|dest| !dest.is_empty());
            if let Some(dest) = dest {
                state.ask(Request::Reveal(dest));
            }
        }
        Action::Open(link) => state.ask(Request::OpenUrl(link)),
        Action::StartDownload { link, passphrase, subfolder } => {
            let dest_dir = state.download_dir.clone();
            state.ask(Request::StartDownload { link, passphrase, dest_dir, subfolder });
            // The typed link is spent: `S.linkInfo = null` in `startDownload()`.
            state.link_info = None;
        }
        Action::ClearFinished => state.ask(Request::ClearFinished),
        Action::PickFiles => state.ask(Request::PickFiles),
        Action::PickFolder => state.ask(Request::PickFolder),
        Action::Paste => state.ask(Request::Paste),
        Action::PickDownloadDir => state.ask(Request::PickDownloadDir),
        Action::SignIn => state.ask(Request::Login),
        Action::ChooseDestination(at) => {
            state.ask(Request::OpenDest { mode: DestMode::Upload { pending: Vec::new() }, at });
        }
    }
}

// ---------- painting helpers ----------

fn text_width(ui: &Ui, text: &str, size: f32) -> f32 {
    ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(size), theme::TEXT).size().x
}

/// One line, laid out to `width` and elided with `…` when it does not fit.
fn truncated(ui: &Ui, text: &str, size: f32, color: Color32, width: f32) -> Arc<Galley> {
    egui::WidgetText::from(RichText::new(text).size(size).color(color)).into_galley(
        ui,
        Some(TextWrapMode::Truncate),
        width.max(1.0),
        FontId::proportional(size),
    )
}

#[expect(clippy::too_many_arguments, reason = "a painted line is its place, its text and its colour")]
fn line_left(ui: &Ui, painter: &Painter, text: &str, size: f32, color: Color32, left: f32, cy: f32, width: f32) {
    if width <= 0.0 || text.is_empty() {
        return;
    }
    let g = truncated(ui, text, size, color, width);
    let h = g.size().y;
    painter.galley(pos2(left, cy - h / 2.0), g, color);
}

#[expect(clippy::too_many_arguments, reason = "a painted line is its place, its text and its colour")]
fn line_right(ui: &Ui, painter: &Painter, text: &str, size: f32, color: Color32, right: f32, cy: f32, width: f32) {
    if width <= 0.0 || text.is_empty() {
        return;
    }
    let g = truncated(ui, text, size, color, width);
    let (w, h) = (g.size().x, g.size().y);
    painter.galley(pos2(right - w, cy - h / 2.0), g, color);
}

/// The `↑` / `↓` of `.kind-icon`, drawn rather than typed so no glyph can be missing.
fn draw_arrow(painter: &Painter, centre: egui::Pos2, up: bool, color: Color32) {
    let stroke = Stroke::new(1.5_f32, color);
    let (tail, head) = if up {
        (pos2(centre.x, centre.y + 5.5), pos2(centre.x, centre.y - 5.5))
    } else {
        (pos2(centre.x, centre.y - 5.5), pos2(centre.x, centre.y + 5.5))
    };
    painter.line_segment([tail, head], stroke);
    let dy = head.y - tail.y;
    let sign = if dy < 0.0 { 1.0 } else { -1.0 };
    painter.line_segment([head, pos2(head.x - 4.5, head.y + sign * 4.5)], stroke);
    painter.line_segment([head, pos2(head.x + 4.5, head.y + sign * 4.5)], stroke);
}

/// The chevron of `.dest-pick .chev` and `.a-files`.
fn draw_chevron(painter: &Painter, rect: Rect, color: Color32, up: bool) {
    let stroke = Stroke::new(1.5_f32, color);
    let (l, r, y) = (rect.left(), rect.right(), rect.center().y);
    let mid = rect.center().x;
    let sign = if up { 1.0 } else { -1.0 };
    painter.line_segment([pos2(l, y - sign * 1.5), pos2(mid, y + sign * 1.5)], stroke);
    painter.line_segment([pos2(mid, y + sign * 1.5), pos2(r, y - sign * 1.5)], stroke);
}

/// The folder glyph of `#dl-dest` and `.a-reveal`.
fn draw_folder(painter: &Painter, rect: Rect, color: Color32) {
    let body = Rect::from_min_max(pos2(rect.left(), rect.top() + 2.5), pos2(rect.right(), rect.bottom() - 1.5));
    painter.rect_stroke(body, theme::R_TRACK, Stroke::new(1.4_f32, color), StrokeKind::Middle);
    let tab = egui::Rect::from_min_max(
        pos2(body.left() + 1.0, body.top()),
        pos2(body.left() + 6.0, body.top() - 1.5),
    );
    painter.line_segment([pos2(tab.left(), tab.bottom()), pos2(tab.left(), tab.top())], Stroke::new(1.4_f32, color));
    painter.line_segment([pos2(tab.left(), tab.top()), pos2(tab.right(), tab.top())], Stroke::new(1.4_f32, color));
    painter.line_segment([pos2(tab.right(), tab.top()), pos2(tab.right() + 1.0, body.top())], Stroke::new(1.4_f32, color));
}

/// `I.open` — the box with the arrow leaving it.
fn draw_open(painter: &Painter, rect: Rect, color: Color32) {
    let s = rect.width() / 20.0;
    let at = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    let stroke = Stroke::new(1.5_f32, color);
    // `M8 5H5.5A1.5 1.5 0 0 0 4 6.5v8A1.5 1.5 0 0 0 5.5 16h8a1.5 1.5 0 0 0 1.5-1.5V12`
    painter.line_segment([at(8.0, 5.0), at(5.5, 5.0)], stroke);
    svg_arc(painter, at(5.5, 6.5), 1.5 * s, -90.0, -180.0, stroke);
    painter.line_segment([at(4.0, 6.5), at(4.0, 14.5)], stroke);
    svg_arc(painter, at(5.5, 14.5), 1.5 * s, 180.0, 90.0, stroke);
    painter.line_segment([at(5.5, 16.0), at(13.5, 16.0)], stroke);
    svg_arc(painter, at(13.5, 14.5), 1.5 * s, 90.0, 0.0, stroke);
    painter.line_segment([at(15.0, 14.5), at(15.0, 12.0)], stroke);
    // `M11 4h5v5M16 4l-7 7`
    painter.line_segment([at(11.0, 4.0), at(16.0, 4.0)], stroke);
    painter.line_segment([at(16.0, 4.0), at(16.0, 9.0)], stroke);
    painter.line_segment([at(16.0, 4.0), at(9.0, 11.0)], stroke);
}

/// `I.copy` — two overlapping rounded squares.
fn draw_copy(painter: &Painter, rect: Rect, color: Color32) {
    let s = rect.width() / 20.0;
    let at = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    let stroke = Stroke::new(1.4_f32, color);
    let front = Rect::from_min_max(at(7.0, 7.0), at(16.0, 16.0));
    painter.rect_stroke(front, CornerRadius::same(1), stroke, StrokeKind::Middle);
    painter.line_segment([at(13.0, 7.0), at(13.0, 5.5)], stroke);
    svg_arc(painter, at(11.5, 5.5), 1.5 * s, 0.0, -90.0, stroke);
    painter.line_segment([at(11.5, 4.0), at(5.5, 4.0)], stroke);
    svg_arc(painter, at(5.5, 5.5), 1.5 * s, -90.0, -180.0, stroke);
    painter.line_segment([at(4.0, 5.5), at(4.0, 11.5)], stroke);
    svg_arc(painter, at(5.5, 11.5), 1.5 * s, 180.0, 90.0, stroke);
    painter.line_segment([at(5.5, 13.0), at(7.0, 13.0)], stroke);
}

/// `I.retry` — an open circle with an arrow head, the way `app.js` draws it.
fn draw_retry(painter: &Painter, rect: Rect, color: Color32) {
    let s = rect.width() / 20.0;
    let at = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    let stroke = Stroke::new(1.5_f32, color);
    svg_arc(painter, at(10.0, 10.0), 5.5 * s, 0.0, 315.0, stroke);
    painter.line_segment([at(15.5, 4.0), at(15.5, 7.4)], stroke);
    painter.line_segment([at(15.5, 4.0), at(12.1, 4.0)], stroke);
}

/// `I.x` — the Cancel cross.
fn draw_cancel(painter: &Painter, rect: Rect, color: Color32) {
    let s = rect.width() / 20.0;
    let at = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    let stroke = Stroke::new(1.5_f32, color);
    painter.line_segment([at(6.0, 6.0), at(14.0, 14.0)], stroke);
    painter.line_segment([at(14.0, 6.0), at(6.0, 14.0)], stroke);
}

/// The window glyph of `.dest-pick .seg.root` (`.dest-pick svg`).
fn draw_project(painter: &Painter, rect: Rect, color: Color32) {
    let s = rect.width() / 20.0;
    let at = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    let stroke = Stroke::new(1.4_f32, color);
    let body = Rect::from_min_max(at(3.0, 4.0), at(17.0, 16.0));
    painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Middle);
    painter.line_segment([at(3.0, 8.0), at(17.0, 8.0)], stroke);
}

/// The chain glyph of `.input-icon` in the link field, from the same SVG path
/// `index.html` uses (`M8.5 11.5l3-3M7.2 9.3l-1.6 1.6a2.6…`).
fn draw_link(painter: &Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.5_f32, color);
    let s = rect.width() / 20.0;
    let at = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    painter.line_segment([at(8.5, 11.5), at(11.5, 8.5)], stroke);
    svg_arc(painter, at(7.45, 12.75), 2.62 * s, -135.0, -315.0, stroke);
    svg_arc(painter, at(12.55, 7.25), 2.62 * s, 45.0, -135.0, stroke);
}

/// A circle arc sampled as a polyline, in the SVG's y-down coordinates.
fn svg_arc(painter: &Painter, centre: Pos2, radius: f32, from_deg: f32, to_deg: f32, stroke: Stroke) {
    const STEPS: usize = 12;
    let points: Vec<Pos2> = (0..=STEPS)
        .map(|i| {
            let turn = from_deg + (to_deg - from_deg) * (i as f32 / STEPS as f32);
            let angle = turn.to_radians();
            pos2(centre.x + radius * angle.cos(), centre.y + radius * angle.sin())
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke));
}

/// The up-and-down glyph of `#jobs-empty`.
fn draw_sync_glyph(painter: &Painter, rect: Rect, color: Color32) {
    let up = Rect::from_center_size(pos2(rect.center().x - 5.0, rect.center().y), vec2(12.0, 20.0));
    let down = Rect::from_center_size(pos2(rect.center().x + 6.0, rect.center().y), vec2(12.0, 20.0));
    draw_arrow(painter, up.center(), true, color);
    draw_arrow(painter, down.center(), false, color);
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileView, JobView, Snapshot, UiState};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn stamp_job(id: &str, kind: &str, title: &str, status: Status, started_at: i64) -> JobView {
        JobView {
            id: id.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            status,
            started_at,
            dest: "Delivery / Reel 1".to_string(),
            ..JobView::default()
        }
    }

    /// Three jobs in three states, exactly what the screen has to keep apart:
    /// one uploading, one failed, one finished.
    fn three_jobs() -> Vec<JobView> {
        let mut running = stamp_job("a", "upload", "clip-a.mov", Status::Running, 300);
        running.total = 100;
        running.done = 25;
        running.speed = 10_485_760.0;
        running.eta = 65.0;
        running.files_total = 2;
        running.files_done = 1;
        running.files_active = 1;

        let mut failed = stamp_job("b", "download", "clip-b.mov", Status::Failed, 200);
        failed.source = "https://acme.frame.io/s/abc123/".to_string();
        failed.dest = "~/Movies/Reel 1".to_string();
        failed.total = 50;
        failed.done = 10;
        failed.error = "The share link has expired".to_string();
        failed.files_total = 1;

        let mut done = stamp_job("c", "upload", "clip-c.mov", Status::Done, 100);
        done.total = 200;
        done.done = 200;
        done.elapsed = 65.0;
        done.avg_speed = 3_200_000.0;
        done.link = "https://acme.frame.io/share/xyz".to_string();
        done.files_total = 3;
        done.files_done = 3;

        vec![running, failed, done]
    }

    fn two_files() -> Vec<FileView> {
        vec![
            FileView {
                name: "one.mov".to_string(),
                rel_path: "scene/one.mov".to_string(),
                size: 100,
                done: 40,
                status: Status::Running,
                ..FileView::default()
            },
            FileView {
                name: "two.mov".to_string(),
                rel_path: "scene/two.mov".to_string(),
                size: 100,
                done: 100,
                status: Status::Done,
                ..FileView::default()
            },
        ]
    }

    fn state_with(jobs: Vec<JobView>, files: Vec<FileView>) -> UiState {
        let mut state = UiState {
            snapshot: Snapshot { jobs, speed: 0.0, active_streams: 0, stream_limit: 16 },
            files,
            destination: "Delivery / Reel 1".to_string(),
            logged_in: true,
            ..UiState::default()
        };
        state.settings.download_dir = "/Users/chaos/Movies".to_string();
        state
    }

    fn screen_harness(state: UiState) -> Harness<'static, UiState> {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(956.0, 700.0))
            .build_ui_state(|ui, state: &mut UiState| show(ui, state), state);
        harness.run();
        harness
    }

    // ---- the layout maths ----

    #[test]
    fn the_job_grid_fills_the_row_and_drops_columns_when_narrow() {
        for width in [320.0_f32, 500.0, 760.0, 800.0, 900.0, 1100.0, 1600.0] {
            let cols = columns(width);
            // Every pixel is accounted for: the columns and their gaps are the row.
            assert!(
                (cols.total() - width).abs() < 0.01 || cols.name <= 0.01,
                "width {width}: columns total {} != {width}",
                cols.total()
            );
            assert!(cols.name >= 0.0);
            assert!(cols.progress >= 0.0);
            // Progress keeps a workable track whenever there is room for one.
            if width >= 600.0 {
                assert!(cols.progress >= PROGRESS_MIN - 0.01, "width {width}");
            }
        }
        // The stylesheet's breakpoints: speed goes first, then size.
        assert!(!columns(DROP_SPEED_BELOW - 1.0).show_speed);
        assert!(columns(DROP_SPEED_BELOW).show_speed);
        assert!(!columns(DROP_SIZE_BELOW - 1.0).show_size);
        assert!(columns(DROP_SIZE_BELOW).show_size);
        // A wide row is dominated by the name column, as `minmax(0, 1.6fr)` says.
        let wide = columns(1200.0);
        assert!(wide.name > wide.progress);
        assert_eq!(wide.visible().len(), 6);
        // A narrow one hides two columns and still fits.
        assert_eq!(columns(700.0).visible().len(), 4);
    }

    #[test]
    fn the_file_grid_leaves_the_name_the_space_that_is_left() {
        let cols = file_columns(600.0);
        assert!((cols.total() - 600.0).abs() < 0.01);
        assert!(cols.name > 0.0);
        // A box narrower than the fixed columns degrades to a zero-width name.
        assert_eq!(file_columns(100.0).name, 0.0);
    }

    // ---- the words ----

    #[test]
    fn every_status_says_what_the_webview_front_end_says() {
        let mut job = stamp_job("x", "upload", "clip.mov", Status::Running, 0);
        job.eta = 65.0;
        assert_eq!(status_text(&job), "1m 05s left");

        job.eta = 0.0;
        assert_eq!(status_text(&job), "Uploading");
        job.kind = "download".to_string();
        assert_eq!(status_text(&job), "Downloading");
        job.kind = "upload".to_string();
        job.total = 100;
        job.done = 100;
        assert_eq!(status_text(&job), "Verifying");

        let mut done = stamp_job("x", "upload", "clip.mov", Status::Done, 0);
        done.files_total = 12;
        done.elapsed = 65.0;
        assert_eq!(status_text(&done), "12 files · 1m 05s");

        let mut partial = stamp_job("x", "upload", "clip.mov", Status::Partial, 0);
        partial.files_total = 4;
        partial.files_failed = 2;
        assert_eq!(status_text(&partial), "2 of 4 failed");

        assert_eq!(status_label("canceled", "upload"), "Canceled");
        assert_eq!(status_label("resolving", "upload"), "Preparing");
        assert_eq!(status_label("interrupted", "download"), "Interrupted");
        assert_eq!(status_label("something-new", "upload"), "something-new");
    }

    #[test]
    fn the_cells_word_the_sizes_speeds_and_file_counts_like_the_webview() {
        let mut running = stamp_job("x", "upload", "clip.mov", Status::Running, 0);
        running.total = 1_500_000;
        running.done = 750_000;
        running.files_total = 4;
        running.files_done = 1;
        assert_eq!(size_text(&running), "750 KB / 1.50 MB");
        assert_eq!(file_count(&running), "1 of 4 files");
        assert_eq!(percent_text(&running), "50%");
        assert_eq!(bar_fraction(&running), Some(0.5));

        let mut done = stamp_job("x", "upload", "clip.mov", Status::Done, 0);
        done.total = 1_500_000;
        done.done = 1_500_000;
        done.files_total = 1;
        done.files_done = 1;
        assert_eq!(size_text(&done), "1.50 MB");
        assert_eq!(file_count(&done), "1 of 1 file");
        assert_eq!(percent_text(&done), "100%");

        let unknown = stamp_job("x", "upload", "clip.mov", Status::Resolving, 0);
        assert_eq!(size_text(&unknown), "—");
        assert_eq!(speed_text(&unknown), "—");
        assert_eq!(percent_text(&unknown), "");
        assert_eq!(bar_fraction(&unknown), None);

        assert_eq!(short_link("https://acme.frame.io/s/abc/"), "acme.frame.io/s/abc");
        assert_eq!(short_link("acme.frame.io/s/abc"), "acme.frame.io/s/abc");

        let mut download = stamp_job("x", "download", "clip.mov", Status::Running, 0);
        download.source = "https://acme.frame.io/s/abc/".to_string();
        download.dest = "~/Movies".to_string();
        download.files_total = 2;
        download.files_done = 1;
        assert_eq!(subtitle(&download), "acme.frame.io/s/abc → ~/Movies · 1 of 2 files");
    }

    #[test]
    fn the_file_rows_word_and_colour_their_states_like_the_webview() {
        let mut failed = FileView { status: Status::Failed, error: "disk full".to_string(), ..FileView::default() };
        assert_eq!(file_status_text(&failed, "upload"), "disk full");
        failed.error.clear();
        assert_eq!(file_status_text(&failed, "upload"), "Failed");
        assert_eq!(file_status_color("failed"), theme::ERROR_TEXT);

        let skipped = FileView { status: Status::Skipped, note: "unchanged".to_string(), ..FileView::default() };
        assert_eq!(file_status_text(&skipped, "upload"), "unchanged");

        let retrying = FileView { status: Status::Running, retries: 1, ..FileView::default() };
        assert_eq!(file_status_text(&retrying, "upload"), "Uploading · 1 retry");
        let retrying = FileView { status: Status::Running, retries: 3, ..FileView::default() };
        assert_eq!(file_status_text(&retrying, "upload"), "Uploading · 3 retries");

        let mut done = FileView { status: Status::Done, size: 200, ..FileView::default() };
        assert_eq!(file_fraction(&done), 0.0);
        done.done = 50;
        assert!((file_fraction(&done) - 0.25).abs() < 1e-6);
        assert_eq!(file_track_fill("done"), theme::TEXT_4);
        assert_eq!(file_track_fill("running"), theme::BRAND);
    }

    #[test]
    fn the_pill_colours_follow_the_stylesheet() {
        assert_eq!(status_dot_color("running"), theme::BRAND);
        assert_eq!(status_dot_color("done"), theme::SUCCESS);
        assert_eq!(status_dot_color("failed"), theme::ERROR);
        assert_eq!(status_dot_color("partial"), theme::NOTICE);
        assert_eq!(status_dot_color("canceled"), theme::TEXT_4);
        assert_eq!(status_dot_color("skipped"), theme::TEXT_4);
        assert_eq!(status_dot_color("resolving"), theme::INFO);
        assert_eq!(track_fill("done"), theme::SUCCESS);
        assert_eq!(track_fill("failed"), theme::ERROR);
        assert_eq!(track_fill("canceled"), theme::TEXT_4);
    }

    #[test]
    fn jobs_come_newest_first_and_files_come_working_first() {
        let jobs = three_jobs();
        let ordered = ordered_jobs(&jobs);
        let ids: Vec<&str> = ordered.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);

        // Ties keep the host's order (the app already sends them newest first).
        let mut tied = three_jobs();
        for job in &mut tied {
            job.started_at = 0;
        }
        let ids: Vec<&str> = ordered_jobs(&tied).iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);

        let files = vec![
            FileView { rel_path: "done.mov".to_string(), status: Status::Done, ..FileView::default() },
            FileView { rel_path: "run.mov".to_string(), status: Status::Running, ..FileView::default() },
            FileView { rel_path: "bad.mov".to_string(), status: Status::Failed, ..FileView::default() },
        ];
        let order: Vec<&str> = ordered_files(&files).iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(order, ["run.mov", "bad.mov", "done.mov"]);
    }

    #[test]
    fn the_row_actions_match_the_webview_front_end() {
        let jobs = three_jobs();
        let running = actions_for(&jobs[0]);
        assert!(running.cancel && !running.retry && !running.copy_link && !running.reveal);
        assert!(running.files);

        let failed = actions_for(&jobs[1]);
        assert!(!failed.cancel && failed.retry);
        assert!(failed.reveal, "a download can be shown in Finder");

        let done = actions_for(&jobs[2]);
        assert!(!done.cancel && !done.retry && done.copy_link);
    }

    #[test]
    fn nothing_panics_on_empty_or_missing_data() {
        let state = state_with(Vec::new(), Vec::new());
        let harness = screen_harness(state);
        harness.get_by_label("No transfers"); // the empty state is drawn

        // A job with nothing filled in renders without a title or a size.
        let blank = JobView { started_at: 1, kind: "upload".to_string(), ..JobView::default() };
        let state = state_with(vec![blank], Vec::new());
        let blank_harness = screen_harness(UiState { destination: String::new(), ..state });
        blank_harness.get_by_label("Upload"); // `updateJob()`'s fallback title
    }

    // ---- the screen, through the accessibility tree ----

    #[test]
    fn three_jobs_draw_three_rows_newest_first_and_one_opens_its_files() {
        let state = state_with(three_jobs(), two_files());
        let mut harness = screen_harness(state);

        // One row per job, each labelled with its title, newest at the top.
        let a = harness.get_by_label("clip-a.mov");
        let b = harness.get_by_label("clip-b.mov");
        let c = harness.get_by_label("clip-c.mov");
        assert!(a.rect().top() < b.rect().top(), "clip-a is the newest, so it leads");
        assert!(b.rect().top() < c.rect().top(), "clip-b is newer than clip-c");
        // The row title is the row's own label, and nothing else claims it twice.
        assert_eq!(harness.query_all_by_label("clip-a.mov").count(), 1);
        assert!(harness.query_by_label("clip-z.mov").is_none());

        // A row also carries the webview's own action titles, and its state.
        harness.get_by_label("Show in Finder"); // the failed download
        harness.get_by_label("Copy share link"); // the finished upload
        harness.get_by_label("Open in Frame.io"); // the finished upload
        harness.get_by_label("Cancel"); // the running upload
        harness.get_by_label("Retry"); // the failed download
        harness.get_by_label("1m 05s left");

        // Nothing is open yet.
        assert_eq!(harness.state().expanded, None);
        assert!(harness.query_by_label("scene/one.mov").is_none());

        // Clicking the row opens it, and its files are drawn.
        harness.get_by_label("clip-a.mov").click_accesskit();
        harness.run();
        assert_eq!(harness.state().expanded.as_deref(), Some("a"));
        let one = harness.get_by_label("scene/one.mov");
        let two = harness.get_by_label("scene/two.mov");
        assert!(one.rect().top() < two.rect().top(), "the running file leads");
        harness.get_by_label("Uploading"); // the running file's state
        harness.get_by_label("Completed"); // the finished file's state

        // Clicking it again closes it.
        harness.get_by_label("clip-a.mov").click_accesskit();
        harness.run();
        assert_eq!(harness.state().expanded, None);
        assert!(harness.query_by_label("scene/one.mov").is_none());
    }

    #[test]
    fn a_narrow_window_and_long_text_do_not_panic() {
        let mut job = stamp_job("long", "upload", &"clip-".repeat(60), Status::Running, 10);
        job.dest = "/".repeat(400);
        job.files_total = 2;
        job.files_done = 1;
        job.phase = "\u{2026}".to_string();
        let mut state = state_with(vec![job], two_files());
        state.destination = "Delivery / ".repeat(40);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 320.0)) // narrower than MIN_WIDTH, on purpose
            .build_ui_state(|ui, state: &mut UiState| show(ui, state), state);
        harness.run();
        // The list still draws, and the row is still reachable by its title.
        harness.get_by_label_contains("clip-clip-clip");
        // The composer still lays out: the entry points and the download row fit.
        harness.get_by_label("Files…");
        harness.get_by_label("Folder…");
        harness.get_by_label("Download");
    }








    #[test]
    fn a_job_with_many_files_draws_its_own_scroll_box() {
        let jobs = three_jobs();
        let files: Vec<FileView> = (0..120)
            .map(|i| FileView {
                name: format!("f{i}.mov"),
                rel_path: format!("Day {}/f{i}.mov", i % 4),
                size: 1_000_000,
                done: 500_000,
                status: if i % 3 == 0 { Status::Done } else { Status::Queued },
                ..FileView::default()
            })
            .collect();
        let mut state = state_with(jobs, files);
        state.expanded = Some("a".to_string());
        let harness = screen_harness(state);
        // The first file is drawn; the box scrolls for the rest (`.files { max-height: 300px }`).
        harness.get_by_label("Day 0/f0.mov");
    }

    #[test]
    fn the_head_and_the_composer_draw_what_index_html_draws() {
        // One terminal job, so `.head-actions` shows its button.
        let harness = screen_harness(state_with(three_jobs(), Vec::new()));
        harness.get_by_label("Clear finished"); // `.btn.subtle` in .head-actions
        harness.get_by_label("Delivery / Reel 1"); // `.dest-pick`, labelled with the crumbs
        harness.get_by_label("Files…");
        harness.get_by_label("Folder…");
        harness.get_by_label("Paste"); // `.input-btn` inside the link field
        harness.get_by_label("Download"); // `.btn.primary`
        harness.get_by_label("New folder"); // `#subfolder`
        // The download folder button is named after the folder itself (`#dl-dest-label`).
        harness.get_by_label("Movies");

        // Nothing finished: `.head-actions` is empty.
        let mut running_only = three_jobs();
        running_only.retain(|job| !job.terminal());
        let harness = screen_harness(state_with(running_only, Vec::new()));
        assert!(harness.query_by_label("Clear finished").is_none());
    }

    #[test]
    fn the_composer_says_it_is_not_signed_in_when_the_state_says_so() {
        let mut state = state_with(Vec::new(), Vec::new());
        state.logged_in = false;
        state.settings.client_id = "abc".to_string();
        let harness = screen_harness(state);
        harness.get_by_label("Sign in"); // `.btn.primary` #cta-signin
        assert!(harness.query_by_label("Files…").is_none());

        let mut unset = state_with(Vec::new(), Vec::new());
        unset.logged_in = false;
        unset.settings.client_id = String::new();
        let harness = screen_harness(unset);
        harness.get_by_label("Set up"); // the `#cta-setup` path
    }

    #[test]
    fn download_is_disabled_until_a_link_is_typed_and_then_asks_for_the_transfer() {
        use egui::accesskit::Role;
        let mut harness = screen_harness(state_with(three_jobs(), Vec::new()));

        // An empty link: the button is inert (`#download-btn` starts disabled).
        harness.get_by_label("Download").click_accesskit();
        harness.run();
        assert!(harness.state().requests.is_empty(), "a disabled Download must do nothing");

        // Type a link, tick "New folder", then Download asks for the transfer.
        let field = harness.get_by_role(Role::TextInput);
        field.click();
        field.type_text("https://acme.frame.io/s/abc");
        harness.run();
        harness.get_by_label("New folder").click_accesskit();
        harness.run();
        assert_eq!(harness.state().link, "https://acme.frame.io/s/abc", "the buffer is the record");
        harness.state_mut().download_dir = "/Users/chaos/Movies/Incoming".to_string();
        harness.run();
        harness.get_by_label("Download").click_accesskit();
        harness.run();
        assert_eq!(
            harness.state().requests,
            vec![Request::StartDownload {
                link: "https://acme.frame.io/s/abc".to_string(),
                passphrase: String::new(),
                dest_dir: "/Users/chaos/Movies/Incoming".to_string(),
                subfolder: true,
            }]
        );
        assert!(harness.state().toasts.is_empty(), "Download says nothing of its own");
        // `startDownload()` empties the field, and the state with it.
        assert!(harness.state().link.is_empty());
    }

    #[test]
    fn the_entry_buttons_ask_the_app_for_a_panel() {
        let mut harness = screen_harness(state_with(Vec::new(), Vec::new()));
        harness.get_by_label("Files…").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::PickFiles]);

        let mut harness = screen_harness(state_with(Vec::new(), Vec::new()));
        harness.get_by_label("Folder…").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::PickFolder]);

        // The download folder button is a panel too (`PickDownloadDir`).
        let mut harness = screen_harness(state_with(Vec::new(), Vec::new()));
        harness.get_by_label("Movies").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::PickDownloadDir]);
    }

    #[test]
    fn the_download_folder_button_names_the_folder_that_will_be_written() {
        // A folder chosen for this download (`S.dlDest`) wins over the setting.
        let mut state = state_with(Vec::new(), Vec::new());
        state.download_dir = "/Users/chaos/Movies/Incoming/Dailies".to_string();
        let harness = screen_harness(state);
        harness.get_by_label("Dailies");
        assert_eq!(download_dir(harness.state()), "/Users/chaos/Movies/Incoming/Dailies");

        // Nothing chosen: the setting is what the button names, as before.
        let harness = screen_harness(state_with(Vec::new(), Vec::new()));
        harness.get_by_label("Movies");
        assert_eq!(download_dir(harness.state()), "/Users/chaos/Movies");
    }

    #[test]
    fn the_composer_asks_for_the_clipboard_and_takes_what_the_shell_puts_back() {
        let mut harness = screen_harness(state_with(Vec::new(), Vec::new()));
        harness.get_by_label("Paste").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::Paste]);

        // The shell answers by writing `UiState::link`; the field shows it, and
        // the composer hands it back rather than overwriting it with its buffer.
        harness.state_mut().link = "https://acme.frame.io/s/abc".to_string();
        harness.run();
        assert_eq!(harness.state().link, "https://acme.frame.io/s/abc");
        harness.run();
        assert_eq!(harness.state().link, "https://acme.frame.io/s/abc");
    }

    #[test]
    fn a_link_the_state_already_holds_is_the_one_the_field_starts_with() {
        // A fresh window with a link already in the state: the composer adopts it
        // instead of starting empty and wiping it (`Composer::load`).
        let mut state = state_with(Vec::new(), Vec::new());
        state.link = "https://acme.frame.io/s/abc".to_string();
        state.passphrase = "secret".to_string();
        state.new_folder = true;
        let harness = screen_harness(state);
        assert_eq!(harness.state().link, "https://acme.frame.io/s/abc");
        assert_eq!(harness.state().passphrase, "secret");
        assert!(harness.state().new_folder);
        // A link in the state enables Download, so the passphrase field is drawn
        // only when `InspectLink` asks for it — here it does not.
        harness.get_by_label("Download");
    }

    #[test]
    fn the_row_actions_ask_the_app_for_what_they_draw() {
        // One click, one request: each harness answers a single row action.
        let mut harness = screen_harness(state_with(three_jobs(), Vec::new()));
        harness.get_by_label("Cancel").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::Cancel("a".to_string())]);

        let mut harness = screen_harness(state_with(three_jobs(), Vec::new()));
        harness.get_by_label("Retry").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::Retry("b".to_string())]);

        let mut harness = screen_harness(state_with(three_jobs(), Vec::new()));
        harness.get_by_label("Copy share link").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::ShareLink("c".to_string())]);

        // `.a-reveal` reveals the job's destination, found by its id.
        let mut harness = screen_harness(state_with(three_jobs(), Vec::new()));
        harness.get_by_label("Show in Finder").click_accesskit();
        harness.run();
        assert_eq!(
            harness.state().requests,
            vec![Request::Reveal("~/Movies/Reel 1".to_string())]
        );

        let mut harness = screen_harness(state_with(three_jobs(), Vec::new()));
        harness.get_by_label("Open in Frame.io").click_accesskit();
        harness.run();
        assert_eq!(
            harness.state().requests,
            vec![Request::OpenUrl("https://acme.frame.io/share/xyz".to_string())]
        );

        // `.head-actions`' "Clear finished".
        let mut harness = screen_harness(state_with(three_jobs(), Vec::new()));
        harness.get_by_label("Clear finished").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::ClearFinished]);
    }

    #[test]
    fn the_destination_chip_opens_the_picker() {
        let mut harness = screen_harness(state_with(Vec::new(), Vec::new()));
        harness.get_by_label("Delivery / Reel 1").click_accesskit();
        harness.run();
        // An accessibility click has no pointer position: the whole destination.
        assert_eq!(
            harness.state().requests,
            vec![Request::OpenDest { mode: DestMode::Upload { pending: Vec::new() }, at: None }]
        );
        assert!(harness.state().toasts.is_empty());
    }

    #[test]
    fn the_signed_out_composer_asks_to_sign_in_or_opens_settings() {
        // `#cta-signin` is `login()`.
        let mut state = state_with(Vec::new(), Vec::new());
        state.logged_in = false;
        state.settings.client_id = "abc".to_string();
        let mut harness = screen_harness(state);
        harness.get_by_label("Sign in").click_accesskit();
        harness.run();
        assert_eq!(harness.state().requests, vec![Request::Login]);

        // `#cta-setup` is `showView('settings')`: the client id is typed there.
        let mut state = state_with(Vec::new(), Vec::new());
        state.logged_in = false;
        state.settings.client_id = String::new();
        let mut harness = screen_harness(state);
        harness.get_by_label("Set up").click_accesskit();
        harness.run();
        assert_eq!(harness.state().view, View::Settings);
        assert!(harness.state().requests.is_empty());
    }

    #[test]
    fn the_preview_line_is_the_one_set_preview_builds() {
        let info = LinkInfo {
            name: "Nightfall dailies".to_string(),
            files: 42,
            total_bytes: 21_500_000_000,
            ..LinkInfo::default()
        };
        assert_eq!(preview_line(&info), "Nightfall dailies · 42 files · 21.5 GB");

        let mut one = LinkInfo { files: 1, ..LinkInfo::default() };
        assert_eq!(preview_line(&one), "Shared files · 1 file · 0 B");
        one.unavailable = 2;
        assert_eq!(preview_line(&one), "Shared files · 1 file · 0 B · 2 unavailable");
    }

    #[test]
    fn the_passphrase_field_appears_when_the_link_needs_one() {
        use egui::accesskit::Role;
        // A typed link with no answer yet: one field, Download live.
        let mut state = state_with(Vec::new(), Vec::new());
        state.link = "https://acme.frame.io/s/abc".to_string();
        let harness = screen_harness(state);
        assert_eq!(harness.query_all_by_role(Role::TextInput).count(), 1);
        assert!(harness.query_by_role(Role::PasswordInput).is_none());

        // `InspectLink` says the link needs a passphrase: the second field appears.
        let mut state = state_with(Vec::new(), Vec::new());
        state.link = "https://acme.frame.io/s/abc".to_string();
        state.link_info = Some(LinkInfo {
            needs_passphrase: true,
            error: "This link needs a passphrase".to_string(),
            ..LinkInfo::default()
        });
        let harness = screen_harness(state);
        harness.get_by_role(Role::PasswordInput); // `#passphrase`, now visible

        // A link the app rejected outright leaves Download dim.
        let mut state = state_with(Vec::new(), Vec::new());
        state.link = "https://acme.frame.io/s/nope".to_string();
        state.link_info = Some(LinkInfo {
            error: "That link is not a share".to_string(),
            ..LinkInfo::default()
        });
        let mut harness = screen_harness(state);
        harness.get_by_label("Download").click_accesskit();
        harness.run();
        assert!(harness.state().requests.is_empty(), "a rejected link cannot download");
    }

    #[test]
    fn a_resolving_job_shows_its_phase_instead_of_a_bar() {
        let mut job = stamp_job("r", "download", "portal-drop", Status::Resolving, 10);
        job.phase = "Reading the folder tree".to_string();
        job.files_total = 12;
        let state = screen_harness(state_with(vec![job], Vec::new()));
        // The phase is drawn where the bar would be, and the pill says "Preparing".
        assert_eq!(state.query_all_by_label("Reading the folder tree").count(), 1);
        state.get_by_label("Preparing");
        // ... and there is no bar to read: no percentage, no fraction.
        let job = &state.state().snapshot.jobs[0];
        assert_eq!(percent_text(job), "");
        assert_eq!(bar_fraction(job), None);
    }
}
