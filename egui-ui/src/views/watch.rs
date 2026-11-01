//! The Watch Folders screen.
//!
//! The webview's table (`#view-watch .watch-grid` in `index.html`): a header with
//! "Add", then one 56 px row per watched folder — the folder and its path, the
//! Frame.io destination with the direction icon, the status dot and word, the
//! file count, the last transfer, and the row's actions (pause/resume, edit,
//! show in Finder, remove). The empty state is "No watch folders".
//!
//! A Portal-managed project is one job, not two rows: a **Smart In/Out Tray**
//! whose title line is the project's name and then `· Smart In/Out Tray` in the
//! row's detail style, with a line per sync lane under it — `Inbound (In Tray)`
//! and `Outbound (Out Tray)` — because Portal publishes a project as two one-way
//! folders and the person watching it sees one job. The job's title line also
//! carries `Open in Portal`, which opens the project's link on the Portal server.
//! A duplex with one lane configured shows that lane and says plainly which one
//! is missing. A watch the user made is still one row of its own, drawn exactly
//! as it was.
//!
//! The pencil opens the **watch folder editor** (`.modal-box` over the list): the
//! local folder — typed, or chosen from the folder panel — the Frame.io
//! destination's account and folder ids, a label, the direction, the rule for a
//! file that changes, and the checkbox for files already there. "Add" opens the
//! same dialog in create mode. A watch Portal owns opens it read-only, with the
//! sentence that says where to change it and no Save: the next reconcile would
//! write over anything an edit here kept.
//!
//! The shell (`crate::app`) supplies the `.view` padding, the scroll area and
//! the page head; the tokens come from [`crate::theme`].

use crate::model::{
    fit_crumbs, watch_blocks, DestMode, Lane, Request, UiState, WatchEditor, WatchRow, DIRECTION_DOWNLOAD,
    DIRECTION_MIRROR, DIRECTION_UPLOAD, ON_CHANGE_KEEP, ON_CHANGE_OVERWRITE,
};
use crate::theme;
use egui::{
    Align, Checkbox, Color32, CornerRadius, Id, Label, Layout, Margin, Painter, Rect, RichText,
    Sense, Shape, Stroke, StrokeKind, TextEdit, WidgetInfo, WidgetType, pos2, vec2,
};

/// The sidebar's width: the stylesheet's media queries are about the *window*,
/// and a screen only knows the main column it was given.
const SIDEBAR_W: f32 = theme::size::SIDEBAR_W;

// `.list-head { height: 32px }`, `.row-main { min-height: 56px }`.
const HEAD_HEIGHT: f32 = theme::size::LIST_HEAD_H;
/// `.row-main { min-height: 56px }` plus its own `border-bottom: 1px`, so a
/// row measures 57 px from border to border.
const ROW_HEIGHT: f32 = theme::size::ROW_H + 1.0;
/// `.row, .list-head { padding: 0 12px }`, `.watch-grid { gap: 12px }`.
const ROW_PADDING_X: f32 = theme::space::ROW_X;
const COLUMN_GAP: f32 = theme::space::XL;
/// `.watch-grid`: `minmax(0, 1.3fr) minmax(0, 1fr) 150px 140px 124px 150px`.
const STATUS_WIDTH: f32 = 150.0;
const FILES_WIDTH: f32 = 140.0;
const LAST_WIDTH: f32 = 124.0;
const ACTIONS_WIDTH: f32 = 150.0;
const MIN_NAME_WIDTH: f32 = 140.0;
const MIN_DEST_WIDTH: f32 = 110.0;

/// `WATCH_STATUS` in app.js, for the two states a [`WatchRow`] can be in.
const STATUS_WATCHING: &str = "Watching";
const STATUS_PAUSED: &str = "Paused";

/// The marker a watch Portal owns carries, on its detail line, where the screen
/// already puts its secondary text.
pub const PORTAL_MARKER: &str = "Portal";
/// What the marker means, and the answer to the one thing this screen cannot do:
/// a Portal watch removed here is recreated by the next reconcile, because
/// Portal still asks for it. The row keeps its Remove button — the sentence says
/// what pressing it costs, rather than the screen hiding a button that would
/// lie about being final.
pub const PORTAL_SENTENCE: &str =
    "Kept in step by Portal. Remove the marker in Portal to drop it.";

// The words the job a duplex is, and its lanes, are said in. This is the
// screen's copy; the code behind it is `Lane`, `WatchRow::duplex_id` and
// [`crate::model::watch_blocks`].

/// The job one Portal-managed project is, after its destination label.
pub const SMART_IN_OUT_TRAY: &str = "Smart In/Out Tray";
/// The words that say what the job is, after the project's name on the title
/// line: [`SMART_IN_OUT_TRAY`] behind the separator the title has always used,
/// set in the row's own detail style — one step down from the name's body, and
/// muted — so the project is what the row reads first. It used to be one run with
/// the name, which made the tray read as loud as the project.
pub const TRAY_SUFFIX: &str = "· Smart In/Out Tray";
/// The job's way into Portal, on its title line beside Remove.
pub const OPEN_IN_PORTAL: &str = "Open in Portal";
/// Why [`OPEN_IN_PORTAL`] does nothing: the address it opens is a setting, and an
/// empty one is how Portal is switched off. The link says this on hover rather
/// than vanishing, so the row always offers the same things.
pub const PORTAL_ADDRESS_MISSING: &str = "set the Portal address in Settings";
/// The inbound lane, its tray in brackets: the download watch on the client's
/// folder.
pub const LANE_INBOUND: &str = "Inbound (In Tray)";
/// The outbound lane, its tray in brackets: the upload watch on the exports
/// folder.
pub const LANE_OUTBOUND: &str = "Outbound (Out Tray)";
/// What a lane with no watch behind it says, after its label and an em dash.
pub const LANE_NOT_CONFIGURED: &str = "not configured";

/// A lane's own words on screen.
fn lane_label(lane: Lane) -> &'static str {
    match lane {
        Lane::Inbound => LANE_INBOUND,
        Lane::Outbound => LANE_OUTBOUND,
        Lane::None => "",
    }
}

/// What the screen asks the shell to do.
///
/// A view is a pure renderer over [`UiState`], so everything that needs the
/// disk or Frame.io is reported back instead of called: the shell turns these
/// into `call('PickWatchFolder')`, `call('SetWatchEnabled', …)` and so on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// "Add": open the editor for a watch that does not exist yet.
    AddWatch,
    /// The pause/resume button; `index` points into `state.watches`.
    SetEnabled { index: usize, enabled: bool },
    /// Open the watch folder editor for this row (`.w-edit`), with the values the
    /// row is showing. On a watch Portal owns it opens read-only.
    Edit(usize),
    /// The editor's "Choose…": a folder panel for the local folder.
    ChooseFolder,
    /// The editor's Frame.io folder: the destination picker.
    ChooseDestination,
    /// The editor's Save: one watch's fields, as they were left in the dialog.
    /// [`ask`] turns it into `UpdateWatch` or `AddWatch`.
    SaveWatch(WatchEditor),
    /// Remove the watch folder (`.w-remove`).
    Remove(usize),
    /// Remove a job's lanes together: one Remove on a Smart In/Out Tray, which
    /// takes both of the watches Portal made for it.
    RemoveDuplex { rows: Vec<usize> },
    /// The job's `Open in Portal`: the page for this project's upload link on
    /// the Portal server, `{portal_url}/upload-links/{link id}`. Both lanes are
    /// the same link, so the job needs one of these, not one per lane.
    OpenInPortal(String),
    /// Show the folder in Finder (`.w-reveal`).
    Reveal(usize),
}

/// The screen, wired to the app: whatever [`show_with_actions`] reports is
/// handed to the shell as a [`Request`].
pub fn show(ui: &mut egui::Ui, state: &mut UiState) {
    if let Some(action) = show_with_actions(ui, state) {
        ask(state, action);
    }
}

/// Hands one [`Action`] to the app, in the app's terms.
///
/// A watch call is addressed by the app's watch id, which every row carries: the
/// index the row reported is looked up on the state the click was drawn from, so
/// the id is that row's and no other.
fn ask(state: &mut UiState, action: Action) {
    match action {
        Action::SetEnabled { index, enabled } => {
            if let Some(id) = state.watches.get(index).map(|watch| watch.id.clone()) {
                state.ask(Request::SetWatchEnabled { id, on: enabled });
            }
        }
        Action::Remove(index) => {
            if let Some(id) = state.watches.get(index).map(|watch| watch.id.clone()) {
                state.ask(Request::RemoveWatch(id));
            }
        }
        Action::RemoveDuplex { rows } => {
            // The job's lanes go together: Portal made them as one project, and
            // taking one would leave a lane standing with nothing above it. The
            // sentence on the job says Portal puts them back unless its own
            // marker is cleared.
            let ids: Vec<String> = rows
                .iter()
                .filter_map(|index| state.watches.get(*index).map(|watch| watch.id.clone()))
                .collect();
            for id in ids {
                state.ask(Request::RemoveWatch(id));
            }
        }
        Action::Reveal(index) => {
            if let Some(local) = state.watches.get(index).map(|watch| watch.local.clone()) {
                state.ask(Request::Reveal(local));
            }
        }

        // The pencil opens the editor with the values the row is drawing, so what
        // the dialog shows is what the app has — not a stale copy of it.
        Action::Edit(index) => {
            let editor = state.watches.get(index).map(WatchEditor::editing);
            if let Some(editor) = editor {
                state.watch_editor = Some(editor);
            }
        }
        // "Add" opens the same dialog with nothing in it but the destination the
        // app remembers; the folder is chosen inside it.
        Action::AddWatch => {
            let editor = WatchEditor::creating(&state.settings);
            state.watch_editor = Some(editor);
        }
        Action::ChooseFolder => state.ask(Request::PickWatchFolder),
        Action::ChooseDestination => state.ask(Request::OpenDest { mode: DestMode::Watch, at: None }),
        // Save is the only thing that asks the app for anything; the dialog is
        // closed first, so a refusal is a toast over the list rather than over a
        // form the user can no longer fix.
        Action::SaveWatch(editor) => {
            state.watch_editor = None;
            state.ask(editor.request());
        }
        // The shell opens it in the browser (`App::open_url`): the app already
        // has the one way out to a URL, and this is that way.
        Action::OpenInPortal(url) => state.ask(Request::OpenUrl(url)),
    }
}

/// The screen, reporting at most one [`Action`] per frame.
pub fn show_with_actions(ui: &mut egui::Ui, state: &mut UiState) -> Option<Action> {
    let mut action = None;
    // The stylesheet's blocks abut: `.list { margin-top: 20px }`, and a row's
    // only gap is its 1 px border. The shell's 8 px item spacing would add a
    // gap to every one of those, so it is zeroed here and the gaps are explicit.
    ui.spacing_mut().item_spacing = vec2(theme::space::M, 0.0);

    crate::app::page_head(ui, "Watch Folders", |ui| {
        if primary_button(ui, "Add") {
            action = Some(Action::AddWatch);
        }
    });

    // `.list { margin-top: 20px }`
    ui.add_space(theme::space::HUGE);

    if state.watches.is_empty() {
        empty_state(ui, "No watch folders");
        // An empty list can still have a dialog open: that is what Add is for.
        editor(ui, state, &mut action);
        return action;
    }

    let columns = Columns::for_width(ui.available_width());
    list_head(ui, &columns);
    // One block per job: a duplex's lanes under its title, a watch the user made
    // on its own row. The block's rows are indices into `state.watches`, which is
    // what every action is addressed by. The job's `Open in Portal` needs the
    // server's address off `Settings`, which is not a row's: it is read here, so
    // the rows stay indices and the address stays one value for the screen.
    let home = state.home.clone();
    let portal_url = state.settings.portal_url.clone();
    for block in watch_blocks(&state.watches) {
        match &block.duplex_id {
            Some(_) => duplex_block(
                ui,
                &block.rows,
                &mut state.watches,
                &home,
                &portal_url,
                &columns,
                &mut action,
            ),
            None => {
                for index in &block.rows {
                    watch_row(ui, *index, &mut state.watches[*index], &home, &columns, &mut action);
                }
            }
        }
    }

    // The editor goes last and on top, as the Space screen's confirmation does:
    // `egui::Modal` puts it in the foreground layer and keeps clicks off the rows
    // underneath.
    editor(ui, state, &mut action);

    action
}

/// `.watch-grid`, with the columns the stylesheet drops on a narrow window.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Columns {
    name: f32,
    dest: f32,
    status: f32,
    files: Option<f32>,
    last: Option<f32>,
    actions: f32,
}

impl Columns {
    fn for_width(available: f32) -> Self {
        // `@media (max-width: 1279px) { .watch-grid > .w-last { display: none } }`
        // and `@media (max-width: 1099px) { … > .w-uploaded { display: none } }`.
        let window = available + SIDEBAR_W;
        let files = if window > 1099.0 { Some(FILES_WIDTH) } else { None };
        let last = if window > 1279.0 { Some(LAST_WIDTH) } else { None };

        let mut fixed = STATUS_WIDTH + ACTIONS_WIDTH;
        if let Some(width) = files {
            fixed += width;
        }
        if let Some(width) = last {
            fixed += width;
        }
        let mut gaps = 5.0 * COLUMN_GAP;
        if files.is_none() {
            gaps -= COLUMN_GAP;
        }
        if last.is_none() {
            gaps -= COLUMN_GAP;
        }

        // 1.3fr for the folder, 1fr for the destination, in what is left.
        let free = (available - ROW_PADDING_X * 2.0 - fixed - gaps).max(MIN_NAME_WIDTH + MIN_DEST_WIDTH);
        let name = free * 1.3 / 2.3;
        let dest = free - name;

        Columns {
            name: name.max(MIN_NAME_WIDTH),
            dest: dest.max(MIN_DEST_WIDTH),
            status: STATUS_WIDTH,
            files,
            last,
            actions: ACTIONS_WIDTH,
        }
    }
}

/// `.list-head`: the dim 11.5 px row of column names.
fn list_head(ui: &mut egui::Ui, columns: &Columns) {
    strip(ui, HEAD_HEIGHT, false, |ui| {
        head_cell(ui, columns.name, false, "Folder");
        head_cell(ui, columns.dest, false, "Frame.io");
        head_cell(ui, columns.status, false, "Status");
        if let Some(width) = columns.files {
            head_cell(ui, width, true, "Files");
        }
        if let Some(width) = columns.last {
            head_cell(ui, width, false, "Last transfer");
        }
        cell(ui, columns.actions, HEAD_HEIGHT, false, |_ui| {});
    });
}

fn head_cell(ui: &mut egui::Ui, width: f32, right: bool, text: &str) {
    cell(ui, width, HEAD_HEIGHT, right, |ui| {
        ui.add(theme::truncating(
            RichText::new(text)
                .font(theme::caption())
                .color(theme::TEXT_3)
                .strong(),
        ));
    });
}

/// One watched folder (`.row .row-main`): a watch the user made, or a Portal row
/// whose origin names no sync lane this screen can place. Both draw exactly what
/// they drew before there were job blocks — the folder and its path, the
/// Frame.io destination, the status, the file count, and the row's actions.
fn watch_row(
    ui: &mut egui::Ui,
    index: usize,
    watch: &mut WatchRow,
    home: &str,
    columns: &Columns,
    action: &mut Option<Action>,
) {
    let name = base_name(&watch.local);
    let path = short_path(&watch.local, home);

    let row = strip(ui, ROW_HEIGHT, true, |ui| {
        // Folder: the 30 px tile, then the name and the shortened path.
        cell(ui, columns.name, ROW_HEIGHT, false, |ui| {
            ui.spacing_mut().item_spacing.x = theme::space::XL;
            icon_tile(ui, Glyph::Folder);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(theme::truncating(
                    RichText::new(&name)
                        .font(theme::body())
                        .color(if watch.enabled { theme::TEXT } else { theme::TEXT_2 })
                        .strong(),
                ))
                .on_hover_text(&watch.local);
                // The detail line: the path, or — on a watch Portal owns —
                // `Portal` and then the path, the marker in the same muted 12 px
                // the path is set in. A watch the user made takes the first
                // branch and draws this line exactly as it did before there was
                // a second one.
                if watch.from_portal {
                    // One line, the height of the path's own text: a plain
                    // `ui.horizontal` would centre the line in whatever is left
                    // under the name and drop it, which the user's rows must not
                    // get and this one should not have either.
                    detail_line(ui, |ui| {
                        portal_marker(ui).on_hover_text(PORTAL_SENTENCE);
                        path_line(ui, &path, &watch.local);
                    });
                } else {
                    path_line(ui, &path, &watch.local);
                }
            });
        });

        middle_cells(ui, watch, columns);
        actions_cell(ui, columns, index, watch, Some(Action::Remove(index)), action);
    });

    if watch.from_portal {
        // Anywhere on the row says whose it is. The marker is four words wide and
        // the sentence is the whole answer, so the tooltip is on the strip as
        // well as on the marker itself.
        row.on_hover_text(PORTAL_SENTENCE);
    }
}

/// One Portal-managed project as one job: the title line, then a line for each
/// sync lane under it.
///
/// The rows are the job's lanes in `UiState::watches`, inbound first — the order
/// [`crate::model::watch_blocks`] put them in. A lane the link has no watch for
/// is drawn as the line that says so, rather than left out: the job is a Smart
/// In/Out Tray, and half of it being missing is part of what there is to see.
fn duplex_block(
    ui: &mut egui::Ui,
    rows: &[usize],
    watches: &mut [WatchRow],
    home: &str,
    portal_url: &str,
    columns: &Columns,
    action: &mut Option<Action>,
) {
    let Some(&first) = rows.first() else {
        return;
    };
    let label = watches[first].label.clone();
    // The page the job's one link is on, or nothing to open: `duplex_id` is the
    // record's `origin` with the outbound lane's `#exports` already off, so both
    // lanes answer the same address.
    let link_url = watches[first]
        .duplex_id
        .as_deref()
        .and_then(|id| portal_link_url(portal_url, id));
    job_head(ui, &label, link_url.as_deref(), rows, columns, action);

    for lane in [Lane::Inbound, Lane::Outbound] {
        match rows.iter().copied().find(|&index| watches[index].lane == lane) {
            Some(index) => sync_lane_row(ui, index, &mut watches[index], home, columns, action),
            None => missing_lane(ui, lane, columns),
        }
    }
}

/// The job's title line: the project's name, then [`TRAY_SUFFIX`] beside it in
/// the row's detail style, and at the right the job's own actions — `Open in
/// Portal`, and the one Remove that takes both lanes.
///
/// `link_url` is the Portal page the job's link is on; `None` is a Portal address
/// not set, which is why `Open in Portal` is drawn off there rather than left
/// out.
fn job_head(
    ui: &mut egui::Ui,
    label: &str,
    link_url: Option<&str>,
    rows: &[usize],
    columns: &Columns,
    action: &mut Option<Action>,
) {
    let head = strip(ui, ROW_HEIGHT, true, |ui| {
        cell(ui, columns.name, ROW_HEIGHT, false, |ui| {
            ui.spacing_mut().item_spacing.x = theme::space::XL;
            icon_tile(ui, Glyph::Folder);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                // One line, two runs: the project at the row's own weight, and
                // the words that say what the job is one step down and muted, so
                // the project is what reads first.
                title_line(ui, |ui| {
                    ui.add(theme::truncating(
                        RichText::new(label)
                            .font(theme::body())
                            .color(theme::TEXT)
                            .strong(),
                    ));
                    ui.add(theme::truncating(
                        RichText::new(TRAY_SUFFIX)
                            .font(theme::detail())
                            .color(theme::TEXT_3),
                    ));
                });
                // The second line is the marker a Portal row already carries, in
                // the same muted 12 px: the job is Portal's, and the paths are
                // on the lanes below rather than here.
                detail_line(ui, |ui| {
                    portal_marker(ui).on_hover_text(PORTAL_SENTENCE);
                });
            });
        });

        // The middle columns stay empty on the title line, so the job's actions
        // land in the column the rows keep theirs in.
        empty_cells(ui, columns);
        cell(ui, columns.actions, ROW_HEIGHT, false, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_button_with(ui, Glyph::Trash, "Remove", PORTAL_SENTENCE) {
                    *action = Some(Action::RemoveDuplex { rows: rows.to_vec() });
                }
                // Beside the Remove, the one thing the job offers that acts on
                // Portal rather than on the watches: the page both lanes are.
                // It answers with the address it was clicked with, so a link
                // drawn off for want of a Portal address asks for nothing.
                if let Some(url) = open_in_portal(ui, link_url) {
                    *action = Some(Action::OpenInPortal(url.to_string()));
                }
            });
        });
    });

    // One sentence for the job, the one a Portal row already carries: removing
    // this here is not the end of it, and the Remove above is not hidden for
    // saying so.
    head.on_hover_text(PORTAL_SENTENCE);
}

/// One sync lane of a job: the lane's words, its folder and path, the Frame.io
/// side, and the lane's own state — the same cells a row draws, in the same
/// fonts and the same 56 px row.
fn sync_lane_row(
    ui: &mut egui::Ui,
    index: usize,
    watch: &mut WatchRow,
    home: &str,
    columns: &Columns,
    action: &mut Option<Action>,
) {
    let lane = lane_label(watch.lane).to_string();
    let path = short_path(&watch.local, home);

    strip(ui, ROW_HEIGHT, true, |ui| {
        cell(ui, columns.name, ROW_HEIGHT, false, |ui| {
            // Indented under the title's text: the folder tile's width and its
            // gap, so a lane reads as part of the job above it.
            ui.add_space(theme::size::KIND_ICON + theme::space::XL);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(theme::truncating(
                    RichText::new(&lane)
                        .font(theme::body())
                        .color(if watch.enabled { theme::TEXT } else { theme::TEXT_2 })
                        .strong(),
                ));
                path_line(ui, &path, &watch.local);
            });
        });

        middle_cells(ui, watch, columns);
        // The lane keeps every action but Remove: one Remove, on the job.
        actions_cell(ui, columns, index, watch, None, action);
    });
}

/// The lane a job does not have: its words, then what is missing — `Outbound
/// (Out Tray) — not configured`.
fn missing_lane(ui: &mut egui::Ui, lane: Lane, columns: &Columns) {
    let text = format!("{} — {LANE_NOT_CONFIGURED}", lane_label(lane));

    strip(ui, ROW_HEIGHT, false, |ui| {
        cell(ui, columns.name, ROW_HEIGHT, false, |ui| {
            ui.add_space(theme::size::KIND_ICON + theme::space::XL);
            ui.add(theme::truncating(
                RichText::new(&text)
                    .font(theme::body())
                    .color(theme::TEXT_3),
            ));
        });
        empty_cells(ui, columns);
        cell(ui, columns.actions, ROW_HEIGHT, false, |_ui| {});
    });
}

/// The Frame.io, status, files and last-transfer cells of a row — the same on a
/// lane and on a watch the user made.
fn middle_cells(ui: &mut egui::Ui, watch: &WatchRow, columns: &Columns) {
    let verb = direction_verb(&watch.direction);
    let status = if watch.enabled { STATUS_WATCHING } else { STATUS_PAUSED };

    // Frame.io: the direction icon and the destination label.
    cell(ui, columns.dest, ROW_HEIGHT, false, |ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        glyph(ui, Glyph::of_direction(&watch.direction), theme::TEXT_3);
        ui.add(theme::truncating(
            RichText::new(&watch.label)
                .font(theme::body())
                .color(theme::TEXT_2),
        ))
        .on_hover_text(format!("{verb} {}", watch.label));
    });

    // Status: the dot and the word (`.status.watching` / `.status.paused`).
    cell(ui, columns.status, ROW_HEIGHT, false, |ui| {
        let key = if watch.enabled { "watching" } else { "paused" };
        dot(ui, theme::status_color(key));
        ui.add(theme::truncating(
            RichText::new(status)
                .font(theme::note())
                .color(theme::TEXT_2),
        ));
    });

    if let Some(width) = columns.files {
        cell(ui, width, ROW_HEIGHT, true, |ui| {
            ui.add(theme::truncating(
                RichText::new(count_word(watch.pending))
                    .font(theme::body())
                    .color(theme::TEXT_2),
            ))
            .on_hover_text(format!("{} pending", watch.pending));
        });
    }

    if let Some(width) = columns.last {
        cell(ui, width, ROW_HEIGHT, false, |ui| {
            // `WatchRow` carries no last-transfer time; the webview prints
            // an em dash when there is none.
            ui.label(
                RichText::new("—")
                    .font(theme::body())
                    .color(theme::TEXT_2),
            );
        });
    }
}

/// The middle columns with nothing in them, for a line that has no figures of
/// its own — the job's title, and a lane that is not configured.
fn empty_cells(ui: &mut egui::Ui, columns: &Columns) {
    cell(ui, columns.dest, ROW_HEIGHT, false, |_ui| {});
    cell(ui, columns.status, ROW_HEIGHT, false, |_ui| {});
    if let Some(width) = columns.files {
        cell(ui, width, ROW_HEIGHT, true, |_ui| {});
    }
    if let Some(width) = columns.last {
        cell(ui, width, ROW_HEIGHT, false, |_ui| {});
    }
}

/// A row's actions, right to left: pause/resume, edit, show in Finder, and the
/// Remove when the row owns one.
fn actions_cell(
    ui: &mut egui::Ui,
    columns: &Columns,
    index: usize,
    watch: &mut WatchRow,
    remove: Option<Action>,
    action: &mut Option<Action>,
) {
    cell(ui, columns.actions, ROW_HEIGHT, false, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if let Some(remove) = remove {
                if icon_button(ui, Glyph::Trash, "Remove") {
                    *action = Some(remove);
                }
            }
            if icon_button(ui, Glyph::Reveal, "Show in Finder") {
                *action = Some(Action::Reveal(index));
            }
            if icon_button(ui, Glyph::Pencil, "Edit") {
                *action = Some(Action::Edit(index));
            }
            let (glyph, label) = if watch.enabled {
                (Glyph::Pause, "Pause")
            } else {
                (Glyph::Play, "Resume")
            };
            if icon_button(ui, glyph, label) {
                let enabled = !watch.enabled;
                watch.enabled = enabled;
                *action = Some(Action::SetEnabled { index, enabled });
            }
        });
    });
}

// ---------- the watch folder editor ----------

/// The dialog's words. They are quoted in the tests, so none of them can be
/// softened without a test noticing.
pub const EDITOR_TITLE_EDIT: &str = "Edit watch folder";
pub const EDITOR_TITLE_NEW: &str = "Add a watch folder";
/// What a watch Portal owns says, where the Save would be.
pub const EDITOR_PORTAL: &str = "Kept in step by Portal — change it in Portal";
pub const EDITOR_SAVE: &str = "Save";
pub const EDITOR_CANCEL: &str = "Cancel";
/// The read-only dialog has nothing to cancel, so it says what it does instead.
pub const EDITOR_CLOSE: &str = "Close";
pub const EDITOR_CHOOSE: &str = "Choose…";
pub const EDITOR_LABEL_FOLDER: &str = "Local folder";
pub const EDITOR_LABEL_DEST: &str = "Destination";
/// The destination button's name; it shows the folder's path, or
/// [`EDITOR_DEST_NONE`] before one is chosen.
pub const EDITOR_DEST_BUTTON: &str = "Frame.io folder";
pub const EDITOR_DEST_NONE: &str = "Choose destination";
pub const EDITOR_LABEL_NAME: &str = "Label";
pub const EDITOR_LABEL_DIRECTION: &str = "Direction";
pub const EDITOR_LABEL_ON_CHANGE: &str = "On change";
pub const EDITOR_INCLUDE: &str = "Include files already there";
/// What the checkbox means, under it: the core reads it on every save, and only a
/// side that is new has anything already there to include.
pub const EDITOR_INCLUDE_HELP: &str =
    "Ticked, what is already in the folder is transferred too. Unticked, only what \
     appears after the save is.";

/// The modal's id, so egui keeps its place and the backdrop across frames.
const EDITOR_ID: &str = "fiotransfer-egui.watch.editor";
/// `.edit-row`'s label column is 80 px, as the Space screen's dialog has it.
const EDITOR_LABEL_W: f32 = 80.0;
/// Room kept for "Choose…" on the local-folder row, as `.set-line` keeps 100 px
/// for "Change…".
const EDITOR_CHOOSE_W: f32 = 100.0;

/// The two segmented controls, in the order the stylesheet's `.segmented` has
/// them: the core's value first, then the words on screen.
const DIRECTIONS: [(&str, &str); 3] = [
    (DIRECTION_UPLOAD, "Upload"),
    (DIRECTION_DOWNLOAD, "Download"),
    (DIRECTION_MIRROR, "Mirror"),
];
const ON_CHANGES: [(&str, &str); 2] = [(ON_CHANGE_OVERWRITE, "Overwrite"), (ON_CHANGE_KEEP, "Keep")];

/// What the dialog decided this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    None,
    Cancel,
    Save,
    Choose,
    Destination,
}

/// Draws the editor when one is open, and reports what it decided.
///
/// Cancel and Save both close it here, so the dialog the next frame draws is the
/// one the shell asked for — never a form still open over a list that has moved
/// on under it.
fn editor(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let Some(editor) = state.watch_editor.as_mut() else {
        return;
    };
    let ctx = ui.ctx().clone();
    // The destination picker opens over the dialog: its keys aren't the dialog's.
    let covered = state.dest_picker.is_some();
    match editor_dialog(&ctx, editor, covered) {
        Decision::None => {}
        Decision::Cancel => state.watch_editor = None,
        Decision::Choose => *action = Some(Action::ChooseFolder),
        Decision::Destination => *action = Some(Action::ChooseDestination),
        Decision::Save => {
            if let Some(editor) = state.watch_editor.take() {
                *action = Some(Action::SaveWatch(editor));
            }
        }
    }
}

/// `.modal-box`: the folder, the Frame.io destination, the two rules, and the
/// checkbox for files already there.
///
/// Cancel is the default in the same three ways the Space screen's confirmation
/// makes it one: it takes the keyboard focus when the dialog opens, Enter takes
/// it, and so do Escape and a click on the backdrop. Save is the only control
/// that asks the app for anything, and it is off — with the reason under the
/// fields — until every field it needs is usable. A watch Portal owns has no
/// Save at all: its values are shown, off, and the sentence says where to change
/// them.
fn editor_dialog(ctx: &egui::Context, editor: &mut WatchEditor, covered: bool) -> Decision {
    let mut decision = Decision::None;
    let mut cancel_clicked = false;
    let mut save_clicked = false;
    let mut choose_clicked = false;
    let mut dest_clicked = false;

    let width = (ctx.content_rect().width() * 0.92).min(theme::size::MODAL_W);
    let read_only = editor.read_only;
    let reason = editor.reason();
    let title = if editor.id.is_some() { EDITOR_TITLE_EDIT } else { EDITOR_TITLE_NEW };
    let hint = editor.label_or_folder();

    let modal = egui::Modal::new(Id::new(EDITOR_ID))
        .frame(theme::modal_frame())
        .backdrop_color(theme::SCRIM_MODAL)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);

            // `.modal-head { padding: 14px 12px 10px 18px }`: the title, and the
            // close cross — the same dismissal as Cancel, said in one glyph.
            let _ = egui::Frame::new()
                .inner_margin(Margin { left: 18, right: 12, top: 14, bottom: 10 })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = theme::space::M;
                        ui.add(Label::new(
                            RichText::new(title)
                                .font(theme::section())
                                .color(theme::TEXT)
                                .strong(),
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // The cross is the same dismissal as Cancel, and it is
                            // drawn only when there is something to discard: a
                            // read-only dialog has one way out, the Close in its
                            // foot, rather than two buttons that do the same thing.
                            if !read_only && icon_button(ui, Glyph::Close, EDITOR_CLOSE) {
                                cancel_clicked = true;
                            }
                        });
                    });
                });

            // `.edit-body { padding: 4px 18px 14px; gap: 10px }`
            let _ = egui::Frame::new()
                .inner_margin(Margin { left: 18, right: 18, top: 4, bottom: 14 })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = theme::space::L;

                    if read_only {
                        // `.space-warn-box`: the one sentence this dialog exists
                        // to say on a watch Portal owns.
                        note_box(ui, EDITOR_PORTAL);
                    }

                    ui.add_enabled_ui(!read_only, |ui| {
                        editor_row(ui, EDITOR_LABEL_FOLDER, |ui, id| {
                            let field_w = (ui.available_width()
                                - EDITOR_CHOOSE_W
                                - theme::space::M)
                                .max(120.0);
                            let field_hint = "Choose the folder to watch";
                            editor_field(ui, &mut editor.local, field_hint, field_w)
                                .labelled_by(id);
                            if subtle_button(ui, EDITOR_CHOOSE).clicked() {
                                choose_clicked = true;
                            }
                        });
                        editor_row(ui, EDITOR_LABEL_DEST, |ui, _id| {
                            let chosen = !editor.folder_id.trim().is_empty();
                            let shown = if !chosen {
                                String::new()
                            } else if editor.label.trim().is_empty() {
                                editor.label_or_folder()
                            } else {
                                editor.label.clone()
                            };
                            if dest_button(ui, &shown) {
                                dest_clicked = true;
                            }
                        });
                        // The label defaults to the folder's own name, which is
                        // what the field's hint shows while it is empty.
                        editor_row(ui, EDITOR_LABEL_NAME, |ui, id| {
                            let width = ui.available_width();
                            let hint = hint.clone();
                            editor_field(ui, &mut editor.label, &hint, width).labelled_by(id);
                        });
                        editor_row(ui, EDITOR_LABEL_DIRECTION, |ui, _id| {
                            segmented(ui, &DIRECTIONS, &mut editor.direction);
                        });
                        editor_row(ui, EDITOR_LABEL_ON_CHANGE, |ui, _id| {
                            segmented(ui, &ON_CHANGES, &mut editor.on_change);
                        });

                        let boxed = ui.add(Checkbox::new(
                            &mut editor.include_existing,
                            RichText::new(EDITOR_INCLUDE)
                                .font(theme::body())
                                .color(theme::TEXT_2),
                        ));
                        boxed.on_hover_text(EDITOR_INCLUDE_HELP);
                        ui.add_space(theme::space::S);
                        ui.add(
                            Label::new(
                                RichText::new(EDITOR_INCLUDE_HELP)
                                    .font(theme::detail())
                                    .color(theme::TEXT_3),
                            )
                            .wrap(),
                        );
                    });

                    // Why Save is off, in the app's own words for it: under the
                    // fields, where the one thing to fix is.
                    if let Some(reason) = reason {
                        ui.add_space(theme::space::M);
                        ui.add(Label::new(
                            RichText::new(reason).font(theme::detail()).color(theme::ERROR_TEXT),
                        ));
                    }
                });

            // `.modal-foot { border-top: 1px solid var(--border); padding: 12px 14px }`,
            // with the buttons at the right: Cancel, then the one that writes.
            let foot = egui::Frame::new()
                .inner_margin(Margin { left: 14, right: 14, top: 12, bottom: 12 })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = theme::space::M;
                        let save = if read_only {
                            None
                        } else {
                            Some(primary_button_enabled(ui, EDITOR_SAVE, reason.is_none()))
                        };
                        let cancel = subtle_button(
                            ui,
                            if read_only { EDITOR_CLOSE } else { EDITOR_CANCEL },
                        );
                        // Cancel is focused the moment the dialog opens, as
                        // `openSpaceConfirm` focused `#sc-cancel`.
                        if !covered && ui.memory(|memory| memory.focused().is_none()) {
                            cancel.request_focus();
                        }
                        // Enter takes the focused button — Cancel — unless Save
                        // is the one that has it. Escape is `should_close`.
                        let enter = !covered && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if enter && !save.as_ref().is_some_and(egui::Response::has_focus) {
                            cancel_clicked = true;
                        }
                        save_clicked |= save.is_some_and(|button| button.clicked());
                        cancel_clicked |= cancel.clicked();
                    });
                });
            // The foot's 1 px rule sits on its top edge.
            ui.painter().hline(
                foot.response.rect.x_range(),
                foot.response.rect.top(),
                theme::hairline(theme::BORDER),
            );
        });

    if cancel_clicked || modal.should_close() {
        decision = Decision::Cancel;
    }
    if choose_clicked {
        decision = Decision::Choose;
    }
    if dest_clicked {
        decision = Decision::Destination;
    }
    if save_clicked {
        decision = Decision::Save;
    }
    decision
}

/// `.edit-row`: an 80 px dim label, then the control, which takes the rest of the
/// width. The label is a real widget so the field it names can be `labelled_by`
/// it — for a screen reader, and for this screen's own tests.
fn editor_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui, Id)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::space::XL;
        let label = ui.allocate_ui_with_layout(
            vec2(EDITOR_LABEL_W, theme::size::CONTROL_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(EDITOR_LABEL_W);
                ui.add(Label::new(
                    RichText::new(label).font(theme::detail()).color(theme::TEXT_3),
                ))
            },
        );
        add(ui, label.inner.id);
    });
}

/// The destination: a `.dest-pick` box with the folder glyph and the path (or
/// the placeholder), opening the picker. Answers whether it was clicked.
fn dest_button(ui: &mut egui::Ui, shown: &str) -> bool {
    let size = vec2(ui.available_width(), theme::size::CONTROL_H);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let border = if response.hovered() { theme::BORDER_3 } else { theme::BORDER_2 };
    painter.rect_filled(rect, theme::R_S, theme::SURFACE);
    painter.rect_stroke(rect, theme::R_S, theme::hairline(border), StrokeKind::Inside);
    let icon = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(16.0, 16.0));
    paint(painter, icon, Glyph::Folder, theme::TEXT_3, 16.0);
    let (text, colour) = if shown.is_empty() { (EDITOR_DEST_NONE, theme::TEXT_4) } else { (shown, theme::TEXT) };
    let width = rect.right() - 12.0 - (icon.right() + 8.0);
    // Too long: the middle of the path goes, so the folder it ends at shows.
    let measure = |t: &str| painter.layout_no_wrap(t.to_string(), theme::body(), colour).size().x;
    let crumbs: Vec<&str> = text.split(" / ").collect();
    let widths: Vec<f32> = crumbs.iter().map(|c| measure(c)).collect();
    let fitted: Vec<&str> = fit_crumbs(&widths, measure(" / "), measure("…"), width)
        .into_iter()
        .map(|item| item.map_or("…", |i| crumbs[i]))
        .collect();
    let mut job = egui::text::LayoutJob::single_section(
        fitted.join(" / "),
        egui::text::TextFormat { font_id: theme::body(), color: colour, ..Default::default() },
    );
    job.wrap = egui::text::TextWrapping { max_width: width.max(0.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let galley = painter.layout_job(job);
    painter.galley(pos2(icon.right() + 8.0, rect.center().y - galley.size().y / 2.0), galley, colour);
    let response = response.on_hover_text(text);
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, EDITOR_DEST_BUTTON));
    response.clicked()
}

/// `.input`: the surface fill, 10 px either side, the width it is given.
fn editor_field(
    ui: &mut egui::Ui,
    text: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    ui.scope(|ui| {
        ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
        ui.add(
            TextEdit::singleline(text)
                .hint_text(hint)
                .margin(Margin::symmetric(theme::space::L as i8, theme::space::M as i8))
                .desired_width(width.max(60.0)),
        )
    })
    .inner
}

/// `.segmented`: a `--surface` tray with the active button in `--raised`.
fn segmented(ui: &mut egui::Ui, options: &[(&str, &str)], value: &mut String) {
    theme::segmented_frame().show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = theme::space::XXS;
        ui.spacing_mut().interact_size.y = theme::size::CONTROL_SM_H;
        ui.spacing_mut().button_padding = vec2(theme::space::XL, theme::space::XS);
        for (key, label) in options {
            let active = value == key;
            let colour = if active { theme::TEXT } else { theme::TEXT_2 };
            let button = egui::Button::new(RichText::new(*label).font(theme::body()).color(colour))
                .fill(if active { theme::RAISED } else { Color32::TRANSPARENT })
                .stroke(Stroke::NONE)
                .corner_radius(CornerRadius::same(4))
                .min_size(vec2(0.0, theme::size::CONTROL_SM_H));
            if ui.add(button).clicked() && !active {
                *value = (*key).to_string();
            }
        }
    });
}

/// `.space-warn-box`: a `--surface` box with a border, for the one sentence a
/// dialog exists to say.
fn note_box(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(theme::stroke_border_2())
        .corner_radius(theme::R_S)
        .inner_margin(Margin { left: 12, right: 12, top: 10, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                Label::new(RichText::new(text).font(theme::note()).color(theme::TEXT_2)).wrap(),
            );
        });
}

/// `.btn.subtle`: no fill, no border, `--text-2` until it is under the pointer.
fn subtle_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.hovered.weak_bg_fill = theme::HOVER;
            widgets.active.weak_bg_fill = theme::HOVER;
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.hovered.bg_stroke = Stroke::NONE;
            widgets.active.bg_stroke = Stroke::NONE;
            widgets.inactive.fg_stroke = theme::hairline(theme::TEXT_2);
            widgets.hovered.fg_stroke = theme::hairline(theme::TEXT);
            widgets.active.fg_stroke = theme::hairline(theme::TEXT);
        }
        ui.add(
            egui::Button::new(RichText::new(text).font(theme::body()))
                .min_size(vec2(0.0, theme::size::CONTROL_H)),
        )
    })
    .inner
}

// ---------- the pieces of the table ----------

/// One full-width strip: the hover wash, the row padding, the 1 px bottom
/// border, and a child `Ui` laid out left to right.
fn strip(
    ui: &mut egui::Ui,
    height: f32,
    hoverable: bool,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let width = ui.available_width().max(0.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if hoverable && response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::ROW_HOVER);
    }

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(ROW_PADDING_X, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 0.0;
    add(&mut child);

    // The border is the last pixel *inside* the strip (`border-bottom`), so the
    // next row starts on the following pixel and the pitch is 57.
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 1.0,
        theme::hairline(theme::BORDER),
    );
    response
}

/// A fixed-width column of a row.
fn cell(ui: &mut egui::Ui, width: f32, height: f32, right: bool, add: impl FnOnce(&mut egui::Ui)) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let layout = if right {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
    child.spacing_mut().item_spacing.x = COLUMN_GAP;
    add(&mut child);
    ui.add_space(COLUMN_GAP);
}

/// `.status .dot`: 7 px, in the status colour.
fn dot(ui: &mut egui::Ui, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(theme::size::DOT, theme::size::DOT), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), theme::size::DOT * 0.5, colour);
    ui.add_space(7.0);
}

/// `.kind-icon`: a 30 px `--card` tile with the folder outline in it.
fn icon_tile(ui: &mut egui::Ui, glyph: Glyph) {
    let size = theme::size::KIND_ICON;
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::R_S, theme::CARD);
    painter.rect_stroke(rect, theme::R_S, theme::stroke_border(), StrokeKind::Inside);
    paint(painter, rect, glyph, theme::TEXT_2, 16.0);
}

/// A bare 16 px glyph, for the destination's direction icon.
fn glyph(ui: &mut egui::Ui, glyph: Glyph, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    paint(ui.painter(), rect, glyph, colour, 14.0);
}

/// The job's title line: the project's name and [`TRAY_SUFFIX`] side by side,
/// at the body font's own height.
///
/// A plain `ui.horizontal` assumes there is something interactive on the line
/// and sizes itself from `interact_size`, which would push the marker under it
/// down the row. This is [`detail_line`] one font step up: the line the job's
/// name has always been given, with the tray's words on it.
fn title_line(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let line = vec2(
        ui.available_width(),
        ui.fonts_mut(|fonts| fonts.row_height(&theme::body())),
    );
    ui.allocate_ui_with_layout(line, Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = theme::space::S;
        add(ui);
    });
}

/// One line of a folder cell's secondary text, at the detail font's own height.
///
/// A plain `ui.horizontal` would centre the line in whatever is left under the
/// name and drop it; this keeps the line where the name's second line belongs.
fn detail_line(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let line = vec2(
        ui.available_width(),
        ui.fonts_mut(|fonts| fonts.row_height(&theme::detail())),
    );
    ui.allocate_ui_with_layout(line, Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = theme::space::S;
        add(ui);
    });
}

/// The row's detail line: the shortened path, its tooltip the whole path.
fn path_line(ui: &mut egui::Ui, path: &str, local: &str) {
    ui.add(theme::truncating(
        RichText::new(path)
            .font(theme::detail())
            .color(theme::TEXT_3),
    ))
    .on_hover_text(local);
}

/// The marker a Portal-owned row carries ([`PORTAL_MARKER`]): the path line's own
/// muted 12 px, and nothing else — no new colour, no new box, no new font.
fn portal_marker(ui: &mut egui::Ui) -> egui::Response {
    ui.add(egui::Label::new(
        RichText::new(PORTAL_MARKER)
            .font(theme::detail())
            .color(theme::TEXT_3),
    ))
}

/// The Portal page a job's link is on: `{portal_url}/upload-links/{link id}` —
/// the server's address out of `Settings::portal_url`, and the link id out of
/// the watch record's `origin`.
///
/// `link_id` is the watch record's `origin` without its lane suffix
/// ([`WatchRow::duplex_id`]): `portal:L1` and `portal:L1#exports` are one link,
/// so both lanes open the same page and the job offers one action rather than
/// two. An empty address is nothing to open, and answers `None`.
pub fn portal_link_url(portal_url: &str, link_id: &str) -> Option<String> {
    let base = portal_url.trim().trim_end_matches('/');
    if base.is_empty() || link_id.is_empty() {
        return None;
    }
    Some(format!("{base}/upload-links/{link_id}"))
}

/// `.a` in the stylesheet, in the row's action column: a link — 12.5 px,
/// `--link`, lighter on hover — rather than a button, because it leaves the app
/// instead of acting on the watch.
///
/// `url` is the page it opens, and its tooltip; the page it answers with is the
/// one it was clicked with, and nothing at all is the answer when it was not
/// clicked. `None` is a Portal address not set: the link is then drawn off,
/// muted, with [`PORTAL_ADDRESS_MISSING`] on hover, and answers nothing.
fn open_in_portal<'url>(ui: &mut egui::Ui, url: Option<&'url str>) -> Option<&'url str> {
    let mut clicked = false;
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            // No box, in any state: `a` is text, not a `.btn`.
            for widget in [
                &mut widgets.inactive,
                &mut widgets.hovered,
                &mut widgets.active,
                &mut widgets.noninteractive,
            ] {
                widget.weak_bg_fill = Color32::TRANSPARENT;
                widget.bg_stroke = Stroke::NONE;
            }
            if url.is_some() {
                widgets.inactive.fg_stroke = theme::hairline(theme::LINK);
                widgets.hovered.fg_stroke = theme::hairline(theme::LINK_HOVER);
                widgets.active.fg_stroke = theme::hairline(theme::LINK);
            } else {
                // Off: the mute the row already sets its paths in, so it reads
                // as waiting on a setting rather than as something to press.
                widgets.noninteractive.fg_stroke = theme::hairline(theme::TEXT_3);
            }
            ui.style_mut().spacing.button_padding = vec2(0.0, 0.0);
        }
        let link = egui::Button::new(RichText::new(OPEN_IN_PORTAL).font(theme::note())).small();
        let response = ui.add_enabled(url.is_some(), link);
        clicked = match url {
            Some(url) => response.on_hover_text(url).clicked(),
            None => {
                response.on_disabled_hover_text(PORTAL_ADDRESS_MISSING);
                false
            }
        };
    });
    url.filter(|_| clicked)
}

/// `.icon-btn`: 28 x 28, transparent, `--text-3` until hovered. The button is
/// painted, and `WidgetInfo` gives it the name a screen reader and a test use.
fn icon_button(ui: &mut egui::Ui, glyph: Glyph, label: &str) -> bool {
    icon_button_with(ui, glyph, label, label)
}

/// The same button with its tooltip said in other words: for an action whose
/// cost is not in the glyph — the job's Remove, which Portal undoes. The name a
/// screen reader and a test use is still `label`.
fn icon_button_with(ui: &mut egui::Ui, glyph: Glyph, label: &str, tooltip: &str) -> bool {
    let size = vec2(theme::size::ICON_BTN, theme::size::ICON_BTN);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.on_hover_text(tooltip);
    let colour = if response.hovered() {
        theme::TEXT
    } else {
        theme::TEXT_3
    };
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::R_S_PX),
            if response.is_pointer_button_down_on() {
                theme::ACTIVE
            } else {
                theme::HOVER
            },
        );
    }
    paint(ui.painter(), rect, glyph, colour, 16.0);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    response.clicked()
}

/// The glyphs the row draws. `I.<name>` in app.js are the same shapes as SVG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Folder,
    Up,
    Down,
    Sync,
    Pause,
    Play,
    Pencil,
    Reveal,
    Trash,
    /// `.icon-btn`'s close cross, as the Space screen's dialog has it.
    Close,
}

impl Glyph {
    /// `DIR_ICON` in app.js: `upload` is an up arrow, `download` a down arrow,
    /// `mirror` the two-arrow sync mark.
    fn of_direction(direction: &str) -> Glyph {
        match direction {
            "download" => Glyph::Down,
            "mirror" => Glyph::Sync,
            _ => Glyph::Up,
        }
    }
}

/// Paint `glyph` inside `rect`, on a canvas `size` px tall.
fn paint(painter: &Painter, rect: Rect, glyph: Glyph, colour: Color32, size: f32) {
    let c = rect.center();
    let s = size * 0.5;
    let stroke = Stroke::new(1.4_f32, colour);
    match glyph {
        Glyph::Folder => {
            // A tab on the left shoulder, then the body.
            let body = Rect::from_center_size(c, vec2(s * 1.8, s * 1.4));
            let tab = Rect::from_min_size(
                pos2(body.left(), body.top() - s * 0.35),
                vec2(s * 0.8, s * 0.35),
            );
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            painter.line_segment([tab.left_bottom(), tab.left_top()], stroke);
            painter.line_segment([tab.left_top(), tab.right_top()], stroke);
            painter.line_segment([tab.right_top(), tab.right_bottom()], stroke);
        }
        Glyph::Up | Glyph::Down => {
            let dir = if glyph == Glyph::Up { -1.0 } else { 1.0 };
            let tip = pos2(c.x, c.y + dir * s * 0.9);
            let tail = pos2(c.x, c.y - dir * s * 0.9);
            painter.line_segment([tail, tip], stroke);
            let wing = s * 0.45;
            painter.line_segment(
                [pos2(tip.x - wing, tip.y - dir * wing), tip],
                stroke,
            );
            painter.line_segment(
                [pos2(tip.x + wing, tip.y - dir * wing), tip],
                stroke,
            );
        }
        Glyph::Sync => {
            // Two arrows, one each way.
            let y = s * 0.45;
            painter.line_segment([pos2(c.x - s * 0.9, c.y - y), pos2(c.x + s * 0.9, c.y - y)], stroke);
            painter.line_segment([pos2(c.x + s * 0.9, c.y + y), pos2(c.x - s * 0.9, c.y + y)], stroke);
            let w = s * 0.4;
            painter.line_segment(
                [pos2(c.x + s * 0.9 - w, c.y - y - w), pos2(c.x + s * 0.9, c.y - y)],
                stroke,
            );
            painter.line_segment(
                [pos2(c.x - s * 0.9 + w, c.y + y + w), pos2(c.x - s * 0.9, c.y + y)],
                stroke,
            );
        }
        Glyph::Pause => {
            let y = s * 0.6;
            for x in [-s * 0.3, s * 0.3] {
                painter.line_segment([pos2(c.x + x, c.y - y), pos2(c.x + x, c.y + y)], stroke);
            }
        }
        Glyph::Play => {
            let shape = Shape::convex_polygon(
                vec![
                    pos2(c.x - s * 0.35, c.y - s * 0.6),
                    pos2(c.x - s * 0.35, c.y + s * 0.6),
                    pos2(c.x + s * 0.55, c.y),
                ],
                colour,
                Stroke::NONE,
            );
            painter.add(shape);
        }
        Glyph::Pencil => {
            // The stylesheet's `I.pencil`: a shaft along the diagonal with a
            // pointed tip at the lower left and the eraser line across the top.
            // (The previous two strokes read as a golf club at 18 px.)
            let map = |x: f32, y: f32| {
                pos2(c.x + (x - 10.0) / 10.0 * s, c.y + (y - 10.0) / 10.0 * s)
            };
            painter.add(egui::Shape::closed_line(
                vec![map(4.0, 16.0), map(4.7, 12.8), map(13.3, 4.2), map(15.3, 6.2), map(6.7, 14.8)],
                stroke,
            ));
            painter.line_segment([map(12.8, 4.7), map(15.3, 7.2)], stroke);
        }
        Glyph::Reveal => {
            painter.circle_stroke(
                pos2(c.x - s * 0.2, c.y - s * 0.2),
                s * 0.45,
                stroke,
            );
            painter.line_segment(
                [pos2(c.x + s * 0.12, c.y + s * 0.12), pos2(c.x + s * 0.6, c.y + s * 0.6)],
                stroke,
            );
        }
        Glyph::Close => {
            let arm = s * 0.55;
            painter.line_segment(
                [pos2(c.x - arm, c.y - arm), pos2(c.x + arm, c.y + arm)],
                stroke,
            );
            painter.line_segment(
                [pos2(c.x + arm, c.y - arm), pos2(c.x - arm, c.y + arm)],
                stroke,
            );
        }
        Glyph::Trash => {
            let body = Rect::from_center_size(
                pos2(c.x, c.y + s * 0.12),
                vec2(s * 1.1, s * 1.3),
            );
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            painter.line_segment(
                [pos2(c.x - s * 0.7, body.top()), pos2(c.x + s * 0.7, body.top())],
                stroke,
            );
            painter.line_segment(
                [
                    pos2(c.x - s * 0.25, body.top() - s * 0.25),
                    pos2(c.x + s * 0.25, body.top() - s * 0.25),
                ],
                stroke,
            );
        }
    }
}

/// `DIR_VERB` in app.js.
fn direction_verb(direction: &str) -> &'static str {
    match direction {
        "download" => "Downloads from",
        "mirror" => "Mirrors",
        _ => "Uploads to",
    }
}

/// The number in the Files column. The webview counts what a folder has moved;
/// a [`WatchRow`] carries what is still to move.
fn count_word(pending: i64) -> String {
    match pending {
        n if n < 0 => "—".to_string(),
        n => n.to_string(),
    }
}

/// `baseName` in app.js: the last path component.
pub fn base_name(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => trimmed.to_string(),
    }
}

/// `shortPath` in app.js: `~` for the home folder, the path unchanged otherwise.
pub fn short_path(path: &str, home: &str) -> String {
    let path = path.trim();
    let home = home.trim().trim_end_matches('/');
    if path.is_empty() {
        return String::new();
    }
    if !home.is_empty() && (path == home || path.starts_with(&format!("{home}/"))) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

/// `.btn.primary`: `--brand` fill, white text, 32 px tall.
///
/// The fill goes on the widget states rather than on the button, so `:hover`
/// still brightens to `--brand-hover`.
fn primary_button(ui: &mut egui::Ui, text: &str) -> bool {
    primary_button_enabled(ui, text, true).clicked()
}

/// The same button, off: `.btn:disabled` is the same `--brand` fill at 40 %
/// alpha, with no interaction at all.
///
/// egui asks `noninteractive` for a widget that is off, so the fill, the stroke
/// and the words are set there too rather than being left to the default grey.
fn primary_button_enabled(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = theme::BRAND;
            widgets.hovered.weak_bg_fill = theme::BRAND_HOVER;
            widgets.active.weak_bg_fill = theme::BRAND_HOVER;
            widgets.noninteractive.weak_bg_fill = theme::BRAND.gamma_multiply(0.4);
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.hovered.bg_stroke = Stroke::NONE;
            widgets.active.bg_stroke = Stroke::NONE;
            widgets.noninteractive.bg_stroke = Stroke::NONE;
            widgets.inactive.fg_stroke = theme::hairline(theme::WHITE);
            widgets.hovered.fg_stroke = theme::hairline(theme::WHITE);
            widgets.active.fg_stroke = theme::hairline(theme::WHITE);
            widgets.noninteractive.fg_stroke = theme::hairline(theme::WHITE.gamma_multiply(0.4));
        }
        if !enabled {
            ui.disable();
        }
        ui.add(
            egui::Button::new(
                RichText::new(text)
                    .font(theme::body())
                    .color(if enabled { theme::WHITE } else { theme::WHITE.gamma_multiply(0.4) }),
            )
            .min_size(vec2(0.0, theme::size::CONTROL_H)),
        )
    })
    .inner
}

/// `.empty`: 72 px of air, a 44 px icon tile, then the title.
fn empty_state(ui: &mut egui::Ui, heading: &str) {
    ui.add_space(72.0);
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = theme::space::XL;
        let (rect, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
        let radius = CornerRadius::same(10);
        let painter = ui.painter();
        painter.rect_filled(rect, radius, theme::CARD);
        painter.rect_stroke(rect, radius, theme::stroke_border(), StrokeKind::Inside);
        // The webview draws an eye here; two circles read the same at 22 px.
        painter.circle_stroke(rect.center(), 8.0, Stroke::new(1.4_f32, theme::TEXT_3));
        painter.circle_filled(rect.center(), 2.4, theme::TEXT_3);

        ui.label(
            RichText::new(heading)
                .font(theme::section())
                .color(theme::TEXT)
                .strong(),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT as _, Queryable};

    fn watch(local: &str, label: &str, direction: &str, enabled: bool, pending: i64) -> WatchRow {
        WatchRow {
            // The app's watch id, kept distinct from the path so a test can tell
            // which one the screen used.
            id: format!("id:{local}"),
            local: local.to_string(),
            label: label.to_string(),
            direction: direction.to_string(),
            enabled,
            pending,
            from_portal: false,
            duplex_id: None,
            lane: Lane::None,
            // The two ids and the rule, as the app's `WatchView` carries them:
            // the editor opens on these, so a test can tell them from a default.
            account_id: "acct-fixture".to_string(),
            folder_id: format!("fld:{local}"),
            on_change: "overwrite".to_string(),
        }
    }

    /// The same row, as Portal's, on a watch whose origin names no lane this
    /// screen can place: the marker, and nothing about duplexes.
    fn portal_watch(local: &str, label: &str, direction: &str, enabled: bool, pending: i64) -> WatchRow {
        WatchRow {
            from_portal: true,
            ..watch(local, label, direction, enabled, pending)
        }
    }

    /// One sync lane of a Portal-managed project: the record's origin is
    /// `portal:<link id>` for the inbound lane and `portal:<link id>#exports`
    /// for the outbound one, and the lane is read from that and the direction.
    fn lane_watch(
        local: &str,
        label: &str,
        direction: &str,
        enabled: bool,
        pending: i64,
        duplex: &str,
        lane: Lane,
    ) -> WatchRow {
        WatchRow {
            from_portal: true,
            duplex_id: Some(duplex.to_string()),
            lane,
            ..watch(local, label, direction, enabled, pending)
        }
    }

    /// A duplex with both lanes: the client's folder downloaded into, the
    /// exports folder uploaded from, and one destination label between them.
    fn luc() -> Vec<WatchRow> {
        vec![
            lane_watch("/Users/chaos/Live_Work/LUC/1_DRIVE", "LUC", "download", true, 4, "LUC", Lane::Inbound),
            lane_watch("/Users/chaos/Live_Work/LUC/0_EXPORTS", "LUC", "upload", true, 2, "LUC", Lane::Outbound),
        ]
    }

    fn state_with(watches: Vec<WatchRow>) -> UiState {
        UiState {
            watches,
            home: "/Users/chaos".to_string(),
            ..UiState::default()
        }
    }

    /// The main column of the default 1180 px window (224 px sidebar), which is
    /// the width the webview's own screenshot is taken at.
    fn screen_harness(state: UiState) -> Harness<'static, UiState> {
        Harness::builder()
            .with_size(vec2(956.0, 780.0))
            .build_state(
                |ctx, state: &mut UiState| {
                    crate::app::apply_style_once(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| show(ui, state));
                },
                state,
            )
    }

    /// The job's title line on screen: the project's name, and the tray suffix
    /// beside it.
    ///
    /// The name is not unique — the Frame.io column prints the same words on
    /// every lane — so it is the one that shares the suffix's line.
    fn job_title<'h>(
        harness: &'h Harness<'_, UiState>,
        name: &'h str,
    ) -> (egui_kittest::Node<'h>, egui_kittest::Node<'h>) {
        let suffix = harness.get_by_label(TRAY_SUFFIX);
        let title = harness
            .get_all_by_label(name)
            .find(|node| (node.rect().center().y - suffix.rect().center().y).abs() < 2.0)
            .unwrap_or_else(|| panic!("no `{name}` on the job's title line"));
        (title, suffix)
    }

    #[test]
    fn the_empty_state_says_no_watch_folders() {
        let harness = screen_harness(state_with(Vec::new()));
        harness.get_by_label("No watch folders");
        harness.get_by_label("Add");
        assert!(
            harness.query_by_label("Folder").is_none(),
            "the webview hides the table header when there is nothing to list"
        );
    }

    #[test]
    fn the_header_has_the_webview_columns() {
        let harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            3,
        )]));
        for head in ["Folder", "Frame.io", "Status", "Files"] {
            harness.get_by_label(head);
        }
        // `@media (max-width: 1279px)` drops "Last transfer" at this width.
        assert!(harness.query_by_label("Last transfer").is_none());
    }

    #[test]
    fn a_row_shows_the_folder_its_path_the_destination_and_the_status() {
        let harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            3,
        )]));

        harness.get_by_label("Rushes");
        harness.get_by_label("~/Movies/Rushes");
        harness.get_by_label("Rushes 2026");
        harness.get_by_label("Watching");
        harness.get_by_label("3");
        harness.get_by_label("Pause");
        harness.get_by_label("Edit");
        harness.get_by_label("Show in Finder");
        harness.get_by_label("Remove");
    }

    #[test]
    fn a_paused_mirror_row_reads_paused_and_mirrors() {
        let harness = screen_harness(state_with(vec![
            watch("/Users/chaos/Movies/Archive", "Archive", "mirror", false, 0),
            watch("/Users/chaos/Movies/In", "Inbox", "download", true, 12),
        ]));

        harness.get_by_label("Paused");
        harness.get_by_label("Resume");
        harness.get_by_label("Inbox");
        harness.get_by_label("12");
    }

    #[test]
    fn the_pause_button_flips_the_watch() {
        let mut harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            0,
        )]));

        assert!(harness.state().watches[0].enabled);
        harness.get_by_label("Pause").click();
        harness.run();

        assert!(!harness.state().watches[0].enabled);
        harness.get_by_label("Resume");
    }

    #[test]
    fn the_pause_button_asks_the_app_with_the_rows_id() {
        let mut harness = screen_harness(state_with(vec![
            watch("/Users/chaos/Movies/Rushes", "Rushes 2026", "upload", true, 0),
            watch("/Users/chaos/Movies/In", "Inbox", "download", true, 7),
        ]));

        // The second row's pause, so the id has to be looked up, not guessed.
        {
            let buttons: Vec<_> = harness.get_all_by_label("Pause").collect();
            if let Some(second) = buttons.last() {
                second.click();
            }
        }
        harness.run();

        assert!(
            harness.state().requests.contains(&Request::SetWatchEnabled {
                id: "id:/Users/chaos/Movies/In".to_string(),
                on: false,
            }),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn remove_and_reveal_ask_the_app() {
        let mut harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            0,
        )]));

        harness.get_by_label("Remove").click();
        harness.run();
        assert!(
            harness
                .state()
                .requests
                .contains(&Request::RemoveWatch("id:/Users/chaos/Movies/Rushes".to_string())),
            "requests: {:?}",
            harness.state().requests
        );

        harness.get_by_label("Show in Finder").click();
        harness.run();
        assert!(
            harness
                .state()
                .requests
                .contains(&Request::Reveal("/Users/chaos/Movies/Rushes".to_string())),
            "requests: {:?}",
            harness.state().requests
        );
    }

    // ---------- the watch folder editor ----------

    /// A draft with the two Frame.io ids in it: everything a save needs but the
    /// folder, which is what the tests about the folder vary.
    fn draft(local: &str) -> WatchEditor {
        WatchEditor {
            id: Some("id:/Users/chaos/Movies/Rushes".to_string()),
            local: local.to_string(),
            account_id: "acct-1".to_string(),
            folder_id: "fld-1".to_string(),
            label: "Rushes 2026".to_string(),
            direction: DIRECTION_UPLOAD.to_string(),
            on_change: ON_CHANGE_OVERWRITE.to_string(),
            include_existing: false,
            read_only: false,
        }
    }

    fn state_with_editor(watches: Vec<WatchRow>, editor: WatchEditor) -> UiState {
        let mut state = state_with(watches);
        state.watch_editor = Some(editor);
        state
    }

    #[test]
    fn the_pencil_opens_the_editor_with_the_rows_values() {
        let mut harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            3,
        )]));

        harness.get_by_label("Edit").click();
        harness.run();

        // The dialog, and every row it has, in the words it says them in.
        harness.get_by_label(EDITOR_TITLE_EDIT);
        for label in [
            EDITOR_LABEL_FOLDER,
            EDITOR_LABEL_DEST,
            EDITOR_DEST_BUTTON,
            EDITOR_LABEL_NAME,
            EDITOR_LABEL_DIRECTION,
            EDITOR_LABEL_ON_CHANGE,
        ] {
            harness.get_by_label(label);
        }
        harness.get_by_label(EDITOR_INCLUDE);

        // The fields are the row's own values, drawn in the fields themselves —
        // not the app's last destination, and not a blank form.
        assert_eq!(
            harness.get_by_label(EDITOR_LABEL_FOLDER).value().as_deref(),
            Some("/Users/chaos/Movies/Rushes")
        );
        let editor = harness.state().watch_editor.clone().unwrap();
        assert_eq!(
            (editor.account_id.as_str(), editor.folder_id.as_str()),
            ("acct-fixture", "fld:/Users/chaos/Movies/Rushes")
        );
        assert_eq!(harness.get_by_label(EDITOR_LABEL_NAME).value().as_deref(), Some("Rushes 2026"));

        // Opening a dialog asks the app for nothing.
        assert!(
            harness.state().requests.is_empty(),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn the_editor_saves_exactly_the_values_it_shows() {
        let mut harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            3,
        )]));
        harness.get_by_label("Edit").click();
        harness.run();

        // Retype the label the way a person does: focus the field, select what is
        // there, type over it.
        harness.get_by_label(EDITOR_LABEL_NAME).focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_label(EDITOR_LABEL_NAME).type_text("Rushes v2");
        harness.run();
        assert_eq!(harness.get_by_label(EDITOR_LABEL_NAME).value().as_deref(), Some("Rushes v2"));

        // Keep the files that changed rather than overwriting them, so the second
        // segmented control is shown to save what was picked too.
        harness.get_by_label("Keep").click();
        harness.run();

        harness.get_by_label(EDITOR_SAVE).click();
        harness.run();

        let expected = Request::UpdateWatch {
            id: "id:/Users/chaos/Movies/Rushes".to_string(),
            local: "/Users/chaos/Movies/Rushes".to_string(),
            acct: "acct-fixture".to_string(),
            folder: "fld:/Users/chaos/Movies/Rushes".to_string(),
            label: "Rushes v2".to_string(),
            dir: DIRECTION_UPLOAD.to_string(),
            on_change: ON_CHANGE_KEEP.to_string(),
            include_existing: false,
        };
        assert!(
            harness.state().requests.contains(&expected),
            "requests: {:?}",
            harness.state().requests
        );
        // One request, the save, and the dialog is closed.
        assert_eq!(harness.state().requests.len(), 1, "{:?}", harness.state().requests);
        assert!(harness.state().watch_editor.is_none());
        assert!(harness.query_by_label(EDITOR_TITLE_EDIT).is_none());
    }

    #[test]
    fn cancel_and_escape_close_the_editor_and_ask_for_nothing() {
        let mut harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            3,
        )]));

        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label(EDITOR_CANCEL).click();
        harness.run();
        assert!(harness.state().watch_editor.is_none(), "Cancel closes the dialog");
        assert!(
            harness.state().requests.is_empty(),
            "nothing is saved: {:?}",
            harness.state().requests
        );

        harness.get_by_label("Edit").click();
        harness.run();
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.state().watch_editor.is_none(), "Escape closes the dialog");
        assert!(
            harness.state().requests.is_empty(),
            "nothing is saved: {:?}",
            harness.state().requests
        );
        assert!(harness.query_by_label(EDITOR_TITLE_EDIT).is_none());
        // The row is on screen, unchanged, with its pencil still there.
        harness.get_by_label("Edit");
    }

    #[test]
    fn save_is_off_with_its_reason_while_the_folder_is_empty() {
        let mut harness = screen_harness(state_with_editor(
            vec![watch("/Users/chaos/Movies/Rushes", "Rushes 2026", "upload", true, 3)],
            WatchEditor { local: String::new(), ..draft("") },
        ));
        harness.run();

        let save = harness.get_by_label(EDITOR_SAVE);
        assert!(
            save.accesskit_node().is_disabled(),
            "Save is off until there is a folder to watch"
        );
        harness.get_by_label("Choose the folder to watch first.");

        // A path that is not absolute is not a folder either, and the sentence
        // changes to say what is wrong with it.
        harness.get_by_label(EDITOR_LABEL_FOLDER).focus();
        harness.run();
        harness.get_by_label(EDITOR_LABEL_FOLDER).type_text("Movies/Rushes");
        harness.run();
        harness.get_by_label("The folder has to be an absolute path, starting at /.");
        assert!(harness.get_by_label(EDITOR_SAVE).accesskit_node().is_disabled());

        // Select it and type the absolute path: Save comes on.
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_label(EDITOR_LABEL_FOLDER).type_text("/Users/chaos/Movies/Rushes");
        harness.run();
        assert!(
            !harness.get_by_label(EDITOR_SAVE).accesskit_node().is_disabled(),
            "an absolute folder and both ids are enough to save"
        );
        assert!(
            harness.query_by_label("Choose the folder to watch first.").is_none(),
            "no reason is shown when there is nothing to fix"
        );
    }

    #[test]
    fn a_watch_portal_owns_is_read_only_and_can_never_be_saved() {
        let mut harness = screen_harness(state_with(vec![portal_watch(
            "/Users/chaos/Live_Work/Ep 04",
            "Shows / Nightfall / Ep 04",
            "mirror",
            true,
            0,
        )]));

        harness.get_by_label("Edit").click();
        harness.run();

        // The sentence, and no Save to press: the reconciler would write over
        // whatever an edit here kept.
        harness.get_by_label(EDITOR_PORTAL);
        assert!(
            harness.query_by_label(EDITOR_SAVE).is_none(),
            "a Portal watch has no Save"
        );
        // The values are still there to read.
        assert_eq!(
            harness.get_by_label(EDITOR_LABEL_FOLDER).value().as_deref(),
            Some("/Users/chaos/Live_Work/Ep 04")
        );

        harness.get_by_label(EDITOR_CLOSE).click();
        harness.run();
        assert!(harness.state().watch_editor.is_none());
        assert!(
            harness.state().requests.is_empty(),
            "a Portal row never asks for an update: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn add_opens_the_same_editor_for_a_new_watch() {
        let mut state = state_with(Vec::new());
        state.settings.last_account_id = "acct-9".to_string();
        state.settings.last_folder_id = "fld-9".to_string();
        state.settings.last_folder_label = "Shows / Nightfall".to_string();
        let mut harness = screen_harness(state);

        harness.get_by_label("Add").click();
        harness.run();

        // Create mode: the title says so, the folder is the one thing still to
        // fill in, and the destination starts at the one the app remembers.
        harness.get_by_label(EDITOR_TITLE_NEW);
        assert_eq!(harness.get_by_label(EDITOR_LABEL_FOLDER).value().as_deref(), Some(""));
        assert_eq!(harness.state().watch_editor.as_ref().unwrap().account_id, "acct-9");
        assert_eq!(
            harness.get_by_label(EDITOR_LABEL_NAME).value().as_deref(),
            Some("Shows / Nightfall")
        );
        assert!(harness.state().watch_editor.as_ref().unwrap().id.is_none());
        assert!(
            harness.state().requests.is_empty(),
            "nothing is asked for until Save: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn the_frameio_folder_opens_the_destination_picker() {
        let mut harness = screen_harness(state_with_editor(Vec::new(), draft("")));
        harness.run();
        harness.get_by_label(EDITOR_DEST_BUTTON).click();
        harness.run();
        assert_eq!(
            harness.state().requests,
            vec![Request::OpenDest { mode: DestMode::Watch, at: None }]
        );
        assert!(harness.state().watch_editor.is_some(), "the dialog stays under the picker");
    }

    #[test]
    fn a_save_in_create_mode_asks_for_a_new_watch() {
        let editor = WatchEditor {
            id: None,
            local: "/Users/chaos/Movies/New".to_string(),
            account_id: "acct-9".to_string(),
            folder_id: "fld-9".to_string(),
            label: String::new(),
            ..draft("")
        };
        let mut harness = screen_harness(state_with_editor(Vec::new(), editor));
        harness.run();

        harness.get_by_label(EDITOR_SAVE).click();
        harness.run();

        // The label was left empty, so it is the folder's own name — the default
        // the field's hint shows.
        let expected = Request::AddWatch {
            local: "/Users/chaos/Movies/New".to_string(),
            acct: "acct-9".to_string(),
            folder: "fld-9".to_string(),
            label: "New".to_string(),
            dir: DIRECTION_UPLOAD.to_string(),
            on_change: ON_CHANGE_OVERWRITE.to_string(),
            include_existing: false,
        };
        assert!(
            harness.state().requests.contains(&expected),
            "requests: {:?}",
            harness.state().requests
        );
    }


    /// The whole screen with the editor open over it: the dialog, its six rows,
    /// and the list it was opened from underneath.
    #[test]
    fn the_watch_editor_renders_to_png() {
        let mut harness = screen_harness(state_with(vec![
            watch("/Users/chaos/Movies/Rushes", "Rushes 2026", "upload", true, 3),
            portal_watch("/Users/chaos/Live_Work/Ep 04", "Shows / Nightfall / Ep 04", "mirror", true, 0),
        ]));
        if let Some(pencil) = harness.get_all_by_label("Edit").next() {
            pencil.click();
        }
        // Three frames: the click's, then the two a modal `Area` measures itself
        // in before it is drawn. (Rendering after two has the whole dialog
        // clipped away — in the tree, but not in the picture.)
        harness.run_steps(4);
        harness.get_by_label(EDITOR_TITLE_EDIT);
        harness.get_by_label(EDITOR_SAVE);
        // The render is the artefact: read it back, so a dialog that is in the
        // tree but not on the screen cannot pass for a picture of one.
        let image = harness.render().expect("render");
        let panel = image
            .pixels()
            .filter(|pixel| (pixel[0], pixel[1], pixel[2]) == (0x12, 0x14, 0x1e))
            .count();
        assert!(panel > 100_000, "the dialog's panel is not on the screen: {panel} px");
        harness.snapshot("watch_editor");
    }

    #[test]
    fn add_is_reported_as_an_action() {
        let seen: std::rc::Rc<std::cell::RefCell<Vec<Action>>> = Default::default();
        let sink = seen.clone();
        let mut harness = Harness::builder()
            .with_size(vec2(956.0, 780.0))
            .build_state(
                move |ctx, state: &mut UiState| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if let Some(action) = show_with_actions(ui, state) {
                            sink.borrow_mut().push(action);
                        }
                    });
                },
                state_with(Vec::new()),
            );

        harness.get_by_label("Add").click();
        harness.run();

        assert!(
            seen.borrow().contains(&Action::AddWatch),
            "actions seen: {:?}",
            seen.borrow()
        );
    }

    #[test]
    fn removing_the_second_row_reports_its_index() {
        let seen: std::rc::Rc<std::cell::RefCell<Vec<Action>>> = Default::default();
        let sink = seen.clone();
        let mut harness = Harness::builder()
            .with_size(vec2(956.0, 780.0))
            .build_state(
                move |ctx, state: &mut UiState| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if let Some(action) = show_with_actions(ui, state) {
                            sink.borrow_mut().push(action);
                        }
                    });
                },
                state_with(vec![
                    watch("/Users/chaos/Movies/One", "One", "upload", true, 0),
                    watch("/Users/chaos/Movies/Two", "Two", "upload", true, 0),
                ]),
            );

        {
            let removals: Vec<_> = harness.get_all_by_label("Remove").collect();
            if let Some(second) = removals.last() {
                second.click();
            }
        }
        harness.run();

        assert!(
            seen.borrow().contains(&Action::Remove(1)),
            "actions seen: {:?}",
            seen.borrow()
        );
    }

    #[test]
    fn the_stylesheet_drops_columns_as_the_window_narrows() {
        // 1180 px window: Files stays, Last transfer is dropped (> 1279 needed).
        let default = Columns::for_width(956.0);
        assert!(default.files.is_some());
        assert!(default.last.is_none());

        // 1400 px window: both are shown.
        let wide = Columns::for_width(1176.0);
        assert!(wide.files.is_some());
        assert!(wide.last.is_some());

        // 900 px window: both are dropped.
        let narrow = Columns::for_width(676.0);
        assert!(narrow.files.is_none());
        assert!(narrow.last.is_none());

        // The folder and destination columns never collapse below their minimum.
        for width in [300.0, 500.0, 676.0, 956.0, 1600.0] {
            let columns = Columns::for_width(width);
            assert!(columns.name >= MIN_NAME_WIDTH);
            assert!(columns.dest >= MIN_DEST_WIDTH);
        }
    }

    #[test]
    fn paths_are_shortened_the_way_the_webview_does() {
        assert_eq!(base_name("/Users/chaos/Movies/Rushes"), "Rushes");
        assert_eq!(base_name("/Users/chaos/Movies/Rushes/"), "Rushes");
        assert_eq!(base_name("Rushes"), "Rushes");
        assert_eq!(base_name(""), "");
        assert_eq!(short_path("/Users/chaos/Movies", "/Users/chaos"), "~/Movies");
        assert_eq!(short_path("/Users/chaos", "/Users/chaos"), "~");
        assert_eq!(short_path("/Volumes/Raid", "/Users/chaos"), "/Volumes/Raid");
        // No home in the state: the path is shown whole.
        assert_eq!(short_path("/Volumes/Raid", ""), "/Volumes/Raid");
        assert_eq!(short_path("", "/Users/chaos"), "");
    }

    #[test]
    fn every_direction_has_a_verb_and_an_icon() {
        assert_eq!(direction_verb("upload"), "Uploads to");
        assert_eq!(direction_verb("download"), "Downloads from");
        assert_eq!(direction_verb("mirror"), "Mirrors");
        assert_eq!(direction_verb("something-else"), "Uploads to");
        assert_eq!(Glyph::of_direction("upload"), Glyph::Up);
        assert_eq!(Glyph::of_direction("download"), Glyph::Down);
        assert_eq!(Glyph::of_direction("mirror"), Glyph::Sync);
        assert_eq!(Glyph::of_direction(""), Glyph::Up);
    }

    // ---------- whose row is it ----------

    /// A row Portal owns says so, in the secondary text the screen already puts
    /// under the folder name, and the sentence that explains what that means is
    /// on the row. A row the user made says neither.
    #[test]
    fn a_portal_row_carries_the_marker_and_the_sentence() {
        let mut harness = screen_harness(state_with(vec![portal_watch(
            "/Users/chaos/Movies/Live_Work/Ep 04",
            "Shows / Nightfall / Ep 04",
            "upload",
            true,
            2,
        )]));

        // The marker is a label of its own, in the folder's secondary line.
        let marker = harness.get_by_label(PORTAL_MARKER);
        let path = harness.get_by_label("~/Movies/Live_Work/Ep 04");
        assert!(
            (marker.rect().center().y - path.rect().center().y).abs() < 2.0,
            "the marker sits on the path's line, at {:?} and {:?}",
            marker.rect(),
            path.rect()
        );
        assert!(
            marker.rect().right() <= path.rect().left(),
            "the marker goes before the path, not over it"
        );

        // The sentence: hovering the row brings it up, in full.
        marker.hover();
        harness.run();
        assert!(
            harness.query_by_label(PORTAL_SENTENCE).is_some(),
            "the sentence is the marker's tooltip"
        );
    }

    #[test]
    fn a_row_the_user_made_shows_no_marker_and_no_sentence() {
        let mut harness = screen_harness(state_with(vec![watch(
            "/Users/chaos/Movies/Rushes",
            "Rushes 2026",
            "upload",
            true,
            3,
        )]));

        assert!(
            harness.query_by_label(PORTAL_MARKER).is_none(),
            "a watch the user made is not Portal's"
        );
        // Nor does hovering it bring the sentence up.
        let path = harness.get_by_label("~/Movies/Rushes");
        path.hover();
        harness.run();
        assert!(
            harness.query_by_label(PORTAL_SENTENCE).is_none(),
            "no sentence on a row Portal has nothing to do with"
        );
        // And the row still draws exactly what it drew before: the common
        // actions, in the common order.
        for label in ["Pause", "Edit", "Show in Finder", "Remove"] {
            harness.get_by_label(label);
        }
    }

    /// One of each, in one list: the marker belongs to the Portal row only, and
    /// the two rows keep the geometry they had.
    #[test]
    fn one_portal_row_and_one_user_row_in_the_same_list() {
        let harness = screen_harness(state_with(vec![
            watch("/Users/chaos/Movies/Rushes", "Rushes 2026", "upload", true, 3),
            portal_watch("/Users/chaos/Live_Work/Ep 04", "Shows / Nightfall / Ep 04", "mirror", true, 0),
        ]));

        assert_eq!(
            harness.get_all_by_label(PORTAL_MARKER).count(),
            1,
            "one Portal row, one marker"
        );
        // The user's row first, the Portal row second, both on the same pitch.
        let rushes = harness.get_by_label("~/Movies/Rushes").rect();
        let live = harness.get_by_label("~/Live_Work/Ep 04").rect();
        assert!((live.center().y - rushes.center().y - ROW_HEIGHT).abs() < 1.0, "{:?} {:?}", rushes, live);

        // The Portal row keeps its Remove button: the sentence explains what
        // pressing it costs, and the screen offers no second way out.
        assert_eq!(harness.get_all_by_label("Remove").count(), 2);
    }

    /// The whole screen with a Portal row on it, at the size the other watch
    /// tests draw: the marker is a real label on a real row.
    #[test]
    fn the_watch_screen_with_a_portal_row_renders_to_png() {
        let mut harness = screen_harness(state_with(vec![
            watch("/Users/chaos/Movies/Rushes", "Rushes 2026", "upload", true, 3),
            portal_watch("/Users/chaos/Live_Work/Ep 04", "Shows / Nightfall / Ep 04", "mirror", true, 0),
        ]));
        harness.run_steps(2);
        harness.get_by_label(PORTAL_MARKER);
        harness.snapshot("watch_portal");
    }

    #[test]
    fn a_very_long_path_is_elided_but_kept() {
        let long = format!("/Users/chaos/{}", "a-very-long-folder-name/".repeat(40));
        let harness = screen_harness(state_with(vec![watch(&long, &"Label ".repeat(60), "upload", true, 999)]));
        harness.get_by_label("999");
        // The sub line carries the whole path, shortened to `~`.
        let shortened = short_path(&long, "/Users/chaos");
        harness.get_by_label(&shortened);
    }

    // ---------- a duplex is one job with two sync lanes ----------

    /// The title line the owner asked for, exactly: the project's name and then
    /// `· Smart In/Out Tray`, and a line per lane under it with its tray in
    /// brackets.
    #[test]
    fn a_duplex_draws_one_job_with_both_lane_labels() {
        let harness = screen_harness(state_with(luc()));

        // The job, in the owner's words: the project, then the tray.
        let (title, _) = job_title(&harness, "LUC");
        // Its two lanes, each with its own tray named.
        let inbound = harness.get_by_label(LANE_INBOUND);
        let outbound = harness.get_by_label(LANE_OUTBOUND);
        assert_eq!(LANE_INBOUND, "Inbound (In Tray)");
        assert_eq!(LANE_OUTBOUND, "Outbound (Out Tray)");

        // Under the title, inbound first, one row apart, the rows on the pitch
        // the list has always used.
        assert!(inbound.rect().center().y > title.rect().center().y);
        assert!(outbound.rect().center().y > inbound.rect().center().y);
        assert!(
            (outbound.rect().center().y - inbound.rect().center().y - ROW_HEIGHT).abs() < 1.0,
            "the lanes are rows: {:?} {:?}",
            inbound.rect(),
            outbound.rect()
        );

        // Each lane carries its folder's path, the Frame.io side, its own count
        // and its own state.
        harness.get_by_label("~/Live_Work/LUC/1_DRIVE");
        harness.get_by_label("~/Live_Work/LUC/0_EXPORTS");
        // `LUC` three times: the title line's name, and the Frame.io side on
        // each of the two lanes.
        assert_eq!(harness.get_all_by_label("LUC").count(), 3, "the title, and both lanes");
        harness.get_by_label("4");
        harness.get_by_label("2");
        assert_eq!(harness.get_all_by_label("Watching").count(), 2, "each lane its own state");

        // One Remove for the job, and every other action still on its lane.
        assert_eq!(harness.get_all_by_label("Remove").count(), 1);
        assert_eq!(harness.get_all_by_label("Pause").count(), 2);
        assert_eq!(harness.get_all_by_label("Edit").count(), 2);
        assert_eq!(harness.get_all_by_label("Show in Finder").count(), 2);

        // The two watches are not two rows of their own: no folder name is
        // printed, because the job is what is listed.
        assert!(harness.query_by_label("1_DRIVE").is_none());
        assert!(harness.query_by_label("0_EXPORTS").is_none());
    }

    /// A link with a sync rule but no exports folder: one lane, and a line that
    /// says which one is missing, in the owner's words.
    #[test]
    fn a_duplex_with_one_lane_says_which_is_not_configured() {
        let harness = screen_harness(state_with(vec![lane_watch(
            "/Users/chaos/Live_Work/LUC/1_DRIVE",
            "LUC",
            "download",
            true,
            4,
            "LUC",
            Lane::Inbound,
        )]));

        job_title(&harness, "LUC");
        harness.get_by_label(LANE_INBOUND);
        // The lane that is not there, and the plain statement of it.
        assert!(
            harness.query_by_label(LANE_OUTBOUND).is_none(),
            "the missing lane has no row of its own"
        );
        let absent = format!("{LANE_OUTBOUND} — {LANE_NOT_CONFIGURED}");
        let missing = harness.get_by_label(&absent);
        // One row under the lane that exists: a row's first line sits on the
        // same pitch the list has always used, and this line — alone in its row,
        // with no path under it — is centred in its row rather than on that first
        // line, so it reads a little lower than a lane's own words.
        let inbound = harness.get_by_label(LANE_INBOUND);
        let step = missing.rect().center().y - inbound.rect().center().y;
        assert!(
            step > ROW_HEIGHT && step < ROW_HEIGHT * 2.0,
            "the missing lane is the lane's next row, {step} px on: {:?} {:?}",
            missing.rect(),
            inbound.rect()
        );
        // A job with one lane still has one Remove, and one of every other
        // action.
        assert_eq!(harness.get_all_by_label("Remove").count(), 1);
        assert_eq!(harness.get_all_by_label("Pause").count(), 1);
    }

    /// The same, the other way round: the exports folder configured, the client's
    /// folder not. The line names the lane that is missing, not a fixed one.
    #[test]
    fn an_outbound_only_duplex_names_the_inbound_lane_as_missing() {
        let harness = screen_harness(state_with(vec![lane_watch(
            "/Users/chaos/Live_Work/LUC/0_EXPORTS",
            "LUC",
            "upload",
            false,
            0,
            "LUC",
            Lane::Outbound,
        )]));

        harness.get_by_label(LANE_OUTBOUND);
        harness.get_by_label(&format!("{LANE_INBOUND} — {LANE_NOT_CONFIGURED}"));
        harness.get_by_label("Paused");
        assert!(harness.query_by_label(LANE_INBOUND).is_none());
    }

    /// A watch the user made keeps the row it has always had: no title, no lane
    /// labels, no marker — and it sits beside a job without taking anything from
    /// it.
    #[test]
    fn a_user_watch_keeps_its_own_row_beside_a_job() {
        let harness = screen_harness(state_with(vec![
            watch("/Users/chaos/Movies/Rushes", "Rushes 2026", "upload", true, 3),
            lane_watch("/Users/chaos/Live_Work/LUC/1_DRIVE", "LUC", "download", true, 4, "LUC", Lane::Inbound),
            lane_watch("/Users/chaos/Live_Work/LUC/0_EXPORTS", "LUC", "upload", true, 2, "LUC", Lane::Outbound),
        ]));

        // The user's row, exactly as before: the folder's name, its path, its
        // destination, its count, its four actions.
        harness.get_by_label("Rushes");
        harness.get_by_label("~/Movies/Rushes");
        harness.get_by_label("Rushes 2026");
        harness.get_by_label("3");
        assert_eq!(harness.get_all_by_label("Remove").count(), 2, "the user's row and the job's");
        // Its row is one row, and the job follows it on the same pitch: the
        // folder's name and the job's title are both a row's first line.
        let rushes = harness.get_by_label("Rushes").rect();
        let title = job_title(&harness, "LUC").1.rect();
        assert!(
            (title.center().y - rushes.center().y - ROW_HEIGHT).abs() < 1.0,
            "{rushes:?} {title:?}"
        );
        // And only the job is Portal's: one marker on the screen.
        assert_eq!(harness.get_all_by_label(PORTAL_MARKER).count(), 1);
    }

    /// Two duplexes in one list: each is one block, and the blocks are in label
    /// order whatever order the records arrived in — the list of a portal listing
    /// is not the order a person reads.
    #[test]
    fn two_duplexes_make_two_blocks_in_label_order() {
        let harness = screen_harness(state_with(vec![
            lane_watch("/Users/chaos/Live_Work/LUC/1_DRIVE", "LUC", "download", true, 0, "LUC", Lane::Inbound),
            lane_watch("/Users/chaos/Live_Work/ARCH/0_EXPORTS", "Archive", "upload", true, 0, "ARCH", Lane::Outbound),
            lane_watch("/Users/chaos/Live_Work/LUC/0_EXPORTS", "LUC", "upload", true, 0, "LUC", Lane::Outbound),
            lane_watch("/Users/chaos/Live_Work/ARCH/1_DRIVE", "Archive", "download", true, 0, "ARCH", Lane::Inbound),
        ]));

        // Each job's title line, in the order the blocks are drawn.
        let titles: Vec<_> = harness.get_all_by_label(TRAY_SUFFIX).map(|n| n.rect()).collect();
        assert_eq!(titles.len(), 2, "two jobs, two title lines");
        let (archive, luc) = (titles[0], titles[1]);
        assert!(archive.top() < luc.top(), "Archive first: {archive:?} {luc:?}");

        // Each block holds its own two lanes, in its own place: four lanes, two
        // titles, and the lanes of one job under that job's title.
        assert_eq!(harness.get_all_by_label(LANE_INBOUND).count(), 2);
        assert_eq!(harness.get_all_by_label(LANE_OUTBOUND).count(), 2);
        let inbound: Vec<_> = harness.get_all_by_label(LANE_INBOUND).map(|r| r.rect()).collect();
        assert!(inbound[0].top() < inbound[1].top());
        assert!(
            (inbound[0].center().y - archive.center().y - ROW_HEIGHT).abs() < 1.0,
            "the first job's inbound lane sits under the first title"
        );
        // Two jobs, one Remove each.
        assert_eq!(harness.get_all_by_label("Remove").count(), 2);
    }

    /// The job's Remove is the job's: it asks the app to drop both watches, since
    /// Portal made them as one project. A lane's own pause still names its own
    /// watch.
    #[test]
    fn the_jobs_remove_and_a_lanes_pause_are_addressed_by_their_own_rows() {
        let mut harness = screen_harness(state_with(luc()));

        harness.get_by_label("Remove").click();
        harness.run();
        assert_eq!(
            harness.state().requests,
            vec![
                Request::RemoveWatch("id:/Users/chaos/Live_Work/LUC/1_DRIVE".to_string()),
                Request::RemoveWatch("id:/Users/chaos/Live_Work/LUC/0_EXPORTS".to_string()),
            ],
            "requests: {:?}",
            harness.state().requests
        );

        // The second lane's pause is the second row's watch.
        let mut harness = screen_harness(state_with(luc()));
        let pauses: Vec<_> = harness.get_all_by_label("Pause").collect();
        pauses[1].click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::SetWatchEnabled {
                id: "id:/Users/chaos/Live_Work/LUC/0_EXPORTS".to_string(),
                on: false,
            }),
            "requests: {:?}",
            harness.state().requests
        );
        // The row that was clicked is the one that reads Paused now, and the lane
        // above it is still watching.
        assert!(!harness.state().watches[1].enabled);
        assert!(harness.state().watches[0].enabled);
    }

    /// The job is Portal's, and says so with the sentence that already exists:
    /// removing it here is not the end of it.
    #[test]
    fn the_job_carries_the_portal_sentence() {
        let mut harness = screen_harness(state_with(luc()));

        // The marker is the same four words a Portal row carries, with the same
        // sentence on it; the title line and the job's own tooltip say it too.
        harness.get_by_label(PORTAL_MARKER).hover();
        harness.run();
        assert!(
            harness.query_by_label(PORTAL_SENTENCE).is_some(),
            "the job's tooltip is the sentence a Portal row already carries"
        );
        assert!(PORTAL_SENTENCE.contains("Remove the marker in Portal"));

        // And the Remove itself says what pressing it costs: Portal puts the job
        // back unless its marker is cleared in Portal. Its name is still Remove.
        let mut harness = screen_harness(state_with(luc()));
        harness.get_by_label("Remove").hover();
        harness.run();
        assert!(
            harness.query_by_label(PORTAL_SENTENCE).is_some(),
            "the job's Remove carries the sentence"
        );
    }

    // ---------- the title line's two runs, and the job's way into Portal ----------

    /// The name and the tray are two runs on one line, in two styles: the
    /// project's at the row's own weight, and `· Smart In/Out Tray` in the detail
    /// style the row already sets `Portal` and its paths in — one step down, and
    /// muted — so the project reads as the name and the tray as what the job is.
    #[test]
    fn the_jobs_title_sets_the_project_first_and_the_tray_one_step_down() {
        let harness = screen_harness(state_with(luc()));
        let (name, suffix) = job_title(&harness, "LUC");

        // The words, unchanged: `· ` and the tray's own name.
        assert_eq!(TRAY_SUFFIX, format!("· {SMART_IN_OUT_TRAY}"));

        // One line, the tray after the name rather than under it.
        assert!(
            (suffix.rect().center().y - name.rect().center().y).abs() < 2.0,
            "one line: {:?} {:?}",
            name.rect(),
            suffix.rect()
        );
        assert!(
            suffix.rect().left() >= name.rect().right(),
            "the tray's words come after the project's: {:?} {:?}",
            name.rect(),
            suffix.rect()
        );

        // Two styles, each the font it is set in: the name is a line of the body
        // font and the suffix a line of the detail font, so the suffix is the
        // smaller of the two by exactly the step the row's paths already take.
        let body = harness.ctx.fonts_mut(|fonts| fonts.row_height(&theme::body()));
        let detail = harness.ctx.fonts_mut(|fonts| fonts.row_height(&theme::detail()));
        assert!(
            (name.rect().height() - body).abs() < 0.5,
            "the name is a body line: {:?}, {body} px",
            name.rect()
        );
        assert!(
            (suffix.rect().height() - detail).abs() < 0.5,
            "the tray is a detail line: {:?}, {detail} px",
            suffix.rect()
        );
        assert!(detail < body, "the detail step is the smaller one");
    }

    /// `Open in Portal` opens the page the job's link is on: the server from
    /// Settings, and the link id out of the watch's `origin` — the outbound
    /// lane's `#exports` off it, so both lanes answer the same page.
    #[test]
    fn open_in_portal_asks_the_app_for_the_links_page() {
        let mut harness = screen_harness(state_with(luc()));
        harness.state_mut().settings.portal_url = "https://portal.nightfall.tv".to_string();
        harness.run();

        let url = "https://portal.nightfall.tv/upload-links/LUC".to_string();
        // The tooltip is the address, before it is pressed — and the job's
        // sentence is not on screen as well: one link, one tooltip.
        harness.get_by_label(OPEN_IN_PORTAL).hover();
        harness.run();
        assert!(harness.query_by_label(&url).is_some(), "the tooltip is the URL it opens");
        assert!(
            harness.query_by_label(PORTAL_SENTENCE).is_none(),
            "the link's own tooltip, not the job's sentence over it"
        );

        harness.get_by_label(OPEN_IN_PORTAL).click();
        harness.run();
        assert_eq!(
            harness.state().requests,
            vec![Request::OpenUrl(url)],
            "one action, one page, and nothing else asked of the app"
        );
    }

    /// With no Portal address there is nothing to open: the action is drawn off,
    /// says why on hover, and pushes nothing at all.
    #[test]
    fn open_in_portal_is_off_without_a_portal_address() {
        let mut harness = screen_harness(state_with(luc()));
        assert!(harness.state().settings.portal_url.is_empty());

        harness.get_by_label(OPEN_IN_PORTAL).hover();
        harness.run();
        assert!(
            harness.query_by_label(PORTAL_ADDRESS_MISSING).is_some(),
            "the reason on hover: `{PORTAL_ADDRESS_MISSING}`"
        );

        harness.get_by_label(OPEN_IN_PORTAL).click();
        harness.run();
        assert!(
            harness.state().requests.is_empty(),
            "an action with nothing to open asks for nothing: {:?}",
            harness.state().requests
        );
        // The job's other actions are untouched by it.
        assert_eq!(harness.get_all_by_label("Remove").count(), 1);
    }

    /// A link's page is the server's address and the id, and nothing else: the
    /// outbound lane's origin is the same id with a suffix on it.
    #[test]
    fn a_links_page_is_the_servers_address_and_the_id() {
        assert_eq!(
            portal_link_url("https://host", "L1").as_deref(),
            Some("https://host/upload-links/L1")
        );
        // Settings stores the address without a trailing slash; one typed with
        // it anyway does not double it.
        assert_eq!(
            portal_link_url("https://host/", "L1").as_deref(),
            Some("https://host/upload-links/L1")
        );
        assert_eq!(
            portal_link_url("  https://host  ", "L1").as_deref(),
            Some("https://host/upload-links/L1")
        );
        // The outbound lane's origin carries `#exports`; its page does not.
        let (id, lane) = crate::model::lane_of("portal:L1#exports", "upload");
        assert_eq!(lane, Lane::Outbound);
        assert_eq!(
            portal_link_url("https://host", id.as_deref().unwrap_or_default()).as_deref(),
            Some("https://host/upload-links/L1")
        );
        // Nothing to open: no address, or no id.
        assert_eq!(portal_link_url("", "L1"), None);
        assert_eq!(portal_link_url("   ", "L1"), None);
        assert_eq!(portal_link_url("https://host", ""), None);
    }

    /// The two-lane job, at the size the other watch snapshots are drawn at.
    ///
    /// With a Portal address set, so the snapshot shows the job as a configured
    /// app draws it: the title's two runs, and `Open in Portal` live beside the
    /// Remove.
    #[test]
    fn the_watch_screen_with_a_duplex_renders_to_png() {
        let mut rows = vec![watch("/Users/chaos/Movies/Rushes", "Rushes 2026", "upload", true, 3)];
        rows.extend(luc());
        let mut harness = screen_harness(state_with(rows));
        harness.state_mut().settings.portal_url = "https://portal.nightfall.tv".to_string();
        harness.run_steps(2);
        job_title(&harness, "LUC");
        harness.get_by_label(LANE_INBOUND);
        harness.get_by_label(LANE_OUTBOUND);
        harness.snapshot("watch_duplex");
    }

}
