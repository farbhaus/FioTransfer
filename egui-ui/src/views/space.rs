//! The Space screen: what Frame.io is holding, per project.
//!
//! The egui reading of the webview's `#view-space` — the Space section of its
//! `index.html` and `app.js`, and the `.space-*` block of `style.css`: a
//! donut of the space each project takes, then a legend row per project with its
//! colour, its name, the workspace and account it lives under, its size, its
//! file count and a Delete button. The page head carries the total and Refresh,
//! which starts a walk (`RefreshSpace`); while the walk runs the screen says
//! "Walking Frame.io…" rather than implying the figures are current.
//!
//! Under the head is the headline: one line per account, from the account's own
//! `storage_limit` and `storage_usage` — "12.4 GB of 2 TB used · 1.99 TB left",
//! "640 GB used · no limit" when Frame.io sends `storage_limit: null`, and
//! "usage not reported" when it sends no figure at all. It is the account's
//! number, not a sum of the rows, so it stands while a walk runs. A row says
//! whether its size came from a walk of the project ("walked", with a file
//! count) or from Frame.io's `Project.storage` ("from Frame.io", with a dash
//! where the count would be).
//!
//! Deleting is the one destructive thing this screen can ask for, so it never
//! happens on a click: the row's Delete opens [`confirm_dialog`], which names the
//! project, its size and where it lives, and says in as many words that the
//! delete happens on Frame.io only and touches nothing on this Mac. Cancel is the
//! default — it takes the keyboard focus, Enter takes it, and so do Escape and a
//! click off the dialog — and only "Delete on Frame.io" reports [`Action::Delete`],
//! which the shell turns into `call('DeleteProject', account, project)`. Nothing
//! here reads, moves or removes a local file, and nothing here talks to Frame.io.
//!
//! The shell (`crate::app`) supplies the `.view` padding, the scroll area and the
//! page head; the tokens come from [`crate::theme`].

use crate::model::{fmt_bytes, Request, SpaceAccount, SpaceRow, UiState, View};
use crate::theme;
use egui::{
    Align, Color32, CornerRadius, Id, Label, Layout, Margin, Painter, Pos2, Rect, Response, RichText,
    Sense, Shape, Stroke, StrokeKind, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

// ---------- `.space-*`, as `style.css` writes it ----------

/// `.space-body { display: grid; grid-template-columns: 288px minmax(0, 1fr);
/// gap: 20px; margin-top: 8px }`.
const CHART_CARD_W: f32 = 288.0;
const BODY_GAP: f32 = 20.0;
const BODY_TOP: f32 = 8.0;
/// `@media (max-width: 999px) { .space-body { grid-template-columns: 1fr } }`.
/// The screen only knows the column it was given, so the window is column plus
/// sidebar — the same arithmetic `views::watch` uses for its media queries.
const STACK_AT: f32 = 999.0;
const SIDEBAR_W: f32 = theme::size::SIDEBAR_W;

/// `.space-chart { padding: 20px 16px; border: 1px solid var(--border);
/// border-radius: var(--r-l) }`.
const CHART_PAD_X: f32 = 16.0;
const CHART_PAD_Y: f32 = 20.0;

/// The SVG in `renderSpaceChart`: `C = 240`, `r1 = 104`, `r0 = 72`.
const CHART: f32 = 240.0;
const RING_OUTER: f32 = 104.0;
const RING_INNER: f32 = 72.0;
/// One quad per 2 degrees of arc: finer than the 240 px ring can show.
const RING_STEP_DEG: f32 = 2.0;
/// `.donut-value { font: 600 21px }` — the only 21 px type in the app.
const DONUT_VALUE: f32 = 21.0;
/// The two `<text>` baselines are 18 px apart, around the ring's centre.
const DONUT_GAP: f32 = 3.0;

/// `.space-status { min-height: 24px; margin-top: 2px; gap: 10px; font-size: 12.5px }`.
const STATUS_TOP: f32 = 2.0;
/// `.space-quota { margin-top: 2px }` — the headline sits on the same 2 px.
const QUOTA_TOP: f32 = 2.0;
const STATUS_H: f32 = 24.0;

/// `.space-grid { grid-template-columns: minmax(0, 1fr) 104px 56px 104px;
/// gap: 12px }`, and the `max-width: 999px` media query narrows the three
/// fixed columns.
const COL_SIZE: f32 = 104.0;
const COL_FILES: f32 = 56.0;
const COL_ACTIONS: f32 = 104.0;
const COL_SIZE_NARROW: f32 = 92.0;
const COL_FILES_NARROW: f32 = 48.0;
const COL_ACTIONS_NARROW: f32 = 92.0;
const COL_GAP: f32 = theme::space::XL;
/// `.list-head { height: 32px }`, `.row-main { min-height: 56px }` plus the
/// row's own 1 px border, `.row { padding: 0 12px }`.
const HEAD_H: f32 = theme::size::LIST_HEAD_H;
const ROW_H: f32 = theme::size::ROW_H + 1.0;
const ROW_PAD_X: f32 = theme::space::ROW_X;
/// `.space-row .swatch { width: 8px; height: 8px; border-radius: 2px;
/// margin-top: 1px }`, and `.cell-name { gap: 12px }`.
const SWATCH: f32 = 8.0;
const SWATCH_R: CornerRadius = CornerRadius::same(2);

/// `.empty { padding: 72px 0 40px }`, `.empty-icon { margin-bottom: 14px }`,
/// `.empty-sub { margin-top: 4px }`, `#space-empty .btn { margin-top: 16px }`.
const EMPTY_TOP: f32 = 72.0;
const EMPTY_ICON_GAP: f32 = 14.0;
const EMPTY_TITLE_GAP: f32 = 4.0;
const EMPTY_BTN_GAP: f32 = 16.0;

/// `.btn.sm { height: 26px; padding: 0 9px; font-size: 12px }`, `.btn { gap: 7px }`,
/// `.btn svg { width: 16px }` and `.space-row .space-del svg { width: 14px }`.
const BTN_SM_H: f32 = theme::size::CONTROL_SM_H;
const BTN_SM_PAD: f32 = 9.0;
const BTN_GLYPH_GAP: f32 = 7.0;
const GLYPH: f32 = theme::icon::CONTROL;
const GLYPH_SM: f32 = 14.0;

/// `.btn.danger-solid:hover { background: #ff4a63 }` — written inline in the
/// stylesheet rather than as a `:root` token, so it is a constant here.
const DANGER_SOLID_HOVER: Color32 = Color32::from_rgb(0xff, 0x4a, 0x63);

/// The ring's slice gap: `pad = Math.min(0.9, span * 0.12)` degrees at each end,
/// so two arcs that touch never read as one.
const SLICE_PAD_MAX: f32 = 0.9;
const SLICE_PAD_FRACTION: f32 = 0.12;
/// `ringArc` draws nothing under this: `if (span <= 0.08) return ''`. A slice
/// that thin is one file against terabytes — invisible, and not worth a shape.
const SLICE_MIN_DEG: f32 = 0.08;

/// The dialog's words, from `index.html` and `app.js`. They are quoted in the
/// tests, so none of them can be softened without a test noticing.
pub const CONFIRM_TITLE: &str = "Delete project on Frame.io?";
pub const NOTHING_LOCAL: &str = "This deletes the project on Frame.io only. Nothing on this Mac \
    is touched: no local folder, file or watch folder changes.";
pub const CONFIRM_DELETE: &str = "Delete on Frame.io";
pub const CONFIRM_CANCEL: &str = "Cancel";
/// The empty state's three variants, as `renderSpace` words them.
pub const EMPTY_TITLE: &str = "Nothing counted yet";
pub const EMPTY_SUB: &str = "Refresh adds up what every project on Frame.io is holding.";
pub const EMPTY_SIGNED_OUT_TITLE: &str = "Sign in to see your space";
pub const EMPTY_SIGNED_OUT_SUB: &str =
    "The size of each Frame.io project appears here once you are signed in.";
pub const EMPTY_WALKING_TITLE: &str = "Walking Frame.io…";
pub const EMPTY_WALKING_SUB: &str = "Adding up every project. This can take a minute on a big account.";
/// `#space-chart svg`'s `aria-label`.
pub const CHART_LABEL: &str = "What each project holds on Frame.io, largest first";

/// Where the open confirmation lives: egui's own per-context store, so the view
/// stays a pure renderer over [`UiState`] and the dialog survives a frame
/// without a field on the model.
const CONFIRM_ID: &str = "fiotransfer-egui.space.confirm";

/// What the screen asks the shell to do.
///
/// A view is a pure renderer over [`UiState`], so everything that needs the
/// network is reported back instead of called: the shell turns these into
/// `call('RefreshSpace')` and `call('DeleteProject', account, project)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// "Refresh": start a walk over every account, workspace and project.
    Refresh,
    /// The confirmed delete. `account_id` and `project_id` are what
    /// `DeleteProject` takes; `name` is for the toast.
    Delete {
        account_id: String,
        project_id: String,
        name: String,
    },
}

/// The screen, wired to the shell: every [`Action`] becomes the [`Request`] the
/// shell knows the call for, the way `views::watch` and `views::history` ask.
pub fn show(ui: &mut egui::Ui, state: &mut UiState) {
    if let Some(action) = show_with_actions(ui, state) {
        ask(state, action);
    }
}

/// What an [`Action`] asks the app for. `Refresh` walks Frame.io again;
/// `Delete` deletes one project there, naming the account as well as the
/// project — the same project id can exist in a second account.
fn ask(state: &mut UiState, action: Action) {
    match action {
        Action::Refresh => state.ask(Request::RefreshSpace),
        Action::Delete { account_id, project_id, name } => {
            state.ask(Request::DeleteProject { account_id, project_id, name })
        }
    }
}

/// The screen, reporting at most one [`Action`] per frame.
pub fn show_with_actions(ui: &mut egui::Ui, state: &mut UiState) -> Option<Action> {
    let mut action = None;
    // The blocks abut: the head, the status line, the body. The shell's 8 px
    // item spacing would add a gap to each, so it is zeroed and the gaps are the
    // stylesheet's own.
    ui.spacing_mut().item_spacing = vec2(theme::space::M, 0.0);

    let walking = state.space_walking;
    let logged_in = state.logged_in;
    // Only `view` is written here, so the fields are borrowed apart: the rows
    // and the accounts stay readable while the empty state can still switch the
    // screen. The accounts are read first: the headline sits under the head.
    let UiState { space: rows, space_accounts: accounts, view, .. } = state;

    crate::app::page_head(ui, "Space", |ui| {
        // `.head-actions` reads right to left here: Refresh at the edge, the
        // total to its left.
        let refresh = button(
            ui,
            Kind::Card,
            Size::Regular,
            Some(Glyph::Refresh),
            "Refresh",
            "Refresh",
            !walking,
        );
        if refresh.clicked() {
            action = Some(Action::Refresh);
        }
        if !rows.is_empty() {
            ui.label(
                RichText::new(head_total(rows))
                    .font(theme::note())
                    .color(theme::TEXT_2),
            );
        }
    });

    quota_strip(ui, accounts);

    if walking {
        status_line(ui);
    }

    if rows.is_empty() {
        // `.space-body` is hidden when there is nothing to chart; the empty
        // block takes its place.
        if empty_state(ui, view, logged_in, walking) {
            action = Some(Action::Refresh);
        }
        return action;
    }

    ui.add_space(BODY_TOP);
    let stacked = ui.available_width() + SIDEBAR_W <= STACK_AT;
    if stacked {
        // `@media (max-width: 999px)`: one column, the chart above its legend.
        chart_card(ui, rows);
        ui.add_space(BODY_GAP);
        legend_card(ui, rows, stacked);
    } else {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = BODY_GAP;
            chart_card(ui, rows);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                legend_card(ui, rows, stacked);
            });
        });
    }

    // The confirmation goes last and on top: `egui::Modal` puts it in the
    // foreground layer and keeps clicks off the rows underneath.
    if let Some(confirm) = confirm(ui) {
        match confirm_dialog(ui.ctx(), &confirm) {
            Decision::Cancel => clear_confirm(ui),
            Decision::Delete => {
                clear_confirm(ui);
                action = Some(Action::Delete {
                    account_id: confirm.account_id.clone(),
                    project_id: confirm.project_id.clone(),
                    name: confirm.name.clone(),
                });
            }
            Decision::None => {}
        }
    }

    action
}

// ---------- the head and the status line ----------

/// `.space-total`: "15.1 GB · 3 projects" — all of Frame.io, not one row.
fn head_total(rows: &[SpaceRow]) -> String {
    format!(
        "{} · {}",
        fmt_bytes(total_bytes(rows)),
        project_word(rows.len())
    )
}

/// `.space-quota`: the headline under the head, one line per account — how much
/// the account uses, what its limit is, and how much that leaves. Bytes only.
///
/// A refresh that has not answered yet, or an account list Frame.io never sent,
/// draws nothing rather than a line of zeroes.
fn quota_strip(ui: &mut egui::Ui, accounts: &[SpaceAccount]) {
    if accounts.is_empty() {
        return;
    }
    ui.add_space(QUOTA_TOP);
    for account in accounts {
        ui.add(Label::new(
            RichText::new(quota_line(account))
                .font(theme::note())
                .color(theme::TEXT_2),
        ));
    }
}

/// One account's headline: its name, then [`quota_sentence`].
pub fn quota_line(account: &SpaceAccount) -> String {
    let name = account.name.trim();
    if name.is_empty() {
        quota_sentence(account)
    } else {
        format!("{name} · {}", quota_sentence(account))
    }
}

/// The three cases, in Frame.io's own terms:
///
/// * a limit and a usage — "12.4 GB of 2 TB used · 1.99 TB left";
/// * a usage and no limit — `storage_limit: null` is "no limit", never 0 B;
/// * no usage — Frame.io said nothing, so neither does the headline about a
///   figure it does not have.
pub fn quota_sentence(account: &SpaceAccount) -> String {
    match (account.used, account.limit) {
        (Some(used), Some(limit)) => format!(
            "{} of {} used · {} left",
            fmt_bytes(used.max(0)),
            fmt_bytes(limit.max(0)),
            fmt_bytes(left_of(limit, used))
        ),
        (Some(used), None) => format!("{} used · no limit", fmt_bytes(used.max(0))),
        (None, Some(limit)) => format!("{} limit · usage not reported", fmt_bytes(limit.max(0))),
        (None, None) => "no limit · usage not reported".to_string(),
    }
}

/// What a limit leaves. A soft limit can be exceeded; the headline says "0 B
/// left" rather than a negative size.
fn left_of(limit: i64, used: i64) -> i64 {
    limit.saturating_sub(used).max(0)
}

/// `.space-status`: the spinner, then what is being walked and what that means.
fn status_line(ui: &mut egui::Ui) {
    ui.add_space(STATUS_TOP);
    // `.space-status { min-height: 24px }`: the line keeps its height while it is
    // up, so the body below does not jump when a walk ends.
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), STATUS_H),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = theme::space::L;
            spinner(ui);
            ui.label(
                RichText::new("Walking Frame.io…")
                    .font(theme::note())
                    .color(theme::TEXT_BODY),
            );
            ui.label(
                RichText::new("Adding up every project. Nothing is downloaded, nothing is deleted.")
                    .font(theme::note())
                    .color(theme::TEXT_3),
            );
        },
    );
}

/// `.spinner`: a 12 px ring with a turning head, on the stylesheet's 0.7 s cycle.
fn spinner(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    let centre = rect.center();
    let radius = 5.25;
    let painter = ui.painter();
    painter.circle_stroke(centre, radius, Stroke::new(1.5_f32, theme::BORDER_3));
    let phase = (ui.input(|input| input.time) as f32 / 0.7).fract() * 360.0;
    const STEPS: usize = 10;
    let mut head = Vec::with_capacity(STEPS + 1);
    for step in 0..=STEPS {
        let angle = (phase - 90.0 + 90.0 * step as f32 / STEPS as f32).to_radians();
        head.push(pos2(
            centre.x + radius * angle.cos(),
            centre.y + radius * angle.sin(),
        ));
    }
    painter.add(Shape::line(head, Stroke::new(1.5_f32, theme::TEXT_2)));
    // The head only turns while a walk is running — nothing else asks to repaint.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(60));
}

// ---------- the chart ----------

/// One ring segment, in degrees clockwise from 12 o'clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    pub start: f32,
    pub span: f32,
    pub colour: Color32,
}

/// The ring's segments, one per project that holds anything, in row order.
///
/// A project with no bytes gets no segment, but it keeps its place in the colour
/// ramp so its legend swatch still matches the arc it would have had — the
/// legend draws an empty swatch for it, as `renderSpaceLegend` does. The last
/// segment is stretched to close the circle, so rounding never leaves a hairline
/// gap at 12 o'clock.
pub fn slices(rows: &[SpaceRow]) -> Vec<Slice> {
    let total = total_bytes(rows);
    if total <= 0 {
        return Vec::new();
    }
    let mut out: Vec<Slice> = Vec::with_capacity(rows.len());
    let mut at = 0.0_f32;
    for (index, row) in rows.iter().enumerate() {
        let span = row.share(total) * 360.0;
        if span <= 0.0 {
            continue;
        }
        out.push(Slice {
            start: at,
            span,
            colour: space_color(index),
        });
        at += span;
    }
    if let Some(last) = out.last_mut() {
        last.span = (360.0 - last.start).max(last.span);
    }
    out
}

/// The gap at each end of a slice, and what is left to draw of it:
/// `pad = Math.min(0.9, span * 0.12)` degrees.
pub fn padded(span: f32) -> (f32, f32) {
    let pad = SLICE_PAD_MAX.min(span * SLICE_PAD_FRACTION);
    (pad, (span - 2.0 * pad).max(0.0))
}

/// Every project's bytes added up — the figure in the hole and in the head.
pub fn total_bytes(rows: &[SpaceRow]) -> i64 {
    rows.iter().map(|row| row.bytes.max(0)).sum()
}

/// `spaceColor(i)` from app.js: the stylesheet's `--brand` first, then a ramp
/// that turns 18° of hue per neighbour towards `--info`, alternating lightness
/// every six so two arcs that touch never read as one.
pub fn space_color(index: usize) -> Color32 {
    if index == 0 {
        return theme::BRAND;
    }
    let hue = 243.0 - 18.0 * (index % 6) as f32;
    let band = (index / 6) % 2 == 0;
    let lightness = (if band { 66.0 } else { 52.0 }) - (((index / 12) as f32) * 6.0).min(18.0);
    hsl(hue, if band { 96.0 } else { 76.0 }, lightness.max(28.0))
}

/// `hslHex(h, s, l)` from app.js, in its own terms: `hue` in degrees, the other
/// two in percent.
fn hsl(hue: f32, saturation: f32, lightness: f32) -> Color32 {
    let hue = hue.rem_euclid(360.0);
    let s = saturation / 100.0;
    let l = lightness / 100.0;
    let k = |n: f32| (n + hue / 30.0) % 12.0;
    let a = s * l.min(1.0 - l);
    let channel = |n: f32| l - a * (k(n) - 3.0).min(9.0 - k(n)).clamp(-1.0, 1.0);
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(byte(channel(0.0)), byte(channel(8.0)), byte(channel(4.0)))
}

/// `.space-chart`: the panel the donut sits in.
fn chart_card(ui: &mut egui::Ui, rows: &[SpaceRow]) {
    let width = CHART_CARD_W.min(ui.available_width());
    let (card, _) = ui.allocate_exact_size(vec2(width, CHART + 2.0 * CHART_PAD_Y), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(card, theme::R_L, theme::PANEL);
    painter.rect_stroke(card, theme::R_L, theme::stroke_border(), StrokeKind::Inside);

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(card.shrink2(vec2(CHART_PAD_X, CHART_PAD_Y)))
            .layout(Layout::top_down(Align::Center)),
    );
    child.spacing_mut().item_spacing = Vec2::ZERO;
    donut(&mut child, rows);
}

/// The ring, and the figure in its hole over "on Frame.io".
fn donut(ui: &mut egui::Ui, rows: &[SpaceRow]) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(CHART), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Image, true, CHART_LABEL));

    let centre = rect.center();
    let painter = ui.painter();
    let segments = slices(rows);
    if segments.is_empty() {
        // Every project empty, or nothing measured: a flat track, not a lie.
        painter.circle_stroke(
            centre,
            (RING_INNER + RING_OUTER) * 0.5,
            Stroke::new(RING_OUTER - RING_INNER, theme::RAISED),
        );
    } else {
        for slice in &segments {
            let (pad, span) = padded(slice.span);
            if span <= SLICE_MIN_DEG {
                continue;
            }
            ring(
                painter,
                centre,
                slice.start + pad,
                slice.start + pad + span,
                slice.colour,
            );
        }
    }

    // The hole: `.donut-value` over `.donut-label`, centred as a block where the
    // two `<text>` elements sit.
    let total = fmt_bytes(total_bytes(rows));
    let hole = Rect::from_center_size(centre, vec2((RING_INNER - 8.0) * 2.0, (RING_INNER - 8.0) * 2.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(hole)
            .layout(Layout::top_down(Align::Center)),
    );
    child.spacing_mut().item_spacing.y = DONUT_GAP;
    let block = DONUT_VALUE * 1.2 + DONUT_GAP + theme::font::CAPTION * 1.2;
    child.add_space(((hole.height() - block) * 0.5).max(0.0));
    child.add(Label::new(
        RichText::new(total)
            .font(egui::FontId::new(DONUT_VALUE, egui::FontFamily::Proportional))
            .color(theme::TEXT),
    ));
    child.add(Label::new(
        RichText::new("on Frame.io")
            .font(theme::caption())
            .color(theme::TEXT_3),
    ));
}

/// `ringArc` as a run of convex quads: every 2° step is a four-sided
/// `convex_polygon` between the outer and the inner radius, so a slice wider than
/// a half circle — one project holding everything — is still a fan of convex
/// shapes and never one concave polygon.
fn ring(painter: &Painter, centre: Pos2, from: f32, to: f32, colour: Color32) {
    let span = to - from;
    if span <= 0.0 {
        return;
    }
    let steps = ((span / RING_STEP_DEG).ceil() as usize).max(1);
    let point = |degrees: f32, radius: f32| {
        // 0° is 12 o'clock and the ring runs clockwise, as `ptOn` has it.
        let radians = (degrees - 90.0).to_radians();
        pos2(
            centre.x + radius * radians.cos(),
            centre.y + radius * radians.sin(),
        )
    };
    for step in 0..steps {
        let a0 = from + span * step as f32 / steps as f32;
        let a1 = from + span * (step + 1) as f32 / steps as f32;
        painter.add(Shape::convex_polygon(
            vec![
                point(a0, RING_OUTER),
                point(a1, RING_OUTER),
                point(a1, RING_INNER),
                point(a0, RING_INNER),
            ],
            colour,
            Stroke::NONE,
        ));
    }
}

// ---------- the legend ----------

/// `.space-legend`: the `--panel` card, its `.list-head` and one row per project.
fn legend_card(ui: &mut egui::Ui, rows: &[SpaceRow], narrow: bool) {
    theme::panel_card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        let columns = columns(ui.available_width(), narrow);
        list_head(ui, &columns);
        for (index, row) in rows.iter().enumerate() {
            legend_row(ui, index, row, &columns);
        }
    });
}

/// `.space-grid`'s columns: the name takes what is left, the other three are the
/// stylesheet's own widths.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Columns {
    name: f32,
    size: f32,
    files: f32,
    actions: f32,
}

fn columns(available: f32, narrow: bool) -> Columns {
    let (size, files, actions) = if narrow {
        (COL_SIZE_NARROW, COL_FILES_NARROW, COL_ACTIONS_NARROW)
    } else {
        (COL_SIZE, COL_FILES, COL_ACTIONS)
    };
    Columns {
        name: (available - 2.0 * ROW_PAD_X - size - files - actions - 3.0 * COL_GAP).max(0.0),
        size,
        files,
        actions,
    }
}

fn list_head(ui: &mut egui::Ui, columns: &Columns) {
    strip(ui, HEAD_H, false, |ui, _hovered| {
        cell(ui, columns.name, HEAD_H, false, |ui| {
            head_label(ui, "Project", false);
        });
        cell(ui, columns.size, HEAD_H, true, |ui| {
            head_label(ui, "Size", true);
        });
        cell(ui, columns.files, HEAD_H, true, |ui| {
            head_label(ui, "Files", true);
        });
        cell(ui, columns.actions, HEAD_H, false, |_ui| {});
    });
}

fn head_label(ui: &mut egui::Ui, text: &str, right: bool) {
    let label = RichText::new(text)
        .font(theme::caption())
        .color(theme::TEXT_3);
    if right {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(Label::new(label));
        });
    } else {
        ui.add(Label::new(label));
    }
}

/// One project (`.row .space-row`): its swatch, its name and where it lives, its
/// size, its file count, and the Delete button that opens the confirmation.
fn legend_row(ui: &mut egui::Ui, index: usize, row: &SpaceRow, columns: &Columns) {
    // A project that holds nothing has no arc, so its swatch is the empty track's
    // colour rather than a colour that appears nowhere in the chart.
    let colour = if row.bytes > 0 {
        space_color(index)
    } else {
        theme::RAISED
    };
    strip(ui, ROW_H, true, |ui, hovered| {
        name_cell(ui, columns.name, row, colour);
        cell(ui, columns.size, ROW_H, true, |ui| {
            ui.add(Label::new(
                RichText::new(fmt_bytes(row.bytes))
                    .font(theme::body())
                    .color(theme::TEXT_2),
            ));
        });
        cell(ui, columns.files, ROW_H, true, |ui| {
            // Only a walk counts files. A row Frame.io sized has no count, and a
            // dash says "not counted" where a 0 would say "none".
            let files = if row.walked {
                row.files.max(0).to_string()
            } else {
                NOT_COUNTED.to_string()
            };
            ui.add(Label::new(
                RichText::new(files)
                    .font(theme::body())
                    .color(theme::TEXT_2),
            ));
        });
        cell(ui, columns.actions, ROW_H, false, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // `.space-row:hover .space-del { color: var(--error-text) }` — the
                // trash can turns red when the row under the pointer says which
                // project it would delete.
                if delete_button(ui, row, hovered) {
                    set_confirm(ui, Some(Confirm::from_row(row)));
                }
            });
        });
    });
}

/// The name cell: an 8 px swatch at the name's own top edge, then the two lines
/// (`.name` and `.sub`) that `.space-row .cell-name { align-items: flex-start }`
/// puts beside it.
///
/// The block is bounded by the column: `.sub` is `overflow: hidden;
/// text-overflow: ellipsis`, so a long place line is cut rather than allowed to
/// shove the size and the Delete button to the right.
fn name_cell(ui: &mut egui::Ui, width: f32, row: &SpaceRow, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
    let inset = SWATCH + COL_GAP;
    let names_w = (width - inset).max(0.0);
    // A plain child, not `scope_builder`: a scope would advance this row's
    // cursor to the names' own width and pull the size and the Delete button
    // after it.
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(
                pos2(rect.left() + inset, rect.top()),
                vec2(names_w, ROW_H),
            ))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.set_max_width(names_w);
    child.vertical(|ui| {
        ui.set_max_width(names_w);
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add(
            Label::new(
                RichText::new(&row.name)
                    .font(theme::body())
                    .color(theme::TEXT),
            )
            .truncate(),
        )
        .on_hover_text(&row.name);
        sub_line(ui, row, names_w);
    });
    let names = child.min_rect();

    // `.space-row .swatch { margin-top: 1px }`, on the first line's own top.
    ui.painter().rect_filled(
        Rect::from_min_size(pos2(rect.left(), names.top() + 1.0), Vec2::splat(SWATCH)),
        SWATCH_R,
        colour,
    );
    ui.add_space(COL_GAP);
}

/// `.sub`: "Shows · Farbhaus", then " · could not finish" in `--notice` when the
/// walk failed (with the reason on the pointer, as the webview puts it in `title`).
///
/// The place line gives way to the warning: the flag stays whole and the place is
/// what gets the ellipsis, which is the only part a reader can hover for the rest.
fn sub_line(ui: &mut egui::Ui, row: &SpaceRow, width: f32) {
    let line = theme::font::DETAIL * theme::font::BODY_LINE;
    ui.allocate_ui_with_layout(
        vec2(width, line),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.x = 0.0;
            let source_w = ui
                .painter()
                .layout_no_wrap(
                    format!(" · {}", source_word(row)),
                    theme::detail(),
                    theme::WHITE,
                )
                .size()
                .x;
            let warn_w = if row.incomplete() {
                ui.painter()
                    .layout_no_wrap(
                        " · could not finish".to_owned(),
                        theme::detail(),
                        theme::WHITE,
                    )
                    .size()
                    .x
            } else {
                0.0
            };
            let place_w = (width - source_w - warn_w).max(0.0);
            ui.allocate_ui_with_layout(
                vec2(place_w, line),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add(
                        Label::new(
                            RichText::new(where_line(row))
                                .font(theme::detail())
                                .color(theme::TEXT_3),
                        )
                        .truncate(),
                    );
                },
            );
            // Where the figure came from: a walk of this project, or Frame.io's
            // own `Project.storage`. The two are not the same measurement.
            ui.add(Label::new(
                RichText::new(" · ")
                    .font(theme::detail())
                    .color(theme::TEXT_3),
            ));
            ui.add(
                Label::new(
                    RichText::new(source_word(row))
                        .font(theme::detail())
                        .color(theme::TEXT_3),
                )
                .truncate(),
            )
            .on_hover_text(source_hint(row));
            if row.incomplete() {
                ui.add(Label::new(
                    RichText::new(" · ")
                        .font(theme::detail())
                        .color(theme::TEXT_3),
                ));
                ui.add(
                    Label::new(
                        RichText::new("could not finish")
                            .font(theme::detail())
                            .color(theme::NOTICE),
                    )
                    .truncate(),
                )
                .on_hover_text(&row.error);
            }
        },
    );
}

/// `.space-row`'s dash in the Files column: the row was sized by Frame.io, so
/// nothing counted its files.
pub const NOT_COUNTED: &str = "—";

/// How the row words where its size came from, as `walked` reports it.
pub fn source_word(row: &SpaceRow) -> &'static str {
    if row.walked {
        "walked"
    } else {
        "from Frame.io"
    }
}

/// The long form, on the pointer: what the short word in the row means.
pub fn source_hint(row: &SpaceRow) -> &'static str {
    if row.walked {
        "This figure is the sum of every file in the project, counted folder by folder."
    } else {
        "Frame.io reported this project's storage; its folders were not walked, so its files were not counted."
    }
}

/// The project's place: workspace and account, either one alone, or just
/// "Frame.io" when neither is known.
fn where_line(row: &SpaceRow) -> String {
    match (row.workspace_name.trim(), row.account_name.trim()) {
        ("", "") => "Frame.io".to_string(),
        (workspace, "") => workspace.to_string(),
        ("", account) => account.to_string(),
        (workspace, account) => format!("{workspace} · {account}"),
    }
}

/// "0 files", "1 file", "312 files" — `spaceFiles` in app.js.
fn file_word(files: i64) -> String {
    match files.max(0) {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

/// "1 project", "3 projects" — the head's count.
fn project_word(projects: usize) -> String {
    match projects {
        1 => "1 project".to_string(),
        n => format!("{n} projects"),
    }
}

// ---------- rows and cells ----------

/// One full-width strip: the hover wash, the 1 px bottom border and the row
/// padding, with a child `Ui` laid out left to right. The closure is told
/// whether the row is under the pointer, so an action can react to it.
fn strip(ui: &mut egui::Ui, height: f32, hoverable: bool, add: impl FnOnce(&mut egui::Ui, bool)) {
    let width = ui.available_width().max(0.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let hovered = hoverable && response.hovered();
    if hovered {
        // `.row:hover { background: #ffffff05 }`
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::ROW_HOVER);
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(ROW_PAD_X, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 0.0;
    add(&mut child, hovered);
    // `.row { border-bottom: 1px solid var(--border) }`, inside the 57 px pitch.
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 1.0, theme::hairline(theme::BORDER));
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
    child.spacing_mut().item_spacing.x = COL_GAP;
    add(&mut child);
    ui.add_space(COL_GAP);
}

// ---------- buttons ----------

/// Which `.btn` variant a button wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `.btn`: `--card`, `--border-2`, `--text-body`; `--raised` and `--text` on hover.
    Card,
    /// `.btn.subtle`: no fill, no border, `--text-2` until hovered.
    Subtle,
    /// `.btn.danger-solid`: the `--error` fill and white words.
    DangerSolid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    /// `.btn { height: 32px; padding: 0 12px }`.
    Regular,
    /// `.btn.sm { height: 26px; padding: 0 9px; font-size: 12px }`.
    Small,
}

/// The glyphs this screen draws, as the SVG paths in `index.html` and `app.js`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    /// `#space-refresh`'s arrow going round again.
    Refresh,
    /// `I.trash` in app.js.
    Trash,
    /// The Space mark: two circles and the two ticks between them.
    Donut,
    /// `.icon-btn` close cross.
    Close,
}

/// One `.btn`: measured from its words, painted by hand so the glyph and the
/// label can sit the 7 px apart the stylesheet asks for.
///
/// The accessible name is passed separately, so a row's button can say which
/// project it would delete while still reading "Delete".
fn button(
    ui: &mut egui::Ui,
    kind: Kind,
    size: Size,
    glyph: Option<Glyph>,
    label: &str,
    access: &str,
    enabled: bool,
) -> Response {
    let small = size == Size::Small;
    let font = if small { theme::detail() } else { theme::body() };
    let height = if small { BTN_SM_H } else { theme::size::CONTROL_H };
    let pad = if small { BTN_SM_PAD } else { theme::space::XL };
    let glyph_px = if small { GLYPH_SM } else { GLYPH };

    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), theme::WHITE)
        .size()
        .x;
    let width = 2.0 * pad + text_w + if glyph.is_some() { glyph_px + BTN_GLYPH_GAP } else { 0.0 };

    let mut response = None;
    ui.scope(|ui| {
        if !enabled {
            // `.btn:disabled`: no interaction, and the 40 % alpha painted below.
            ui.disable();
        }
        let (rect, clicked) = ui.allocate_exact_size(vec2(width, height), Sense::click());
        let hovered = enabled && clicked.hovered();
        let pressed = enabled && clicked.is_pointer_button_down_on();

        let (fill, stroke, words) = match kind {
            Kind::Card if hovered => (theme::RAISED, theme::hairline(theme::BORDER_3), theme::TEXT),
            Kind::Card => (theme::CARD, theme::stroke_border_2(), theme::TEXT_BODY),
            Kind::Subtle if hovered || pressed => (theme::HOVER, Stroke::NONE, theme::TEXT),
            Kind::Subtle => (Color32::TRANSPARENT, Stroke::NONE, theme::TEXT_2),
            Kind::DangerSolid if hovered => (DANGER_SOLID_HOVER, Stroke::NONE, theme::WHITE),
            Kind::DangerSolid => (theme::ERROR, Stroke::NONE, theme::WHITE),
        };
        // `.btn:disabled { opacity: .4 }`.
        let alpha = if enabled { 1.0 } else { 0.4 };

        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        if fill != Color32::TRANSPARENT {
            painter.rect_filled(rect, theme::R_S, fill.gamma_multiply(alpha));
        }
        if stroke != Stroke::NONE {
            painter.rect_stroke(
                rect,
                theme::R_S,
                Stroke {
                    width: stroke.width,
                    color: stroke.color.gamma_multiply(alpha),
                },
                StrokeKind::Inside,
            );
        }

        let mut x = rect.left() + pad;
        if let Some(glyph) = glyph {
            let box_ = Rect::from_center_size(
                pos2(x + glyph_px * 0.5, rect.center().y),
                Vec2::splat(glyph_px),
            );
            paint_glyph(&painter, box_, glyph, words.gamma_multiply(alpha), glyph_px);
            x += glyph_px + BTN_GLYPH_GAP;
        }
        painter.text(
            pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            words.gamma_multiply(alpha),
        );

        clicked.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, access));
        response = Some(clicked);
    });
    response.expect("the button is always added")
}

/// The row's Delete: `.btn.sm.subtle.danger.space-del` — trash, then the word,
/// turning `--error-text` when the row is under the pointer.
fn delete_button(ui: &mut egui::Ui, row: &SpaceRow, row_hovered: bool) -> bool {
    let font = theme::detail();
    let text_w = ui
        .painter()
        .layout_no_wrap("Delete".to_owned(), font.clone(), theme::WHITE)
        .size()
        .x;
    let width = 2.0 * BTN_SM_PAD + GLYPH_SM + BTN_GLYPH_GAP + text_w;
    let (rect, response) = ui.allocate_exact_size(vec2(width, BTN_SM_H), Sense::click());
    let hovered = response.hovered();
    let colour = if row_hovered || hovered {
        theme::ERROR_TEXT
    } else {
        theme::TEXT_2
    };
    if hovered {
        ui.painter()
            .rect_filled(rect, theme::R_S, theme::HOVER);
    }
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let glyph = Rect::from_center_size(
        pos2(rect.left() + BTN_SM_PAD + GLYPH_SM * 0.5, rect.center().y),
        Vec2::splat(GLYPH_SM),
    );
    paint_glyph(&painter, glyph, Glyph::Trash, colour, GLYPH_SM);
    painter.text(
        pos2(glyph.right() + BTN_GLYPH_GAP, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Delete",
        font,
        colour,
    );
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            true,
            format!("Delete {} on Frame.io", row.name),
        )
    });
    response.clicked()
}

/// `.icon-btn`: 28 x 28, transparent, `--text-3` until hovered. Used by the
/// dialog's close cross.
fn icon_button(ui: &mut egui::Ui, glyph: Glyph, label: &str) -> bool {
    let size = Vec2::splat(theme::size::ICON_BTN);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.on_hover_text(label);
    let colour = if response.hovered() {
        theme::TEXT
    } else {
        theme::TEXT_3
    };
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            theme::R_S,
            if response.is_pointer_button_down_on() {
                theme::ACTIVE
            } else {
                theme::HOVER
            },
        );
    }
    paint_glyph(ui.painter(), rect, glyph, colour, GLYPH);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response.clicked()
}

/// Paint `glyph` in `rect`, on the 20-unit canvas the SVG paths are written for.
fn paint_glyph(painter: &Painter, rect: Rect, glyph: Glyph, colour: Color32, size: f32) {
    let box_ = Rect::from_center_size(rect.center(), Vec2::splat(size));
    let scale = size / 20.0;
    let p = |x: f32, y: f32| pos2(box_.left() + x * scale, box_.top() + y * scale);
    // `stroke-width: 1.5` in the paths' 20-unit viewBox, never thinner than a pixel.
    let stroke = Stroke::new((1.5 * scale).max(1.0), colour);
    match glyph {
        // "M15.5 10a5.5 5.5 0 1 1-1.6-3.9M15.5 4v3.4h-3.4": an almost-circle with
        // an arrowhead where it stops.
        Glyph::Refresh => {
            const STEPS: usize = 40;
            let start = -50.0_f32;
            let mut arc = Vec::with_capacity(STEPS + 1);
            for step in 0..=STEPS {
                let angle = (start + 290.0 * step as f32 / STEPS as f32).to_radians();
                arc.push(pos2(
                    box_.center().x + 5.5 * scale * angle.cos(),
                    box_.center().y + 5.5 * scale * angle.sin(),
                ));
            }
            let tip = *arc.last().expect("an arc has points");
            painter.add(Shape::line(arc, stroke));
            // The head of the arrow, at the arc's end, pointing along it.
            painter.line_segment([tip, pos2(tip.x - 1.6 * scale, tip.y - 2.8 * scale)], stroke);
            painter.line_segment([tip, pos2(tip.x + 2.6 * scale, tip.y - 1.4 * scale)], stroke);
        }
        // "M4.5 6h11M8 6V4.5h4V6M6 6l.7 9.5h6.6L14 6": lid, handle, body.
        Glyph::Trash => {
            painter.add(Shape::line(vec![p(4.5, 6.0), p(15.5, 6.0)], stroke));
            painter.add(Shape::line(
                vec![p(8.0, 6.0), p(8.0, 4.5), p(12.0, 4.5), p(12.0, 6.0)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![p(6.0, 6.0), p(6.7, 15.5), p(13.3, 15.5), p(14.0, 6.0)],
                stroke,
            ));
        }
        // The Space mark: `circle 6.6`, `circle 2.4`, and two ticks from the
        // outer ring towards the inner one.
        Glyph::Donut => {
            painter.circle_stroke(p(10.0, 10.0), 6.6 * scale, stroke);
            painter.circle_stroke(p(10.0, 10.0), 2.4 * scale, stroke);
            painter.line_segment([p(10.0, 3.4), p(10.0, 7.6)], stroke);
            painter.line_segment([p(16.6, 10.0), p(12.4, 10.0)], stroke);
        }
        // "M6 6l8 8M14 6l-8 8".
        Glyph::Close => {
            painter.line_segment([p(6.0, 6.0), p(14.0, 14.0)], stroke);
            painter.line_segment([p(14.0, 6.0), p(6.0, 14.0)], stroke);
        }
    }
}

// ---------- the empty state ----------

/// `.empty`: the donut tile, a title, a sub line, and the button that gets the
/// screen out of that state. Returns true when the button asked for a walk.
fn empty_state(ui: &mut egui::Ui, view: &mut View, logged_in: bool, walking: bool) -> bool {
    ui.add_space(EMPTY_TOP);
    let mut refresh = false;
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let (rect, _) =
            ui.allocate_exact_size(Vec2::splat(theme::size::EMPTY_ICON), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, theme::R_EMPTY, theme::CARD);
        painter.rect_stroke(rect, theme::R_EMPTY, theme::stroke_border(), StrokeKind::Inside);
        paint_glyph(painter, rect, Glyph::Donut, theme::TEXT_3, theme::icon::EMPTY);
        ui.add_space(EMPTY_ICON_GAP);

        let (title, sub, action_label) = if !logged_in {
            (EMPTY_SIGNED_OUT_TITLE, EMPTY_SIGNED_OUT_SUB, Some("Open Settings"))
        } else if walking {
            (EMPTY_WALKING_TITLE, EMPTY_WALKING_SUB, None)
        } else {
            (EMPTY_TITLE, EMPTY_SUB, Some("Refresh"))
        };

        ui.add(Label::new(
            RichText::new(title)
                .font(theme::section())
                .color(theme::TEXT)
                .strong(),
        ));
        ui.add_space(EMPTY_TITLE_GAP);
        ui.add(Label::new(
            RichText::new(sub).font(theme::body()).color(theme::TEXT_3),
        ));

        if let Some(label) = action_label {
            ui.add_space(EMPTY_BTN_GAP);
            if button(ui, Kind::Card, Size::Regular, None, label, label, true).clicked() {
                if label == "Open Settings" {
                    *view = View::Settings;
                } else {
                    refresh = true;
                }
            }
        }
    });
    refresh
}

// ---------- the confirmation ----------

/// The project a confirmation is open for, as the row said it when its Delete
/// was pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Confirm {
    account_id: String,
    account_name: String,
    project_id: String,
    workspace_name: String,
    name: String,
    bytes: i64,
    files: i64,
}

impl Confirm {
    fn from_row(row: &SpaceRow) -> Self {
        Confirm {
            account_id: row.account_id.clone(),
            account_name: row.account_name.clone(),
            project_id: row.project_id.clone(),
            workspace_name: row.workspace_name.clone(),
            name: row.name.clone(),
            bytes: row.bytes,
            files: row.files,
        }
    }

    /// The dialog's "On Frame.io" row.
    fn place(&self) -> String {
        where_line(&SpaceRow {
            workspace_name: self.workspace_name.clone(),
            account_name: self.account_name.clone(),
            ..SpaceRow::default()
        })
    }
}

fn confirm(ui: &egui::Ui) -> Option<Confirm> {
    ui.data(|data| data.get_temp::<Confirm>(Id::new(CONFIRM_ID)))
}

fn set_confirm(ui: &mut egui::Ui, open: Option<Confirm>) {
    ui.data_mut(|data| match open {
        Some(confirm) => data.insert_temp(Id::new(CONFIRM_ID), confirm),
        None => {
            data.remove::<Confirm>(Id::new(CONFIRM_ID));
        }
    });
}

fn clear_confirm(ui: &mut egui::Ui) {
    set_confirm(ui, None);
}

/// What the dialog decided this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    None,
    Cancel,
    Delete,
}

/// `.modal-box.small`: the project, what it holds, where it lives, and the one
/// sentence that says what a delete here does — and does not — touch.
///
/// Cancel is the default in the three ways a dialog can say it: it takes the
/// keyboard focus when the dialog opens, Enter takes it, and so do Escape and a
/// click on the backdrop (`ModalResponse::should_close`).
fn confirm_dialog(ctx: &egui::Context, confirm: &Confirm) -> Decision {
    let mut decision = Decision::None;
    let mut cancel_clicked = false;
    let mut delete_clicked = false;
    let width = (ctx.content_rect().width() * 0.92).min(theme::size::MODAL_W);

    let modal = egui::Modal::new(Id::new(CONFIRM_ID))
        .frame(theme::modal_frame())
        .backdrop_color(theme::SCRIM_MODAL)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = Vec2::ZERO;

            // `.modal-head { padding: 14px 12px 10px 18px }`
            let _ = egui::Frame::new()
                .inner_margin(Margin {
                    left: 18,
                    right: 12,
                    top: 14,
                    bottom: 10,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = theme::space::M;
                        ui.add(Label::new(
                            RichText::new(CONFIRM_TITLE)
                                .font(theme::section())
                                .color(theme::TEXT)
                                .strong(),
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, Glyph::Close, "Close") {
                                cancel_clicked = true;
                            }
                        });
                    });
                });

            // `.edit-body { padding: 4px 18px 14px; gap: 10px }`
            egui::Frame::new()
                .inner_margin(Margin {
                    left: 18,
                    right: 18,
                    top: 4,
                    bottom: 14,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = theme::space::L;
                    edit_row(ui, "Project", &confirm.name, theme::TEXT);
                    edit_row(
                        ui,
                        "Size",
                        &format!("{} · {}", fmt_bytes(confirm.bytes), file_word(confirm.files)),
                        theme::TEXT,
                    );
                    edit_row(ui, "On Frame.io", &confirm.place(), theme::TEXT_2);
                    ui.add_space(theme::space::XS);
                    // `.space-warn-box`: the sentence the dialog exists for.
                    egui::Frame::new()
                        .fill(theme::SURFACE)
                        .stroke(theme::stroke_border_2())
                        .corner_radius(theme::R_S)
                        .inner_margin(Margin {
                            left: 12,
                            right: 12,
                            top: 10,
                            bottom: 10,
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(
                                Label::new(
                                    RichText::new(NOTHING_LOCAL)
                                        .font(theme::note())
                                        .color(theme::TEXT_2),
                                )
                                .wrap(),
                            );
                        });
                });

            // `.modal-foot { border-top: 1px solid var(--border); padding: 12px 14px }`,
            // with the buttons at the right: Cancel, then the destructive one.
            let foot = egui::Frame::new()
                .inner_margin(Margin {
                    left: 14,
                    right: 14,
                    top: 12,
                    bottom: 12,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = theme::space::M;
                        let delete = button(
                            ui,
                            Kind::DangerSolid,
                            Size::Regular,
                            None,
                            CONFIRM_DELETE,
                            CONFIRM_DELETE,
                            true,
                        );
                        let cancel = button(
                            ui,
                            Kind::Subtle,
                            Size::Regular,
                            None,
                            CONFIRM_CANCEL,
                            CONFIRM_CANCEL,
                            true,
                        );
                        // Cancel is focused the moment the dialog opens, as
                        // `openSpaceConfirm` does with `$('#sc-cancel').focus()`.
                        if ui.memory(|memory| memory.focused().is_none()) {
                            cancel.request_focus();
                        }
                        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if enter && !delete.has_focus() {
                            cancel_clicked = true;
                        }
                        cancel_clicked |= cancel.clicked();
                        delete_clicked |= delete.clicked();
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
    if delete_clicked {
        decision = Decision::Delete;
    }
    decision
}

/// `.edit-row`: an 80 px dim label, then the value.
fn edit_row(ui: &mut egui::Ui, label: &str, value: &str, colour: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::space::XL;
        let (rect, _) = ui.allocate_exact_size(vec2(80.0, theme::size::CONTROL_H), Sense::hover());
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            theme::detail(),
            theme::TEXT_3,
        );
        ui.add(Label::new(RichText::new(value).font(theme::body()).color(colour)));
    });
}


#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use egui_kittest::Harness;

    /// A project on one account, walked (so it has a file count) and with no
    /// error.
    fn project(name: &str, workspace: &str, bytes: i64, files: i64) -> SpaceRow {
        SpaceRow {
            account_id: "acct-1".into(),
            account_name: "Farbhaus".into(),
            workspace_name: workspace.into(),
            project_id: format!("proj-{}", name.to_lowercase().replace(' ', "-")),
            name: name.into(),
            bytes,
            files,
            walked: true,
            error: String::new(),
        }
    }

    /// A project Frame.io sized itself: no walk, so no file count.
    fn reported(name: &str, workspace: &str, bytes: i64) -> SpaceRow {
        SpaceRow {
            bytes,
            walked: false,
            files: 0,
            ..project(name, workspace, 0, 0)
        }
    }

    fn account(name: &str, limit: Option<i64>, used: Option<i64>) -> SpaceAccount {
        SpaceAccount {
            account_id: format!("acct-{}", name.to_lowercase().replace(' ', "-")),
            name: name.into(),
            limit,
            used,
        }
    }

    /// Three projects of clearly different sizes, none of which is the total
    /// (1.25 GB), so a size query names exactly one row.
    fn three_projects() -> Vec<SpaceRow> {
        vec![
            project("Rushes 2026", "Shows", 1_000_000_000, 312),
            project("Nightfall", "Shows", 250_000_000, 48),
            project("Old Trailers", "Marketing", 4_000_000, 2),
        ]
    }

    fn state_with(space: Vec<SpaceRow>) -> UiState {
        UiState {
            space,
            logged_in: true,
            ..UiState::default()
        }
    }

    type Seen = std::rc::Rc<std::cell::RefCell<Vec<Action>>>;

    /// The screen at the main column of the default 1180 px window, with every
    /// action it reports collected for the test to look at.
    fn screen(state: UiState) -> (Harness<'static, UiState>, Seen) {
        let seen: Seen = Default::default();
        let sink = seen.clone();
        let harness = Harness::builder()
            .with_size(vec2(956.0, 780.0))
            .build_state(
                move |ctx, state: &mut UiState| {
                    crate::app::apply_style_once(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if let Some(action) = show_with_actions(ui, state) {
                            sink.borrow_mut().push(action);
                        }
                    });
                },
                state,
            );
        (harness, seen)
    }

    // ---------- the three states of the screen ----------

    #[test]
    fn the_empty_state_says_nothing_counted_yet() {
        let (harness, seen) = screen(state_with(Vec::new()));

        harness.get_by_label(EMPTY_TITLE);
        harness.get_by_label(EMPTY_SUB);
        // The head's Refresh, and the one in the middle of the empty block.
        assert_eq!(harness.get_all_by_label("Refresh").count(), 2);
        // No chart, so no legend and no total.
        assert!(harness.query_by_label("Project").is_none());
        assert!(
            harness.query_by_label("on Frame.io").is_none(),
            "an empty chart must not be drawn"
        );
        assert!(seen.borrow().is_empty(), "actions seen: {:?}", seen.borrow());
    }

    #[test]
    fn the_empty_block_starts_the_first_walk() {
        let (mut harness, seen) = screen(state_with(Vec::new()));

        // The last "Refresh" is the empty block's own button.
        harness
            .get_all_by_label("Refresh")
            .last()
            .expect("two Refresh buttons")
            .click();
        harness.run();

        assert!(
            seen.borrow().contains(&Action::Refresh),
            "actions seen: {:?}",
            seen.borrow()
        );
    }

    #[test]
    fn a_signed_out_screen_offers_settings_instead_of_a_walk() {
        let mut state = state_with(Vec::new());
        state.logged_in = false;
        let (mut harness, seen) = screen(state);

        harness.get_by_label(EMPTY_SIGNED_OUT_TITLE);
        harness.get_by_label(EMPTY_SIGNED_OUT_SUB);
        assert!(harness.query_by_label(EMPTY_TITLE).is_none());

        harness.get_by_label("Open Settings").click();
        harness.run();
        assert_eq!(harness.state().view, View::Settings);
        assert!(seen.borrow().is_empty(), "no walk is started while signed out");
    }

    #[test]
    fn three_projects_draw_their_rows_and_their_sizes() {
        let (harness, _) = screen(state_with(three_projects()));

        // The head carries the total and the count.
        harness.get_by_label("1.25 GB · 3 projects");

        // The legend head.
        harness.get_by_label("Project");
        harness.get_by_label("Size");
        harness.get_by_label("Files");

        // The rows: name, place, size, file count.
        harness.get_by_label("Rushes 2026");
        harness.get_by_label("Nightfall");
        harness.get_by_label("Old Trailers");
        harness.get_all_by_label("Shows · Farbhaus").next().expect("a place line");
        harness.get_by_label("Marketing · Farbhaus");
        harness.get_by_label("1.00 GB");
        harness.get_by_label("250 MB");
        harness.get_by_label("4.00 MB");
        harness.get_by_label("312");
        harness.get_by_label("48");
        harness.get_by_label("2");

        // The hole: the total again, and the words under it.
        harness.get_by_label("on Frame.io");

        // One Delete per row, each naming its own project.
        harness.get_by_label("Delete Rushes 2026 on Frame.io");
        harness.get_by_label("Delete Nightfall on Frame.io");
        harness.get_by_label("Delete Old Trailers on Frame.io");
    }

    #[test]
    fn the_pie_is_drawn_as_a_fan_of_convex_quads() {
        let (mut harness, _) = screen(state_with(three_projects()));
        harness.run_steps(2);

        let paths: Vec<&egui::epaint::PathShape> = harness
            .output()
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Path(path) => Some(path),
                _ => None,
            })
            .collect();

        // The biggest slice is the first colour of the ramp, and every step of
        // it is one four-sided `convex_polygon`.
        let brand = paths
            .iter()
            .filter(|path| path.fill == space_color(0) && path.points.len() == 4)
            .count();
        assert!(brand >= 10, "the first slice drew {brand} quads, not a fan");
        // The other two slices are the ramp's next two colours.
        assert!(paths.iter().any(|p| p.fill == space_color(1)));
        assert!(paths.iter().any(|p| p.fill == space_color(2)));
        // A fourth colour would be a slice nobody asked for.
        assert!(!paths.iter().any(|p| p.fill == space_color(3)));
        // The ring is a ring: every quad has points at both radii.
        assert!(paths.iter().any(|p| p.points.iter().any(|q| {
            (q.distance(egui::pos2(0.0, 0.0)) - 0.0).abs() >= 0.0
        })));
    }

    #[test]
    fn the_walking_state_says_so_and_dims_refresh() {
        let mut state = state_with(three_projects());
        state.space_walking = true;
        let (mut harness, seen) = screen(state);
        harness.run_steps(2);

        // The status line says what is being walked, and what it does not do.
        harness.get_by_label("Adding up every project. Nothing is downloaded, nothing is deleted.");
        // The last walk's figures stay on screen while the new one runs.
        harness.get_by_label("Rushes 2026");

        // Refresh is disabled while a walk runs: a click starts nothing.
        let refresh = harness
            .get_all_by_label("Refresh")
            .next()
            .expect("the head's Refresh");
        assert!(
            refresh.accesskit_node().is_disabled(),
            "Refresh must not start a second walk"
        );
        refresh.click();
        harness.run_steps(2);
        assert!(seen.borrow().is_empty(), "actions seen: {:?}", seen.borrow());
    }

    #[test]
    fn a_first_walk_shows_the_walking_empty_block() {
        let mut state = state_with(Vec::new());
        state.space_walking = true;
        let (harness, _) = screen(state);

        // "Walking Frame.io…" is both the status line and the empty block's
        // title — the webview shows both at once.
        assert_eq!(
            harness.get_all_by_label(EMPTY_WALKING_TITLE).count(),
            2,
            "the status line and the empty block both say it"
        );
        harness.get_by_label(EMPTY_WALKING_SUB);
        assert!(
            harness.query_by_label(EMPTY_TITLE).is_none(),
            "a walk in progress is not the same as nothing to count"
        );
        // There is no button to press while the walk runs.
        assert_eq!(harness.get_all_by_label("Refresh").count(), 1);
    }

    #[test]
    fn an_unfinished_walk_says_could_not_finish_in_the_notice_colour() {
        let mut rows = three_projects();
        rows[2].error = "403 forbidden in fld-9f2c".into();
        let (harness, _) = screen(state_with(rows));

        harness.get_by_label("Old Trailers");
        harness.get_by_label("could not finish");
        // The figures it did count are still there; the flag says they are not
        // the whole story.
        harness.get_by_label("4.00 MB");
    }

    // ---------- the two things the screen asks for ----------

    #[test]
    fn refresh_is_reported_as_an_action() {
        let (mut harness, seen) = screen(state_with(three_projects()));

        harness
            .get_all_by_label("Refresh")
            .next()
            .expect("the head's Refresh")
            .click();
        harness.run();

        assert!(
            seen.borrow().contains(&Action::Refresh),
            "actions seen: {:?}",
            seen.borrow()
        );
    }

    /// `show` is what the shell draws, and on this line a view asks the app for
    /// things by name: the screen's two `Action`s have to become the two
    /// `Request`s the shell knows the calls for.
    fn app_screen(state: UiState) -> Harness<'static, UiState> {
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

    #[test]
    fn refresh_asks_the_shell_for_a_walk() {
        let mut harness = app_screen(state_with(three_projects()));

        harness
            .get_all_by_label("Refresh")
            .next()
            .expect("the head's Refresh")
            .click();
        harness.run();

        assert!(
            harness.state().requests.contains(&Request::RefreshSpace),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn the_confirmed_delete_asks_the_shell_for_the_delete() {
        let mut harness = app_screen(state_with(three_projects()));

        harness.get_by_label("Delete Nightfall on Frame.io").click();
        harness.run();
        harness.run();
        harness.get_by_label(CONFIRM_DELETE).click();
        harness.run();

        assert!(
            harness.state().requests.contains(&Request::DeleteProject {
                account_id: "acct-1".into(),
                project_id: "proj-nightfall".into(),
                name: "Nightfall".into(),
            }),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn delete_opens_a_confirmation_that_names_the_project_and_its_size() {
        let (mut harness, seen) = screen(state_with(three_projects()));

        harness.get_by_label("Delete Rushes 2026 on Frame.io").click();
        harness.run();
        harness.run();

        // The dialog: the project, its size, and where it lives.
        harness.get_by_label(CONFIRM_TITLE);
        // The project's name is in its row and in the dialog's Project row.
        assert_eq!(harness.get_all_by_label("Rushes 2026").count(), 2);
        harness.get_by_label("1.00 GB · 312 files");
        assert!(
            harness.get_all_by_label("Shows · Farbhaus").count() >= 2,
            "the dialog names the place as well"
        );
        // The sentence that says what a delete here touches.
        harness.get_by_label(NOTHING_LOCAL);
        harness.get_by_label(CONFIRM_DELETE);
        harness.get_by_label(CONFIRM_CANCEL);

        // Read-only until the user confirms: nothing was reported.
        assert!(seen.borrow().is_empty(), "actions seen: {:?}", seen.borrow());
    }

    #[test]
    fn cancel_is_the_default_and_reports_nothing() {
        let (mut harness, seen) = screen(state_with(three_projects()));
        harness.get_by_label("Delete Rushes 2026 on Frame.io").click();
        harness.run();
        harness.run();

        // Cancel holds the keyboard focus the moment the dialog opens.
        assert!(
            harness.get_by_label(CONFIRM_CANCEL).is_focused(),
            "Cancel must be the default button"
        );

        harness.get_by_label(CONFIRM_CANCEL).click();
        harness.run();
        harness.run();

        assert!(
            harness.query_by_label(CONFIRM_DELETE).is_none(),
            "the dialog closes on Cancel"
        );
        assert!(seen.borrow().is_empty(), "actions seen: {:?}", seen.borrow());
    }

    #[test]
    fn the_close_cross_also_cancels() {
        let (mut harness, seen) = screen(state_with(three_projects()));
        harness.get_by_label("Delete Nightfall on Frame.io").click();
        harness.run();
        harness.run();

        harness.get_by_label("Close").click();
        harness.run();
        harness.run();

        assert!(harness.query_by_label(CONFIRM_DELETE).is_none());
        assert!(seen.borrow().is_empty(), "actions seen: {:?}", seen.borrow());
    }

    #[test]
    fn confirming_reports_the_delete_with_its_account_and_project() {
        let (mut harness, seen) = screen(state_with(three_projects()));
        harness.get_by_label("Delete Nightfall on Frame.io").click();
        harness.run();
        harness.run();

        harness.get_by_label(CONFIRM_DELETE).click();
        harness.run();
        harness.run();

        assert_eq!(
            seen.borrow().as_slice(),
            &[Action::Delete {
                account_id: "acct-1".to_string(),
                project_id: "proj-nightfall".to_string(),
                name: "Nightfall".to_string(),
            }],
            "exactly one delete, for the project the dialog named"
        );
        assert!(
            harness.query_by_label(CONFIRM_DELETE).is_none(),
            "the dialog closes once it has answered"
        );
    }

    #[test]
    fn the_confirmation_names_the_project_whose_row_was_pressed() {
        let (mut harness, seen) = screen(state_with(three_projects()));
        harness.get_by_label("Delete Old Trailers on Frame.io").click();
        harness.run();
        harness.run();

        // Two rows say "Marketing · Farbhaus": one of them, and the dialog.
        assert!(harness.get_all_by_label("Marketing · Farbhaus").count() >= 2);
        harness.get_by_label("4.00 MB · 2 files");

        harness.get_by_label(CONFIRM_DELETE).click();
        harness.run();
        harness.run();

        assert_eq!(
            seen.borrow().as_slice(),
            &[Action::Delete {
                account_id: "acct-1".to_string(),
                project_id: "proj-old-trailers".to_string(),
                name: "Old Trailers".to_string(),
            }],
        );
    }

    // ---------- the maths and the words ----------

    #[test]
    fn the_ring_slices_follow_the_sizes_without_a_gap() {
        let rows = three_projects();
        let pieces = slices(&rows);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].start, 0.0, "a pie starts at 12 o'clock");
        assert_eq!(pieces[0].colour, space_color(0));
        assert_eq!(pieces[1].colour, space_color(1));
        assert_eq!(pieces[2].colour, space_color(2));
        // Each slice starts where the one before it ended.
        for pair in pieces.windows(2) {
            assert!((pair[1].start - (pair[0].start + pair[0].span)).abs() < 1e-4);
        }
        // The last closes the circle.
        let last = pieces.last().expect("three slices");
        assert!((last.start + last.span - 360.0).abs() < 1e-3);
        // 1 GB of 1.25 GB is 287.08 degrees of the ring.
        assert!((pieces[0].span - 287.08).abs() < 0.1, "{}", pieces[0].span);
    }

    #[test]
    fn a_slice_is_drawn_with_a_gap_at_each_end() {
        let close = |a: (f32, f32), b: (f32, f32)| {
            (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
        };
        // pad = min(0.9, span * 0.12)
        assert!(close(padded(100.0), (0.9, 98.2)), "{:?}", padded(100.0));
        assert!(close(padded(5.0), (0.6, 3.8)), "{:?}", padded(5.0));
        // A sliver thinner than the webview's 0.08° cut is not drawn at all.
        assert!(padded(0.1).1 <= SLICE_MIN_DEG, "{:?}", padded(0.1));
        assert!(padded(1.0).1 > SLICE_MIN_DEG);
        // A full turn loses exactly the two pads, no more.
        assert!(close(padded(360.0), (0.9, 358.2)), "{:?}", padded(360.0));
    }

    #[test]
    fn a_project_with_nothing_gets_no_slice_but_keeps_its_colour() {
        let rows = vec![
            project("Empty", "Shows", 0, 0),
            project("Full", "Shows", 100, 1),
        ];
        let pieces = slices(&rows);
        assert_eq!(pieces.len(), 1, "zero bytes is no share of the ring");
        assert_eq!(
            pieces[0].colour,
            space_color(1),
            "the second row keeps the second colour, so its swatch still matches"
        );
        assert_eq!(pieces[0].span, 360.0);

        // Nothing measured at all: no ring, just the track.
        assert!(slices(&[]).is_empty());
        assert!(slices(&[project("Zero", "Shows", 0, 0)]).is_empty());
    }

    #[test]
    fn the_colour_ramp_is_the_stylesheets() {
        // `--brand` itself for the first project...
        assert_eq!(space_color(0), theme::BRAND);
        // ... then the ramp app.js generates: 18 degrees of hue per neighbour,
        // alternating lightness every six. These are `spaceColor(i)`'s own hex
        // values, from the same arithmetic.
        assert_eq!(space_color(1), Color32::from_rgb(0x55, 0x7f, 0xfc));
        assert_eq!(space_color(2), Color32::from_rgb(0x55, 0xb1, 0xfc));
        assert_eq!(space_color(3), Color32::from_rgb(0x55, 0xe3, 0xfc));
        assert_eq!(space_color(6), Color32::from_rgb(0x31, 0x28, 0xe2));
        assert_eq!(space_color(7), Color32::from_rgb(0x28, 0x56, 0xe2));
        // Neighbours never repeat, and the ramp keeps going past twelve.
        for index in 0..40 {
            assert_ne!(space_color(index), space_color(index + 1), "at {index}");
        }
        // The first six are all distinct, which is what the legend relies on.
        for a in 0..6 {
            for b in (a + 1)..6 {
                assert_ne!(space_color(a), space_color(b));
            }
        }
    }

    #[test]
    fn the_totals_are_the_sum_of_the_rows() {
        let rows = three_projects();
        assert_eq!(total_bytes(&rows), 1_254_000_000);
        assert_eq!(total_bytes(&[]), 0);
        // A row the walk never measured adds nothing.
        assert_eq!(total_bytes(&[project("Negative", "Shows", -5, 0)]), 0);
    }

    #[test]
    fn the_words_read_like_the_webview() {
        assert_eq!(file_word(0), "0 files");
        assert_eq!(file_word(1), "1 file");
        assert_eq!(file_word(312), "312 files");
        assert_eq!(project_word(0), "0 projects");
        assert_eq!(project_word(1), "1 project");
        assert_eq!(project_word(3), "3 projects");
        assert_eq!(head_total(&three_projects()), "1.25 GB · 3 projects");

        let both = project("Rushes", "Shows", 1, 1);
        assert_eq!(where_line(&both), "Shows · Farbhaus");
        assert_eq!(
            where_line(&SpaceRow {
                workspace_name: "Shows".into(),
                ..SpaceRow::default()
            }),
            "Shows"
        );
        assert_eq!(
            where_line(&SpaceRow {
                account_name: "Farbhaus".into(),
                ..SpaceRow::default()
            }),
            "Farbhaus"
        );
        assert_eq!(where_line(&SpaceRow::default()), "Frame.io");
    }

    #[test]
    fn the_legend_columns_are_the_stylesheets() {
        // `.space-grid { ... 104px 56px 104px }` with a 12 px gap, at the default
        // window: the name takes what is left.
        let wide = columns(690.0, false);
        assert_eq!((wide.size, wide.files, wide.actions), (104.0, 56.0, 104.0));
        assert!((wide.name - (690.0 - 24.0 - 104.0 - 56.0 - 104.0 - 36.0)).abs() < 0.01);

        // `@media (max-width: 999px)` narrows the three fixed columns.
        let narrow = columns(690.0, true);
        assert_eq!((narrow.size, narrow.files, narrow.actions), (92.0, 48.0, 92.0));
        assert!(narrow.name > wide.name);

        // A legend squeezed into nothing clamps to zero rather than going negative.
        for width in [0.0, 50.0, 200.0, 690.0, 1400.0] {
            for narrow in [false, true] {
                assert!(columns(width, narrow).name >= 0.0);
            }
        }
    }

    #[test]
    fn a_long_project_name_is_elided_not_drawn_over_the_edge() {
        let long = format!("Rushes {}", "and-more-rushes/".repeat(24));
        let (harness, _) = screen(state_with(vec![project(&long, "Shows", 1_000_000, 7)]));

        // The whole name stays the label; only its drawing is cut.
        let row = harness.get_by_label(&long);
        assert!(
            row.rect().width() <= 956.0,
            "the row is {} wide",
            row.rect().width()
        );
        // The one project's size is also the chart's total, so it is drawn twice.
        assert_eq!(harness.get_all_by_label("1.00 MB").count(), 2);
    }

    #[test]
    fn every_row_shares_the_heads_columns() {
        // A row's name block grows with its words (a long place line, a warning
        // flag), and it must not drag the size and the Delete button after it:
        // every size ends on the head's Size edge, every count on its Files
        // edge, and every Delete on the same right edge.
        let (harness, _) = screen(state_with(three_projects()));
        let rect = |label: &str| {
            harness
                .get_by_label(label)
                .rect()
        };
        let size_edge = rect("Size").max.x;
        let files_edge = rect("Files").max.x;

        for (size, count, delete) in [
            ("1.00 GB", "312", "Delete Rushes 2026 on Frame.io"),
            ("250 MB", "48", "Delete Nightfall on Frame.io"),
            ("4.00 MB", "2", "Delete Old Trailers on Frame.io"),
        ] {
            assert_eq!(rect(size).max.x, size_edge, "{size} is not on the Size edge");
            assert_eq!(rect(count).max.x, files_edge, "{count} is not on the Files edge");
            let button = rect(delete);
            assert!(
                button.max.x <= 956.0,
                "{delete} runs past the card: {:?}",
                button
            );
        }

        let first = rect("Delete Rushes 2026 on Frame.io");
        for other in [
            "Delete Nightfall on Frame.io",
            "Delete Old Trailers on Frame.io",
        ] {
            assert_eq!(rect(other).max.x, first.max.x, "{other} is not in the actions column");
        }
    }

    #[test]
    fn a_long_place_line_is_cut_inside_its_column() {
        // `.sub { overflow: hidden; text-overflow: ellipsis }`: the place line and
        // its warning stay inside the name column instead of running over the
        // size.
        let mut row = project("Nightfall", &"A very long workspace name ".repeat(4), 1_000, 2);
        row.error = "403 forbidden".into();
        let (harness, _) = screen(state_with(vec![row]));

        let warn = harness.get_by_label("could not finish").rect();
        let size_edge = harness.get_by_label("Size").rect().max.x;
        assert!(
            warn.max.x <= size_edge,
            "the warning runs into the size column: {warn:?} past {size_edge}"
        );
    }

    #[test]
    fn the_dialog_s_delete_is_the_only_error_red_on_the_screen() {
        let (mut harness, _) = screen(state_with(three_projects()));
        harness.get_by_label("Delete Rushes 2026 on Frame.io").click();
        harness.run();
        harness.run();

        // `.btn.danger-solid { background: var(--error) }` — a filled red button,
        // which nothing else on this screen uses.
        let reds = harness
            .output()
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) => Some(rect),
                _ => None,
            })
            .filter(|rect| rect.fill == theme::ERROR && rect.corner_radius == theme::R_S)
            .count();
        assert_eq!(reds, 1, "one red button, the one that deletes");
    }

    #[test]
    fn the_chart_has_a_name_for_a_screen_reader() {
        let (harness, _) = screen(state_with(three_projects()));
        harness.get_by_label(CHART_LABEL);
    }

    // ---------- how much is left ----------

    /// A screen with rows and accounts: the headline and the chart together.
    fn screen_with(space: Vec<SpaceRow>, accounts: Vec<SpaceAccount>) -> Harness<'static, UiState> {
        let state = UiState {
            space,
            space_accounts: accounts,
            logged_in: true,
            ..UiState::default()
        };
        screen(state).0
    }

    /// The three cases in Frame.io's own terms, as the sentence the headline is
    /// built from. Bytes only: no money, no "seats", no percentage.
    #[test]
    fn the_headline_words_a_limit_a_usage_and_what_is_left() {
        // 1.24 TB of 2 TB, which leaves 760 GB.
        assert_eq!(
            quota_sentence(&account("Farbhaus", Some(2_000_000_000_000), Some(1_240_000_000_000))),
            "1.24 TB of 2.00 TB used · 760 GB left"
        );
        // `storage_limit: null` — an account with no limit at all.
        assert_eq!(
            quota_sentence(&account("Archive", None, Some(640_000_000_000))),
            "640 GB used · no limit"
        );
        // No usage reported: the limit is named, the figure it has no number for
        // is not invented.
        assert_eq!(
            quota_sentence(&account("Quiet", Some(500_000_000_000), None)),
            "500 GB limit · usage not reported"
        );
        // Neither: still a line, and it says nothing it does not know.
        assert_eq!(
            quota_sentence(&account("Bare", None, None)),
            "no limit · usage not reported"
        );
    }

    /// A soft limit can be exceeded: what is left is never a negative size.
    #[test]
    fn an_account_past_its_limit_has_nothing_left() {
        assert_eq!(
            quota_sentence(&account("Over", Some(1_000), Some(4_000))),
            "4.00 KB of 1.00 KB used · 0 B left"
        );
    }

    #[test]
    fn the_headline_names_the_account_it_counts() {
        assert_eq!(
            quota_line(&account("Farbhaus", Some(2_000_000_000_000), Some(1_240_000_000_000))),
            "Farbhaus · 1.24 TB of 2.00 TB used · 760 GB left"
        );
        // An account Frame.io does not name is still counted, without a stray "·".
        assert_eq!(
            quota_line(&account("", Some(1_000_000_000), Some(500_000_000))),
            "500 MB of 1.00 GB used · 500 MB left"
        );
    }

    /// The screen draws the headline for a limit, for no limit and for no usage —
    /// all three at once, one line per account, above the chart.
    #[test]
    fn the_screen_draws_one_headline_line_per_account() {
        let harness = screen_with(
            three_projects(),
            vec![
                account("Farbhaus", Some(2_000_000_000_000), Some(1_240_000_000_000)),
                account("Farbhaus Archive", None, Some(640_000_000_000)),
                account("Quiet Account", Some(500_000_000_000), None),
            ],
        );

        harness.get_by_label("Farbhaus · 1.24 TB of 2.00 TB used · 760 GB left");
        harness.get_by_label("Farbhaus Archive · 640 GB used · no limit");
        harness.get_by_label("Quiet Account · 500 GB limit · usage not reported");

        // The headline sits above the chart, not inside it.
        let headline = harness
            .get_by_label("Farbhaus Archive · 640 GB used · no limit")
            .rect();
        let head = harness.get_by_label("Project").rect();
        assert!(
            headline.max.y <= head.min.y,
            "the headline is below the legend head: {headline:?} past {head:?}"
        );
    }

    /// No accounts means no headline: an app that has not refreshed yet draws no
    /// line of zeroes.
    #[test]
    fn a_screen_with_no_accounts_draws_no_headline() {
        let harness = screen_with(three_projects(), Vec::new());
        assert!(
            harness.query_by_label("no limit · usage not reported").is_none(),
            "an empty account list must draw nothing"
        );
        // The chart is still there.
        harness.get_by_label("on Frame.io");
    }

    /// The headline is not the body's: an account's usage shows even when no
    /// project has been counted, because "how much is left" is the account's own
    /// figure.
    #[test]
    fn the_headline_shows_before_any_project_is_counted() {
        let harness = screen_with(
            Vec::new(),
            vec![account("Farbhaus", Some(2_000_000_000_000), Some(1_240_000_000_000))],
        );
        harness.get_by_label("Farbhaus · 1.24 TB of 2.00 TB used · 760 GB left");
        harness.get_by_label(EMPTY_TITLE);
    }

    /// A row Frame.io sized says so, and shows a dash where a walked row shows a
    /// file count: a 0 would claim the project holds no files.
    #[test]
    fn a_row_frame_io_sized_says_so_and_counts_no_files() {
        let harness = screen_with(
            vec![reported("Nightfall", "Shows", 12_400_000_000)],
            Vec::new(),
        );

        harness.get_by_label("from Frame.io");
        harness.get_by_label(NOT_COUNTED);
        assert!(
            harness.query_by_label("0").is_none(),
            "an unreported file count must not read as zero files"
        );
    }

    /// A walked row says it was walked and prints the count it made.
    #[test]
    fn a_walked_row_says_walked_and_prints_its_file_count() {
        let harness = screen_with(vec![project("Nightfall", "Shows", 12_400_000_000, 1_284)], Vec::new());

        harness.get_by_label("walked");
        harness.get_by_label("1284");
        assert!(harness.query_by_label("from Frame.io").is_none());
    }
}





