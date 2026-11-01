//! The History screen.
//!
//! The filters row (search field, then the All / Uploads / Downloads segmented
//! filter) and the table of past transfers, as `frontend/dist/index.html` and
//! `app.js` build them: a dim 11.5 px header over 56 px rows showing the name
//! with its kind tile, the date, the file count, the size and the status.
//!
//! The shell (`crate::app`) supplies the `.view` padding, the scroll area and
//! the page head; the tokens come from [`crate::theme`].

use crate::icons;
use crate::model::{HistoryRow, Request, UiState};
use crate::theme;
use egui::{
    Align, Color32, CornerRadius, Id, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind,
    vec2,
};

/// `.list-head { height: 32px }`.
const HEAD_HEIGHT: f32 = theme::size::LIST_HEAD_H;
/// `.row-main { min-height: 56px }`.
/// `.row-main { min-height: 56px }` plus its own `border-bottom: 1px`, so a
/// row measures 57 px from border to border.
const ROW_HEIGHT: f32 = theme::size::ROW_H + 1.0;
/// `.row { padding: 0 12px }`.
const ROW_PADDING_X: f32 = theme::space::ROW_X;
/// `.hist-grid { gap: 12px }`.
const COLUMN_GAP: f32 = theme::space::XL;
/// The fixed columns of `.hist-grid` this screen keeps; the name takes the rest.
const DATE_WIDTH: f32 = 160.0;
const FILES_WIDTH: f32 = 72.0;
const SIZE_WIDTH: f32 = 76.0;
const STATUS_WIDTH: f32 = 150.0;
/// Below this the name column would vanish; the table keeps it and lets the
/// fixed columns hold their width.
const MIN_NAME_WIDTH: f32 = 120.0;
/// `.filters .search { flex: 0 1 320px; min-width: 160px }`.
const SEARCH_MAX_WIDTH: f32 = 320.0;
const SEARCH_MIN_WIDTH: f32 = 160.0;

/// Which kinds the table shows: `data-kind` in the webview, "" / "upload" /
/// "download".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KindFilter {
    #[default]
    All,
    Uploads,
    Downloads,
}

impl KindFilter {
    pub const ALL: [KindFilter; 3] = [KindFilter::All, KindFilter::Uploads, KindFilter::Downloads];

    /// The button words in `index.html`.
    pub fn label(self) -> &'static str {
        match self {
            KindFilter::All => "All",
            KindFilter::Uploads => "Uploads",
            KindFilter::Downloads => "Downloads",
        }
    }

    /// The `kind` a row must have to show under this filter.
    pub fn matches(self, kind: &str) -> bool {
        match self {
            KindFilter::All => true,
            KindFilter::Uploads => kind == "upload",
            KindFilter::Downloads => kind == "download",
        }
    }

    /// The filter as the app spells it (`History(query, kind, 500)`): "" for
    /// every kind, "upload" or "download" for one.
    pub fn query(self) -> &'static str {
        match self {
            KindFilter::All => "",
            KindFilter::Uploads => "upload",
            KindFilter::Downloads => "download",
        }
    }
}

/// What the screen asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// "Open log folder" (`OpenLogs`).
    OpenLogs,
    /// "Export CSV" (`ExportHistory`).
    ExportCsv,
    /// The search text or the kind filter changed: ask the backend again for
    /// the first 500 rows (`call('History', query, kind, 500)`). Until that
    /// answer lands the table filters the rows it already holds.
    Filter { query: String, kind: KindFilter },
}

/// How long the search box waits for typing to stop before it asks the app
/// again: `$('#history-q').addEventListener('input', debounce(loadHistory, 250))`
/// in app.js. The local filter narrows the rows already held on every keystroke,
/// so nothing on screen waits for the round trip.
const SEARCH_DEBOUNCE: f64 = 0.25;

/// The screen, wired to the app: whatever [`show_with_actions`] reports is
/// handed to the shell as a [`Request`].
pub fn show(ui: &mut egui::Ui, state: &mut UiState) {
    if let Some(action) = show_with_actions(ui, state) {
        ask(state, action);
    }
}

/// Hands one [`Action`] to the app, in the app's terms.
fn ask(state: &mut UiState, action: Action) {
    match action {
        Action::OpenLogs => state.ask(Request::OpenLogs),
        Action::ExportCsv => state.ask(Request::ExportCsv),
        Action::Filter { query, kind } => state.ask(Request::History {
            query,
            kind: kind.query().to_string(),
        }),
    }
}

/// The screen, reporting at most one [`Action`] per frame.
pub fn show_with_actions(ui: &mut egui::Ui, state: &mut UiState) -> Option<Action> {
    let mut action = None;
    // `.filters` sits straight under the 64 px head, `.list` is 20 px below it,
    // and `.row` has only its 1 px border: the shell's 8 px item spacing would
    // add a gap to every one of those, so it is zeroed here.
    ui.spacing_mut().item_spacing = vec2(theme::space::M, 0.0);

    crate::app::page_head(ui, "History", |ui| {
        if subtle_button(ui, "Open log folder") {
            action = Some(Action::OpenLogs);
        }
        if ui.button(RichText::new("Export CSV").font(theme::body())).clicked() {
            action = Some(Action::ExportCsv);
        }
    });

    // ---------- filters ----------
    let query_id = Id::new("fiotransfer-egui.history.query");
    let kind_id = Id::new("fiotransfer-egui.history.kind");
    let mut query = ui
        .data(|data| data.get_temp::<String>(query_id))
        .unwrap_or_default();
    let mut kind = ui
        .data(|data| data.get_temp::<KindFilter>(kind_id))
        .unwrap_or_default();

    // When the field was last typed in, and the (query, kind) the app was last
    // asked for: together they are the webview's `debounce(loadHistory, 250)`.
    let pending_id = Id::new("fiotransfer-egui.history.pending");
    let sent_id = Id::new("fiotransfer-egui.history.sent");

    let mut query_changed = false;
    let mut kind_changed = false;
    let filters_width = ui.available_width();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::space::L;

        let search_width = (filters_width * 0.4).clamp(SEARCH_MIN_WIDTH, SEARCH_MAX_WIDTH);
        query_changed = search_field(ui, search_width, &mut query);
        kind_changed = segmented(ui, &mut kind);
    });

    // `.list { margin-top: 20px }`
    ui.add_space(theme::space::HUGE);

    // A kind button reloads the table on the click (`#history-kind button`), and
    // cancels a query that was still waiting.
    let now = ui.input(|input| input.time);
    let mut wanted: Option<(String, KindFilter)> = None;
    if query_changed {
        ui.data_mut(|data| {
            data.insert_temp(query_id, query.clone());
            data.insert_temp(pending_id, now);
        });
    }
    if kind_changed {
        ui.data_mut(|data| {
            data.insert_temp(kind_id, kind);
            data.remove_temp::<f64>(pending_id);
        });
        wanted = Some((query.clone(), kind));
    }

    // The query is asked once the field has been quiet for the debounce.
    if let Some(at) = ui.data(|data| data.get_temp::<f64>(pending_id)) {
        let waited = now - at;
        if waited >= SEARCH_DEBOUNCE {
            ui.data_mut(|data| {
                data.remove_temp::<f64>(pending_id);
            });
            wanted.get_or_insert_with(|| (query.clone(), kind));
        } else {
            // No keystroke will come: ask for the frame that does the asking.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(SEARCH_DEBOUNCE - waited));
        }
    }

    if let Some((wanted_query, wanted_kind)) = wanted {
        let asked = ui.data(|data| data.get_temp::<(String, KindFilter)>(sent_id));
        if asked != Some((wanted_query.clone(), wanted_kind)) {
            ui.data_mut(|data| {
                data.insert_temp(query_id, wanted_query.clone());
                data.insert_temp(kind_id, wanted_kind);
                data.insert_temp(sent_id, (wanted_query.clone(), wanted_kind));
            });
            action = Some(Action::Filter {
                query: wanted_query,
                kind: wanted_kind,
            });
        }
    }

    // ---------- table ----------
    let rows = visible_rows(&state.history, &query, kind);
    if rows.is_empty() {
        empty_state(ui, "No history");
        return action;
    }

    let columns = columns((ui.available_width() - 2.0 * ROW_PADDING_X).max(0.0));
    list_head(ui, &columns);
    for row in rows {
        history_row(ui, row, &columns);
    }

    action
}

/// The rows the search box and the kind filter leave showing.
///
/// The webview asks the backend (`History(query, kind, 500)`); this front end
/// narrows the rows it already holds, so typing never waits on a round trip.
pub fn visible_rows<'a>(
    rows: &'a [HistoryRow],
    query: &str,
    kind: KindFilter,
) -> Vec<&'a HistoryRow> {
    let needle = query.trim().to_lowercase();
    rows.iter()
        .filter(|row| kind.matches(&row.kind))
        .filter(|row| needle.is_empty() || row.title.to_lowercase().contains(&needle))
        .collect()
}

/// `.hist-grid`: the name takes what is left, the other columns are fixed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Columns {
    name: f32,
    date: f32,
    files: f32,
    size: f32,
    status: f32,
}

fn columns(available: f32) -> Columns {
    let fixed = DATE_WIDTH + FILES_WIDTH + SIZE_WIDTH + STATUS_WIDTH + 4.0 * COLUMN_GAP;
    Columns {
        name: (available - fixed).max(MIN_NAME_WIDTH),
        date: DATE_WIDTH,
        files: FILES_WIDTH,
        size: SIZE_WIDTH,
        status: STATUS_WIDTH,
    }
}

/// `.input-wrap.search`: a `--surface` field with the magnifier inside it
/// (`.input-icon { left: 9px }`) and the text inset 32 px for it. On focus the
/// border turns `--brand` with a `--brand-soft` ring, as `input:focus` does.
fn search_field(ui: &mut egui::Ui, width: f32, query: &mut String) -> bool {
    let (rect, _) = ui.allocate_exact_size(
        vec2(width, theme::size::CONTROL_H),
        Sense::hover(),
    );
    let radius = theme::R_S;
    ui.painter()
        .rect_filled(rect, radius, theme::SURFACE);

    // The magnifier: a 16 px `.input-icon` at 9 px in from the left edge.
    let icon = Rect::from_center_size(
        egui::pos2(rect.left() + 9.0 + 8.0, rect.center().y),
        vec2(16.0, 16.0),
    );
    icons::paint(ui.painter(), icon.center(), theme::icon::CONTROL, icons::Icon::Search, theme::TEXT_3);

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(
                egui::pos2(rect.left() + 32.0, rect.top()),
                rect.max,
            ))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let response = child.add(
        egui::TextEdit::singleline(query)
            .hint_text("Search name, file or link")
            .frame(false)
            .margin(Margin::symmetric(0, theme::space::M as i8))
            .desired_width(child.available_width()),
    );

    let border = if response.has_focus() {
        theme::stroke_brand()
    } else {
        theme::stroke_border_2()
    };
    // `border: 1px` is inside the 32 px box (`box-sizing: border-box`).
    ui.painter()
        .rect_stroke(rect, radius, border, StrokeKind::Inside);
    if response.has_focus() {
        // `box-shadow: 0 0 0 3px var(--brand-soft)`
        ui.painter().rect_stroke(
            rect.expand(3.0),
            CornerRadius::same(theme::R_S_PX.saturating_add(3)),
            Stroke::new(3.0_f32, theme::BRAND_SOFT),
            StrokeKind::Outside,
        );
    }
    response.changed()
}

/// One full-width strip of the table: the hover wash, the row padding, the
/// 1 px bottom border, and a child `Ui` laid out left to right.
fn strip(ui: &mut egui::Ui, height: f32, hoverable: bool, add: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width().max(0.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if hoverable && response.hovered() {
        // `.row:hover { background: #ffffff05 }`
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
}

/// The dim header of the table (`.list-head`).
fn list_head(ui: &mut egui::Ui, columns: &Columns) {
    strip(ui, HEAD_HEIGHT, false, |ui| {
        head_cell(ui, columns.name, false, "Name");
        head_cell(ui, columns.date, false, "Date");
        head_cell(ui, columns.files, true, "Files");
        head_cell(ui, columns.size, true, "Size");
        head_cell(ui, columns.status, false, "Status");
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

/// One past transfer (`.row .row-main`).
fn history_row(ui: &mut egui::Ui, row: &HistoryRow, columns: &Columns) {
    strip(ui, ROW_HEIGHT, true, |ui| {
        // Name: the kind tile, then the title.
        cell(ui, columns.name, ROW_HEIGHT, false, |ui| {
            ui.spacing_mut().item_spacing.x = theme::space::XL;
            kind_tile(ui, &row.kind);
            ui.add(theme::truncating(
                RichText::new(&row.title)
                    .font(theme::body())
                    .color(theme::TEXT)
                    .strong(),
            ))
            .on_hover_text(&row.title);
        });

        cell(ui, columns.date, ROW_HEIGHT, false, |ui| {
            ui.add(theme::truncating(
                RichText::new(&row.when)
                    .font(theme::body())
                    .color(theme::TEXT_2),
            ));
        });
        cell(ui, columns.files, ROW_HEIGHT, true, |ui| {
            ui.label(
                RichText::new(&row.files)
                    .font(theme::body())
                    .color(theme::TEXT_2),
            );
        });
        cell(ui, columns.size, ROW_HEIGHT, true, |ui| {
            ui.label(
                RichText::new(&row.size)
                    .font(theme::body())
                    .color(theme::TEXT_2),
            );
        });
        cell(ui, columns.status, ROW_HEIGHT, false, |ui| {
            status_chip(ui, &row.status, &row.kind);
        });
    });
}

/// A fixed-width column of a row.
///
/// The column is claimed with `allocate_exact_size` before its content is
/// drawn, so a short cell (a date, a size) still holds its full width and the
/// columns below stay in line with the header.
fn cell(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    right: bool,
    add: impl FnOnce(&mut egui::Ui),
) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let layout = if right {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(layout),
    );
    child.spacing_mut().item_spacing.x = COLUMN_GAP;
    add(&mut child);
    ui.add_space(COLUMN_GAP);
}

/// `.kind-icon`: a 30 x 30 tile in `--card`, with an up or down arrow.
fn kind_tile(ui: &mut egui::Ui, kind: &str) {
    let size = theme::size::KIND_ICON;
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::R_S, theme::CARD);
    painter.rect_stroke(rect, theme::R_S, theme::stroke_border(), StrokeKind::Inside);

    // `I.up` / `I.down` in app.js.
    let arrow = if kind == "download" { icons::Icon::Down } else { icons::Icon::Up };
    icons::paint(painter, rect.center(), theme::icon::CONTROL, arrow, theme::TEXT_2);
}

/// `.status`: a 7 px dot in the status colour, then the status word.
fn status_chip(ui: &mut egui::Ui, status: &str, kind: &str) {
    let (dot, _) = ui.allocate_exact_size(vec2(theme::size::DOT, theme::size::DOT), Sense::hover());
    ui.painter()
        .circle_filled(dot.center(), theme::size::DOT * 0.5, theme::status_color(status));
    ui.add_space(theme::space::S);

    let word = status_label(status, kind);
    ui.add(theme::truncating(
        RichText::new(&word)
            .font(theme::note())
            .color(theme::status_text(status)),
    ))
    .on_hover_text(word);
}

/// `statusLabel` in app.js, word for word.
pub fn status_label(status: &str, kind: &str) -> String {
    let word = match status {
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
        "canceled" => "Cancelled",
        "interrupted" => "Interrupted",
        "skipped" => "Skipped",
        _ => return status.to_string(),
    };
    word.to_string()
}

/// `.empty`: 72 px of air, then the title. History has no icon tile.
fn empty_state(ui: &mut egui::Ui, heading: &str) {
    ui.add_space(72.0);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new(heading)
                .font(theme::section())
                .color(theme::TEXT)
                .strong(),
        );
    });
}

/// `.btn.subtle`: no fill, no outline, `--text-2` until hovered.
fn subtle_button(ui: &mut egui::Ui, text: &str) -> bool {
    let mut clicked = false;
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.inactive.fg_stroke = theme::hairline(theme::TEXT_2);
            widgets.hovered.weak_bg_fill = theme::HOVER;
            widgets.hovered.bg_stroke = Stroke::NONE;
            widgets.hovered.fg_stroke = theme::hairline(theme::TEXT);
            widgets.active.weak_bg_fill = theme::ACTIVE;
            widgets.active.bg_stroke = Stroke::NONE;
            widgets.active.fg_stroke = theme::hairline(theme::TEXT);
        }
        clicked = ui
            .add(
                egui::Button::new(
                    RichText::new(text)
                        .font(theme::body())
                        .color(theme::TEXT_2),
                )
                .min_size(vec2(0.0, theme::size::CONTROL_H)),
            )
            .clicked();
    });
    clicked
}

/// `.segmented`: a `--surface` tray with the active button in `--raised`.
fn segmented(ui: &mut egui::Ui, kind: &mut KindFilter) -> bool {
    let mut changed = false;
    theme::segmented_frame().show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = theme::space::XXS;
        // `.segmented button { height: 26px; padding: 0 12px }`.
        ui.spacing_mut().interact_size.y = theme::size::CONTROL_SM_H;
        ui.spacing_mut().button_padding = vec2(theme::space::XL, theme::space::XS);
        for option in KindFilter::ALL {
            let active = *kind == option;
            let colour = if active { theme::TEXT } else { theme::TEXT_2 };
            let button = egui::Button::new(
                RichText::new(option.label()).font(theme::body()).color(colour),
            )
            .fill(if active { theme::RAISED } else { Color32::TRANSPARENT })
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(4))
            .min_size(vec2(0.0, theme::size::CONTROL_SM_H));
            if ui.add(button).clicked() && !active {
                *kind = option;
                changed = true;
            }
        }
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn row(title: &str, kind: &str, status: &str, when: &str) -> HistoryRow {
        HistoryRow {
            title: title.to_string(),
            kind: kind.to_string(),
            status: status.to_string(),
            when: when.to_string(),
            files: "4/5".to_string(),
            size: "1.50 GB".to_string(),
        }
    }

    /// Three rows with distinct counts and sizes: a table of identical cells
    /// would make every query ambiguous.
    fn three_rows() -> Vec<HistoryRow> {
        vec![
            HistoryRow {
                title: "Reel A.mov".to_string(),
                kind: "upload".to_string(),
                status: "done".to_string(),
                when: "Today, 10:24".to_string(),
                files: "1/1".to_string(),
                size: "1.50 GB".to_string(),
            },
            HistoryRow {
                title: "Rushes".to_string(),
                kind: "download".to_string(),
                status: "failed".to_string(),
                when: "Yesterday, 18:02".to_string(),
                files: "3/9".to_string(),
                size: "812 MB".to_string(),
            },
            HistoryRow {
                title: "Interview.wav".to_string(),
                kind: "upload".to_string(),
                status: "partial".to_string(),
                when: "Mar 4, 2026, 09:00".to_string(),
                files: "2/2".to_string(),
                size: "244 KB".to_string(),
            },
        ]
    }

    fn screen_harness(state: UiState) -> Harness<'static, UiState> {
        Harness::new_state(
            |ctx, state: &mut UiState| {
                crate::app::apply_style_once(ctx);
                egui::CentralPanel::default().show(ctx, |ui| show(ui, state));
            },
            state,
        )
    }

    #[test]
    fn the_table_shows_the_dim_header_and_every_row() {
        let harness = screen_harness(UiState {
            history: three_rows(),
            ..UiState::default()
        });

        for head in ["Name", "Date", "Files", "Size", "Status"] {
            harness.get_by_label(head);
        }
        harness.get_by_label("Reel A.mov");
        harness.get_by_label("Today, 10:24");
        harness.get_by_label("1/1");
        harness.get_by_label("3/9");
        harness.get_by_label("2/2");
        harness.get_by_label("1.50 GB");
        harness.get_by_label("812 MB");
        harness.get_by_label("244 KB");
        harness.get_by_label("Completed");
        harness.get_by_label("Failed");
        harness.get_by_label("Completed with errors");
        harness.get_by_label("Export CSV");
        harness.get_by_label("Open log folder");
        harness.get_by_label("All");
        harness.get_by_label("Uploads");
        harness.get_by_label("Downloads");
    }

    #[test]
    fn typing_in_the_search_box_filters_the_table() {
        let mut harness = screen_harness(UiState {
            history: three_rows(),
            ..UiState::default()
        });

        // The placeholder is the webview's: "Search name, file or link".
        {
            let field = harness.get_by_role(egui::accesskit::Role::TextInput);
            field.focus();
            field.type_text("reel");
        }
        harness.run();

        harness.get_by_label("Reel A.mov");
        assert!(
            harness.query_by_label("Rushes").is_none(),
            "a row that does not match the query must not be drawn"
        );
    }

    #[test]
    fn the_empty_state_says_no_history_and_hides_the_header() {
        let harness = screen_harness(UiState::default());

        harness.get_by_label("No history");
        assert!(
            harness.query_by_label("Name").is_none(),
            "the webview hides the table header when there are no rows"
        );
        // The filters stay usable: a search with no rows is not an error.
        harness.get_by_label("All");
    }

    #[test]
    fn the_kind_filter_narrows_the_rows() {
        let rows = three_rows();
        assert_eq!(visible_rows(&rows, "", KindFilter::All).len(), 3);
        assert_eq!(visible_rows(&rows, "", KindFilter::Uploads).len(), 2);
        assert_eq!(visible_rows(&rows, "", KindFilter::Downloads).len(), 1);
        let downloads = visible_rows(&rows, "", KindFilter::Downloads);
        assert_eq!(downloads.first().map(|r| r.title.as_str()), Some("Rushes"));
    }

    #[test]
    fn the_search_box_matches_the_title_and_ignores_case() {
        let rows = three_rows();
        assert_eq!(visible_rows(&rows, "reel", KindFilter::All).len(), 1);
        assert_eq!(visible_rows(&rows, "  REEL  ", KindFilter::All).len(), 1);
        assert_eq!(visible_rows(&rows, "nothing here", KindFilter::All).len(), 0);
        // The two filters combine.
        assert_eq!(visible_rows(&rows, "reel", KindFilter::Uploads).len(), 1);
        assert_eq!(visible_rows(&rows, "reel", KindFilter::Downloads).len(), 0);
    }

    #[test]
    fn the_segmented_filter_switches_rows() {
        let mut harness = screen_harness(UiState {
            history: three_rows(),
            ..UiState::default()
        });

        harness.get_by_label("Reel A.mov");
        harness.get_by_label("Rushes");

        harness.get_by_label("Downloads").click();
        harness.run();

        harness.get_by_label("Rushes");
        assert!(
            harness.query_by_label("Reel A.mov").is_none(),
            "an upload must not show under the Downloads filter"
        );
    }

    #[test]
    fn export_csv_is_reported_as_an_action() {
        let seen: std::rc::Rc<std::cell::RefCell<Vec<Action>>> = Default::default();
        let sink = seen.clone();
        let mut harness = Harness::new_state(
            move |ctx, state: &mut UiState| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(action) = show_with_actions(ui, state) {
                        sink.borrow_mut().push(action);
                    }
                });
            },
            UiState {
                history: three_rows(),
                ..UiState::default()
            },
        );

        harness.get_by_label("Export CSV").click();
        harness.run();

        assert!(
            seen.borrow().contains(&Action::ExportCsv),
            "actions seen: {:?}",
            seen.borrow()
        );
    }

    #[test]
    fn the_kind_filter_is_spelled_the_way_the_app_takes_it() {
        assert_eq!(KindFilter::All.query(), "");
        assert_eq!(KindFilter::Uploads.query(), "upload");
        assert_eq!(KindFilter::Downloads.query(), "download");
    }

    #[test]
    fn open_log_folder_asks_the_app() {
        let mut harness = screen_harness(UiState {
            history: three_rows(),
            ..UiState::default()
        });

        harness.get_by_label("Open log folder").click();
        harness.run();

        assert!(
            harness.state().requests.contains(&Request::OpenLogs),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn export_csv_asks_the_app() {
        let mut harness = screen_harness(UiState {
            history: three_rows(),
            ..UiState::default()
        });

        harness.get_by_label("Export CSV").click();
        harness.run();

        assert!(
            harness.state().requests.contains(&Request::ExportCsv),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn the_kind_filter_asks_the_app_at_once() {
        let mut harness = screen_harness(UiState {
            history: three_rows(),
            ..UiState::default()
        });

        // A segmented button reloads the table on the click, undebounced.
        harness.get_by_label("Downloads").click();
        harness.run();

        assert!(
            harness.state().requests.contains(&Request::History {
                query: String::new(),
                kind: "download".to_string(),
            }),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn typing_asks_the_app_once_the_debounce_elapses() {
        let mut harness = screen_harness(UiState {
            history: three_rows(),
            ..UiState::default()
        });

        {
            let field = harness.get_by_role(egui::accesskit::Role::TextInput);
            field.focus();
            field.type_text("reel");
        }
        harness.run();
        // The rows already held are narrowed at once; the app is asked later.
        harness.get_by_label("Reel A.mov");

        std::thread::sleep(std::time::Duration::from_millis(400));
        harness.run();

        assert!(
            harness.state().requests.contains(&Request::History {
                query: "reel".to_string(),
                kind: String::new(),
            }),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn status_words_match_the_webview() {
        assert_eq!(status_label("running", "upload"), "Uploading");
        assert_eq!(status_label("running", "download"), "Downloading");
        assert_eq!(status_label("running", "mirror"), "In progress");
        assert_eq!(status_label("resolving", "upload"), "Preparing");
        assert_eq!(status_label("partial", "upload"), "Completed with errors");
        assert_eq!(status_label("canceled", "upload"), "Cancelled");
        assert_eq!(status_label("skipped", "upload"), "Skipped");
        assert_eq!(status_label("something-new", "upload"), "something-new");
    }

    #[test]
    fn the_status_dot_uses_the_stylesheet_colours() {
        assert_eq!(theme::status_color("done"), theme::SUCCESS);
        assert_eq!(theme::status_color("failed"), theme::ERROR);
        assert_eq!(theme::status_color("partial"), theme::NOTICE);
        assert_eq!(theme::status_color("queued"), theme::INFO);
        assert_eq!(theme::status_color("running"), theme::BRAND);
        // `.status.error { color: var(--error-text) }`; a failed row keeps
        // `--text-2`, the same as the webview.
        assert_eq!(theme::status_text("error"), theme::ERROR_TEXT);
        assert_eq!(theme::status_text("failed"), theme::TEXT_2);
    }

    #[test]
    fn the_columns_fit_the_window_and_never_collapse() {
        let wide = columns(1000.0);
        assert_eq!(wide.date, DATE_WIDTH);
        assert_eq!(wide.files, FILES_WIDTH);
        assert_eq!(wide.size, SIZE_WIDTH);
        assert_eq!(wide.status, STATUS_WIDTH);
        assert!(wide.name > MIN_NAME_WIDTH);

        // Narrower than the fixed columns: the name keeps its minimum instead
        // of going negative.
        let narrow = columns(120.0);
        assert_eq!(narrow.name, MIN_NAME_WIDTH);
        assert_eq!(columns(0.0).name, MIN_NAME_WIDTH);
    }

    #[test]
    fn long_titles_and_wide_rows_do_not_panic() {
        let long = "x".repeat(4000);
        let harness = screen_harness(UiState {
            history: vec![row(&long, "upload", "done", "Today, 10:24")],
            ..UiState::default()
        });
        // Truncation is a paint decision; the screen still holds the whole title.
        harness.get_by_label(&long);
    }
}
