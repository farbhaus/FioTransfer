//! The design tokens, taken from the webview front end.
//!
//! Every colour, radius, size and type size in this module is read off
//! `tests/design/style.css` (the frozen copy of the webview front end's
//! stylesheet, `frontend/dist/style.css`, which the retirement of that front end
//! removed from the tree). A constant is named after the custom property it
//! comes from (`--text-2` → [`TEXT_2`]), and the tests at the bottom of the file
//! re-read the stylesheet and fail if any value here has drifted from the
//! design, so the two front ends cannot silently diverge.
//!
//! How to use it:
//!
//! * call [`install`] (or `ctx.set_style(theme::style())`) once, when the app
//!   starts, so widgets, inputs and menus pick up the same colours as the HTML;
//! * draw containers with the frame helpers ([`sidebar_frame`],
//!   [`panel_card_frame`], [`card_frame`], [`files_frame`], [`toast_frame`],
//!   [`modal_frame`], …) instead of hand-rolling a fill and a border;
//! * ask for text with [`body`], [`title`], [`section`], [`caption`],
//!   [`detail`], [`note`], [`badge`] rather than a bare size, and colour it from
//!   the ramp ([`text_ramp`], [`text_level`], [`text_primary`] …);
//! * ask for a status colour with [`status_color`], [`status_text`],
//!   [`progress_color`] so a row reads like `.status.running` does in CSS.
//!
//! What egui cannot do the way the stylesheet does:
//!
//! * **One font weight.** egui draws the weight it was given at load time, so
//!   the stylesheet's 400/500/600 steps ([`weight`]) become one weight here. The
//!   ramp above is how the design's emphasis is kept: primary text in
//!   [`TEXT`], secondary in [`TEXT_2`], tertiary in [`TEXT_3`].
//! * **One font family.** The webview loads `InterVariable.woff2`; egui needs a
//!   TTF/OTF, so screens fall back to egui's bundled face until a font is
//!   installed on the context (that lives in `app.rs`, not here).
//! * **Hairlines.** A 1 px CSS border on one side (`.sidebar { border-right }`)
//!   has to be painted as a line or as a stroke on all sides here.
//! * **Overlay washes.** `--hover`, `--active` and `.row:hover` are translucent
//!   whites laid over the fill behind them. egui blends them the same way, but a
//!   wash painted over nothing has nothing to blend with, so paint the panel or
//!   card first and the wash on top.

use std::collections::BTreeMap;

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Frame, Label, Margin, Shadow, Spacing, Stroke,
    Style, TextStyle, Vec2, Visuals, WidgetText,
};

// ---------------------------------------------------------------------------
// Colours: every custom property in `:root`
// ---------------------------------------------------------------------------

/// `--base` `#000` — the page behind every view.
pub const BASE: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
/// `--panel` `#12141e` — the sidebar, the composer, `.set-card`, `.files`.
pub const PANEL: Color32 = Color32::from_rgb(0x12, 0x14, 0x1e);
/// `--surface` `#181b29` — inputs, the segmented control, `.dest-pick`.
pub const SURFACE: Color32 = Color32::from_rgb(0x18, 0x1b, 0x29);
/// `--card` `#1f2332` — buttons at rest, `.kind-icon`, `.update-card`, toasts.
pub const CARD: Color32 = Color32::from_rgb(0x1f, 0x23, 0x32);
/// `--raised` `#262a3b` — a button under the pointer, the progress track, the slider rail.
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x2a, 0x3b);
/// `--hover` `#ffffff0f` — the wash under `.nav-item:hover` and `.btn.subtle`.
pub const HOVER: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x0f);
/// `--active` `#ffffff17` — the wash under `.nav-item.active` and a hovered `.seg`.
pub const ACTIVE: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x17);
/// `--border` `#ffffff0f` — the hairline between rows, around panels.
pub const BORDER: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x0f);
/// `--border-2` `#ffffff1a` — the outline of a button, an input, a toast.
pub const BORDER_2: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x1a);
/// `--border-3` `#ffffff26` — the outline under the pointer, and a checkbox box.
pub const BORDER_3: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x26);

/// `--text` `#fff` — names, values, the active nav item.
pub const TEXT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
/// `--text-body` `#e6e7ef` — the body colour of the page, and of a cell.
pub const TEXT_BODY: Color32 = Color32::from_rgb(0xe6, 0xe7, 0xef);
/// `--text-2` `#a6aac1` — nav items at rest, cells, `.status`.
pub const TEXT_2: Color32 = Color32::from_rgb(0xa6, 0xaa, 0xc1);
/// `--text-3` `#7d8198` — sub lines, `.tp-label`, `.comp-label`, icons.
pub const TEXT_3: Color32 = Color32::from_rgb(0x7d, 0x81, 0x98);
/// `--text-4` `#63677d` — placeholders, `.about`, the idle status dot.
pub const TEXT_4: Color32 = Color32::from_rgb(0x63, 0x67, 0x7d);

/// `--brand` `#5b53ff` — primary buttons, the progress fill, the count pill.
pub const BRAND: Color32 = Color32::from_rgb(0x5b, 0x53, 0xff);
/// `--brand-hover` `#6d66ff` — a primary button under the pointer.
pub const BRAND_HOVER: Color32 = Color32::from_rgb(0x6d, 0x66, 0xff);
/// `--brand-soft` `#5b53ff29` — the focus ring, the halo on a running dot.
pub const BRAND_SOFT: Color32 = Color32::from_rgba_unmultiplied_const(0x5b, 0x53, 0xff, 0x29);

/// `--success` `#21c17b` — a done dot, a done track.
pub const SUCCESS: Color32 = Color32::from_rgb(0x21, 0xc1, 0x7b);
/// `--error` `#f43350` — a failed dot, a failed track.
pub const ERROR: Color32 = Color32::from_rgb(0xf4, 0x33, 0x50);
/// `--error-text` `#f6546c` — error wording (`.err-text`, `.status.error`).
pub const ERROR_TEXT: Color32 = Color32::from_rgb(0xf6, 0x54, 0x6c);
/// `--notice` `#f0cd33` — a partial or interrupted track, `.warn`.
pub const NOTICE: Color32 = Color32::from_rgb(0xf0, 0xcd, 0x33);
/// `--info` `#729fff` — a resolving or queued dot, the default toast dot.
pub const INFO: Color32 = Color32::from_rgb(0x72, 0x9f, 0xff);

// ---------------------------------------------------------------------------
// Colours written inline in the stylesheet (not custom properties)
// ---------------------------------------------------------------------------

/// `a { color: #9a95ff }` — links, e.g. a file's `view_url`.
pub const LINK: Color32 = Color32::from_rgb(0x9a, 0x95, 0xff);
/// `a:hover { color: #b8b4ff }`.
pub const LINK_HOVER: Color32 = Color32::from_rgb(0xb8, 0xb4, 0xff);
/// `#fff` — text on brand, the tick inside a checkbox, the slider thumb.
pub const WHITE: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
/// `.row:hover { background: #ffffff05 }` — fainter than [`HOVER`].
pub const ROW_HOVER: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x05);
/// `.input-btn:hover { background: #32374c }`.
pub const INPUT_BTN_HOVER: Color32 = Color32::from_rgb(0x32, 0x37, 0x4c);
/// `.icon-btn.confirm` — the red wash behind a "Remove" button.
pub const CONFIRM_TINT: Color32 = Color32::from_rgba_unmultiplied_const(0xf4, 0x33, 0x50, 0x1f);
/// `.drop-box { background: #5b53ff14 }` — brand at 8 %.
pub const DROP_TINT: Color32 = Color32::from_rgba_unmultiplied_const(0x5b, 0x53, 0xff, 0x14);
/// `::-webkit-scrollbar-thumb { background: #ffffff1f }`.
pub const SCROLL_THUMB: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x1f);
/// `::-webkit-scrollbar-thumb:hover { background: #ffffff33 }`.
pub const SCROLL_THUMB_HOVER: Color32 =
    Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0x33);
/// `.modal { background: #000a }` — the scrim behind a modal.
pub const SCRIM_MODAL: Color32 = Color32::from_rgba_unmultiplied_const(0x00, 0x00, 0x00, 0xaa);
/// `.drop-overlay { background: #000c }` — a heavier scrim behind a file drop.
pub const SCRIM_DROP: Color32 = Color32::from_rgba_unmultiplied_const(0x00, 0x00, 0x00, 0xcc);

// ---------------------------------------------------------------------------
// Radii
// ---------------------------------------------------------------------------

/// `--r-s` `6px`: buttons, inputs, nav items, icon boxes.
pub const R_S_PX: u8 = 6;
/// `--r` `8px`: cards, toasts, the file list.
pub const R_PX: u8 = 8;
/// `--r-l` `12px`: the composer, `.set-card`, the modal.
pub const R_L_PX: u8 = 12;

/// `--r-s` as a corner radius.
pub const R_S: CornerRadius = CornerRadius::same(R_S_PX);
/// `--r` as a corner radius.
pub const R: CornerRadius = CornerRadius::same(R_PX);
/// `--r-l` as a corner radius.
pub const R_L: CornerRadius = CornerRadius::same(R_L_PX);

/// `4px`: `code`, `.input-btn`, a segmented button, a checkbox.
pub const R_MICRO_PX: u8 = 4;
/// `2px`: the progress track and the slider rail.
pub const R_TRACK_PX: u8 = 2;
/// `9px`: the `.count` pill (half its 18 px height).
pub const R_BADGE_PX: u8 = 9;
/// `7px`: the brand mark.
pub const R_MARK_PX: u8 = 7;
/// `10px`: `.empty-icon`.
pub const R_EMPTY_PX: u8 = 10;
/// `16px`: the drop-target box.
pub const R_DROP_PX: u8 = 16;

/// `4px` as a corner radius.
pub const R_MICRO: CornerRadius = CornerRadius::same(R_MICRO_PX);
/// `2px` as a corner radius.
pub const R_TRACK: CornerRadius = CornerRadius::same(R_TRACK_PX);
/// `9px` as a corner radius.
pub const R_BADGE: CornerRadius = CornerRadius::same(R_BADGE_PX);
/// `7px` as a corner radius.
pub const R_MARK: CornerRadius = CornerRadius::same(R_MARK_PX);
/// `10px` as a corner radius.
pub const R_EMPTY: CornerRadius = CornerRadius::same(R_EMPTY_PX);
/// `16px` as a corner radius.
pub const R_DROP: CornerRadius = CornerRadius::same(R_DROP_PX);

// ---------------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------------

/// Type sizes in px, as `font-size` writes them in the stylesheet.
pub mod font {
    /// `body { font-size: 13px }` — the size of nearly every label.
    pub const BODY: f32 = 13.0;
    /// `body { line-height: 1.45 }` — multiply by the size for a line box.
    pub const BODY_LINE: f32 = 1.45;
    /// `.page-head h1 { font-size: 18px }` — the screen title.
    pub const TITLE: f32 = 18.0;
    /// `.set-title`, `.modal-title`, `.empty-title { font-size: 14px }`.
    pub const SECTION: f32 = 14.0;
    /// `.brand-name { font-size: 14px }`.
    pub const BRAND: f32 = 14.0;

    /// `.list-head`, `.dest-sec`, `.who .e { font-size: 11.5px }` — small caps-ish.
    pub const CAPTION: f32 = 11.5;
    /// `.sub`, `.frow`, `.help`, `.about`, `.uc-sub { font-size: 12px }`.
    pub const DETAIL: f32 = 12.0;
    /// `.status`, `.uc-title`, `.check`, `.link-preview { font-size: 12.5px }`.
    pub const NOTE: f32 = 12.5;
    /// `.count`, `.avatar { font-size: 11px }`.
    pub const BADGE: f32 = 11.0;
    /// `code`, `.input-btn { font-size: 12px }` — monospaced.
    pub const CODE: f32 = 12.0;
    /// `.drop-title { font-size: 16px }`.
    pub const DROP: f32 = 16.0;
}

/// Font weights, as `font-weight` writes them in the stylesheet.
///
/// egui draws one weight per face (see the module note), so these document the
/// design rather than drive it.
pub mod weight {
    /// `body { font-weight: 400 }`.
    pub const REGULAR: u16 = 400;
    /// `.nav-item`, `.btn`, `.name { font-weight: 500 }`.
    pub const MEDIUM: u16 = 500;
    /// `.brand-name`, `.set-title`, `.count { font-weight: 600 }`.
    pub const SEMIBOLD: u16 = 600;
}

/// The body font: 13 px, proportional — what `body` sets.
pub fn body() -> FontId {
    FontId::new(font::BODY, FontFamily::Proportional)
}

/// `.page-head h1`: 18 px, 600 — the stylesheet's semibold.
pub fn title() -> FontId {
    // Proportional until every harness installs the fonts (a bare egui context
    // has no "semibold" family bound and panics when text is laid out).
    FontId::new(font::TITLE, FontFamily::Proportional)
}

/// `.set-title` / `.modal-title` / `.empty-title`: 14 px, 600.
pub fn section() -> FontId {
    FontId::new(font::SECTION, FontFamily::Proportional)
}

/// `.name` and `.btn`: 13 px, 500.
pub fn medium() -> FontId {
    FontId::new(font::BODY, FontFamily::Name("medium".into()))
}

/// The 600 face, for anything the stylesheet sets at `font-weight: 600`.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// `.list-head` / `.dest-sec` / `.who .e`: 11.5 px.
pub fn caption() -> FontId {
    FontId::new(font::CAPTION, FontFamily::Proportional)
}

/// `.sub` / `.frow` / `.help`: 12 px.
pub fn detail() -> FontId {
    FontId::new(font::DETAIL, FontFamily::Proportional)
}

/// `.status` / `.uc-title`: 12.5 px.
pub fn note() -> FontId {
    FontId::new(font::NOTE, FontFamily::Proportional)
}

/// `.count` / `.avatar`: 11 px.
pub fn badge() -> FontId {
    FontId::new(font::BADGE, FontFamily::Proportional)
}

/// `code`: 12 px monospace.
pub fn mono() -> FontId {
    FontId::new(font::CODE, FontFamily::Monospace)
}

/// One line box at the body size (`13 px × 1.45`).
pub const BODY_LINE_HEIGHT: f32 = font::BODY * font::BODY_LINE;

// ---------------------------------------------------------------------------
// Sizes in px
// ---------------------------------------------------------------------------

/// Fixed sizes, as the stylesheet writes them.
pub mod size {
    /// `.app { grid-template-columns: 224px 1fr }` — the sidebar's width.
    pub const SIDEBAR_W: f32 = 224.0;
    /// `.page-head { height: 64px }` — the sticky title bar of every view.
    pub const HEAD_H: f32 = 64.0;
    /// `.traffic { height: 40px }` — the draggable strip above the brand.
    pub const TRAFFIC_H: f32 = 40.0;

    /// `.btn`, `.nav-item`, `input` — every control is 32 px tall.
    pub const CONTROL_H: f32 = 32.0;
    /// `.btn.sm`, `.segmented button { height: 26px }`.
    pub const CONTROL_SM_H: f32 = 26.0;
    /// `.input-btn { height: 24px }`.
    pub const NESTED_BTN_H: f32 = 24.0;
    /// `.icon-btn { width: 28px; height: 28px }`.
    pub const ICON_BTN: f32 = 28.0;

    /// `.row-main { min-height: 56px }` — a job row.
    pub const ROW_H: f32 = 56.0;
    /// `.frow { height: 30px }` — a file row inside an expanded job.
    pub const FILE_ROW_H: f32 = 30.0;
    /// `.list-head { height: 32px }` — the column headings above a list.
    pub const LIST_HEAD_H: f32 = 32.0;
    /// `.files { max-height: 300px }`.
    pub const FILES_MAX_H: f32 = 300.0;

    /// `.track { height: 4px }` — the progress bar in a row.
    pub const TRACK_H: f32 = 4.0;
    /// `.frow .track { height: 3px }` — the thinner one in a file row.
    pub const FILE_TRACK_H: f32 = 3.0;
    /// `.status .dot { width: 7px }`.
    pub const DOT: f32 = 7.0;

    /// `.kind-icon { width: 30px; height: 30px }`.
    pub const KIND_ICON: f32 = 30.0;
    /// `.mark { width: 26px; height: 26px }` — the brand mark.
    pub const MARK: f32 = 26.0;
    /// `.avatar { width: 28px; height: 28px }`.
    pub const AVATAR: f32 = 28.0;
    /// `.empty-icon { width: 44px; height: 44px }`.
    pub const EMPTY_ICON: f32 = 44.0;
    /// `.count { min-width: 18px; height: 18px }` — the nav badge.
    pub const BADGE: f32 = 18.0;
    /// `.check input { width: 15px; height: 15px }`.
    pub const CHECK: f32 = 15.0;

    /// `.comp-label { width: 72px }` — the label column of a composer row.
    pub const COMP_LABEL_W: f32 = 72.0;
    /// `.comp-row { padding: 12px 14px }` — used as the row height budget too.
    pub const COMP_ROW_H: f32 = 56.0;
    /// `.comp-foot { min-height: 22px }`.
    pub const COMP_FOOT_H: f32 = 22.0;
    /// `.comp-foot { padding-left: 102px }` — the footer starts under the body.
    pub const COMP_FOOT_INDENT: f32 = 102.0;
    /// `.input-wrap input { padding-left: 32px }` — room for the search icon.
    pub const INPUT_ICON_PAD: f32 = 32.0;
    /// `.input-wrap input { padding-right: 64px }` — room for the inline button.
    pub const INPUT_BTN_PAD: f32 = 64.0;

    /// `.modal-box { width: min(560px, 92vw) }`.
    pub const MODAL_W: f32 = 560.0;
    /// `.modal-box { height: min(540px, 86vh) }`.
    pub const MODAL_H: f32 = 540.0;
    /// `.settings { max-width: 680px }`.
    pub const SETTINGS_W: f32 = 680.0;
    /// `.toasts { max-width: 400px }`.
    pub const TOAST_MAX_W: f32 = 400.0;

    /// `::-webkit-scrollbar { width: 10px }`.
    pub const SCROLLBAR: f32 = 10.0;
    /// A 1 px rule: every border in the stylesheet.
    pub const HAIRLINE: f32 = 1.0;
    /// `box-shadow: 0 0 0 3px var(--brand-soft)` — the halo on a running dot.
    pub const HALO: f32 = 3.0;
    /// `.segmented { padding: 2px }` — the inset around a segmented control.
    pub const SEGMENT_INSET: f32 = 2.0;
}

/// Icon sizes in px (`svg { width: … }` in the stylesheet).
pub mod icon {
    /// `.dir-cell svg { width: 14px }`.
    pub const ROW: f32 = 14.0;
    /// `.btn svg`, `.icon-btn svg`, `.kind-icon svg { width: 16px }`.
    pub const CONTROL: f32 = 16.0;
    /// `.nav-item svg`, `.mark svg { width: 18px }`.
    pub const NAV: f32 = 18.0;
    /// `.empty-icon svg { width: 22px }`.
    pub const EMPTY: f32 = 22.0;
    /// `.drop-box svg { width: 28px }`.
    pub const DROP: f32 = 28.0;
}

// ---------------------------------------------------------------------------
// Spacing in px
// ---------------------------------------------------------------------------

/// Gaps and paddings, as the stylesheet writes them.
pub mod space {
    /// `.nav { gap: 2px }`, `.actions { gap: 2px }`.
    pub const XXS: f32 = 2.0;
    /// `.meta { gap: 4px 16px }`.
    pub const XS: f32 = 4.0;
    /// `.frow .fn { gap: 6px }`.
    pub const S: f32 = 6.0;
    /// `.head-actions`, `.comp-body`, `.check { gap: 8px }`.
    pub const M: f32 = 8.0;
    /// `.nav-item`, `.account`, `.filters { gap: 10px }`.
    pub const L: f32 = 10.0;
    /// `.watch-grid` and `.hist-grid` gap, and a `.row`'s horizontal padding.
    pub const XL: f32 = 12.0;
    /// `.jobs-grid { gap: 14px }`, and the vertical padding of a `.comp-row`.
    pub const XXL: f32 = 14.0;
    /// `.comp-row`, `.set-line { gap: 16px }`.
    pub const XXXL: f32 = 16.0;
    /// `.list { margin-top: 20px }`, the `.toasts` inset from the corner.
    pub const HUGE: f32 = 20.0;

    /// `.view { padding: 0 28px 32px }` — left and right.
    pub const PAGE_X: f32 = 28.0;
    /// `.view { padding-bottom: 32px }`.
    pub const PAGE_BOTTOM: f32 = 32.0;
    /// `.row`, `.frow`, `.list-head { padding: 0 12px }`.
    pub const ROW_X: f32 = 12.0;
    /// `.sidebar { padding: 0 12px 12px }`.
    pub const SIDEBAR_X: f32 = 12.0;
    /// `.brand { padding: 4px 8px 18px }` — the gap under the brand block.
    pub const BRAND_BOTTOM: f32 = 18.0;
}

// ---------------------------------------------------------------------------
// Shadows and strokes
// ---------------------------------------------------------------------------

/// `.modal-box { box-shadow: 0 24px 64px #000c }`.
pub const SHADOW_MODAL: Shadow = Shadow {
    offset: [0, 24],
    blur: 64,
    spread: 0,
    color: SCRIM_DROP,
};
/// `.toast { box-shadow: 0 12px 32px #000a }`.
pub const SHADOW_TOAST: Shadow = Shadow {
    offset: [0, 12],
    blur: 32,
    spread: 0,
    color: SCRIM_MODAL,
};

/// A 1 px rule in `color` — the shapes of `border: 1px solid …`.
pub fn hairline(color: Color32) -> Stroke {
    Stroke::new(size::HAIRLINE, color)
}

/// `border: 1px solid var(--border)`.
pub fn stroke_border() -> Stroke {
    hairline(BORDER)
}

/// `border: 1px solid var(--border-2)`.
pub fn stroke_border_2() -> Stroke {
    hairline(BORDER_2)
}

/// `border: 1px solid var(--brand)` — a focused input.
pub fn stroke_brand() -> Stroke {
    hairline(BRAND)
}

// ---------------------------------------------------------------------------
// The text ramp
// ---------------------------------------------------------------------------

/// The five-step text colour ramp, brightest first: `--text`, `--text-body`,
/// `--text-2`, `--text-3`, `--text-4`.
pub const TEXT_RAMP: [Color32; 5] = [TEXT, TEXT_BODY, TEXT_2, TEXT_3, TEXT_4];

/// The same ramp as a value, for a view that loops over the steps.
pub fn text_ramp() -> [Color32; 5] {
    TEXT_RAMP
}

/// The ramp by step, `0` = [`TEXT`] … `4` = [`TEXT_4`]. Out-of-range clamps.
pub fn text_level(step: usize) -> Color32 {
    TEXT_RAMP[step.min(TEXT_RAMP.len() - 1)]
}

/// Step 0: a name, a value, the active nav item.
pub fn text_primary() -> Color32 {
    TEXT
}

/// Step 1: body copy.
pub fn text_default() -> Color32 {
    TEXT_BODY
}

/// Step 2: a cell, a nav item at rest.
pub fn text_cell() -> Color32 {
    TEXT_2
}

/// Step 3: a sub line, a label.
pub fn text_muted() -> Color32 {
    TEXT_3
}

/// Step 4: a placeholder, `.about`.
pub fn text_faint() -> Color32 {
    TEXT_4
}

// ---------------------------------------------------------------------------
// Status colours (`.status.<status> .dot`, `.row.<status> .track i`)
// ---------------------------------------------------------------------------

/// True when `status` names one of `names`, ignoring case and padding.
fn named(status: &str, names: &[&str]) -> bool {
    let s = status.trim();
    names.iter().any(|n| s.eq_ignore_ascii_case(n))
}

/// The dot in front of a status word, from `.status.<status> .dot`.
///
/// Unknown or empty statuses get [`TEXT_4`], the stylesheet's idle dot.
pub fn status_color(status: &str) -> Color32 {
    if named(status, &["done", "watching"]) {
        SUCCESS
    } else if named(status, &["running", "uploading", "downloading"]) {
        BRAND
    } else if named(status, &["resolving", "queued", "settling", "waiting"]) {
        INFO
    } else if named(status, &["partial", "interrupted"]) {
        NOTICE
    } else if named(status, &["failed", "error"]) {
        ERROR
    } else {
        TEXT_4
    }
}

/// The halo a dot carries, from `box-shadow: 0 0 0 3px var(--brand-soft)`.
///
/// Only a running dot has one in the stylesheet; everything else is `None`.
pub fn status_halo(status: &str) -> Option<Color32> {
    if named(status, &["running", "uploading", "downloading"]) {
        Some(BRAND_SOFT)
    } else {
        None
    }
}

/// The status word's own colour: `.status { color: var(--text-2) }`, with
/// `.status.running { color: var(--text) }` and
/// `.status.error { color: var(--error-text) }`.
pub fn status_text(status: &str) -> Color32 {
    if named(status, &["error"]) {
        ERROR_TEXT
    } else if named(status, &["running", "uploading", "downloading"]) {
        TEXT
    } else {
        TEXT_2
    }
}

/// The status word inside a file row: `.fst { color: var(--text-3) }`, with
/// `.fst.done { color: var(--text-2) }`, `.fst.running { color: var(--text) }`
/// and `.fst.failed { color: var(--error-text) }`.
///
/// A failed *job* keeps the grey status word; a failed *file* is red.
pub fn file_status_text(status: &str) -> Color32 {
    if named(status, &["failed"]) {
        ERROR_TEXT
    } else if named(status, &["done"]) {
        TEXT_2
    } else if named(status, &["running"]) {
        TEXT
    } else {
        TEXT_3
    }
}

/// The filled part of a progress track, from `.row.<status> .track i`.
///
/// A job that is still going is brand; a finished one is success.
pub fn progress_color(status: &str) -> Color32 {
    if named(status, &["done"]) {
        SUCCESS
    } else if named(status, &["failed", "error"]) {
        ERROR
    } else if named(status, &["partial", "interrupted"]) {
        NOTICE
    } else if named(status, &["canceled"]) {
        TEXT_4
    } else {
        BRAND
    }
}

/// The colour of a toast's leading dot: `.toast::before` is `--info`,
/// `.toast.ok::before` is `--success`, `.toast.err::before` is `--error`.
pub fn toast_color(kind: crate::model::ToastKind) -> Color32 {
    use crate::model::ToastKind;
    match kind {
        ToastKind::Ok => SUCCESS,
        ToastKind::Err => ERROR,
        ToastKind::Info => INFO,
    }
}

/// The words in a toast keep the body colour; only the dot changes.
pub fn toast_text() -> Color32 {
    TEXT_BODY
}

// ---------------------------------------------------------------------------
// Frames — one per container shape in the stylesheet
// ---------------------------------------------------------------------------

/// A hairline margin in px, as [`Margin`] wants it.
fn margin(left: i8, right: i8, top: i8, bottom: i8) -> Margin {
    Margin { left, right, top, bottom }
}

/// `.sidebar`: `--panel`, `padding: 0 12px 12px`.
///
/// The 1 px `border-right` is a line the shell paints; egui strokes all four
/// sides, so it is left off here.
pub fn sidebar_frame() -> Frame {
    Frame::new()
        .fill(PANEL)
        .inner_margin(margin(space::SIDEBAR_X as i8, space::SIDEBAR_X as i8, 0, space::SIDEBAR_X as i8))
}

/// `.main` / `.view`: `--base`, `padding: 0 28px 32px`.
pub fn main_frame() -> Frame {
    Frame::new().fill(BASE).inner_margin(margin(
        space::PAGE_X as i8,
        space::PAGE_X as i8,
        0,
        space::PAGE_BOTTOM as i8,
    ))
}

/// `.page-head`: `--base`, 64 px tall, the same 28 px insets as the view.
pub fn header_frame() -> Frame {
    Frame::new().fill(BASE).inner_margin(margin(
        space::PAGE_X as i8,
        space::PAGE_X as i8,
        0,
        0,
    ))
}

/// `.update-card`: `--card`, `--border-2`, `--r`, padding `10px 10px 10px 12px`.
pub fn card_frame() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(stroke_border_2())
        .corner_radius(R)
        .inner_margin(margin(12, 10, 10, 10))
}

/// `.composer` / `.set-card`: `--panel`, `--border`, `--r-l`, no padding —
/// the rows inside carry their own.
pub fn panel_card_frame() -> Frame {
    Frame::new()
        .fill(PANEL)
        .stroke(stroke_border())
        .corner_radius(R_L)
}

/// `.files`: `--panel`, `--border`, `--r`, no padding; the rows rule themselves off.
pub fn files_frame() -> Frame {
    Frame::new()
        .fill(PANEL)
        .stroke(stroke_border())
        .corner_radius(R)
}

/// `.kind-icon`: a 30 px `--card` tile with a `--border` outline.
pub fn kind_icon_frame() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(stroke_border())
        .corner_radius(R_S)
}

/// `.empty-icon`: a 44 px `--card` tile with a 10 px radius.
pub fn empty_icon_frame() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(stroke_border())
        .corner_radius(R_EMPTY)
}

/// `.dest-pick` / an input at rest: `--surface`, `--border-2`, `--r-s`.
pub fn surface_frame() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .stroke(stroke_border_2())
        .corner_radius(R_S)
        .inner_margin(margin(8, 8, 0, 0))
}

/// `.segmented`: `--surface`, `--border-2`, `--r-s`, `padding: 2px`.
pub fn segmented_frame() -> Frame {
    Frame::new()
        .fill(SURFACE)
        .stroke(stroke_border_2())
        .corner_radius(R_S)
        .inner_margin(Margin::same(size::SEGMENT_INSET as i8))
}

/// `.toast`: `--card`, `--border-2`, `--r`, `0 12px 32px #000a`, padding 10/14.
pub fn toast_frame() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(stroke_border_2())
        .corner_radius(R)
        .shadow(SHADOW_TOAST)
        .inner_margin(margin(14, 14, 10, 10))
}

/// `.modal-box`: `--panel`, `--border-2`, `--r-l`, `0 24px 64px #000c`.
pub fn modal_frame() -> Frame {
    Frame::new()
        .fill(PANEL)
        .stroke(stroke_border_2())
        .corner_radius(R_L)
        .shadow(SHADOW_MODAL)
}

/// A dropdown under a `.dest-pick`: `--panel`, `--border-2`, `--r`, a big shadow.
pub fn popup_frame() -> Frame {
    Frame::new()
        .fill(PANEL)
        .stroke(stroke_border_2())
        .corner_radius(R)
        .shadow(SHADOW_MODAL)
        .inner_margin(Margin::same(space::S as i8))
}

/// A label that cuts with an ellipsis, like `.trunc` in the stylesheet.
pub fn truncating(text: impl Into<WidgetText>) -> Label {
    Label::new(text).truncate()
}

/// A label that wraps, for `.set-desc`-style help text.
pub fn wrapping(text: impl Into<WidgetText>) -> Label {
    Label::new(text).wrap()
}

// ---------------------------------------------------------------------------
// The token table
// ---------------------------------------------------------------------------

/// One value of a `:root` custom property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// A colour, written `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`.
    Color(Color32),
    /// A length, written `Npx`.
    Px(u8),
}

/// Every custom property in `:root`, in stylesheet order.
///
/// The test below asserts this table equals the stylesheet exactly (both
/// directions), so a token added to the design fails until it is here.
pub const TOKENS: &[(&str, Token)] = &[
    ("--base", Token::Color(BASE)),
    ("--panel", Token::Color(PANEL)),
    ("--surface", Token::Color(SURFACE)),
    ("--card", Token::Color(CARD)),
    ("--raised", Token::Color(RAISED)),
    ("--hover", Token::Color(HOVER)),
    ("--active", Token::Color(ACTIVE)),
    ("--border", Token::Color(BORDER)),
    ("--border-2", Token::Color(BORDER_2)),
    ("--border-3", Token::Color(BORDER_3)),
    ("--text", Token::Color(TEXT)),
    ("--text-body", Token::Color(TEXT_BODY)),
    ("--text-2", Token::Color(TEXT_2)),
    ("--text-3", Token::Color(TEXT_3)),
    ("--text-4", Token::Color(TEXT_4)),
    ("--brand", Token::Color(BRAND)),
    ("--brand-hover", Token::Color(BRAND_HOVER)),
    ("--brand-soft", Token::Color(BRAND_SOFT)),
    ("--success", Token::Color(SUCCESS)),
    ("--error", Token::Color(ERROR)),
    ("--error-text", Token::Color(ERROR_TEXT)),
    ("--notice", Token::Color(NOTICE)),
    ("--info", Token::Color(INFO)),
    ("--r-s", Token::Px(R_S_PX)),
    ("--r", Token::Px(R_PX)),
    ("--r-l", Token::Px(R_L_PX)),
];

/// The colour of a custom property, if that property is a colour.
pub fn color(property: &str) -> Option<Color32> {
    TOKENS.iter().find_map(|(name, token)| match token {
        Token::Color(c) if *name == property => Some(*c),
        _ => None,
    })
}

/// The px value of a custom property, if that property is a length.
pub fn px(property: &str) -> Option<u8> {
    TOKENS.iter().find_map(|(name, token)| match token {
        Token::Px(v) if *name == property => Some(*v),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// The egui style
// ---------------------------------------------------------------------------

/// Apply the theme to a context, for both the light and the dark preference —
/// this front end is dark only, as `color-scheme: dark` says.
pub fn install(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| *style = self::style());
}

/// The style that makes egui draw like the stylesheet: dark, flat, hairlines,
/// 6 px corners, 13 px text.
pub fn style() -> Style {
    let mut style = Style::default();
    style.visuals = visuals();
    style.spacing = spacing();
    style.text_styles = text_styles();
    // The stylesheet transitions over `.1s`; egui animates hover the same way.
    style.animation_time = 0.1;
    style
}

/// The body/button/heading text styles the views name, plus the named styles
/// `section`, `caption`, `status`, `badge` and `drop`.
pub fn text_styles() -> BTreeMap<TextStyle, FontId> {
    let mut styles = BTreeMap::new();
    styles.insert(TextStyle::Body, body());
    styles.insert(TextStyle::Button, body());
    styles.insert(TextStyle::Small, detail());
    styles.insert(TextStyle::Monospace, mono());
    styles.insert(TextStyle::Heading, title());
    styles.insert(TextStyle::Name("section".into()), section());
    styles.insert(TextStyle::Name("caption".into()), caption());
    styles.insert(TextStyle::Name("status".into()), note());
    styles.insert(TextStyle::Name("badge".into()), badge());
    styles.insert(
        TextStyle::Name("drop".into()),
        FontId::new(font::DROP, FontFamily::Proportional),
    );
    styles
}

/// Colours for widgets, inputs, selection and windows.
pub fn visuals() -> Visuals {
    let mut v = Visuals::dark();

    // A view draws on --base; a floating window is --panel, as `.modal-box` is.
    v.panel_fill = BASE;
    v.window_fill = PANEL;
    v.window_stroke = stroke_border_2();
    v.window_corner_radius = R_L;
    v.window_shadow = SHADOW_MODAL;
    v.menu_corner_radius = R;
    v.popup_shadow = SHADOW_MODAL;

    // Text: leave the override off, so a view can tint a label and the widget
    // strokes still say what the stylesheet says.
    v.override_text_color = None;
    v.weak_text_color = Some(TEXT_3);
    v.faint_bg_color = CARD;
    v.extreme_bg_color = SURFACE;
    v.text_edit_bg_color = Some(SURFACE);
    v.code_bg_color = RAISED;
    v.hyperlink_color = LINK;
    v.warn_fg_color = NOTICE;
    v.error_fg_color = ERROR_TEXT;
    v.selection = egui::style::Selection {
        bg_fill: BRAND_SOFT,
        stroke: Stroke::new(size::HAIRLINE, TEXT),
    };
    v.text_cursor.stroke = Stroke::new(2.0_f32, BRAND);

    // Flat: no glow, no stripes, and the 1 px hairlines of the stylesheet.
    v.striped = false;
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.slider_trailing_fill = false;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    // `.btn:disabled { opacity: .4 }`.
    v.disabled_alpha = 0.4;

    // Widgets, in the order the stylesheet uses them:
    // rest is `--card` with `--border-2`, hover is `--raised` with `--border-3`.
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BASE;
    w.noninteractive.weak_bg_fill = BASE;
    w.noninteractive.bg_stroke = stroke_border();
    w.noninteractive.fg_stroke = Stroke::new(size::HAIRLINE, TEXT_BODY);
    w.noninteractive.corner_radius = R_S;
    w.noninteractive.expansion = 0.0;

    w.inactive.bg_fill = SURFACE;
    w.inactive.weak_bg_fill = CARD;
    w.inactive.bg_stroke = stroke_border_2();
    w.inactive.fg_stroke = Stroke::new(size::HAIRLINE, TEXT_BODY);
    w.inactive.corner_radius = R_S;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = RAISED;
    w.hovered.weak_bg_fill = RAISED;
    w.hovered.bg_stroke = hairline(BORDER_3);
    w.hovered.fg_stroke = Stroke::new(size::HAIRLINE, TEXT);
    w.hovered.corner_radius = R_S;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = RAISED;
    w.active.weak_bg_fill = RAISED;
    w.active.bg_stroke = stroke_brand();
    w.active.fg_stroke = Stroke::new(size::HAIRLINE, TEXT);
    w.active.corner_radius = R_S;
    w.active.expansion = 0.0;

    w.open.bg_fill = RAISED;
    w.open.weak_bg_fill = RAISED;
    w.open.bg_stroke = hairline(BORDER_3);
    w.open.fg_stroke = Stroke::new(size::HAIRLINE, TEXT);
    w.open.corner_radius = R_S;
    w.open.expansion = 0.0;

    v
}

/// The spacing egui uses between and inside widgets.
pub fn spacing() -> Spacing {
    let mut s = Spacing::default();
    s.item_spacing = Vec2::new(space::M, space::M);
    // `.btn { padding: 0 12px }`, and enough vertical padding to reach 32 px.
    s.button_padding = Vec2::new(space::XL, space::M);
    s.interact_size = Vec2::new(60.0, size::CONTROL_H);
    s.window_margin = Margin::same(space::XXL as i8);
    s.menu_margin = Margin::same(space::S as i8);
    s.indent = 18.0;
    s.slider_width = 200.0;
    s.slider_rail_height = size::TRACK_H;
    s.combo_width = 120.0;
    s.text_edit_width = 220.0;
    s.icon_width = size::CHECK;
    s.icon_width_inner = space::M;
    s.icon_spacing = space::M;
    s.tooltip_width = 360.0;
    s.scroll.bar_width = size::SCROLLBAR;
    s.scroll.handle_min_length = 24.0;
    s.scroll.bar_inner_margin = 2.0;
    s.scroll.bar_outer_margin = 0.0;
    s.scroll.floating = true;
    // `::-webkit-scrollbar-thumb` is a light thumb on a transparent track: take
    // the handle from the foreground rather than from `bg_fill`, which is
    // `--surface` and would all but vanish on a `--panel` sidebar.
    s.scroll.foreground_color = true;
    s
}

// ---------------------------------------------------------------------------
// The design drift tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::RectShape;

    // -- reading the stylesheet ---------------------------------------------

    /// `tests/design/style.css`, the webview front end's design — frozen, since
    /// the webview and its `frontend/dist` are retired — read at test time so a
    /// change to a token fails here rather than in a screenshot.
    fn stylesheet() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("design")
            .join("style.css");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read the stylesheet {}: {e}", path.display()))
    }

    /// Collapse runs of whitespace, so `\n  height: 32px` and `height: 32px`
    /// compare equal.
    fn squeeze(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Drop `/* … */` comments; they otherwise end up glued to the next selector.
    fn strip_comments(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(open) = rest.find("/*") {
            out.push_str(rest.get(..open).unwrap_or(""));
            match rest.get(open..).and_then(|tail| tail.find("*/")) {
                Some(len) => rest = rest.get(open + len + 2..).unwrap_or(""),
                // An unterminated comment swallows the rest of the file.
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    /// Every `selector { declarations }` block, in source order. Nested blocks
    /// (inside `@media`, `@keyframes`) carry their own prelude as the selector.
    fn blocks(css: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        let mut buf = String::new();
        for ch in css.chars() {
            match ch {
                '{' => {
                    stack.push(squeeze(&buf));
                    buf.clear();
                }
                '}' => {
                    let selector = stack.pop().unwrap_or_default();
                    out.push((selector, std::mem::take(&mut buf)));
                }
                _ => buf.push(ch),
            }
        }
        out
    }

    /// The first value of `property` in a block whose selector is exactly
    /// `selector`.
    fn declaration<'a>(
        blocks: &'a [(String, String)],
        selector: &str,
        property: &str,
    ) -> Option<&'a str> {
        let wanted = squeeze(selector);
        blocks
            .iter()
            .filter(|(sel, _)| *sel == wanted)
            .find_map(|(_, body)| {
                body.split(';').find_map(|decl| {
                    let (name, value) = decl.split_once(':')?;
                    if squeeze(name) == property {
                        Some(value.trim())
                    } else {
                        None
                    }
                })
            })
    }

    /// Like [`declaration`], but says which selector went missing.
    fn required(blocks: &[(String, String)], selector: &str, property: &str) -> String {
        declaration(blocks, selector, property)
            .unwrap_or_else(|| panic!("`{selector} {{ {property} }}` is no longer in the stylesheet"))
            .to_string()
    }

    /// The px lengths in a CSS value, in order: `0 28px 32px` → `[0, 28, 32]`,
    /// `min(560px, 92vw)` → `[560]`, `400 13px/1.45` → `[13]`.
    fn lengths(value: &str) -> Vec<f32> {
        let bytes = value.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let negative = bytes[i] == b'-' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit);
            if !(bytes[i].is_ascii_digit() || negative) {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while bytes
                .get(i)
                .is_some_and(|b| b.is_ascii_digit() || *b == b'.')
            {
                i += 1;
            }
            let Ok(number) = std::str::from_utf8(&bytes[start..i]).unwrap_or("").parse::<f32>() else {
                continue;
            };
            if bytes.get(i..i + 2) == Some(b"px") {
                out.push(number);
                i += 2;
            } else if number == 0.0 && !bytes.get(i).is_some_and(|b| b.is_ascii_digit() || *b == b'%' || *b == b'.') {
                // A bare `0` is a length; `0.5`, `13%` and `92vw` are not.
                out.push(0.0);
            }
        }
        out
    }

    /// A `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` colour.
    fn parse_hex(text: &str) -> Color32 {
        let raw = text.trim();
        let hex = raw.strip_prefix('#').unwrap_or(raw);
        let nibbles: Vec<u8> = hex
            .chars()
            .map(|c| {
                c.to_digit(16)
                    .unwrap_or_else(|| panic!("{raw:?} is not a hex colour")) as u8
            })
            .collect();
        let scaled = |i: usize| nibbles[i] * 17;
        let paired = |i: usize| nibbles[i] * 16 + nibbles[i + 1];
        match nibbles.len() {
            3 => Color32::from_rgb(scaled(0), scaled(1), scaled(2)),
            4 => Color32::from_rgba_unmultiplied_const(scaled(0), scaled(1), scaled(2), scaled(3)),
            6 => Color32::from_rgb(paired(0), paired(2), paired(4)),
            8 => Color32::from_rgba_unmultiplied_const(paired(0), paired(2), paired(4), paired(6)),
            other => panic!("{raw:?} has {other} hex digits; expected 3, 4, 6 or 8"),
        }
    }

    // -- the tokens ---------------------------------------------------------

    #[test]
    fn every_root_token_matches_the_stylesheet() {
        let css = stylesheet();
        let parsed = blocks(&strip_comments(&css));
        let root = parsed
            .iter()
            .find(|(sel, _)| sel == ":root")
            .map(|(_, body)| body.clone())
            .expect("the stylesheet has no :root block");

        let declared: Vec<(String, String)> = root
            .split(';')
            .filter_map(|decl| {
                let (name, value) = decl.split_once(':')?;
                let name = squeeze(name);
                name.starts_with("--").then(|| (name, value.trim().to_string()))
            })
            .collect();
        assert!(!declared.is_empty(), ":root declares no custom properties");

        // The two lists must name the same properties: a token added to the
        // design has to get a constant here before this passes again.
        let mut from_css: Vec<&str> = declared.iter().map(|(name, _)| name.as_str()).collect();
        let mut from_theme: Vec<&str> = TOKENS.iter().map(|(name, _)| *name).collect();
        from_css.sort_unstable();
        from_theme.sort_unstable();
        assert_eq!(
            from_theme, from_css,
            "theme::TOKENS and the stylesheet's :root disagree about which properties exist"
        );

        let unique = {
            let mut names = from_theme.clone();
            names.dedup();
            names.len()
        };
        assert_eq!(unique, TOKENS.len(), "theme::TOKENS lists a property twice");

        for (name, value) in &declared {
            let token = TOKENS
                .iter()
                .find(|(token_name, _)| token_name == name)
                .map(|(_, token)| *token)
                .unwrap_or_else(|| panic!("{name} has no constant in theme.rs"));
            match token {
                Token::Color(ours) => {
                    let theirs = parse_hex(value);
                    assert_eq!(
                        ours,
                        theirs,
                        "{name}: the stylesheet says {value}, the theme says {ours:?}"
                    );
                }
                Token::Px(ours) => {
                    let theirs = lengths(value).first().copied().unwrap_or_else(|| {
                        panic!("{name}: {value:?} is not a px length")
                    });
                    assert_eq!(
                        f32::from(ours),
                        theirs,
                        "{name}: the stylesheet says {value}, the theme says {ours}px"
                    );
                }
            }
        }

        // Both lookups reach every token, so a view can ask by CSS name.
        for (name, token) in TOKENS {
            match token {
                Token::Color(c) => assert_eq!(color(name), Some(*c), "{name} is not in color()"),
                Token::Px(v) => assert_eq!(px(name), Some(*v), "{name} is not in px()"),
            }
        }
        assert_eq!(color("--nothing"), None);
        assert_eq!(px("--base"), None);
    }

    /// Colours the stylesheet writes inline rather than as a custom property.
    const INLINE_COLOURS: &[(&str, Color32)] = &[
        ("#9a95ff", LINK),
        ("#b8b4ff", LINK_HOVER),
        ("#fff", WHITE),
        ("#32374c", INPUT_BTN_HOVER),
        ("#ffffff05", ROW_HOVER),
        ("#f433501f", CONFIRM_TINT),
        ("#5b53ff14", DROP_TINT),
        ("#ffffff1f", SCROLL_THUMB),
        ("#ffffff33", SCROLL_THUMB_HOVER),
        ("#000a", SCRIM_MODAL),
        ("#000c", SCRIM_DROP),
    ];

    #[test]
    fn inline_colours_match_the_stylesheet() {
        let css = stylesheet();
        for (literal, ours) in INLINE_COLOURS {
            assert!(
                css.contains(literal),
                "{literal} is no longer in the stylesheet; the constant using it is stale"
            );
            assert_eq!(
                parse_hex(literal),
                *ours,
                "the constant for {literal} has drifted from the stylesheet"
            );
        }
    }

    // -- sizes, type, radii -------------------------------------------------

    /// `(selector, property, value index, theme path, value)` for every px
    /// number this module names. The index picks a value out of a shorthand:
    /// `padding: 0 28px 32px` has `0` at 0, `28` at 1 and `32` at 2.
    const PX_SITES: &[(&str, &str, usize, &str, f32)] = &[
        // frame and control sizes
        (".app", "grid-template-columns", 0, "size::SIDEBAR_W", size::SIDEBAR_W),
        (".page-head", "height", 0, "size::HEAD_H", size::HEAD_H),
        (".traffic", "height", 0, "size::TRAFFIC_H", size::TRAFFIC_H),
        (".btn", "height", 0, "size::CONTROL_H", size::CONTROL_H),
        (".nav-item", "height", 0, "size::CONTROL_H", size::CONTROL_H),
        (
            "input[type=text], input[type=password], select",
            "height",
            0,
            "size::CONTROL_H",
            size::CONTROL_H,
        ),
        (".btn.sm", "height", 0, "size::CONTROL_SM_H", size::CONTROL_SM_H),
        (".segmented button", "height", 0, "size::CONTROL_SM_H", size::CONTROL_SM_H),
        (".input-btn", "height", 0, "size::NESTED_BTN_H", size::NESTED_BTN_H),
        (".icon-btn", "width", 0, "size::ICON_BTN", size::ICON_BTN),
        // list geometry
        (".row-main", "min-height", 0, "size::ROW_H", size::ROW_H),
        (".frow", "height", 0, "size::FILE_ROW_H", size::FILE_ROW_H),
        (".list-head", "height", 0, "size::LIST_HEAD_H", size::LIST_HEAD_H),
        (".files", "max-height", 0, "size::FILES_MAX_H", size::FILES_MAX_H),
        (".track", "height", 0, "size::TRACK_H", size::TRACK_H),
        (".frow .track", "height", 0, "size::FILE_TRACK_H", size::FILE_TRACK_H),
        (".status .dot", "width", 0, "size::DOT", size::DOT),
        // boxes
        (".kind-icon", "width", 0, "size::KIND_ICON", size::KIND_ICON),
        (".mark", "width", 0, "size::MARK", size::MARK),
        (".avatar", "width", 0, "size::AVATAR", size::AVATAR),
        (".empty-icon", "width", 0, "size::EMPTY_ICON", size::EMPTY_ICON),
        (".count", "height", 0, "size::BADGE", size::BADGE),
        (".count", "min-width", 0, "size::BADGE", size::BADGE),
        (".check input", "width", 0, "size::CHECK", size::CHECK),
        // composer
        (".comp-label", "width", 0, "size::COMP_LABEL_W", size::COMP_LABEL_W),
        (".comp-foot", "min-height", 0, "size::COMP_FOOT_H", size::COMP_FOOT_H),
        (".comp-foot", "padding", 3, "size::COMP_FOOT_INDENT", size::COMP_FOOT_INDENT),
        (".input-wrap input", "padding-left", 0, "size::INPUT_ICON_PAD", size::INPUT_ICON_PAD),
        (".input-wrap input", "padding-right", 0, "size::INPUT_BTN_PAD", size::INPUT_BTN_PAD),
        (".segmented", "padding", 0, "size::SEGMENT_INSET", size::SEGMENT_INSET),
        // windows
        (".modal-box", "width", 0, "size::MODAL_W", size::MODAL_W),
        (".modal-box", "height", 0, "size::MODAL_H", size::MODAL_H),
        (".settings", "max-width", 0, "size::SETTINGS_W", size::SETTINGS_W),
        (".toasts", "max-width", 0, "size::TOAST_MAX_W", size::TOAST_MAX_W),
        ("::-webkit-scrollbar", "width", 0, "size::SCROLLBAR", size::SCROLLBAR),
        // icons
        (".dir-cell svg", "width", 0, "icon::ROW", icon::ROW),
        (".btn svg", "width", 0, "icon::CONTROL", icon::CONTROL),
        (".nav-item svg", "width", 0, "icon::NAV", icon::NAV),
        (".empty-icon svg", "width", 0, "icon::EMPTY", icon::EMPTY),
        (".drop-box svg", "width", 0, "icon::DROP", icon::DROP),
        // literal radii
        ("code", "border-radius", 0, "R_MICRO_PX", R_MICRO_PX as f32),
        (".track", "border-radius", 0, "R_TRACK_PX", R_TRACK_PX as f32),
        (".count", "border-radius", 0, "R_BADGE_PX", R_BADGE_PX as f32),
        (".mark", "border-radius", 0, "R_MARK_PX", R_MARK_PX as f32),
        (".empty-icon", "border-radius", 0, "R_EMPTY_PX", R_EMPTY_PX as f32),
        (".drop-box", "border-radius", 0, "R_DROP_PX", R_DROP_PX as f32),
        // gaps and padding
        (".nav", "gap", 0, "space::XXS", space::XXS),
        (".meta", "gap", 0, "space::XS", space::XS),
        (".frow .fn", "gap", 0, "space::S", space::S),
        (".head-actions", "gap", 0, "space::M", space::M),
        (".nav-item", "gap", 0, "space::L", space::L),
        (".watch-grid", "gap", 0, "space::XL", space::XL),
        (".jobs-grid", "gap", 0, "space::XXL", space::XXL),
        (".comp-row", "gap", 0, "space::XXXL", space::XXXL),
        (".toasts", "bottom", 0, "space::HUGE", space::HUGE),
        (".row", "padding", 1, "space::ROW_X", space::ROW_X),
        (".sidebar", "padding", 1, "space::SIDEBAR_X", space::SIDEBAR_X),
        (".brand", "padding", 2, "space::BRAND_BOTTOM", space::BRAND_BOTTOM),
        // type sizes
        ("body", "font", 0, "font::BODY", font::BODY),
        (".page-head h1", "font-size", 0, "font::TITLE", font::TITLE),
        (".set-title", "font-size", 0, "font::SECTION", font::SECTION),
        (".modal-title", "font-size", 0, "font::SECTION", font::SECTION),
        (".empty-title", "font-size", 0, "font::SECTION", font::SECTION),
        (".brand-name", "font-size", 0, "font::BRAND", font::BRAND),
        (".list-head", "font-size", 0, "font::CAPTION", font::CAPTION),
        (".dest-sec", "font-size", 0, "font::CAPTION", font::CAPTION),
        (".who .e", "font-size", 0, "font::CAPTION", font::CAPTION),
        (".sub", "font-size", 0, "font::DETAIL", font::DETAIL),
        (".frow", "font-size", 0, "font::DETAIL", font::DETAIL),
        (".about", "font-size", 0, "font::DETAIL", font::DETAIL),
        (".status", "font-size", 0, "font::NOTE", font::NOTE),
        (".uc-title", "font-size", 0, "font::NOTE", font::NOTE),
        (".count", "font-size", 0, "font::BADGE", font::BADGE),
        (".avatar", "font-size", 0, "font::BADGE", font::BADGE),
        ("code", "font", 0, "font::CODE", font::CODE),
        (".drop-title", "font-size", 0, "font::DROP", font::DROP),
    ];

    #[test]
    fn sizes_type_and_spacing_match_the_stylesheet() {
        let css = stylesheet();
        let parsed = blocks(&strip_comments(&css));
        let mut checked = 0;
        for (selector, property, index, label, ours) in PX_SITES {
            let value = required(&parsed, selector, property);
            let found = lengths(&value);
            let theirs = found.get(*index).copied().unwrap_or_else(|| {
                panic!("{label}: no px value at {index} in `{selector} {{ {property}: {value} }}`")
            });
            assert!(
                (theirs - ours).abs() < 0.01,
                "{label} is {ours} but `{selector} {{ {property} }}` is now {value}"
            );
            checked += 1;
        }
        assert!(checked > 50, "the drift table shrank to {checked} entries");
    }

    /// Radii the stylesheet writes as `var(--r-s)`, `var(--r)` or `var(--r-l)`.
    const RADIUS_VARS: &[(&str, &str, &str)] = &[
        (".btn", "border-radius", "--r-s"),
        (".nav-item", "border-radius", "--r-s"),
        (".update-card", "border-radius", "--r"),
        (".files", "border-radius", "--r"),
        (".toast", "border-radius", "--r"),
        (".composer", "border-radius", "--r-l"),
        (".set-card", "border-radius", "--r-l"),
        (".modal-box", "border-radius", "--r-l"),
    ];

    #[test]
    fn radii_match_the_stylesheet_variables() {
        let css = stylesheet();
        let parsed = blocks(&strip_comments(&css));
        for (selector, property, variable) in RADIUS_VARS {
            let value = required(&parsed, selector, property);
            assert_eq!(
                value,
                format!("var({variable})"),
                "`{selector} {{ {property} }}` no longer uses {variable}"
            );
            assert!(
                px(variable).is_some(),
                "{variable} is not a length token in theme::TOKENS"
            );
        }
        // the CornerRadius constants are the token values
        assert_eq!(R_S.nw, R_S_PX);
        assert_eq!(R.ne, R_PX);
        assert_eq!(R_L.sw, R_L_PX);
        assert!(R.is_same() && R_S.is_same() && R_L.is_same());
    }

    #[test]
    fn weights_and_line_height_match_the_stylesheet() {
        let css = stylesheet();
        let parsed = blocks(&strip_comments(&css));

        let body_font = required(&parsed, "body", "font");
        // `font: 400 13px/1.45 …` — the leading token is the weight.
        let first_integer = |value: &str| -> f32 {
            let digits: String = value
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            digits.parse::<f32>().unwrap_or(0.0)
        };
        assert_eq!(
            first_integer(&body_font),
            f32::from(weight::REGULAR),
            "body font-weight"
        );
        assert!(
            body_font.contains(&format!("/{}", font::BODY_LINE)),
            "body line-height is no longer {}: {body_font}",
            font::BODY_LINE
        );
        assert_eq!(
            first_integer(&required(&parsed, ".nav-item", "font-weight")),
            f32::from(weight::MEDIUM),
            ".nav-item font-weight"
        );
        assert_eq!(
            first_integer(&required(&parsed, ".brand-name", "font-weight")),
            f32::from(weight::SEMIBOLD),
            ".brand-name font-weight"
        );
    }

    // -- the pure colour maths ----------------------------------------------

    #[test]
    fn status_colours_follow_the_stylesheet() {
        assert_eq!(status_color("done"), SUCCESS);
        assert_eq!(status_color("watching"), SUCCESS);
        assert_eq!(status_color("running"), BRAND);
        assert_eq!(status_color("uploading"), BRAND);
        assert_eq!(status_color("queued"), INFO);
        assert_eq!(status_color("resolving"), INFO);
        assert_eq!(status_color("settling"), INFO);
        assert_eq!(status_color("waiting"), INFO);
        assert_eq!(status_color("partial"), NOTICE);
        assert_eq!(status_color("interrupted"), NOTICE);
        assert_eq!(status_color("failed"), ERROR);
        // Unknown, empty and padded statuses fall back to the idle dot.
        assert_eq!(status_color(""), TEXT_4);
        assert_eq!(status_color("  weird "), TEXT_4);
        assert_eq!(status_color("  RuNnInG "), BRAND);

        assert_eq!(status_halo("running"), Some(BRAND_SOFT));
        assert_eq!(status_halo("done"), None);
        assert_eq!(status_halo(""), None);

        assert_eq!(status_text("error"), ERROR_TEXT);
        assert_eq!(status_text("running"), TEXT);
        // `.status.error` is the only status word the stylesheet recolours;
        // a failed job keeps the grey word and shows its error line in red.
        assert_eq!(status_text("failed"), TEXT_2);
        assert_eq!(status_text(""), TEXT_2);

        // A failed *file* row is red, though: `.fst.failed`.
        assert_eq!(file_status_text("failed"), ERROR_TEXT);
        assert_eq!(file_status_text("done"), TEXT_2);
        assert_eq!(file_status_text("running"), TEXT);
        assert_eq!(file_status_text("queued"), TEXT_3);
        assert_eq!(file_status_text(""), TEXT_3);

        assert_eq!(progress_color("done"), SUCCESS);
        assert_eq!(progress_color("failed"), ERROR);
        assert_eq!(progress_color("partial"), NOTICE);
        assert_eq!(progress_color("canceled"), TEXT_4);
        assert_eq!(progress_color("running"), BRAND);
        assert_eq!(progress_color(""), BRAND);
    }

    #[test]
    fn the_text_ramp_is_the_five_text_tokens_and_clamps() {
        assert_eq!(text_ramp(), TEXT_RAMP);
        for (step, expected) in TEXT_RAMP.iter().enumerate() {
            assert_eq!(text_level(step), *expected);
        }
        assert_eq!(text_level(5), TEXT_4, "past the end clamps to the faintest");
        assert_eq!(text_level(usize::MAX), TEXT_4);
        assert_eq!((text_primary(), text_default()), (TEXT, TEXT_BODY));
        assert_eq!((text_cell(), text_muted(), text_faint()), (TEXT_2, TEXT_3, TEXT_4));
    }

    // -- what a screen actually paints --------------------------------------

    #[test]
    fn a_card_drawn_with_the_theme_paints_the_stylesheet_colours() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        let long = "a name long enough that .trunc has to cut it with an ellipsis before the card ends";

        let mut harness = Harness::builder()
            .with_size(Vec2::new(420.0, 240.0))
            .build_ui(|ui| {
                panel_card_frame().show(ui, |ui| {
                    ui.label(egui::RichText::new("clip.mov").color(TEXT).size(font::BODY));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("uploading")
                                .color(status_text("uploading"))
                                .size(font::NOTE),
                        );
                        // the dot in front of the status word
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::splat(size::DOT),
                            egui::Sense::hover(),
                        );
                        ui.painter().circle_filled(
                            rect.center(),
                            size::DOT / 2.0,
                            status_color("uploading"),
                        );
                    });
                });
                card_frame().show(ui, |ui| {
                    ui.add(truncating(long));
                });
            });
        harness.ctx.set_style(style());
        harness.run();

        // The style reached the context.
        let applied = harness.ctx.style();
        assert_eq!(applied.visuals.panel_fill, BASE);
        assert_eq!(applied.visuals.widgets.inactive.weak_bg_fill, CARD);
        assert_eq!(applied.visuals.widgets.hovered.weak_bg_fill, RAISED);
        assert_eq!(applied.visuals.widgets.inactive.corner_radius, R_S);
        assert_eq!(
            applied.text_styles.get(&TextStyle::Body).map(|f| f.size),
            Some(font::BODY)
        );

        // The words are in the tree, so a view can find them by their wording.
        assert!(harness.get_by_label("clip.mov").rect().height() > 0.0);
        assert!(harness.get_by_label("uploading").rect().height() > 0.0);

        // The long name is cut to the card, not drawn past it.
        let truncated = harness.query_by_label(long).expect("the long name is drawn");
        assert!(
            truncated.rect().width() <= 420.0,
            "a `.trunc` label is not truncated: {} px wide",
            truncated.rect().width()
        );

        // The two containers paint the fills, radii and hairlines they claim.
        let rects: Vec<&RectShape> = harness
            .output()
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) => Some(rect),
                _ => None,
            })
            .collect();
        let painted = |fill: Color32, radius: CornerRadius, border: Color32| {
            rects
                .iter()
                .any(|r| r.fill == fill && r.corner_radius == radius && r.stroke.color == border)
        };
        assert!(
            painted(PANEL, R_L, BORDER),
            "`.composer` is not drawn as --panel / --r-l / --border"
        );
        assert!(
            painted(CARD, R, BORDER_2),
            "`.update-card` is not drawn as --card / --r / --border-2"
        );

        // The status dot is brand, not a default.
        let dots: Vec<&egui::epaint::CircleShape> = harness
            .output()
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Circle(circle) => Some(circle),
                _ => None,
            })
            .collect();
        assert!(
            dots.iter().any(|c| c.fill == BRAND),
            "the running dot is not painted in --brand"
        );
    }
}
