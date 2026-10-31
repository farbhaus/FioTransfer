//! The sidebar: brand, navigation, throughput, account.
//!
//! WORKER B owns this file. It is the egui reading of the `.sidebar` block in
//! `frontend/dist/style.css` (with `.traffic`, `.brand`, `.mark`, `.nav`,
//! `.nav-item`, `.count`, `.throughput`, `.account`, `.avatar`, `.who`) and of
//! the `<aside class="sidebar">` markup in `index.html`. Every number is the
//! stylesheet's own (CSS px == egui points at `pixels_per_point == 1`), taken
//! from `theme.rs`, so a 224 x 780 panel stacks like this:
//!
//! ```text
//!   0.. 40   `.traffic`   the traffic-light spacer (the webview's window buttons sit there)
//!  40.. 88   `.brand`     4 px top padding, 26 px mark + 10 px gap + "FioTransfer" (14 px), 18 px below
//!  88..256   `.nav`       five 32 px items, 2 px apart, 10 px padding and 18 px icon
//! 256..639   `.grow`      the flexible gap: everything the bottom blocks do not need
//! 639..723   `.throughput` 1 px rule, 12 px top, three 17.4 px rows 5 px apart, 10 px bottom
//! 723..768   `.account`  1 px rule, 12 px top, then the 32 px row (avatar, or the Sign in button)
//! 768..780   the `.sidebar` bottom padding
//! ```
//!
//! [`show`] paints the panel fill and the 1 px right border itself and applies
//! that 12 px padding, so the shell must hand it a zero-margin rect (see
//! [`WIDTH`]).
//!
//! Every value the webview gets from `app.js` is read here from [`UiState`]:
//! the Transfers badge is the count of non-terminal jobs, the Watch Folders
//! badge the count of enabled watches (both hidden at zero, exactly as
//! `renderSnapshot`/`renderWatches` hide `#active-count`/`#watch-count`), and
//! the two speeds are the sum of the per-job speeds by kind, as
//! `renderSnapshot` computes `up`/`down` — not `snapshot.speed`.

use crate::model::{Request, UiState, View, fmt_speed};
use crate::theme;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2,
    emath::GuiRounding as _,
};

/// The sidebar's width: `.app { grid-template-columns: 224px 1fr }` in the
/// stylesheet, read here from the theme. The shell gives the panel this width;
/// [`layout`] follows whatever rect it is handed, so a wider panel still works.
pub const WIDTH: f32 = theme::size::SIDEBAR_W;

// ---------- design tokens ----------
// Every colour, radius, size and type size comes from `theme.rs`, which reads
// them off `frontend/dist/style.css` (`:root`) and fails its own tests if a
// value drifts from the stylesheet. What is left below is the sidebar's own
// geometry: the paddings and gaps that are written inline in the CSS rules
// (`.nav-item { padding: 0 10px }`), quoted from the rule that carries them.

/// `.sidebar { border-right: 1px solid var(--border) }`.
const BORDER_W: f32 = theme::size::HAIRLINE;

/// `.sidebar { padding: 0 12px 12px }`.
const PAD_X: f32 = theme::space::SIDEBAR_X;
const PAD_BOTTOM: f32 = theme::space::SIDEBAR_X;

/// `.traffic { height: 40px }` — the strip the webview's window buttons sit in.
const TRAFFIC_H: f32 = theme::size::TRAFFIC_H;

/// `.brand { padding: 4px 8px 18px; gap: 10px }`, `.mark { 26px }`.
const BRAND_PAD_TOP: f32 = theme::space::XS;
const BRAND_PAD_X: f32 = theme::space::M;
const BRAND_PAD_BOTTOM: f32 = theme::space::BRAND_BOTTOM;
const MARK_SIZE: f32 = theme::size::MARK;
const BRAND_GAP: f32 = theme::space::L;

/// `.nav-item { height: 32px; padding: 0 10px; gap: 10px }`, `.nav { gap: 2px }`.
const NAV_H: f32 = theme::size::CONTROL_H;
const NAV_GAP: f32 = theme::space::XXS;
const NAV_PAD_X: f32 = theme::space::L;
const ICON: f32 = 18.0; // `.nav-item svg { width: 18px; height: 18px }`
const ICON_GAP: f32 = theme::space::L;

/// `.count { min-width: 18px; height: 18px; padding: 0 5px }`.
const BADGE_H: f32 = theme::size::BADGE;
const BADGE_MIN_W: f32 = theme::size::BADGE;
const BADGE_PAD_X: f32 = 5.0;

/// `.throughput { padding: 12px 10px 10px; gap: 5px }`, `.tp-row { font-size: 12px }`.
const TP_PAD_TOP: f32 = 12.0;
const TP_PAD_X: f32 = theme::space::L;
const TP_PAD_BOTTOM: f32 = 10.0;
const TP_ROW_H: f32 = theme::font::DETAIL * theme::font::BODY_LINE; // 12px at line-height 1.45
const TP_GAP: f32 = 5.0;

/// `.account { padding: 12px 6px 0; gap: 10px }`, `.avatar { 28px }`, `.btn { 32px }`.
const ACC_PAD_TOP: f32 = 12.0;
const ACC_PAD_X: f32 = theme::space::S;
const ACC_GAP: f32 = theme::space::L;
const AVATAR: f32 = theme::size::AVATAR;
const BTN_H: f32 = theme::size::CONTROL_H;
/// `.who { line-height: 1.25 }` around `.who .n { font-size: 13px }` and
/// `.who .e { font-size: 11.5px }`: the signed-in row is two line boxes tall
/// (30.625 px), which is what sets the height of the whole bottom stack — one
/// pixel taller than the sign-in button's 32 px box would suggest.
const WHO_LINE_NAME: f32 = theme::font::BODY * 1.25;
const WHO_LINE_EMAIL: f32 = theme::font::CAPTION * 1.25;
const WHO_H: f32 = WHO_LINE_NAME + WHO_LINE_EMAIL;
const SIGN_IN_LABEL: &str = "Sign in to Frame.io";

// ---------- the blocks of the sidebar, in one place ----------

/// Where every block of the sidebar sits for a panel of `rect`.
///
/// Pure maths over the stylesheet's heights: no egui state, so it can be
/// checked on its own (see the tests).
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// `.traffic`: the traffic-light spacer, empty.
    pub traffic: Rect,
    /// The brand row's box, padding included.
    pub brand: Rect,
    /// The 26 px mark square inside the brand row.
    pub mark: Rect,
    /// The `.nav` column.
    pub nav: Rect,
    /// One rect per [`View::ALL`] entry, in that order.
    pub nav_items: [Rect; 5],
    /// The flexible `.grow` between the nav and the bottom blocks.
    pub gap: Rect,
    /// The `.throughput` block, border included.
    pub throughput: Rect,
    /// The three `.tp-row` rects: Upload, Download, Streams.
    pub tp_rows: [Rect; 3],
    /// The `.account` row, border and top padding excluded: the button's box.
    pub account: Rect,
    /// The `.account` content box, insets included.
    pub account_box: Rect,
}

/// Lay the sidebar out inside `rect` for a signed-out footer (a 32 px button).
///
/// Same as [`layout_with`] with `account_row_h = theme::size::CONTROL_H`.
pub fn layout(rect: Rect) -> Layout {
    layout_with(rect, BTN_H)
}

/// Lay the sidebar out inside `rect` (the whole panel, [`WIDTH`] x window height),
/// with `account_row_h` as the height of the footer's content row: 32 px for the
/// sign-in button, [`WHO_H`] (30.625 px) for the avatar/name/email.
///
/// The bottom blocks are anchored to the bottom of the panel, so their height
/// moves everything above them: the `.throughput` block and, above that, the
/// flexible gap. The fixed blocks keep their stylesheet heights. In a panel too
/// short for the design the gap collapses to zero and the bottom blocks are
/// pushed down (and clipped) rather than overlapping the navigation.
pub fn layout_with(rect: Rect, account_row_h: f32) -> Layout {
    let account_row_h = account_row_h.max(0.0);
    let content = Rect::from_min_max(
        pos2(rect.left() + PAD_X, rect.top()),
        pos2(rect.right() - PAD_X - BORDER_W, rect.bottom() - PAD_BOTTOM),
    );
    let w = content.width().max(0.0);
    let x = content.left();
    let top = content.top();

    let traffic = Rect::from_min_size(pos2(x, top), vec2(w, TRAFFIC_H));
    let brand_top = traffic.bottom();
    let brand = Rect::from_min_size(
        pos2(x, brand_top),
        vec2(w, BRAND_PAD_TOP + MARK_SIZE + BRAND_PAD_BOTTOM),
    );
    let mark = Rect::from_min_size(
        pos2(x + BRAND_PAD_X, brand_top + BRAND_PAD_TOP),
        Vec2::splat(MARK_SIZE),
    );

    let nav_top = brand.bottom();
    let nav_items: [Rect; 5] = std::array::from_fn(|i| {
        Rect::from_min_size(
            pos2(x, nav_top + i as f32 * (NAV_H + NAV_GAP)),
            vec2(w, NAV_H),
        )
    });
    let nav_bottom = nav_items[4].bottom();
    let nav = Rect::from_min_max(pos2(x, nav_top), pos2(x + w, nav_bottom));

    // The bottom blocks are anchored to the bottom of the panel.
    let account = Rect::from_min_size(
        pos2(x + ACC_PAD_X, (content.bottom() - account_row_h).max(content.top())),
        vec2((w - 2.0 * ACC_PAD_X).max(0.0), account_row_h),
    );
    let account_box = Rect::from_min_max(
        pos2(content.left(), account.top() - ACC_PAD_TOP - BORDER_W),
        pos2(content.right(), content.bottom()),
    );
    let tp_h = BORDER_W + TP_PAD_TOP + 3.0 * TP_ROW_H + 2.0 * TP_GAP + TP_PAD_BOTTOM;
    let mut throughput = Rect::from_min_size(
        pos2(x, account_box.top() - tp_h),
        vec2(w, tp_h),
    );
    let mut account = account;
    let mut account_box = account_box;

    let gap_h = (throughput.top() - nav_bottom).max(0.0);
    if throughput.top() < nav_bottom {
        // Too short for the design: keep the order, overflow at the bottom.
        let shift = nav_bottom - throughput.top();
        throughput = throughput.translate(vec2(0.0, shift));
        account = account.translate(vec2(0.0, shift));
        account_box = account_box.translate(vec2(0.0, shift));
    }
    let gap = Rect::from_min_size(pos2(x, nav_bottom), vec2(w, gap_h));

    let row_x = throughput.left() + TP_PAD_X;
    let row_w = (throughput.width() - 2.0 * TP_PAD_X).max(0.0);
    let tp_rows: [Rect; 3] = std::array::from_fn(|i| {
        Rect::from_min_size(
            pos2(
                row_x,
                throughput.top() + BORDER_W + TP_PAD_TOP + i as f32 * (TP_ROW_H + TP_GAP),
            ),
            vec2(row_w, TP_ROW_H),
        )
    });

    Layout {
        traffic,
        brand,
        mark,
        nav,
        nav_items,
        gap,
        throughput,
        tp_rows,
        account: Rect::from_min_max(
            pos2(account.left(), account.top()),
            pos2(account.right(), account.bottom()),
        ),
        account_box,
    }
}

/// Draw the sidebar into `ui`.
///
/// The shell (`app.rs`) puts this in a 240 px [`WIDTH`]-wide panel; the footer
/// blocks are anchored to the bottom of whatever rect `ui` offers, so a
/// taller or shorter window only changes the flexible gap.
pub fn show(ui: &mut egui::Ui, state: &mut UiState) {
    let mut rect = ui.max_rect();
    if !(rect.width().is_finite() && rect.height().is_finite()) {
        rect = ui.available_rect_before_wrap();
    }
    rect = rect.round_ui(); // keep the 1 px borders crisp
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let clip = rect.intersect(ui.clip_rect());
    if clip.width() < 1.0 || clip.height() < 1.0 {
        return;
    }
    let painter = ui.painter().with_clip_rect(clip);

    // `.sidebar { background: var(--panel); border-right: 1px solid var(--border) }`
    painter.rect_filled(rect, CornerRadius::ZERO, theme::PANEL);
    painter.line_segment(
        [
            pos2(rect.right() - BORDER_W * 0.5, rect.top()),
            pos2(rect.right() - BORDER_W * 0.5, rect.bottom()),
        ],
        theme::stroke_border(),
    );

    // `.account` is as tall as its content: the two `.who` line boxes when signed
    // in, the 32 px button when not. That height is what anchors the throughput
    // block and the gap above it.
    let account_row_h = if state.logged_in { WHO_H.max(AVATAR) } else { BTN_H };
    let l = layout_with(rect, account_row_h);
    let _ = l.traffic; // the traffic-light spacer: the webview's buttons sit there.

    brand(&painter, &l);
    nav(ui, &painter, &l, state);
    throughput(&painter, &l, state);
    account(ui, &painter, &l, state);
}

// ---------- the blocks ----------

/// `.brand`: the rounded brand square with the up/down glyph, then "FioTransfer".
fn brand(painter: &egui::Painter, l: &Layout) {
    let mark = l.mark;
    painter.rect_filled(mark, theme::R_MARK, theme::BRAND);
    // `.mark svg { width: 18px; height: 18px }`, stroke-width 1.7 in a 20 viewBox.
    let glyph = Rect::from_center_size(mark.center(), Vec2::splat(18.0));
    draw_icon(painter, glyph, Icon::Arrows, theme::WHITE, 1.7);

    let name_x = mark.right() + BRAND_GAP;
    let max_w = l.brand.right() - BRAND_PAD_X - name_x;
    let name = elide(painter, "FioTransfer", &brand_font(), max_w);
    paint_text(
        painter,
        pos2(name_x, mark.center().y),
        Align2::LEFT_CENTER,
        &name,
        brand_font(),
        theme::WHITE,
        true, // `.brand-name { font-weight: 600 }`
    );
}

/// `.nav`: five items, each with its SVG icon, its label and — Transfers and
/// Watch Folders only — the brand-coloured count badge.
fn nav(ui: &mut egui::Ui, painter: &egui::Painter, l: &Layout, state: &mut UiState) {
    for (i, view) in View::ALL.iter().enumerate() {
        let item = l.nav_items[i];
        let response = ui.interact(
            item,
            ui.id().with(("sidebar-nav", i)),
            Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                *view == state.view,
                view.label(),
            )
        });
        if response.clicked() {
            state.view = *view;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        let active = *view == state.view;
        let hovered = response.hovered();
        if active {
            // `.nav-item.active { background: var(--active); color: var(--text) }`
            painter.rect_filled(item, theme::R_S, theme::ACTIVE);
        } else if hovered {
            // `.nav-item:hover { background: var(--hover); color: var(--text-body) }`
            painter.rect_filled(item, theme::R_S, theme::HOVER);
        }

        let icon_rect = Rect::from_center_size(
            pos2(item.left() + NAV_PAD_X + ICON * 0.5, item.center().y),
            Vec2::splat(ICON),
        );
        // `.nav-item svg { color: var(--text-3) }`, and only `.nav-item.active svg`
        // goes full strength: hovering an inactive item brightens the label, not the icon.
        let icon_color = if active { theme::WHITE } else { theme::TEXT_3 };
        draw_icon(painter, icon_rect, icon_for(*view), icon_color, 1.5);

        // `.count`: min 18 width, 18 tall, radius 9, brand fill, 11 px 600.
        let badge = nav_badge(state, *view).filter(|n| *n > 0).map(|n| {
            let text = n.to_string();
            let text_w = painter
                .layout_no_wrap(text.clone(), theme::badge(), theme::WHITE)
                .size()
                .x;
            let w = (text_w + 2.0 * BADGE_PAD_X).max(BADGE_MIN_W);
            let r = Rect::from_min_size(
                pos2(item.right() - NAV_PAD_X - w, item.center().y - BADGE_H * 0.5),
                vec2(w, BADGE_H),
            );
            (
                r,
                text,
            )
        });

        let text_x = icon_rect.right() + ICON_GAP;
        let text_right = badge.as_ref().map_or(item.right() - NAV_PAD_X, |(r, _)| r.left() - 10.0);
        let text_color = if active {
            theme::WHITE
        } else if hovered {
            theme::TEXT_BODY
        } else {
            theme::TEXT_2 // `.nav-item { color: var(--text-2); font-weight: 500 }`
        };
        let label = elide(painter, view.label(), &theme::body(), text_right - text_x);
        paint_text(
            painter,
            pos2(text_x, item.center().y),
            Align2::LEFT_CENTER,
            &label,
            theme::body(),
            text_color,
            true, // font-weight: 500
        );

        if let Some((r, text)) = badge {
            painter.rect_filled(r, theme::R_BADGE, theme::BRAND);
            paint_text(
                painter,
                r.center(),
                Align2::CENTER_CENTER,
                &text,
                theme::badge(),
                theme::WHITE,
                true, // font-weight: 600
            );
        }
    }
}

/// `.throughput`: three label/value rows above the account row.
fn throughput(painter: &egui::Painter, l: &Layout, state: &UiState) {
    // `renderSnapshot` sums the per-job speeds by kind: upload jobs one way,
    // everything else the other. `snapshot.speed` is not what the sidebar shows.
    let mut up = 0.0_f64;
    let mut down = 0.0_f64;
    for job in &state.snapshot.jobs {
        if job.is_upload() {
            up += job.speed;
        } else {
            down += job.speed;
        }
    }
    let streams = format!(
        "{} / {}",
        state.snapshot.active_streams, state.snapshot.stream_limit
    );
    let rows = [
        ("Upload", fmt_speed(up), false),
        ("Download", fmt_speed(down), false),
        // `.tp-val.dim` on the streams row only.
        ("Streams", streams, true),
    ];

    // `.throughput { border-top: 1px solid var(--border) }`
    painter.line_segment(
        [
            pos2(l.throughput.left(), l.throughput.top() + BORDER_W * 0.5),
            pos2(l.throughput.right(), l.throughput.top() + BORDER_W * 0.5),
        ],
        theme::stroke_border(),
    );

    for (row, (label, value, dim)) in l.tp_rows.iter().zip(rows) {
        // `.tp-label { color: var(--text-3) }`, 12 px.
        paint_text(
            painter,
            pos2(row.left(), row.center().y),
            Align2::LEFT_CENTER,
            label,
            theme::detail(),
            theme::TEXT_3,
            false,
        );
        // `.tp-val { color: var(--text); font-weight: 500 }`, `.tp-val.dim { color: var(--text-2) }`.
        paint_text(
            painter,
            pos2(row.right(), row.center().y),
            Align2::RIGHT_CENTER,
            &value,
            theme::detail(),
            if dim { theme::TEXT_2 } else { theme::WHITE },
            !dim,
        );
    }
}

/// `.account`: avatar + name + email when signed in, otherwise the sign-in button.
fn account(ui: &mut egui::Ui, painter: &egui::Painter, l: &Layout, state: &mut UiState) {
    // `.account { border-top: 1px solid var(--border) }`
    painter.line_segment(
        [
            pos2(l.account_box.left(), l.account_box.top() + BORDER_W * 0.5),
            pos2(l.account_box.right(), l.account_box.top() + BORDER_W * 0.5),
        ],
        theme::stroke_border(),
    );

    let row = l.account;
    if state.logged_in {
        // `const u = st.user || { name: 'Signed in', email: '' }` in app.js.
        let (name, email) = match &state.user {
            Some(u) if !u.name.is_empty() => (u.name.clone(), u.email.clone()),
            Some(u) => ("Signed in".to_owned(), u.email.clone()),
            None => ("Signed in".to_owned(), String::new()),
        };

        // The whole row opens Settings, as `#account.onclick = () => showView('settings')`.
        let response = ui.interact(row, ui.id().with("sidebar-account"), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Account")
        });
        if response.clicked() {
            state.view = View::Settings;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        // `.avatar { 28px circle, background: var(--raised), 11px 600 }`. The
        // circle is placed on whole pixels, the way the browser snaps a round box,
        // so its edge lands on the same rows as the webview's.
        let avatar_y = (row.top() + (row.height() - AVATAR) * 0.5).round() + AVATAR * 0.5;
        let avatar = Rect::from_center_size(pos2(row.left() + AVATAR * 0.5, avatar_y), Vec2::splat(AVATAR));
        painter.circle_filled(avatar.center(), AVATAR * 0.5, theme::RAISED);
        paint_text(
            painter,
            avatar.center(),
            Align2::CENTER_CENTER,
            &initials(&name),
            theme::badge(),
            theme::WHITE,
            true,
        );

        // `.who { line-height: 1.25 }`: the name (13 px, 500) in the first line
        // box, the email (11.5 px, --text-3) in the second, each line centred in
        // its own box the way CSS puts a line box around its text.
        let text_x = avatar.right() + ACC_GAP;
        let max_w = (row.right() - text_x).max(0.0);
        let name = elide(painter, &name, &theme::body(), max_w);
        let email = elide(painter, &email, &theme::caption(), max_w);
        paint_text(
            painter,
            pos2(text_x, row.top() + WHO_LINE_NAME * 0.5),
            Align2::LEFT_CENTER,
            &name,
            theme::body(),
            theme::WHITE,
            true,
        );
        paint_text(
            painter,
            pos2(text_x, row.top() + WHO_LINE_NAME + WHO_LINE_EMAIL * 0.5),
            Align2::LEFT_CENTER,
            &email,
            theme::caption(),
            theme::TEXT_3,
            false,
        );
    } else {
        // `.account .btn { width: 100%; justify-content: center }`
        let response = ui.interact(row, ui.id().with("sidebar-signin"), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, SIGN_IN_LABEL)
        });
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if response.clicked() {
            // `$$('#signin-btn, #cta-signin, #side-signin').onclick = login()`: the
            // app runs the Frame.io OAuth flow and the shell says where it stands.
            state.ask(Request::Login);
        }

        painter.rect_filled(row, theme::R_S, theme::CARD);
        painter.rect_stroke(
            row,
            theme::R_S,
            theme::stroke_border_2(),
            egui::StrokeKind::Inside,
        );
        paint_text(
            painter,
            row.center(),
            Align2::CENTER_CENTER,
            SIGN_IN_LABEL,
            theme::body(),
            if response.hovered() { theme::WHITE } else { theme::TEXT_BODY },
            true,
        );
    }
}

// ---------- state helpers ----------

/// The badge number for a nav item, or `None` where the webview shows none.
fn nav_badge(state: &UiState, view: View) -> Option<i64> {
    match view {
        View::Transfers => Some(state.active_jobs() as i64),
        View::Watch => Some(state.watches.iter().filter(|w| w.enabled).count() as i64),
        // Space, History and Settings carry no count in the design.
        View::Space | View::History | View::Settings => None,
    }
}

/// `const initials = (name) => ...` from app.js: the first letter of up to two words.
fn initials(name: &str) -> String {
    let mut out = String::new();
    for word in name.split_whitespace().take(2) {
        if let Some(c) = word.chars().next() {
            out.extend(c.to_uppercase());
        }
    }
    if out.is_empty() {
        "?".to_owned()
    } else {
        out
    }
}

// ---------- drawing helpers ----------

/// `.brand-name { font-size: 14px }` — the only 14 px text in the sidebar.
fn brand_font() -> FontId {
    FontId::new(theme::font::BRAND, egui::FontFamily::Proportional)
}

/// `painter.text`, plus the fake bold the default egui fonts need for the
/// stylesheet's 500/600 weights (there is only one weight in the font set, so
/// the glyphs are stamped twice a half pixel apart).
fn paint_text(
    painter: &egui::Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    font_id: FontId,
    color: Color32,
    bold: bool,
) -> Rect {
    let rect = painter.text(pos, anchor, text, font_id.clone(), color);
    if bold && !text.is_empty() {
        painter.text(
            pos + vec2(0.45, 0.0),
            anchor,
            text,
            font_id,
            color,
        );
    }
    rect
}

/// One line, shortened with an ellipsis until it fits `max_w`.
fn elide(painter: &egui::Painter, text: &str, font_id: &FontId, max_w: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let width = |s: &str| {
        painter
            .layout_no_wrap(s.to_owned(), font_id.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        let mut candidate: String = chars[..mid].iter().collect();
        candidate.push('…');
        if width(&candidate) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    if lo < chars.len() {
        out.push('…');
    }
    out
}

/// The five nav icons and the brand glyph, as the SVG paths in `index.html`
/// (`viewBox="0 0 20 20"`, drawn in a rect of the icon's size).
#[derive(Clone, Copy)]
enum Icon {
    Arrows,
    Eye,
    Clock,
    /// The Space donut (`#view-space`'s nav item).
    Pie,
    Sliders,
}

fn icon_for(view: View) -> Icon {
    match view {
        View::Transfers => Icon::Arrows,
        View::Space => Icon::Pie,
        View::Watch => Icon::Eye,
        View::History => Icon::Clock,
        View::Settings => Icon::Sliders,
    }
}

fn draw_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32, stroke_width: f32) {
    let scale = rect.width().min(rect.height()) / 20.0;
    if scale <= 0.0 {
        return;
    }
    let p = |x: f32, y: f32| pos2(rect.left() + x * scale, rect.top() + y * scale);
    let stroke = Stroke::new(stroke_width * scale, color);
    match icon {
        // "M6.5 15V5m0 0L3.8 7.7M6.5 5l2.7 2.7M13.5 5v10m0 0l-2.7-2.7m2.7 2.7l2.7-2.7"
        Icon::Arrows => {
            polyline(painter, &[p(6.5, 15.0), p(6.5, 5.0)], stroke);
            polyline(painter, &[p(6.5, 5.0), p(3.8, 7.7)], stroke);
            polyline(painter, &[p(6.5, 5.0), p(9.2, 7.7)], stroke);
            polyline(painter, &[p(13.5, 5.0), p(13.5, 15.0)], stroke);
            polyline(painter, &[p(13.5, 15.0), p(10.8, 12.3)], stroke);
            polyline(painter, &[p(13.5, 15.0), p(16.2, 12.3)], stroke);
        }
        // The Space mark, from `index.html`: `<circle r=6.6>`, `<circle r=2.4>`
        // and two ticks, `M10 3.4v4.2M16.6 10h-4.2` — a donut seen end on.
        Icon::Pie => {
            painter.circle_stroke(p(10.0, 10.0), 6.6 * scale, stroke);
            painter.circle_stroke(p(10.0, 10.0), 2.4 * scale, stroke);
            polyline(painter, &[p(10.0, 3.4), p(10.0, 7.6)], stroke);
            polyline(painter, &[p(16.6, 10.0), p(12.4, 10.0)], stroke);
        }
        // "M2.5 10s2.7-5 7.5-5 7.5 5 7.5 5-2.7 5-7.5 5-7.5-5-7.5-5z" + circle 2.2
        Icon::Eye => {
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [p(2.5, 10.0), p(5.2, 5.0), p(7.3, 5.0), p(10.0, 5.0)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [p(10.0, 5.0), p(12.7, 5.0), p(14.8, 5.0), p(17.5, 10.0)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [p(17.5, 10.0), p(14.8, 15.0), p(12.7, 15.0), p(10.0, 15.0)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                [p(10.0, 15.0), p(7.3, 15.0), p(5.2, 15.0), p(2.5, 10.0)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
            painter.circle_stroke(p(10.0, 10.0), 2.2 * scale, stroke);
        }
        // "M3.5 10a6.5 6.5 0 1 0 1.9-4.6M3.5 4v3.4h3.4M10 6.5V10l2.5 1.6"
        Icon::Clock => {
            // The SVG arc: the large arc from the 9-o'clock point, round the
            // bottom, the right and the top, back to 10-o'clock.
            const STEPS: usize = 44;
            let mut arc = Vec::with_capacity(STEPS + 1);
            for i in 0..=STEPS {
                let t = i as f32 / STEPS as f32;
                let angle = (180.0 + 315.0 * t).to_radians();
                arc.push(p(
                    10.0 + 6.5 * angle.cos(),
                    10.0 - 6.5 * angle.sin(),
                ));
            }
            polyline(painter, &arc, stroke);
            polyline(painter, &[p(3.5, 4.0), p(3.5, 7.4), p(6.9, 7.4)], stroke);
            polyline(painter, &[p(10.0, 6.5), p(10.0, 10.0), p(12.5, 11.6)], stroke);
        }
        // "M4 6h7m4 0h1M4 14h1m4 0h7" + two 2 px circles
        Icon::Sliders => {
            polyline(painter, &[p(4.0, 6.0), p(11.0, 6.0)], stroke);
            polyline(painter, &[p(15.0, 6.0), p(16.0, 6.0)], stroke);
            polyline(painter, &[p(4.0, 14.0), p(5.0, 14.0)], stroke);
            polyline(painter, &[p(9.0, 14.0), p(16.0, 14.0)], stroke);
            painter.circle_stroke(p(13.0, 6.0), 2.0 * scale, stroke);
            painter.circle_stroke(p(7.0, 14.0), 2.0 * scale, stroke);
        }
    }
}

/// One stroked path, with the round caps and joins the SVGs ask for
/// (`stroke-linecap="round"`, `stroke-linejoin="round"`): egui leaves open
/// paths uncapped, so each vertex also gets a dot of half the stroke width.
fn polyline(painter: &egui::Painter, points: &[Pos2], stroke: Stroke) {
    if points.len() < 2 {
        return;
    }
    painter.add(egui::Shape::line(points.to_vec(), stroke));
    if stroke.width > 0.0 {
        for point in points {
            painter.circle_filled(*point, stroke.width * 0.5, stroke.color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{JobView, Snapshot, User, WatchRow};
    use fiotransfer_core::xfer::Status;
    use egui_kittest::{Harness, kittest::Queryable as _};

    const PANEL_H: f32 = 780.0;

    fn job(
        kind: &str,
        title: &str,
        status: Status,
        total: i64,
        done: i64,
        speed: f64,
    ) -> JobView {
        JobView {
            id: title.to_owned(),
            kind: kind.to_owned(),
            title: title.to_owned(),
            status,
            total,
            done,
            speed,
            files_total: 4,
            files_done: 2,
            ..Default::default()
        }
    }

    /// A signed-in account with two running transfers and one finished.
    fn signed_in_state() -> UiState {
        UiState {
            view: View::Transfers,
            logged_in: true,
            user: Some(User {
                id: "u-dana".into(),
                name: "Dana Okonkwo".into(),
                email: "dana@fiorecut.tv".into(),
            }),
            snapshot: Snapshot {
                jobs: vec![
                    job("upload", "A001_C012_0714XY.mov", Status::Running, 412_000_000, 168_000_000, 24_500_000.0),
                    job("download", "Dailies 2026-10-24", Status::Running, 2_100_000_000, 640_000_000, 8_200_000.0),
                    job("upload", "B-roll select.mov", Status::Done, 90_000_000, 90_000_000, 0.0),
                ],
                speed: 32_700_000.0,
                active_streams: 6,
                stream_limit: 16,
            },
            watches: vec![
                WatchRow {
                    id: "w-incoming".into(),
                    local: "~/Movies/Incoming".into(),
                    label: "Incoming".into(),
                    direction: "upload".into(),
                    enabled: true,
                    pending: 0,
                    from_portal: false,
                    duplex_id: None,
                    lane: crate::model::Lane::None,
                    account_id: "acct-fiorecut".into(),
                    folder_id: "fld-incoming".into(),
                    on_change: "overwrite".into(),
                },
                WatchRow {
                    id: "w-archive".into(),
                    local: "~/Movies/Archive".into(),
                    label: "Archive".into(),
                    direction: "download".into(),
                    enabled: false,
                    pending: 3,
                    from_portal: false,
                    duplex_id: None,
                    lane: crate::model::Lane::None,
                    account_id: "acct-fiorecut".into(),
                    folder_id: "fld-archive".into(),
                    on_change: "overwrite".into(),
                },
            ],
            ..Default::default()
        }
    }

    /// The face the app ships: `assets/InterVariable.ttf`, installed by
    /// `app::apply_style_once` in a running app. The snapshot should show it.
    fn install_inter(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "inter".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../assets/InterVariable.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "inter".to_owned());
        ctx.set_fonts(fonts);
    }

    fn harness(state: UiState) -> Harness<'static, UiState> {
        Harness::builder()
            .with_size([WIDTH, PANEL_H])
            .build_state(
                |ctx, state: &mut UiState| {
                    install_inter(ctx);
                    egui::SidePanel::left("sidebar")
                        .exact_width(WIDTH)
                        .resizable(false)
                        // the stylesheet's 1 px right border is drawn by `show` itself
                        .show_separator_line(false)
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| show(ui, state));
                },
                state,
            )
    }

    #[test]
    fn the_blocks_sit_where_the_stylesheet_puts_them() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, PANEL_H));
        let l = layout(rect);

        // `.traffic { height: 40px }`, full content width (12 px padding a side, 1 px border).
        assert_eq!(l.traffic.top(), 0.0);
        assert_eq!(l.traffic.height(), 40.0);
        assert_eq!(l.traffic.left(), 12.0);
        assert_eq!(l.traffic.right(), WIDTH - 13.0);

        // `.brand { padding: 4px 8px 18px }` around a 26 px mark.
        assert_eq!(l.mark.top(), 44.0);
        assert_eq!(l.mark.left(), 20.0);
        assert_eq!(l.mark.size(), Vec2::splat(26.0));
        assert_eq!(l.brand.height(), 4.0 + 26.0 + 18.0);

        // Four 32 px items, 2 px apart, starting under the brand.
        assert_eq!(l.nav_items[0].top(), l.brand.bottom());
        assert_eq!(l.nav_items[0].height(), 32.0);
        for pair in l.nav_items.windows(2) {
            assert_eq!(pair[1].top() - pair[0].top(), 32.0 + 2.0);
        }
        assert_eq!(l.nav.bottom(), l.nav_items[4].bottom());
        assert_eq!(l.nav_items.len(), View::ALL.len(), "one item per screen");

        // The account row is the last 32 px above the 12 px bottom padding.
        assert_eq!(l.account.bottom(), PANEL_H - PAD_BOTTOM);
        assert_eq!(l.account.height(), BTN_H);
        // ... and the throughput block sits directly above its top border.
        assert_eq!(l.throughput.bottom(), l.account_box.top());
        assert_eq!(l.tp_rows[0].top(), l.throughput.top() + BORDER_W + TP_PAD_TOP);

        // What is left over is the flexible gap.
        assert!(l.gap.height() > 100.0);
        assert_eq!(l.gap.top(), l.nav.bottom());
        assert_eq!(l.gap.bottom(), l.throughput.top());
    }

    #[test]
    fn the_signed_in_footer_is_the_who_line_boxes() {
        // `.account` is sized by its content: the sign-in button is 32 px, but a
        // signed-in row is the two `.who` line boxes (13 x 1.25 + 11.5 x 1.25).
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, PANEL_H));
        let signed_out = layout(rect);
        let signed_in = layout_with(rect, WHO_H);
        assert_eq!(signed_out.account.height(), BTN_H);
        assert!((WHO_H - 30.625).abs() < 1e-4, "WHO_H = {WHO_H}");
        assert!((signed_in.account.height() - WHO_H).abs() < 1e-4);

        // The footer is bottom-anchored, so the shorter row moves the blocks above
        // it *down* by exactly the difference. These are the webview's rows for a
        // 224 x 780 panel: the throughput rule at 639, the account rule at 724.
        assert_eq!(signed_in.account.bottom(), PANEL_H - PAD_BOTTOM);
        assert_eq!(signed_in.account_box.top().round(), 724.0);
        assert_eq!(signed_in.throughput.top().round(), 639.0);
        assert!(
            (signed_in.account_box.top() - signed_out.account_box.top() - (BTN_H - WHO_H)).abs()
                < 1e-3
        );
    }

    #[test]
    fn a_wider_panel_only_widens_the_blocks() {
        // The design's grid column is 224 px, but the layout follows the rect
        // it is given: the same rows, wider.
        let narrow = layout(Rect::from_min_size(Pos2::ZERO, vec2(224.0, PANEL_H)));
        let wide = layout(Rect::from_min_size(Pos2::ZERO, vec2(240.0, PANEL_H)));
        assert_eq!(narrow.nav_items[0].top(), wide.nav_items[0].top());
        assert_eq!(narrow.traffic.height(), wide.traffic.height());
        assert_eq!(narrow.account.height(), wide.account.height());
        assert_eq!(wide.traffic.width() - narrow.traffic.width(), 16.0);
        assert_eq!(wide.nav_items[0].width() - narrow.nav_items[0].width(), 16.0);
        assert_eq!(wide.gap.height(), narrow.gap.height());
    }

    #[test]
    fn a_short_sidebar_keeps_the_blocks_in_order() {
        // 560 px is the window's minimum height; 200 px is nonsense we still survive.
        for height in [560.0_f32, 200.0] {
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, height));
            let l = layout(rect);
            assert!(l.gap.height() >= 0.0);
            assert!(l.nav.bottom() <= l.throughput.top(), "height {height}");
            assert!(l.throughput.bottom() <= l.account.top(), "height {height}");
        }
    }

    #[test]
    fn initials_come_from_the_first_two_words() {
        assert_eq!(initials("Dana Okonkwo"), "DO");
        assert_eq!(initials("dana"), "D");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials(""), "?");
        assert_eq!(initials("a b c"), "AB");
        assert_eq!(initials("éclair au chocolat"), "ÉA");
    }

    #[test]
    fn a_long_name_is_elided_not_drawn_over_the_edge() {
        let ctx = egui::Context::default();
        let mut painter = None;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                painter = Some(ui.painter().clone());
            });
        });
        let painter = painter.expect("a painter");
        let labelled = "…".to_owned();
        assert_eq!(
            elide(&painter, "short", &theme::body(), 200.0),
            "short",
            "a short label is untouched"
        );
        let long = elide(
            &painter,
            "a very long folder name that cannot possibly fit in a 120 px column",
            &theme::body(),
            120.0,
        );
        assert!(long.ends_with('…'));
        assert!(painter
            .layout_no_wrap(long.clone(), theme::body(), Color32::WHITE)
            .size()
            .x
            <= 120.0);
        assert_ne!(long, labelled);
        assert_eq!(elide(&painter, "anything", &theme::body(), 0.0), "");
    }

    #[test]
    fn the_badges_count_what_the_webview_counts() {
        let state = signed_in_state();
        // Three jobs, two of them still running.
        assert_eq!(nav_badge(&state, View::Transfers), Some(2));
        // Two watches, one enabled (`list.filter((w) => w.enabled).length`).
        assert_eq!(nav_badge(&state, View::Watch), Some(1));
        assert_eq!(nav_badge(&state, View::History), None);
        // Space carries no count: it is a measurement, not a queue.
        assert_eq!(nav_badge(&state, View::Space), None);
        assert_eq!(nav_badge(&UiState::default(), View::Transfers), Some(0));
        assert_eq!(nav_badge(&UiState::default(), View::Watch), Some(0));
    }

    #[test]
    fn the_sidebar_matches_the_webview() {
        let mut harness = harness(signed_in_state());
        harness.snapshot("sidebar_224x780_signed_in");
    }

    #[test]
    fn the_sidebar_works_signed_out_and_with_no_jobs() {
        let mut harness = harness(UiState::default());
        // Nothing to count, so neither badge may show.
        harness.get_by_label("Transfers");
        harness.get_by_label("Sign in to Frame.io");
        harness.snapshot("sidebar_224x780_signed_out");
    }

    #[test]
    fn clicking_a_nav_item_changes_the_view() {
        let mut harness = harness(signed_in_state());
        assert_eq!(harness.state().view, View::Transfers);

        harness.get_by_label("History").click();
        harness.run();
        assert_eq!(harness.state().view, View::History);

        harness.get_by_label("Watch Folders").click();
        harness.run();
        assert_eq!(harness.state().view, View::Watch);

        harness.get_by_label("Space").click();
        harness.run();
        assert_eq!(harness.state().view, View::Space);

        harness.get_by_label("Settings").click();
        harness.run();
        assert_eq!(harness.state().view, View::Settings);

        // And the active item is the one that reports itself as selected.
        harness.get_by_label("Transfers").click();
        harness.run();
        assert_eq!(harness.state().view, View::Transfers);
    }

    #[test]
    fn clicking_the_account_row_opens_settings() {
        let mut harness = harness(signed_in_state());
        harness.get_by_label("Account").click();
        harness.run();
        assert_eq!(harness.state().view, View::Settings);
    }

    #[test]
    fn the_sign_in_button_asks_the_app_to_sign_in() {
        let mut harness = harness(UiState::default());
        harness.get_by_label("Sign in to Frame.io").click();
        harness.run();
        // `login()` is one request to the shell, and no toast of its own: the app
        // says where the sign-in stands.
        assert_eq!(harness.state().requests, vec![Request::Login]);
        assert!(harness.state().toasts.is_empty());
    }
}
