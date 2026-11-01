//! The destination picker (`#dest-modal`): Frame.io a level at a time —
//! accounts, workspaces, projects, folders — with a search over the level and,
//! from two letters, over the whole account. Uploads choose their folder here,
//! and so does the watch editor.
//!
//! The view moves the picker's own state (the path, the query, the keyboard
//! row) and asks the shell for anything that needs Frame.io
//! ([`Request::DestLoad`], `DestSearch`, `DestGoTo`, `DestNewFolder`,
//! `DestChoose`); the answers land back in [`DestPicker`].

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    pos2, vec2, Align, Color32, CornerRadius, Id, Key, Label, Layout, Margin, Painter, Pos2, Rect,
    RichText, Sense, Shape, Stroke, TextEdit, Ui, WidgetInfo, WidgetType,
};

use crate::icons::{self, Icon};
use crate::model::{DestMode, DestNode, DestPicker, Request, UiState};
use crate::theme;

pub const TITLE_PICK: &str = "Frame.io folder";
pub const CHOOSE_UPLOAD: &str = "Upload here";
pub const CHOOSE_PICK: &str = "Choose";
pub const CANCEL: &str = "Cancel";
pub const CLOSE: &str = "Close";
pub const NEW_FOLDER: &str = "New folder";
pub const NEW_FOLDER_HINT: &str = "Name";
pub const SEARCH_HINT: &str = "Search";
pub const ALL_PROJECTS: &str = "All projects";
pub const ROOT: &str = "Frame.io";

const ID: &str = "fiotransfer-egui.dest";
/// `.dest-item { height: 36px }`
const ROW_H: f32 = 36.0;
/// `.modal-foot`: 12 px, a 32 px button, 12 px, and its top rule.
const FOOT_H: f32 = 57.0;
/// The account search waits this long after the last keystroke.
const SEARCH_PAUSE: f64 = 0.3;

/// What the user did this frame.
enum Do {
    Open(DestNode),
    Crumb(usize),
    NewFolder,
    Choose,
    Close,
}

/// Draws the picker when one is open, and acts on what was done in it.
pub fn show(ctx: &egui::Context, state: &mut UiState) {
    let Some(picker) = state.dest_picker.as_mut() else { return };
    let account = picker.search_account(&state.settings);
    let now = ctx.input(|i| i.time);
    let escape = ctx.input(|i| i.key_pressed(Key::Escape));
    let mut act: Option<Do> = None;

    let screen = ctx.content_rect();
    let width = (screen.width() * 0.92).min(theme::size::MODAL_W);
    let height = (screen.height() * 0.86).min(theme::size::MODAL_H);
    let title = title(picker);

    let modal = egui::Modal::new(Id::new(ID))
        .frame(theme::modal_frame())
        .backdrop_color(theme::SCRIM_MODAL)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let top = ui.cursor().top();
            head(ui, &title, &mut act);
            let typing = search_field(ui, picker, now);
            crumbs(ui, picker, &mut act);
            let list_h = (height - (ui.cursor().top() - top) - FOOT_H).max(ROW_H * 2.0);
            let (here, there) = picker.rows();
            keyboard(ui, picker, here.len() + there.len(), typing, &mut act, &here, &there);
            list(ui, picker, &here, &there, !account.is_empty(), list_h, &mut act);
            if !ui.is_sizing_pass() {
                picker.scroll_sel = false;
            }
            foot(ui, picker, &mut act);
        });

    if modal.should_close() {
        if escape && !picker.query.is_empty() {
            picker.clear_query();
            picker.focus_query = true;
        } else if escape && picker.new_folder.is_some() {
            picker.new_folder = None;
        } else {
            act = Some(Do::Close);
        }
    }

    // The account search, once typing pauses.
    let wants_search = picker.query.trim().chars().count() >= 2 && !account.is_empty() && !picker.searched;
    if wants_search {
        let waited = now - picker.typed_at;
        if waited >= SEARCH_PAUSE {
            picker.searched = true;
            state.requests.push(Request::DestSearch);
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(SEARCH_PAUSE - waited));
        }
    }

    let Some(act) = act else { return };
    let Some(picker) = state.dest_picker.as_mut() else { return };
    match act {
        Do::Open(node) => {
            picker.clear_query();
            picker.focus_query = true;
            if node.remote {
                state.requests.push(Request::DestGoTo(node));
            } else {
                picker.stack.push(node);
                state.requests.push(Request::DestLoad { skip: true });
            }
        }
        Do::Crumb(depth) => {
            picker.clear_query();
            picker.focus_query = true;
            if depth < picker.stack.len() {
                picker.go_up(depth);
            } else {
                // A dimmed crumb: back down the trail to it.
                let down = picker.stack.len() + picker.forward().len();
                picker.stack = picker.trail[..depth.min(down)].to_vec();
            }
            state.requests.push(Request::DestLoad { skip: false });
        }
        Do::NewFolder => {
            let name = picker.new_folder.as_deref().unwrap_or("").trim().to_string();
            if picker.new_folder.is_none() {
                picker.new_folder = Some(String::new());
                ctx.memory_mut(|m| m.request_focus(Id::new(ID).with("new-folder")));
            } else if !name.is_empty() {
                state.requests.push(Request::DestNewFolder(name));
            }
        }
        Do::Choose => match picker.mode {
            DestMode::Upload { .. } => state.requests.push(Request::DestChoose),
            DestMode::Watch => {
                let chosen = picker.stack.first().map(|a| a.id.clone()).zip(picker.top().map(|t| t.folder_id.clone()));
                let label = picker.label();
                if let (Some((account, folder)), Some(editor)) = (chosen, state.watch_editor.as_mut()) {
                    editor.account_id = account;
                    editor.folder_id = folder;
                    editor.label = label;
                }
                state.dest_picker = None;
            }
        },
        Do::Close => state.dest_picker = None,
    }
}

/// `Upload clip.mov to`, `Upload 3 items to`, `Upload to`, or the watch
/// editor's `Frame.io folder`.
pub fn title(picker: &DestPicker) -> String {
    match &picker.mode {
        DestMode::Watch => TITLE_PICK.to_string(),
        DestMode::Upload { pending } => match pending.len() {
            0 => "Upload to".to_string(),
            1 => format!("Upload {} to", base_name(&pending[0])),
            n => format!("Upload {n} items to"),
        },
    }
}

fn base_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// `.modal-head`: the title and the close cross.
fn head(ui: &mut Ui, title: &str, act: &mut Option<Do>) {
    let _ = egui::Frame::new()
        .inner_margin(Margin { left: 18, right: 12, top: 14, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Sides::new().shrink_left().truncate().show(
                ui,
                |ui| {
                    ui.add(theme::truncating(RichText::new(title).font(theme::section()).color(theme::TEXT).strong()));
                },
                |ui| {
                    if icon_button(ui, Glyph::Close, CLOSE) {
                        *act = Some(Do::Close);
                    }
                },
            );
        });
}

/// `.dest-search`: the search field with its magnifier. Answers whether it has
/// the keyboard.
fn search_field(ui: &mut Ui, picker: &mut DestPicker, now: f64) -> bool {
    let mut focused = false;
    let _ = egui::Frame::new()
        .inner_margin(Margin { left: 14, right: 14, top: 0, bottom: 10 })
        .show(ui, |ui| {
            let width = ui.available_width();
            let response = ui
                .scope(|ui| {
                    ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
                    ui.add(
                        TextEdit::singleline(&mut picker.query)
                            .id(Id::new(ID).with("query"))
                            .hint_text(SEARCH_HINT)
                            .margin(Margin { left: theme::size::INPUT_ICON_PAD as i8, right: 10, top: 8, bottom: 8 })
                            .desired_width(width),
                    )
                })
                .inner;
            let icon = Rect::from_center_size(pos2(response.rect.left() + 16.0, response.rect.center().y), vec2(16.0, 16.0));
            paint(ui.painter(), icon, Glyph::Search, theme::TEXT_3);
            if picker.focus_query {
                picker.focus_query = false;
                response.request_focus();
            }
            if response.changed() {
                picker.hits = None;
                picker.hits_error.clear();
                picker.searched = false;
                picker.sel = 0;
                picker.scroll_sel = true;
                picker.typed_at = now;
                picker.qseq += 1;
            }
            // Enter takes the focus off a single-line field; that frame counts.
            focused = response.has_focus() || response.lost_focus();
        });
    focused
}

/// `.crumbs`: Frame.io, then each level opened, then (dimmed) the rest of the
/// trail below it. A click goes up, or back down, to that level.
fn crumbs(ui: &mut Ui, picker: &DestPicker, act: &mut Option<Do>) {
    let frame = egui::Frame::new()
        .inner_margin(Margin { left: 14, right: 14, top: 0, bottom: 10 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                let names = std::iter::once(ROOT)
                    .chain(picker.stack.iter().map(|n| n.name.as_str()))
                    .chain(picker.forward().iter().map(|n| n.name.as_str()));
                let last = picker.stack.len();
                for (depth, name) in names.enumerate() {
                    if depth > 0 {
                        ui.add(Label::new(RichText::new("/").font(theme::note()).color(theme::TEXT_4)));
                    }
                    let current = depth == last;
                    let colour = match depth.cmp(&last) {
                        std::cmp::Ordering::Less => theme::TEXT_3,
                        std::cmp::Ordering::Equal => theme::TEXT,
                        std::cmp::Ordering::Greater => theme::TEXT_4,
                    };
                    let text = RichText::new(name).font(theme::note()).color(colour);
                    let text = if current { text.strong() } else { text };
                    let button = egui::Button::new(text)
                        .fill(Color32::TRANSPARENT)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(4))
                        .min_size(vec2(0.0, 22.0));
                    if ui.add(button).clicked() && !current {
                        *act = Some(Do::Crumb(depth));
                    }
                }
            });
        });
    ui.painter().hline(frame.response.rect.x_range(), frame.response.rect.bottom(), theme::hairline(theme::BORDER));
}

/// Up and down move the keyboard's row (once typing or the keyboard has picked
/// one), and Enter opens it.
fn keyboard(ui: &Ui, picker: &mut DestPicker, rows: usize, typing: bool, act: &mut Option<Do>, here: &[DestNode], there: &[DestNode]) {
    if picker.new_folder.is_some() && !typing {
        return; // the New folder field has the keys
    }
    let (down, up, enter) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter)));
    let live = !picker.query.is_empty() || picker.kbd;
    if (down || up) && rows > 0 {
        if live {
            picker.sel = if down { (picker.sel + 1).min(rows - 1) } else { picker.sel.saturating_sub(1) };
        }
        picker.kbd = true;
        picker.scroll_sel = true;
        picker.focus_query = true;
    }
    if enter && typing {
        picker.focus_query = true;
    }
    if enter && live && act.is_none() {
        let row = here.iter().chain(there.iter()).nth(picker.sel).cloned();
        if let Some(row) = row {
            *act = Some(Do::Open(row));
        }
    }
}

/// `.dest-list`: this level's rows, then (searching) the account's.
fn list(
    ui: &mut Ui,
    picker: &DestPicker,
    here: &[DestNode],
    there: &[DestNode],
    can_search: bool,
    height: f32,
    act: &mut Option<Do>,
) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Min), |ui| {
        ui.set_min_size(vec2(width, height));
        egui::ScrollArea::vertical().id_salt(Id::new(ID).with("list")).auto_shrink(false).max_height(height).show(ui, |ui| {
            let _ = egui::Frame::new().inner_margin(Margin::same(6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if !picker.error.is_empty() {
                    message(ui, Msg::Text(&picker.error, theme::ERROR_TEXT));
                    return;
                }
                if picker.items.is_none() {
                    message(ui, Msg::Spinner);
                    return;
                }
                let q = picker.query.trim();
                let mark = !q.is_empty() || picker.kbd;
                for (i, node) in here.iter().enumerate() {
                    if row(ui, node, q, mark && picker.sel == i, picker.scroll_sel) {
                        *act = Some(Do::Open(node.clone()));
                    }
                }
                let everywhere = q.chars().count() >= 2 && can_search;
                if q.is_empty() && here.is_empty() {
                    let empty = if picker.in_folder() { "No folders" } else { "Empty" };
                    message(ui, Msg::Text(empty, theme::TEXT_3));
                } else if everywhere && (!picker.hits_error.is_empty() || picker.hits.is_none() || !there.is_empty()) {
                    section(ui, ALL_PROJECTS);
                    if !picker.hits_error.is_empty() {
                        note(ui, Some(&picker.hits_error));
                    } else if picker.hits.is_none() {
                        note(ui, None);
                    } else {
                        for (k, node) in there.iter().enumerate() {
                            if row(ui, node, q, mark && picker.sel == here.len() + k, picker.scroll_sel) {
                                *act = Some(Do::Open(node.clone()));
                            }
                        }
                    }
                } else if here.is_empty() {
                    message(ui, Msg::Text("No matches", theme::TEXT_3));
                }
            });
        });
    });
}

/// `.dest-item`: the kind's glyph, the name (the search's match in bold), the
/// hit's project, a chevron; `scroll` brings it into view when selected.
/// Answers whether it was clicked.
fn row(ui: &mut Ui, node: &DestNode, query: &str, selected: bool, scroll: bool) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &node.name));
    let hovered = response.hovered();
    if selected || hovered {
        ui.painter().rect_filled(rect, theme::R_S, if selected { theme::ACTIVE } else { theme::HOVER });
    }
    if selected && scroll {
        ui.scroll_to_rect(rect, None);
    }
    let painter = ui.painter();
    let glyph = match node.kind.as_str() {
        "account" | "workspace" => Glyph::Team,
        "project" => Glyph::Project,
        _ => Glyph::Folder,
    };
    let icon = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(16.0, 16.0));
    paint(painter, icon, glyph, theme::TEXT_3);
    let chevron_x = rect.right() - 16.0;
    painter.text(pos2(chevron_x, rect.center().y), egui::Align2::CENTER_CENTER, "›", theme::body(), theme::TEXT_4);

    let mut right = chevron_x - 12.0;
    let left = icon.right() + 10.0;
    if !node.where_.is_empty() {
        let max = (right - left) * 0.45;
        let galley = painter.layout_job(one_line(&node.where_, theme::detail(), theme::TEXT_3, max));
        let pos = pos2(right - galley.size().x, rect.center().y - galley.size().y / 2.0);
        painter.galley(pos, galley.clone(), theme::TEXT_3);
        right = pos.x - 10.0;
    }
    let colour = if selected || hovered { theme::TEXT } else { theme::TEXT_BODY };
    // The 600 face when it's installed (a bare test context has only the defaults).
    let bold = if ui.fonts(|f| f.families().contains(&theme::semibold())) {
        theme::semibold()
    } else {
        egui::FontFamily::Proportional
    };
    let galley = painter.layout_job(highlighted(&node.name, query, colour, (right - left).max(0.0), bold));
    painter.galley(pos2(left, rect.center().y - galley.size().y / 2.0), galley, colour);
    response.clicked()
}

/// The name with the query's first match in bold white (`<mark>`), on one line
/// that ends in an ellipsis when it's too long.
pub fn highlighted(name: &str, query: &str, colour: Color32, width: f32, bold: egui::FontFamily) -> LayoutJob {
    let mut job = LayoutJob { wrap: wrap(width), ..Default::default() };
    let plain = TextFormat { font_id: theme::body(), color: colour, ..Default::default() };
    let marked = TextFormat {
        font_id: egui::FontId::new(theme::font::BODY, bold),
        color: theme::TEXT,
        ..Default::default()
    };
    let at = (!query.is_empty()).then(|| find_ignoring_case(name, query)).flatten();
    match at {
        Some((start, end)) => {
            job.append(&name[..start], 0.0, plain.clone());
            job.append(&name[start..end], 0.0, marked);
            job.append(&name[end..], 0.0, plain);
        }
        None => job.append(name, 0.0, plain),
    }
    job
}

/// Where `query` first appears in `name`, ignoring case, as byte offsets into
/// `name`.
fn find_ignoring_case(name: &str, query: &str) -> Option<(usize, usize)> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let starts: Vec<usize> = name.char_indices().map(|(i, _)| i).collect();
    for &start in &starts {
        let mut chars = name[start..].char_indices();
        let mut matched = 0;
        let mut end = start;
        while matched < q.len() {
            let Some((offset, c)) = chars.next() else { break };
            let lower: Vec<char> = c.to_lowercase().collect();
            if q.get(matched..matched + lower.len()) != Some(&lower[..]) {
                break;
            }
            matched += lower.len();
            end = start + offset + c.len_utf8();
        }
        if matched == q.len() && end > start {
            return Some((start, end));
        }
    }
    None
}

fn one_line(text: &str, font: egui::FontId, colour: Color32, width: f32) -> LayoutJob {
    let mut job = LayoutJob { wrap: wrap(width), ..Default::default() };
    job.append(text, 0.0, TextFormat { font_id: font, color: colour, ..Default::default() });
    job
}

fn wrap(width: f32) -> TextWrapping {
    TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') }
}

enum Msg<'a> {
    Text(&'a str, Color32),
    Spinner,
}

/// `.dest-msg`: the list's one line when it has no rows.
fn message(ui: &mut Ui, msg: Msg) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| match msg {
        Msg::Text(text, colour) => {
            ui.add(Label::new(RichText::new(text).font(theme::body()).color(colour)).wrap());
        }
        Msg::Spinner => spinner(ui),
    });
}

/// `.dest-sec`
fn section(ui: &mut Ui, text: &str) {
    let _ = egui::Frame::new().inner_margin(Margin { left: 10, right: 10, top: 12, bottom: 4 }).show(ui, |ui| {
        ui.add(Label::new(RichText::new(text).font(theme::caption()).color(theme::TEXT_3)));
    });
}

/// `.dest-note`: the account search's spinner, or why it failed.
fn note(ui: &mut Ui, error: Option<&str>) {
    let _ = egui::Frame::new().inner_margin(Margin { left: 10, right: 10, top: 6, bottom: 6 }).show(ui, |ui| match error {
        Some(e) => {
            ui.add(Label::new(RichText::new(e).font(theme::note()).color(theme::ERROR_TEXT)).wrap());
        }
        None => spinner(ui),
    });
}

/// `.modal-foot`: New folder at the left; Cancel and the button that chooses at
/// the right. Both of the latter two need a project or folder open.
fn foot(ui: &mut Ui, picker: &mut DestPicker, act: &mut Option<Do>) {
    let in_folder = picker.in_folder() && picker.items.is_some();
    let choose = match picker.mode {
        DestMode::Upload { .. } => CHOOSE_UPLOAD,
        DestMode::Watch => CHOOSE_PICK,
    };
    let frame = egui::Frame::new()
        .inner_margin(Margin { left: 14, right: 14, top: 12, bottom: 12 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.x = theme::space::M;
            let (left, right) = egui::Sides::new().spacing(theme::space::M).show(
                ui,
                |ui| {
                    let mut new_folder = false;
                    if let Some(name) = picker.new_folder.as_mut() {
                        let field = ui
                            .scope(|ui| {
                                ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
                                ui.add(
                                    TextEdit::singleline(name)
                                        .id(Id::new(ID).with("new-folder"))
                                        .hint_text(NEW_FOLDER_HINT)
                                        .margin(Margin::symmetric(10, 8))
                                        .desired_width(190.0),
                                )
                            })
                            .inner;
                        new_folder |= field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    }
                    new_folder |= subtle_button(ui, NEW_FOLDER, in_folder).clicked();
                    new_folder.then_some(Do::NewFolder)
                },
                |ui| {
                    // Right to left: the button that chooses is the outermost.
                    if primary_button(ui, choose, in_folder).clicked() {
                        return Some(Do::Choose);
                    }
                    subtle_button(ui, CANCEL, true).clicked().then_some(Do::Close)
                },
            );
            if let Some(done) = right.or(left) {
                *act = Some(done);
            }
        });
    ui.painter().hline(frame.response.rect.x_range(), frame.response.rect.top(), theme::hairline(theme::BORDER));
}

/// `.btn.subtle`
fn subtle_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            for w in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.noninteractive] {
                w.bg_stroke = Stroke::NONE;
            }
            widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.hovered.weak_bg_fill = theme::HOVER;
            widgets.active.weak_bg_fill = theme::HOVER;
            widgets.inactive.fg_stroke = theme::hairline(theme::TEXT_2);
            widgets.noninteractive.fg_stroke = theme::hairline(theme::TEXT_4);
            widgets.hovered.fg_stroke = theme::hairline(theme::TEXT);
            widgets.active.fg_stroke = theme::hairline(theme::TEXT);
        }
        ui.add_enabled(enabled, egui::Button::new(RichText::new(text).font(theme::body())).min_size(vec2(0.0, theme::size::CONTROL_H)))
    })
    .inner
}

/// `.btn.primary`, and its off state at 40 %.
fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = theme::BRAND;
            widgets.hovered.weak_bg_fill = theme::BRAND_HOVER;
            widgets.active.weak_bg_fill = theme::BRAND_HOVER;
            widgets.noninteractive.weak_bg_fill = theme::BRAND.gamma_multiply(0.4);
            for w in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.noninteractive] {
                w.bg_stroke = Stroke::NONE;
            }
        }
        let colour = if enabled { theme::WHITE } else { theme::WHITE.gamma_multiply(0.4) };
        ui.add_enabled(enabled, egui::Button::new(RichText::new(text).font(theme::body()).color(colour)).min_size(vec2(0.0, theme::size::CONTROL_H)))
    })
    .inner
}

/// `.icon-btn`: 28 × 28, `--text-3` until hovered.
fn icon_button(ui: &mut Ui, glyph: Glyph, label: &str) -> bool {
    let size = vec2(theme::size::ICON_BTN, theme::size::ICON_BTN);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.on_hover_text(label);
    if response.hovered() {
        ui.painter().rect_filled(rect, theme::R_S, theme::HOVER);
    }
    let colour = if response.hovered() { theme::TEXT } else { theme::TEXT_3 };
    paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), glyph, colour);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response.clicked()
}

/// `.spinner`: a 12 px ring with a turning head, on a 0.7 s cycle.
fn spinner(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let centre = rect.center();
    let radius = 5.25;
    ui.painter().circle_stroke(centre, radius, Stroke::new(1.5_f32, theme::BORDER_3));
    let phase = (ui.input(|i| i.time) as f32 / 0.7).fract() * 360.0;
    let head: Vec<Pos2> = (0..=10)
        .map(|step| {
            let angle = (phase - 90.0 + 9.0 * step as f32).to_radians();
            pos2(centre.x + radius * angle.cos(), centre.y + radius * angle.sin())
        })
        .collect();
    ui.painter().add(Shape::line(head, Stroke::new(1.5_f32, theme::TEXT_2)));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
}

/// The glyphs, from `I.*` in the webview's app.js (20 × 20 view boxes).
#[derive(Clone, Copy)]
enum Glyph {
    Team,
    Project,
    Folder,
    Search,
    Close,
}

fn paint(painter: &Painter, rect: Rect, glyph: Glyph, colour: Color32) {
    let icon = match glyph {
        Glyph::Team => Icon::Team,
        Glyph::Project => Icon::Project,
        Glyph::Folder => Icon::Folder,
        Glyph::Search => Icon::Search,
        Glyph::Close => Icon::Close,
    };
    icons::paint(painter, rect.center(), rect.width(), icon, colour);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WatchEditor;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    fn node(kind: &str, name: &str) -> DestNode {
        DestNode { id: format!("{kind}:{name}"), name: name.into(), kind: kind.into(), folder_id: format!("f:{name}"), ..Default::default() }
    }

    /// A picker open at a project, its folders listed.
    fn at_project(mode: DestMode) -> UiState {
        let mut picker = DestPicker::new(mode);
        picker.stack = vec![node("account", "Farbhaus"), node("workspace", "Main"), node("project", "Nightfall")];
        picker.items = Some(vec![node("folder", "Ep 01"), node("folder", "Ep 02"), node("folder", "Renders"), node("folder", "Sound")]);
        picker.focus_query = false;
        UiState { logged_in: true, dest_picker: Some(picker), ..Default::default() }
    }

    fn harness(state: UiState) -> Harness<'static, UiState> {
        Harness::builder().with_size(vec2(956.0, 780.0)).build_state(
            |ctx, state: &mut UiState| {
                crate::app::apply_style_once(ctx);
                egui::CentralPanel::default().show(ctx, |_| {});
                show(ctx, state);
            },
            state,
        )
    }

    #[test]
    fn the_picker_renders_a_projects_folders() {
        let mut h = harness(at_project(DestMode::Upload { pending: vec!["/Volumes/Media/clip.mov".into()] }));
        h.run_steps(4);
        h.get_by_label("Upload clip.mov to");
        h.get_by_label("Renders");
        h.snapshot("dest_picker");
    }

    #[test]
    fn the_picker_renders_a_search_with_hits_from_other_projects() {
        let mut state = at_project(DestMode::Watch);
        let p = state.dest_picker.as_mut().unwrap();
        p.query = "re".into();
        p.searched = true;
        p.hits = Some(vec![
            DestNode { where_: "Daylight".into(), remote: true, ..node("folder", "Reels") },
            DestNode { remote: true, ..node("project", "Reel Talk") },
        ]);
        let mut h = harness(state);
        h.run_steps(4);
        h.get_by_label(ALL_PROJECTS);
        h.get_by_label("Reels");
        h.snapshot("dest_picker_search");
    }

    #[test]
    fn a_row_opens_its_level_and_a_crumb_goes_back() {
        let mut h = harness(at_project(DestMode::Watch));
        h.run_steps(4);
        h.get_by_label("Renders").click();
        h.run();
        let p = h.state().dest_picker.clone().unwrap();
        assert_eq!(p.top().map(|t| t.name.as_str()), Some("Renders"));
        assert_eq!(h.state().requests, vec![Request::DestLoad { skip: true }]);

        h.state_mut().requests.clear();
        h.get_by_label("Main").click();
        h.run();
        let p = h.state().dest_picker.clone().unwrap();
        assert_eq!(p.stack.len(), 2, "back at the workspace's projects");
        assert_eq!(h.state().requests, vec![Request::DestLoad { skip: false }]);
    }

    #[test]
    fn the_rest_of_the_path_stays_on_screen_and_leads_back_down() {
        let mut state = at_project(DestMode::Upload { pending: Vec::new() });
        let p = state.dest_picker.as_mut().unwrap();
        p.trail = p.stack.clone();
        p.trail.extend([node("folder", "Ep 02"), node("folder", "Graded")]);
        // What the shell does when the level lands: the way down is highlighted.
        p.sel = 1;
        p.kbd = true;
        p.scroll_sel = true;
        let mut h = harness(state);
        h.run_steps(4);
        h.snapshot("dest_picker_trail");
        // Below the open project: Ep 02 is a row and a crumb, Graded only a crumb.
        h.get_by_label("Graded").click();
        h.run();
        let p = h.state().dest_picker.clone().unwrap();
        assert_eq!(p.top().map(|t| t.name.as_str()), Some("Graded"));
        assert!(p.forward().is_empty());
        assert_eq!(h.state().requests, vec![Request::DestLoad { skip: false }]);
    }

    #[test]
    fn the_list_scrolls_away_from_the_highlighted_row() {
        let mut state = at_project(DestMode::Upload { pending: Vec::new() });
        let p = state.dest_picker.as_mut().unwrap();
        p.stack.truncate(2);
        p.trail.clear();
        p.items = Some((0..40).map(|i| node("project", &format!("Project {i:02}"))).collect());
        // The destination's project, far down the list, as the shell highlights it.
        p.sel = 30;
        p.kbd = true;
        p.scroll_sel = true;
        let mut h = harness(state);
        h.run_steps(10);
        let shown = h.get_by_label("Project 30").rect();
        assert!(h.get_by_label("Project 00").rect().top() < shown.top() - ROW_H * 20.0, "scrolled down to it");

        // (A wheel event doesn't reach the list in the harness; accesskit's scroll does.)
        for _ in 0..8 {
            h.get_by_label("Project 30").scroll_up();
            h.step();
        }
        h.run_steps(10);
        let after = h.get_by_label("Project 30").rect();
        assert!(after.top() > shown.top() + 700.0, "the list stays where it was scrolled to: {shown:?} → {after:?}");
    }

    #[test]
    fn upload_here_asks_for_the_destination_and_choose_fills_the_editor() {
        let mut h = harness(at_project(DestMode::Upload { pending: Vec::new() }));
        h.run_steps(4);
        h.get_by_label(CHOOSE_UPLOAD).click();
        h.run();
        assert_eq!(h.state().requests, vec![Request::DestChoose]);

        let mut state = at_project(DestMode::Watch);
        state.watch_editor = Some(WatchEditor { local: "/Users/chaos/Rushes".into(), ..WatchEditor::default() });
        let mut h = harness(state);
        h.run_steps(4);
        h.get_by_label(CHOOSE_PICK).click();
        h.run();
        let editor = h.state().watch_editor.clone().unwrap();
        assert_eq!((editor.account_id.as_str(), editor.folder_id.as_str()), ("account:Farbhaus", "f:Nightfall"));
        assert_eq!(editor.label, "Nightfall");
        assert!(h.state().dest_picker.is_none());
    }

    #[test]
    fn nothing_can_be_chosen_above_a_project() {
        let mut state = at_project(DestMode::Upload { pending: Vec::new() });
        let p = state.dest_picker.as_mut().unwrap();
        p.stack.truncate(2);
        p.items = Some(vec![node("project", "Nightfall")]);
        let mut h = harness(state);
        h.run_steps(4);
        h.get_by_label(CHOOSE_UPLOAD).click();
        h.run();
        assert!(h.state().requests.is_empty(), "Upload here is off at a workspace");
    }

    #[test]
    fn the_title_says_what_is_being_uploaded() {
        let mut p = DestPicker::new(DestMode::Upload { pending: vec![] });
        assert_eq!(title(&p), "Upload to");
        p.mode = DestMode::Upload { pending: vec!["/Volumes/Media/clip.mov".into()] };
        assert_eq!(title(&p), "Upload clip.mov to");
        p.mode = DestMode::Upload { pending: vec!["/a".into(), "/b/".into(), "/c".into()] };
        assert_eq!(title(&p), "Upload 3 items to");
        p.mode = DestMode::Watch;
        assert_eq!(title(&p), TITLE_PICK);
    }

    #[test]
    fn a_match_is_found_ignoring_case() {
        assert_eq!(find_ignoring_case("Speed Test", "spee"), Some((0, 4)));
        assert_eq!(find_ignoring_case("Speed Test", "TEST"), Some((6, 10)));
        assert_eq!(find_ignoring_case("Ünïcode Résumé", "résumé"), Some((10, 18)));
        assert_eq!(find_ignoring_case("abc", "x"), None);
    }

    #[test]
    fn highlighting_keeps_every_character() {
        let job = highlighted("Day 1 Rushes", "rush", theme::TEXT_BODY, 200.0, egui::FontFamily::Proportional);
        assert_eq!(job.text, "Day 1 Rushes");
        assert_eq!(job.sections.len(), 3);
        assert_eq!(&job.text[job.sections[1].byte_range.clone()], "Rush");
    }
}
