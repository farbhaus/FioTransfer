//! The window: the sidebar, the active screen, the toasts, and the app behind
//! them.
//!
//! [`FioApp`] is the whole frame. With a [`Shell`] installed (`with_shell`) it
//! pumps the real `fiotransfer_core::App` before it draws — requests out, state
//! in — and hands the shell the two verdicts egui owns: whether the window may
//! close, and the window's own repaint schedule. Without one (`new`, which is
//! what the snapshot tests and `--fixture` build) it is a renderer over a
//! `UiState` and nothing else: no runtime, no engine, no I/O.
//!
//! Everything here is furniture *around* a screen, mirroring the webview's
//! `index.html` / `style.css`:
//!
//!   * the window background (`--base`) and the 224 px left panel that draws
//!     [`crate::views::sidebar::show`] — the sidebar paints its own `--panel`
//!     fill, its 1 px right border and its 12 px insets, so the panel hands it
//!     the column and nothing else;
//!   * the central panel that scrolls (`.view { overflow: auto }`, scrollbar at
//!     the window's edge) and draws the active screen
//!     ([`crate::views::transfers::show`] and friends), with the `.view` padding
//!     of 28 px either side and 32 px below;
//!   * the toast stack, bottom right, 20 px in from the corner: rounded cards in
//!     `--card` with a 7 px dot in the kind's accent.
//!
//! Colours, radii, type sizes and frames all come from [`crate::theme`], so this
//! shell and the five screens cannot drift apart.
//!
//! The page head (`h1` plus the screen's own buttons) belongs to the screen, the
//! same as `index.html` puts `<header class="page-head">` inside each
//! `<section class="view">`: a screen draws it as its first widget. [`page_head`]
//! is here so all four draw the same one.

use egui::{
    Align2, Frame, Id, Label, Layout, Order, Rect, RichText, Sense, Vec2, pos2, vec2,
};

use crate::model::{Toast, ToastKind, UiState, View};
use crate::theme;

// ---------------------------------------------------------------------------
// The shell's geometry
//
// Every number below is the stylesheet's, through `theme` — the sidebar width
// included: `.app { grid-template-columns: 224px 1fr }`. An earlier brief said
// 240 px; the stylesheet, `theme::size::SIDEBAR_W` and `views::sidebar::WIDTH`
// all say 224, and the shell follows them so the panel and the sidebar agree.
// ---------------------------------------------------------------------------

/// `.app { grid-template-columns: 224px 1fr }` — the left panel's width.
pub const SIDEBAR_WIDTH: f32 = theme::size::SIDEBAR_W;
/// `.page-head { height: 64px }`.
pub const PAGE_HEAD_HEIGHT: f32 = theme::size::HEAD_H;
/// `.view { padding: 0 28px 32px }`.
pub const VIEW_PADDING_X: f32 = theme::space::PAGE_X;
pub const VIEW_PADDING_BOTTOM: f32 = theme::space::PAGE_BOTTOM;
/// `.toasts { right: 20px; bottom: 20px; gap: 8px; max-width: 400px }`.
pub const TOAST_INSET: f32 = theme::space::HUGE;
pub const TOAST_GAP: f32 = theme::space::M;
pub const TOAST_MAX_WIDTH: f32 = theme::size::TOAST_MAX_W;
/// `.toast::before { width: 7px; height: 7px; margin-top: 6px }`.
pub const TOAST_DOT: f32 = theme::size::DOT;

/// Whether the window's content runs up under a transparent title bar, the
/// traffic lights over the sidebar, as the webview's `TitleBarHiddenInset`
/// window did: macOS only. Other platforms keep their own title bar.
pub const TITLE_BAR_OVER_CONTENT: bool = cfg!(target_os = "macos");

/// `--wails-draggable: drag`. With the title bar over the content, the strips
/// the webview made draggable — the sidebar's top and brand, every page head —
/// move the window: a drag moves it, a double-click zooms it. Called before the
/// strip's own widgets, so a button on it keeps its click.
pub fn window_drag(ui: &egui::Ui, rect: Rect, id: Id) {
    if !TITLE_BAR_OVER_CONTENT {
        return;
    }
    let response = ui.interact(rect, id, Sense::click_and_drag());
    if response.double_clicked() {
        let zoomed = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!zoomed));
    } else if response.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}

/// The window, with the state every screen draws from.
pub struct FioApp {
    pub state: UiState,
    /// The app behind the window: the runtime, the engine and the channel the
    /// two talk over (`crate::shell`). `None` when there is no backend at all —
    /// the snapshot tests, `--fixture`, and the browser capture.
    #[cfg(not(target_arch = "wasm32"))]
    pub shell: Option<crate::shell::Shell>,
}

impl FioApp {
    /// A window with no backend: what the snapshot tests, `--fixture` and the
    /// browser capture build.
    pub fn new(state: UiState) -> Self {
        FioApp {
            state,
            #[cfg(not(target_arch = "wasm32"))]
            shell: None,
        }
    }

    /// A window over the real app: `shell` answers every request the screens push
    /// and keeps `state` current.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_shell(state: UiState, shell: crate::shell::Shell) -> Self {
        FioApp {
            state,
            shell: Some(shell),
        }
    }

    /// Draw one frame: sidebar, the active screen, then the toasts on top.
    ///
    /// `eframe::App::update` calls this; tests call it from an `egui_kittest`
    /// harness, which is why it takes a plain `&egui::Context` and no frame.
    pub fn draw(&mut self, ctx: &egui::Context) {
        apply_style_once(ctx);
        // The shell goes first: requests out, answers in, the app read on its
        // tick — all before a single pixel of this frame.
        self.drive_shell(ctx);
        self.tick_toasts(ctx);
        self.sidebar(ctx);
        self.screen(ctx);
        // Over whichever screen opened it (Transfers, or the watch editor).
        crate::views::dest::show(ctx, &mut self.state);
        draw_toasts(ctx, &self.state.toasts);
    }

    /// Pump the app, then act on what it asked egui for.
    #[cfg(not(target_arch = "wasm32"))]
    fn drive_shell(&mut self, ctx: &egui::Context) {
        let Some(shell) = self.shell.as_mut() else { return };
        shell.attach(ctx);
        shell.pump(&mut self.state);
        if shell.take_focus() {
            // `Host::show_window`: the menu bar, a Finder service or a sign-in
            // brought the app back to the front, hidden or not.
            fiotransfer_sys::tray::activate();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if shell.take_close() {
            // `Host::quit`, or the question answered with Quit.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // egui only draws when something asks it to; the shell's tick is what
        // moves a transfer's progress, so it sets the pace.
        ctx.request_repaint_after(shell.repaint_in());
    }

    /// No backend, no shell to pump: the browser capture and `--fixture` draw the
    /// state they were given.
    #[cfg(target_arch = "wasm32")]
    fn drive_shell(&mut self, _ctx: &egui::Context) {}

    /// A close was requested. On macOS the shell hides the app instead, unless a
    /// quit was agreed; elsewhere, with transfers running, it asks first
    /// (`Host::question`) and cancels this close. Otherwise the app's
    /// `shutdown()` runs here, before the window goes.
    #[cfg(not(target_arch = "wasm32"))]
    fn on_close_requested(&mut self, ctx: &egui::Context) {
        let Some(shell) = self.shell.as_mut() else { return };
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if shell.on_close_request() {
            shell.shutdown();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn on_close_requested(&mut self, _ctx: &egui::Context) {}

    /// The left panel: exactly [`SIDEBAR_WIDTH`] wide, no frame of its own.
    ///
    /// `views::sidebar::show` paints the `--panel` fill, the 1 px right border
    /// and its own 12 px insets over the rect it is given, so a frame with a
    /// margin or a fill here would inset that paint and double the padding. The
    /// panel hands the sidebar the column and nothing else.
    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("fio-sidebar")
            .exact_width(SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE)
            .show(ctx, |ui| {
                ui.set_width(ui.available_width());
                crate::views::sidebar::show(ui, &mut self.state);
            });
    }

    /// The central panel: `--base`, the `.view` padding inside the scroll
    /// container, and whatever screen the sidebar selected.
    fn screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BASE))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                    .show(ui, |ui| {
                        // `.view` padding lives inside the scroll container, so
                        // the scrollbar sits at the window's edge, not 28 px in.
                        theme::main_frame().show(ui, |ui| {
                            // A screen may be as wide as the panel, never wider.
                            ui.set_width(ui.available_width());
                            show_screen(ui, &mut self.state);
                        });
                    });
            });
    }

    /// Age the toasts out (`Toast::seconds` is the time left). A short repaint
    /// keeps them ticking while any are showing. Frame time is clamped so a
    /// window that was asleep does not skip the whole stack at once.
    fn tick_toasts(&mut self, ctx: &egui::Context) {
        if self.state.toasts.is_empty() {
            return;
        }
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.25);
        for toast in &mut self.state.toasts {
            toast.seconds -= dt;
        }
        self.state.toasts.retain(|toast| toast.seconds > 0.0);
        if !self.state.toasts.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

impl eframe::App for FioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.on_close_requested(ctx);
        self.draw(ctx);
    }

    /// `body { background: var(--base) }`: the clear colour is the window's, so
    /// a resize or a slow frame never shows grey behind the panels.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BASE.to_normalized_gamma_f32()
    }
}

/// Draw whichever screen `state.view` names.
///
/// The sidebar sets `state.view` with its own clicks; the shell reads it back in
/// the same frame, so a click changes the screen without an extra round trip.
pub fn show_screen(ui: &mut egui::Ui, state: &mut UiState) {
    match state.view {
        View::Transfers => crate::views::transfers::show(ui, state),
        View::Space => crate::views::space::show(ui, state),
        View::Watch => crate::views::watch::show(ui, state),
        View::History => crate::views::history::show(ui, state),
        View::Settings => crate::views::settings::show(ui, state),
    }
}

/// The page head every screen opens with (`header.page-head`): a 64 px strip
/// with the screen's `h1` on the left and its buttons on the right. `actions`
/// draws into the head-actions slot; pass `|_| {}` for a screen with none.
///
/// Returns the strip's rect, which is how the tests hold it to 64 px.
pub fn page_head(ui: &mut egui::Ui, title: &str, actions: impl FnOnce(&mut egui::Ui)) -> Rect {
    // `.page-head { --wails-draggable: drag }`; its buttons come after, on top.
    let strip = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), PAGE_HEAD_HEIGHT));
    window_drag(ui, strip, Id::new("fio-page-head-drag"));
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), PAGE_HEAD_HEIGHT),
        Layout::left_to_right(egui::Align::Center),
        |ui| {
            // Not selectable: a drag on the title moves the window.
            ui.add(
                Label::new(
                    RichText::new(title)
                        .font(theme::title())
                        .strong()
                        .color(theme::TEXT),
                )
                .selectable(false),
            );
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = theme::space::M;
                actions(ui);
            });
        },
    )
    .response
    .rect
}

/// The webview's face: `@font-face { font-family: "Inter"; src: url("fonts/
/// InterVariable.woff2") }` in `style.css`. epaint reads neither woff2 nor a
/// variable font's full axis range, so the stylesheet's file is decompressed to
/// this TTF once and shipped in `assets/`. Sizes and weights are the theme's.
const INTER_TTF: &[u8] = include_bytes!("../assets/InterVariable.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../assets/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/Inter-SemiBold.ttf");

/// Apply [`crate::theme`] and the Inter face to a context, once per context.
///
/// `eframe` gets this from `main` before the first frame; the test harness calls
/// [`FioApp::draw`], which calls this, so a snapshot never depends on a caller
/// remembering to install the theme or the font.
pub fn apply_style_once(ctx: &egui::Context) {
    let applied = Id::new("fiotransfer-egui-style-applied");
    if ctx
        .data(|data| data.get_temp::<bool>(applied))
        .unwrap_or(false)
    {
        return;
    }
    install_fonts(ctx);
    theme::install(ctx);
    ctx.data_mut(|data| data.insert_temp(applied, true));
}

/// Inter as egui's proportional face, with the default faces kept behind it as
/// fallbacks, and behind the monospace face for glyphs Inter lacks (`code`).
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(INTER_TTF)),
    );
    // The stylesheet asks for 500 and 600 in places; epaint draws one weight per
    // face, so the two static instances are registered as families of their own
    // with the regular face behind them as a glyph fallback.
    fonts.font_data.insert(
        "inter-medium".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(INTER_MEDIUM)),
    );
    fonts.font_data.insert(
        "inter-semibold".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(INTER_SEMIBOLD)),
    );
    for (family, face) in [("medium", "inter-medium"), ("semibold", "inter-semibold")] {
        let list = fonts.families.entry(egui::FontFamily::Name(family.into())).or_default();
        list.insert(0, face.to_owned());
        list.push("inter".to_owned());
        list.push("NotoEmoji-Regular".to_owned());
        list.push("emoji-icon-font".to_owned());
    }
    // The variable face has no "→" (U+2192), and nor do egui's defaults, so it
    // drew as an empty box in a download's "link → folder" line. The medium
    // instance has it: the arrow comes from Inter rather than from no font.
    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".to_owned());
    proportional.insert(1, "inter-medium".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("inter".to_owned());
    ctx.set_fonts(fonts);
}

/// Where the toast stack may be: bottom right, [`TOAST_INSET`] in from the
/// window's corner, at most [`TOAST_MAX_WIDTH`] wide and never wider than the
/// window leaves room for. Pure maths, so it can be checked without a GPU.
pub fn toast_stack_rect(window: Rect) -> Rect {
    let width = toast_max_width(window.width());
    let height = (window.height() - 2.0 * TOAST_INSET).max(0.0);
    let right = window.right() - TOAST_INSET;
    let bottom = window.bottom() - TOAST_INSET;
    Rect::from_min_max(pos2(right - width, bottom - height), pos2(right, bottom))
}

/// The widest a toast card may be in a window this wide.
pub fn toast_max_width(window_width: f32) -> f32 {
    (window_width - 2.0 * TOAST_INSET).clamp(0.0, TOAST_MAX_WIDTH)
}

/// The accent dot's colour for a toast kind (`.toast.ok` `--success`,
/// `.toast.err` `--error`, otherwise `--info`).
pub fn toast_accent(kind: ToastKind) -> egui::Color32 {
    theme::toast_color(kind)
}

/// The toast stack: newest nearest the corner, older ones pushing up, 8 px apart.
fn draw_toasts(ctx: &egui::Context, toasts: &[Toast]) {
    if toasts.is_empty() {
        return;
    }
    let max_width = toast_max_width(ctx.content_rect().width());
    egui::Area::new(Id::new("fio-toasts"))
        .anchor(Align2::RIGHT_BOTTOM, vec2(-TOAST_INSET, -TOAST_INSET))
        .order(Order::Foreground)
        .interactable(true)
        .show(ctx, |ui| {
            ui.set_max_width(max_width);
            ui.spacing_mut().item_spacing = vec2(0.0, TOAST_GAP);
            for toast in toasts {
                toast_card(ui, toast, max_width);
            }
        });
}

/// One toast card: `--card`, 1 px `--border-2`, 8 px radius, 10/14 px padding,
/// `0 12px 32px #000a`, and a 7 px dot in the kind's accent.
fn toast_card(ui: &mut egui::Ui, toast: &Toast, max_width: f32) {
    theme::toast_frame().show(ui, |ui| {
        ui.set_max_width((max_width - 28.0).max(0.0));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = theme::space::L;
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(TOAST_DOT), Sense::hover());
            // `.toast::before { margin-top: 6px }`: the dot sits on the first
            // line of text, not on the card's centre line.
            let centre = pos2(dot.left() + TOAST_DOT * 0.5, dot.top() + 6.0 + TOAST_DOT * 0.5);
            ui.painter()
                .circle_filled(centre, TOAST_DOT * 0.5, toast_accent(toast.kind));
            ui.add(
                Label::new(
                    RichText::new(&toast.text)
                        .font(theme::body())
                        .color(theme::toast_text()),
                )
                .wrap()
                .selectable(true),
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(crate::WINDOW_WIDTH, crate::WINDOW_HEIGHT))
    }

    #[test]
    fn the_toast_stack_sits_in_the_corner_of_the_window() {
        let stack = toast_stack_rect(window());
        assert_eq!(stack.right(), crate::WINDOW_WIDTH - TOAST_INSET);
        assert_eq!(stack.bottom(), crate::WINDOW_HEIGHT - TOAST_INSET);
        assert_eq!(stack.width(), TOAST_MAX_WIDTH);
        assert!(stack.left() >= 0.0 && stack.top() >= 0.0);
    }

    #[test]
    fn a_narrow_window_still_leaves_the_toasts_on_screen() {
        // 300 px wide: 20 px each side leaves 260, inside the 400 px cap.
        assert_eq!(toast_max_width(300.0), 260.0);
        // Narrower than the two insets: no width at all, never a negative rect.
        let stack = toast_stack_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(24.0, 24.0)));
        assert!(stack.width() >= 0.0 && stack.height() >= 0.0);
        assert!(stack.right() <= 24.0 && stack.top() >= -0.1);
        // Wide windows keep the 400 px cap.
        assert_eq!(toast_max_width(4000.0), TOAST_MAX_WIDTH);
    }

    #[test]
    fn a_toast_kind_keeps_the_stylesheet_accent() {
        assert_eq!(toast_accent(ToastKind::Ok), theme::SUCCESS);
        assert_eq!(toast_accent(ToastKind::Err), theme::ERROR);
        assert_eq!(toast_accent(ToastKind::Info), theme::INFO);
    }

    #[test]
    fn the_shell_geometry_is_the_stylesheets() {
        assert_eq!(PAGE_HEAD_HEIGHT, 64.0);
        assert_eq!(VIEW_PADDING_X, 28.0);
        assert_eq!(VIEW_PADDING_BOTTOM, 32.0);
        assert_eq!(TOAST_INSET, 20.0);
        assert_eq!(TOAST_GAP, 8.0);
        assert_eq!(TOAST_DOT, 7.0);
        // The stylesheet's column, not the 240 px an early brief asked for.
        assert_eq!(SIDEBAR_WIDTH, 224.0);
        assert_eq!(SIDEBAR_WIDTH, theme::size::SIDEBAR_W);
        assert_eq!(SIDEBAR_WIDTH, crate::views::sidebar::WIDTH);
    }
}
