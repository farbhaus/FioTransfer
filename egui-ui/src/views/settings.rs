//! The Settings screen.
//!
//! The four sections `frontend/dist/index.html` puts under `#view-settings`, in
//! its order and with its words: "Frame.io account" (sign-in state, "Adobe
//! Client ID", the setup steps with the redirect URI and "Copy"), "App" ("Open
//! at login", "Version" + "Check now", "Check for updates"), "Performance"
//! ("Parallel streams", "Chunk size") and "Storage" ("Download location" +
//! "Change…", "Save log with media", "App log" + "Show in Finder"), then the
//! `.about` line.
//!
//! Below them is a fifth section the webview never had: **Portal**, the service
//! that publishes this project's links and names the `Live_Work` folder each one
//! belongs to. It is four settings (`App::set_portal` writes them: the address,
//! the local folder, the on/off flag and — to the keychain, never to
//! settings.json — the bearer token), one status line from `App::portal_status`,
//! and a "Reconcile now". Two credentials get in: a pasted access token, or
//! Portal's own sign-in ("Sign in instead", `App::portal_sign_in`), whose session
//! cookie is kept and whose password is not. It draws with the same rows as
//! everything above it: no new colour, no new font, no new height.
//!
//! The shell (`crate::app`) supplies the `.view` padding, the scroll area and
//! the page head; the tokens come from [`crate::theme`]. `.settings
//! { max-width: 680px }` is applied here because it is this screen's own measure.

use crate::model::{PortalView, Request, Secret, UiState};
use crate::theme;
use egui::{
    Align, Checkbox, CollapsingHeader, Color32, ComboBox, CornerRadius, Frame, Layout, Margin,
    RichText, Slider, Stroke, TextEdit, vec2,
};

/// `.settings { max-width: 680px }`.
const SETTINGS_MAX_WIDTH: f32 = 680.0;
/// `.set-section { margin-bottom: 28px }` and `.set-card { margin-top: 12px }`.
const SECTION_GAP: f32 = 28.0;
const CARD_GAP: f32 = theme::space::XL;
/// `.set-line { padding: 14px 16px }`.
const LINE_PADDING: Margin = Margin::symmetric(theme::space::XXXL as i8, theme::space::XXL as i8);
/// The stylesheet sets `line-height: 1.45` on the body, so a 13 px label
/// occupies 18.85 px and a 12 px `.help` line 17.4 px. egui sizes a label to the
/// glyphs instead, so these are set explicitly: without them a `.set-line` is a
/// few pixels shorter than the webview's and the sections drift apart.
const LABEL_LINE: f32 = theme::font::BODY * 1.45;
const HELP_LINE: f32 = theme::font::DETAIL * 1.45;
/// `.set-title`: 14 px in the same line box.
const TITLE_LINE: f32 = theme::font::SECTION * 1.45;
/// A `.check input` is 15 px tall, so a row whose only control is a checkbox is
/// `14 + 18.85 + 14` tall, not the 32 px of a button row.
const CHECK_ROW_CONTROL: f32 = 19.0;
/// `input[type=range] { height: 4px }` with a 14 px thumb: the row is sized by
/// the thumb, not by a 32 px control. 10 px is the smallest height that still
/// draws the round handle without clipping it.
const SLIDER_CONTROL: f32 = 10.0;

/// `#streams-range { min="1" max="64" }`, widened to 1–128 by the brief. The
/// core's `Settings` counts streams and segments in `i32`.
const STREAMS_MIN: i32 = 1;
const STREAMS_MAX: i32 = 128;
/// `#segment option` values, widened to 8–512 MiB by the brief.
const SEGMENT_SIZES: [i32; 7] = [8, 16, 32, 64, 128, 256, 512];
const SEGMENT_MIN: i32 = 8;
const SEGMENT_MAX: i32 = 512;
/// The port the local OAuth callback listens on when settings say nothing.
const DEFAULT_REDIRECT_PORT: i32 = 8765;

/// What the screen asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// "Sign in with Adobe".
    SignIn,
    /// "Sign out".
    SignOut,
    /// "Save" next to the Client ID (`saveSettings({ client_id })`).
    SaveClientId(String),
    /// "Copy" next to the redirect URI.
    CopyRedirect(String),
    /// "Change…" for the download folder (`PickDownloadDir`).
    ChooseDownloadFolder,
    /// The streams slider moved; `settings.streams` already holds the value.
    Streams(i32),
    /// "Chunk size" picked a value; `settings.segment_mb` already holds it.
    Segment(i32),
    /// "Save log with media": the value of `no_folder_logs` it writes.
    FolderLogs(bool),
    /// "Check for updates": the value of `no_update_check` it writes.
    UpdateCheck(bool),
    /// "Open at login" (`SetOpenAtLogin`).
    OpenAtLogin(bool),
    /// "Check now" / "Update" (`CheckForUpdate`, `InstallUpdate`).
    UpdateClicked,
    /// "Show in Finder" on the App log row (`OpenLogs`).
    ShowLogs,

    /// The Portal address, committed from its field on Enter or focus loss. It
    /// has already passed [`portal_url_reason`]: a refused value never gets here.
    SetPortalUrl(String),
    /// The local folder Portal's markers are mirrored under.
    SetPortalRoot(String),
    /// "Save" next to the access token: the token, on its way to the keychain.
    SavePortalToken(Secret),
    /// "Forget" next to the access token.
    ForgetPortalToken,
    /// "Sign in" next to the email and the password: Portal's own login, with
    /// the password on its way to the app as a [`Secret`].
    SignInPortal { email: String, password: Secret },
    /// "Sign in" next to the one-time code: the six digits that finish the
    /// sign-in the password started.
    SignInPortalTotp { code: Secret },
    /// "Sign out" of the stored Portal session.
    SignOutPortal,
    /// The "Enabled" toggle.
    SetPortalEnabled(bool),
    /// "Reconcile now".
    ReconcileNow,
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
/// A setting is written into `state.settings` by the row that draws it, so the
/// request only says "save this": the app reads the same struct the screen just
/// wrote. `Update` and `Check now` are one button, and the update state says
/// which of the two it is.
fn ask(state: &mut UiState, action: Action) {
    match action {
        Action::SignIn => state.ask(Request::Login),
        Action::SignOut => state.ask(Request::Logout),
        Action::SaveClientId(client_id) => {
            state.settings.client_id = client_id;
            state.ask(Request::SaveSettings);
        }
        Action::CopyRedirect(uri) => state.ask(Request::CopyText(uri)),
        Action::ChooseDownloadFolder => state.ask(Request::PickDownloadDir),
        Action::Streams(_) | Action::Segment(_) | Action::FolderLogs(_) | Action::UpdateCheck(_) => {
            state.ask(Request::SaveSettings)
        }
        Action::OpenAtLogin(on) => state.ask(Request::SetOpenAtLogin(on)),
        Action::UpdateClicked => {
            if state.update.available {
                state.ask(Request::InstallUpdate);
            } else {
                state.ask(Request::CheckUpdate);
            }
        }
        Action::ShowLogs => state.ask(Request::OpenLogs),
        Action::SetPortalUrl(url) => state.ask(Request::SetPortalUrl(url)),
        Action::SetPortalRoot(root) => state.ask(Request::SetPortalRoot(root)),
        Action::SavePortalToken(token) => state.ask(Request::SetPortalToken(token)),
        Action::ForgetPortalToken => state.ask(Request::ForgetPortalToken),
        Action::SignInPortal { email, password } => {
            state.ask(Request::SignInPortal { email, password })
        }
        Action::SignInPortalTotp { code } => state.ask(Request::SignInPortalTotp { code }),
        Action::SignOutPortal => state.ask(Request::SignOutPortal),
        Action::SetPortalEnabled(on) => state.ask(Request::SetPortalEnabled(on)),
        Action::ReconcileNow => state.ask(Request::PortalReconcile),
    }
}

/// The screen, reporting at most one [`Action`] per frame.
///
/// The sections are `index.html`'s, in its order: the Frame.io account, App,
/// Performance, Storage. Each is a `.set-title` over a `.set-card`, 12 px below
/// the title and 28 px above the next one.
pub fn show_with_actions(ui: &mut egui::Ui, state: &mut UiState) -> Option<Action> {
    let mut action = None;
    // The stylesheet stacks blocks that abut and spaces them explicitly, so the
    // shell's 8 px item spacing is zeroed before anything is drawn: the page
    // head, then `.set-title`, then `.set-card { margin-top: 12px }`.
    ui.spacing_mut().item_spacing = vec2(theme::space::M, 0.0);

    crate::app::page_head(ui, "Settings", |_ui| {});
    // No gap here: `.settings` starts straight under the 64 px `.page-head`.

    let width = ui.available_width().min(SETTINGS_MAX_WIDTH);
    ui.scope(|ui| {
        ui.set_max_width(width);

        // ---------- Frame.io account ----------
        section_title(ui, "Frame.io account");
        ui.add_space(CARD_GAP);
        card(ui, |ui| {
            sign_in_line(ui, state, &mut action);
            rule(ui);
            client_id_line(ui, state, &mut action);
            rule(ui);
            setup_line(ui, state, &mut action);
        });
        ui.add_space(SECTION_GAP);

        // ---------- App ----------
        section_title(ui, "App");
        ui.add_space(CARD_GAP);
        card(ui, |ui| {
            open_at_login_line(ui, state, &mut action);
            rule(ui);
            version_line(ui, state, &mut action);
            // `renderUpdate` hides the auto-update row in a development build.
            if !update_line(state).is_dev() {
                rule(ui);
                update_check_line(ui, state, &mut action);
            }
        });
        ui.add_space(SECTION_GAP);

        // ---------- Performance ----------
        section_title(ui, "Performance");
        ui.add_space(CARD_GAP);
        card(ui, |ui| {
            streams_line(ui, state, &mut action);
            rule(ui);
            segment_line(ui, state, &mut action);
        });
        ui.add_space(SECTION_GAP);

        // ---------- Storage ----------
        section_title(ui, "Storage");
        ui.add_space(CARD_GAP);
        card(ui, |ui| {
            download_dir_line(ui, state, &mut action);
            rule(ui);
            folder_logs_line(ui, state, &mut action);
            rule(ui);
            log_line(ui, state, &mut action);
        });
        ui.add_space(SECTION_GAP);

        // ---------- Portal ----------
        section_title(ui, "Portal");
        ui.add_space(CARD_GAP);
        card(ui, |ui| {
            portal_url_line(ui, state, &mut action);
            rule(ui);
            portal_token_line(ui, state, &mut action);
            rule(ui);
            portal_signin_line(ui, state, &mut action);
            // While Portal waits for a one-time code the row above is only half
            // the sign-in; this row is the other half, and it is drawn only
            // then.
            if state.portal.status.needs_code {
                rule(ui);
                portal_totp_line(ui, state, &mut action);
            }
            rule(ui);
            portal_root_line(ui, state, &mut action);
            rule(ui);
            portal_enabled_line(ui, state, &mut action);
            rule(ui);
            portal_status_line(ui, state);
            rule(ui);
            portal_reconcile_line(ui, state, &mut action);
        });
        ui.add_space(SECTION_GAP);

        // `.about`: `FioTransfer <version>`, 12 px in `--text-4`.
        if !state.version.trim().is_empty() {
            ui.label(
                RichText::new(format!("FioTransfer {}", state.version.trim()))
                    .font(theme::detail())
                    .line_height(Some(HELP_LINE))
                    .color(theme::TEXT_4),
            );
        }
    });

    action
}

/// A stored value clamped to the slider's range, so an out-of-range setting is
/// shown at the nearest end instead of being dropped.
pub fn clamped_streams(streams: i32) -> i32 {
    streams.clamp(STREAMS_MIN, STREAMS_MAX)
}

/// A stored segment size clamped to the sizes the combo offers.
pub fn clamped_segment(segment_mb: i32) -> i32 {
    segment_mb.clamp(SEGMENT_MIN, SEGMENT_MAX)
}

/// The redirect URI the setup steps ask the user to register.
pub fn redirect_uri(port: i32) -> String {
    let port = if (1..=65535).contains(&port) {
        port
    } else {
        DEFAULT_REDIRECT_PORT
    };
    format!("https://localhost:{port}/callback")
}

/// `Signed in as … · …` / `Not signed in` / `Not set up`, with the button that
/// changes it (`renderState` in app.js).
fn sign_in_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    line(ui, |ui| {
        ui.horizontal(|ui| {
            let text = sign_in_words(state);
            ui.add(theme::truncating(
                RichText::new(text)
                    .font(theme::body())
                    .color(theme::TEXT_2),
            ))
            .on_hover_text("Frame.io account");

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if state.logged_in {
                    if ui.button(RichText::new("Sign out").font(theme::body())).clicked() {
                        *action = Some(Action::SignOut);
                    }
                } else {
                    let ready = !state.settings.client_id.trim().is_empty();
                    if primary_button(ui, "Sign in with Adobe", ready) {
                        *action = Some(Action::SignIn);
                    }
                }
            });
        });
    });
}

/// The account row's words.
fn sign_in_words(state: &UiState) -> String {
    if state.logged_in {
        let name = state
            .user
            .as_ref()
            .map(|user| user.name.trim())
            .filter(|name| !name.is_empty());
        let email = state
            .user
            .as_ref()
            .map(|user| user.email.trim())
            .filter(|email| !email.is_empty());
        match (name, email) {
            (Some(name), Some(email)) => format!("Signed in as {name} · {email}"),
            (Some(name), None) => format!("Signed in as {name}"),
            (None, Some(email)) => format!("Signed in · {email}"),
            (None, None) => "Signed in".to_string(),
        }
    } else if state.settings.client_id.trim().is_empty() {
        "Not set up".to_string()
    } else {
        "Not signed in".to_string()
    }
}

/// `.set-line.col`: the label, then the field and its "Save".
fn client_id_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    line(ui, |ui| {
        field_label(ui, "Adobe Client ID");
        ui.add_space(theme::space::M);
        ui.horizontal(|ui| {
            // Right to left, so "Save" sits on the card's edge and the field
            // takes what is left (`.set-line .inline { display: flex; gap: 8px }`).
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let save = ui
                    .add(
                        egui::Button::new(RichText::new("Save").font(theme::body()))
                            .min_size(vec2(0.0, theme::size::CONTROL_H)),
                    )
                    .clicked();
                // The field's own padding is inside its width: `input` has 10 px
                // each side.
                let field_width =
                    (ui.available_width() - 2.0 * theme::space::L).max(60.0);
                ui.scope(|ui| {
                    ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
                    ui.add(
                        TextEdit::singleline(&mut state.settings.client_id)
                            .hint_text("Client ID (API key)")
                            .margin(Margin::symmetric(theme::space::L as i8, theme::space::M as i8))
                            .desired_width(field_width),
                    );
                });
                if save {
                    *action =
                        Some(Action::SaveClientId(state.settings.client_id.trim().to_string()));
                }
            });
        });
    });
}

/// The `<details class="setup">` block: five steps, the redirect URI, "Copy".
fn setup_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let uri = redirect_uri(state.settings.redirect_port);
    line_with(ui, CHECK_ROW_CONTROL, |ui| {
        // `.setup summary` is a line of text with a chevron, not a button.
        ui.spacing_mut().button_padding = vec2(theme::space::XL, 0.0);
        let open_by_default = state.settings.client_id.trim().is_empty();
        CollapsingHeader::new(
            RichText::new("Setup")
                .font(theme::body())
                .color(theme::TEXT_2)
                .strong(),
        )
        .id_salt("fiotransfer-egui.setup")
        .default_open(open_by_default)
        .show(ui, |ui| {
            // `.setup ol { margin: 10px 0 0 }`, `li { margin: 5px 0 }`.
            ui.add_space(theme::space::S);
            ui.spacing_mut().item_spacing.y = 5.0;
            step(ui, "1", "Adobe Developer Console → Create project");
            step(
                ui,
                "2",
                "Add API → Frame.io API → User Authentication → OAuth Single Page App",
            );
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = theme::space::S;
                ui.label(
                    RichText::new("3")
                        .font(theme::note())
                        .color(theme::TEXT_3),
                );
                ui.label(
                    RichText::new("Default redirect URI and Redirect URI pattern:")
                        .font(theme::note())
                        .color(theme::TEXT_2),
                );
                // `code { background: var(--raised) }`
                Frame::new()
                    .fill(theme::RAISED)
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::symmetric(theme::space::S as i8, 1))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&uri)
                                .font(theme::mono())
                                .color(theme::TEXT),
                        );
                    });
                if link_button(ui, "Copy") {
                    *action = Some(Action::CopyRedirect(uri.clone()));
                }
            });
            step(ui, "4", "Copy the Client ID above");
            step(
                ui,
                "5",
                "On first sign-in, macOS asks for your password once to trust FioTransfer's local certificate",
            );
        });
    });
}

fn step(ui: &mut egui::Ui, number: &str, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = theme::space::S;
        ui.label(
            RichText::new(number)
                .font(theme::note())
                .color(theme::TEXT_3),
        );
        ui.add(theme::wrapping(
            RichText::new(text).font(theme::note()).color(theme::TEXT_2),
        ));
    });
}

/// `.set-line.col` with the slider's value on the right of its label.
fn streams_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut streams = clamped_streams(state.settings.streams);
    line_with(ui, CHECK_ROW_CONTROL, |ui| {
        ui.horizontal(|ui| {
            field_label(ui, "Parallel streams");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(streams.to_string())
                        .font(theme::body())
                        .color(theme::TEXT)
                        .strong(),
                );
            });
        });
        ui.add_space(theme::space::XS);
        ui.spacing_mut().interact_size.y = SLIDER_CONTROL;
        let response = ui.add(
            Slider::new(&mut streams, STREAMS_MIN..=STREAMS_MAX)
                .show_value(false)
                .trailing_fill(true),
        );
        if response.changed() {
            *action = Some(Action::Streams(streams));
        }
    });
    state.settings.streams = streams;
}

/// "Chunk size": the select the webview draws, as a combo box.
fn segment_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut segment = clamped_segment(state.settings.segment_mb);
    line(ui, |ui| {
        ui.horizontal(|ui| {
            let label = field_label(ui, "Chunk size");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let before = segment;
                let combo = ComboBox::from_id_salt("fiotransfer-egui.segment")
                    .selected_text(RichText::new(format!("{segment} MB")).font(theme::body()))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for mb in SEGMENT_SIZES {
                            ui.selectable_value(&mut segment, mb, format!("{mb} MB"));
                        }
                    });
                // `select` is associated with its `label` in the webview too.
                combo.response.labelled_by(label.id);
                if segment != before {
                    *action = Some(Action::Segment(segment));
                }
            });
        });
    });
    state.settings.segment_mb = segment;
}

/// `.set-line` with two lines on the left: the label and the path.
fn download_dir_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    line(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 100.0).max(80.0));
                ui.spacing_mut().interact_size.y = LABEL_LINE;
                field_label(ui, "Download location");
                ui.add_space(theme::space::XXS);
                ui.add(theme::truncating(
                    RichText::new(display_path(&state.settings.download_dir, &state.home))
                        .font(theme::detail())
                        .line_height(Some(HELP_LINE))
                        .color(theme::TEXT_3),
                ))
                .on_hover_text(if state.settings.download_dir.trim().is_empty() {
                    "No download folder chosen".to_string()
                } else {
                    state.settings.download_dir.clone()
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(RichText::new("Change…").font(theme::body())).clicked() {
                    *action = Some(Action::ChooseDownloadFolder);
                }
            });
        });
    });
}

/// "Save log with media": the label on the left, the box on the right.
fn folder_logs_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut save_logs = !state.settings.no_folder_logs;
    line_with(ui, CHECK_ROW_CONTROL, |ui| {
        ui.horizontal(|ui| {
            let label = field_label(ui, "Save log with media");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().interact_size.y = CHECK_ROW_CONTROL;
                let boxed = ui.add(Checkbox::without_text(&mut save_logs)).labelled_by(label.id);
                if boxed.changed() {
                    *action = Some(Action::FolderLogs(save_logs));
                }
            });
        });
    });
    state.settings.no_folder_logs = !save_logs;
}

/// "Check for updates": the same row, reading `no_update_check`.
fn update_check_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut check = !state.settings.no_update_check;
    line_with(ui, CHECK_ROW_CONTROL, |ui| {
        ui.horizontal(|ui| {
            let label = field_label(ui, "Check for updates");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().interact_size.y = CHECK_ROW_CONTROL;
                let boxed = ui.add(Checkbox::without_text(&mut check)).labelled_by(label.id);
                if boxed.changed() {
                    *action = Some(Action::UpdateCheck(check));
                }
            });
        });
    });
    state.settings.no_update_check = !check;
}


/// `UpdateLine` from `renderUpdate` in app.js: the help text under "Version",
/// the button's word, and whether the button is live.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UpdateLine {
    help: String,
    error: String,
    button: &'static str,
    button_enabled: bool,
    button_primary: bool,
    dev: bool,
}

impl UpdateLine {
    fn is_dev(&self) -> bool {
        self.dev
    }
}

/// What `renderUpdate` would write into `#update-line` and `#update-btn`.
fn update_line(state: &UiState) -> UpdateLine {
    let update = &state.update;
    let dev = update.blocker.trim() == "development build";
    let busy = matches!(update.state.as_str(), "downloading" | "restarting");

    let help = if dev {
        "Development build".to_string()
    } else if update.state == "downloading" {
        format!("Downloading… {}%", (update.progress.clamp(0.0, 1.0) * 100.0).floor())
    } else if update.state == "restarting" {
        "Restarting…".to_string()
    } else if update.state == "checking" {
        "Checking…".to_string()
    } else if update.available {
        let mut help = format!("{} available", update.latest.trim());
        if !update.blocker.trim().is_empty() {
            help.push_str(" · ");
            help.push_str(update.blocker.trim());
        }
        help
    } else {
        // `#update-line` is only filled once a check has answered; `checked_at`
        // is not in the state, so an unchecked build shows nothing here.
        String::new()
    };

    UpdateLine {
        help,
        error: update.error.trim().to_string(),
        button: if update.available {
            if update.blocker.trim().is_empty() {
                "Update"
            } else {
                "Download"
            }
        } else {
            "Check now"
        },
        button_enabled: !dev && !busy && update.state != "checking",
        button_primary: update.available && update.blocker.trim().is_empty(),
        dev,
    }
}

/// "Open at login": the label on the left, the box on the right.
fn open_at_login_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut open_at_login = state.open_at_login;
    line_with(ui, CHECK_ROW_CONTROL, |ui| {
        ui.horizontal(|ui| {
            let label = field_label(ui, "Open at login");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().interact_size.y = CHECK_ROW_CONTROL;
                let boxed = ui
                    .add(Checkbox::without_text(&mut open_at_login))
                    .labelled_by(label.id);
                if boxed.changed() {
                    *action = Some(Action::OpenAtLogin(open_at_login));
                }
            });
        });
    });
    state.open_at_login = open_at_login;
}

/// "Version <current>" with the update line beneath and the button on the right.
fn version_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let current = update_current(state);
    let words = update_line(state);
    line(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 130.0).max(80.0));
                ui.spacing_mut().interact_size.y = LABEL_LINE;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = theme::space::XS;
                    field_label(ui, "Version");
                    // `#app-version { color: var(--text-2); font-weight: 400 }`
                    ui.label(
                        RichText::new(&current)
                            .font(theme::body())
                            .color(theme::TEXT_2),
                    );
                });
                if !words.help.is_empty() {
                    ui.add_space(theme::space::XXS);
                    ui.add(theme::truncating(
                        RichText::new(&words.help)
                            .font(theme::detail())
                            .line_height(Some(HELP_LINE))
                            .color(theme::TEXT_3),
                    ))
                    .on_hover_text(&words.help);
                }
                if !words.error.is_empty() {
                    ui.add_space(theme::space::XXS);
                    ui.add(theme::truncating(
                        RichText::new(&words.error)
                            .font(theme::detail())
                            .line_height(Some(HELP_LINE))
                            .color(theme::ERROR_TEXT),
                    ))
                    .on_hover_text(&words.error);
                }
            });
            // `btn.classList.toggle('hidden', dev)`: a development build has no
            // button at all.
            if !words.is_dev() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let clicked = if words.button_primary {
                        primary_button(ui, words.button, words.button_enabled)
                    } else {
                        secondary_button_enabled(ui, words.button, words.button_enabled)
                    };
                    if clicked {
                        *action = Some(Action::UpdateClicked);
                    }
                });
            }
        });
    });
}

/// `u.current`, falling back to the screen's own version.
fn update_current(state: &UiState) -> String {
    let current = state.update.current.trim();
    if current.is_empty() {
        state.version.trim().to_string()
    } else {
        current.to_string()
    }
}

/// "App log": the label, the shortened log path, and "Show in Finder".
fn log_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let path = display_path(&state.log_dir, &state.home);
    let full = state.log_dir.trim().to_string();
    line(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 140.0).max(80.0));
                ui.spacing_mut().interact_size.y = LABEL_LINE;
                field_label(ui, "App log");
                ui.add_space(theme::space::XXS);
                ui.add(theme::truncating(
                    RichText::new(&path)
                        .font(theme::detail())
                        .line_height(Some(HELP_LINE))
                        .color(theme::TEXT_3),
                ))
                .on_hover_text(if full.is_empty() {
                    "No log folder yet".to_string()
                } else {
                    full.clone()
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(RichText::new("Show in Finder").font(theme::body()))
                    .clicked()
                {
                    *action = Some(Action::ShowLogs);
                }
            });
        });
    });
}

/// The path as the webview shows it, or an em dash when nothing is set.
pub fn display_path(path: &str, home: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        "—".to_string()
    } else {
        short_path(trimmed, home)
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

// ---------- Portal ----------

/// The address field's placeholder: what an address looks like, not a default.
const PORTAL_URL_HINT: &str = "https://portal.example.com";
/// The folder field's placeholder: `Settings::PORTAL_ROOT_DIR` under `HOME`, in
/// the words the field can take back (`Settings::portal_root_path` expands it).
const PORTAL_ROOT_HINT: &str = "~/Live_Work";
/// The token field's placeholder. The field never shows the stored token, so an
/// empty field says which of the two states it is in instead.
const TOKEN_STORED: &str = "stored in your keychain";
const TOKEN_MISSING: &str = "not set";
/// The sign-in row's two placeholders: what an admin address looks like, and a
/// password field that says nothing about the password it is waiting for.
const PORTAL_EMAIL_HINT: &str = "admin@example.com";
const PORTAL_PASSWORD_HINT: &str = "your Portal password";
/// A one-time code is six digits (RFC 6238, a 30 s step): Portal's own login
/// asks for exactly that, so the field takes exactly that.
const TOTP_DIGITS: usize = 6;
/// The code field's placeholder, which is the whole instruction it needs.
const PORTAL_CODE_HINT: &str = "6 digits";
/// What the code row says under its field: which code, and the promise that it
/// is not kept.
const PORTAL_CODE_NOTE: &str =
    "Six digits from your authenticator app, to finish this sign-in. The code is not stored.";
/// What the sign-in row says under its fields: the one promise the row makes.
const PASSWORD_NOTE: &str =
    "The password is not stored: only Portal's session cookie is, in your keychain.";
/// The same line while the POST is out.
const SIGNING_IN: &str = "Signing in…";
/// What the address field says when it refuses a value.
const PORTAL_URL_REASON: &str = "The Portal address has to start with http:// or https://";

/// Why this Portal address cannot be used, or `None` when it can.
///
/// `App::set_portal` refuses the same two things — no `http://` / `https://`, or
/// a scheme with nothing after it — but this decides before the write, so a
/// refused address is never the one in `settings.json`. An empty address is
/// allowed: it is how Portal is switched off.
pub fn portal_url_reason(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let rest = match url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        Some(rest) => rest,
        None => return Some(PORTAL_URL_REASON.to_string()),
    };
    if rest.trim_matches('/').is_empty() {
        return Some("The Portal address has no host after the scheme".to_string());
    }
    if rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("The Portal address has a space or a control character in it".to_string());
    }
    None
}

/// Why this local folder cannot be used, or `None` when it can.
///
/// Empty means "the default", which is `~/Live_Work`. The disk root is refused
/// here because `Settings::normalize` would silently replace it: a setting that
/// cannot be saved is better refused out loud.
pub fn portal_root_reason(root: &str) -> Option<String> {
    let root = root.trim();
    if root.is_empty() {
        return None;
    }
    if root == "/" {
        return Some("The whole disk is not a Portal folder".to_string());
    }
    if !root.starts_with('/') && !root.starts_with('~') {
        return Some("Enter an absolute path, or one starting with ~".to_string());
    }
    if root.chars().any(|c| c.is_control()) {
        return Some("The folder has a control character in it".to_string());
    }
    None
}

/// `~/Live_Work` with the home expanded: the absolute path Portal's folders are
/// actually created under.
///
/// The same expansion as `Settings::portal_root_path`, in strings: the browser
/// capture has no core `Settings` to ask, and the row has to show the resolved
/// path either way.
pub fn resolved_portal_root(root: &str, home: &str) -> String {
    let home = home.trim().trim_end_matches('/');
    let root = root.trim();
    let root = if root.is_empty() { format!("{home}/Live_Work") } else { root.to_string() };
    match root.strip_prefix('~') {
        Some(rest) if rest.is_empty() => home.to_string(),
        Some(rest) => match rest.strip_prefix('/') {
            Some(rest) => format!("{home}/{rest}"),
            // `~someone/x` is not this user's home: taken as given.
            None => root,
        },
        None => root,
    }
}

/// One number with its word: `1 link`, `4 links`.
fn counted(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// How long ago the last reconcile ran, in the words a status line can carry.
///
/// `at` and `now` are unix milliseconds. A clock that went backwards (or a
/// report stamped a moment in the future) is "just now", not a negative age.
pub fn portal_when(at: i64, now: i64) -> String {
    if at <= 0 {
        return "never".to_string();
    }
    let secs = ((now - at).max(0)) / 1000;
    let ago = |n: i64, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one} ago")
        } else {
            format!("{n} {many} ago")
        }
    };
    match secs {
        0..=4 => "just now".to_string(),
        5..=59 => "under a minute ago".to_string(),
        60..=3599 => ago(secs / 60, "min", "min"),
        3600..=86_399 => ago(secs / 3600, "h", "h"),
        _ => ago(secs / 86_400, "day", "days"),
    }
}

/// The status line: one sentence, and whether it is bad news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalLine {
    pub text: String,
    /// The last reconcile failed, so the line is drawn in the error colour and
    /// says so: it must never read as a success.
    pub failed: bool,
}

/// What the status line says, from the last status the app gave.
///
/// The order matters: "not set up" and "no token" are the reasons a reconcile
/// cannot run at all, so they come before the numbers; and a failed run says
/// "Failed" with its reason and no numbers, because a report that failed has
/// none to show.
pub fn portal_line(status: &PortalView) -> PortalLine {
    let line = |text: String, failed: bool| PortalLine { text, failed };
    let url = status.url.trim();
    if url.is_empty() {
        return if status.signed_in {
            line("No Portal address yet: add one above.".to_string(), true)
        } else {
            line("Not set up: no Portal address and no token yet.".to_string(), false)
        };
    }
    if !status.signed_in {
        return line(
            "Not signed in yet: save an access token above, or sign in below.".to_string(),
            true,
        );
    }
    if !status.error.trim().is_empty() {
        let when = portal_when(status.last_at, status.now_ms);
        return line(format!("Failed {when}: {}", status.error.trim()), true);
    }
    if status.running {
        return line("Reconciling…".to_string(), false);
    }
    if status.last_at <= 0 {
        // Nothing has run yet, so say what to press and whether the background
        // poller is even on — a signed-in card that stays silent looks broken.
        return line(
            if status.enabled {
                "Configured. Nothing has run yet: press Reconcile now.".to_string()
            } else {
                "Configured. Press Reconcile now, or switch it on to check every 30 s.".to_string()
            },
            false,
        );
    }
    let mut text = format!(
        "{} · {}, {} kept in step, {} removed",
        portal_when(status.last_at, status.now_ms),
        counted(status.links_seen, "link", "links"),
        counted(status.ensured, "folder", "folders"),
        status.removed
    );
    if status.skipped > 0 {
        text.push_str(&format!(", {}", counted(status.skipped, "skipped", "skipped")));
    }
    line(text, false)
}

/// `.input` as the Client ID field draws it: the surface fill, 10 px either
/// side, the full width it is given.
fn portal_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> egui::Response {
    let width = ui.available_width();
    ui.scope(|ui| {
        ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
        ui.add(
            TextEdit::singleline(text)
                .hint_text(hint)
                .margin(Margin::symmetric(theme::space::L as i8, theme::space::M as i8))
                .desired_width(width),
        )
    })
    .inner
}

/// `.help`: 12 px, `--text-3`, one line, the whole words on hover.
fn help_line(ui: &mut egui::Ui, text: &str) {
    ui.add(theme::truncating(
        RichText::new(text)
            .font(theme::detail())
            .line_height(Some(HELP_LINE))
            .color(theme::TEXT_3),
    ))
    .on_hover_text(text);
}

/// The same line, for something the user has to read and fix.
fn reason_line(ui: &mut egui::Ui, text: &str) {
    ui.add(theme::truncating(
        RichText::new(text)
            .font(theme::detail())
            .line_height(Some(HELP_LINE))
            .color(theme::ERROR_TEXT),
    ))
    .on_hover_text(text);
}

/// "Server URL": a field committed on Enter or on focus loss, with the reason
/// under it when the value is refused.
fn portal_url_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut text = state.portal.url_draft.clone().unwrap_or_else(|| state.settings.portal_url.clone());
    line(ui, |ui| {
        let label = field_label(ui, "Server URL");
        ui.add_space(theme::space::M);
        // `label for=` as the webview has it: the field is named by the words
        // above it, for a screen reader and for this screen's own tests.
        let response = portal_field(ui, &mut text, PORTAL_URL_HINT).labelled_by(label.id);
        if response.changed() {
            // Typing is not a commit: the setting waits for Enter or for the
            // field to lose focus.
            state.portal.url_draft = Some(text.clone());
            state.portal.url_error = None;
        }
        if response.lost_focus() {
            match portal_url_reason(&text) {
                None => {
                    let url = text.trim().trim_end_matches('/').to_string();
                    state.settings.portal_url = url.clone();
                    state.portal.url_draft = None;
                    state.portal.url_error = None;
                    *action = Some(Action::SetPortalUrl(url));
                }
                Some(reason) => {
                    // Not saved: the field keeps what was typed so it can be
                    // fixed, and the setting keeps the value already there.
                    state.portal.url_draft = Some(text.clone());
                    state.portal.url_error = Some(reason);
                }
            }
        }
        ui.add_space(theme::space::XXS);
        match state.portal.url_error.clone() {
            Some(reason) => reason_line(ui, &reason),
            None => help_line(ui, "Leave the address empty to switch Portal off."),
        }
    });
}

/// "Access token": a password field, "Save" and "Forget".
///
/// The field is emptied by the shell once the token is stored, so nothing on
/// screen ever holds the token: what is drawn is the mask egui puts over
/// whatever is being typed.
fn portal_token_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let stored = state.portal.status.signed_in;
    let typed = !state.portal.token_draft.trim().is_empty();
    line(ui, |ui| {
        let label = field_label(ui, "Access token");
        ui.add_space(theme::space::M);
        ui.horizontal(|ui| {
            // Right to left, so "Save" sits on the card's edge and the field
            // takes what is left, exactly as the Client ID row does.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let save = ui
                    .add_enabled(
                        typed,
                        egui::Button::new(RichText::new("Save").font(theme::body()))
                            .min_size(vec2(0.0, theme::size::CONTROL_H)),
                    )
                    .clicked();
                let forget = ui
                    .add_enabled(
                        stored,
                        egui::Button::new(RichText::new("Forget").font(theme::body()))
                            .min_size(vec2(0.0, theme::size::CONTROL_H)),
                    )
                    .clicked();
                let field_width = (ui.available_width() - 2.0 * theme::space::L).max(60.0);
                ui.scope(|ui| {
                    ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
                    ui.add(
                        TextEdit::singleline(&mut state.portal.token_draft)
                            .password(true)
                            .hint_text(if stored { TOKEN_STORED } else { TOKEN_MISSING })
                            .margin(Margin::symmetric(theme::space::L as i8, theme::space::M as i8))
                            .desired_width(field_width),
                    )
                    .labelled_by(label.id);
                });
                if save {
                    let token = state.portal.token_draft.trim().to_string();
                    if !token.is_empty() {
                        *action = Some(Action::SavePortalToken(Secret::new(token)));
                    }
                }
                if forget {
                    *action = Some(Action::ForgetPortalToken);
                }
            });
        });
        ui.add_space(theme::space::XXS);
        if stored {
            help_line(ui, "A token is stored in your keychain, never in settings.json.");
        } else {
            help_line(ui, "No token is stored yet. Paste one and press Save.");
        }
    });
}

/// "Sign in instead": Portal's own login, under the access token.
///
/// A bearer token is one way in and this is the other one: the admin's email,
/// the password, and Portal answers with a session cookie. The password is
/// masked, and it is kept nowhere — the note under the row says so and the
/// shell empties the draft the moment the answer lands, whichever way it went.
///
/// A stored session names its admin ("Signed in as …") and offers "Sign out"
/// instead of the two fields; opening the fields again would only ask for a
/// password beside the session that already works.
fn portal_signin_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    // Portal's own name for the session comes first: it is Portal's answer, not
    // the address anybody typed, and it is empty for a bearer token.
    let signed_in_as = state.portal.status.email.trim().to_string();
    if !signed_in_as.is_empty() {
        // The label on the left, "Sign out" on the card's edge, and who it is
        // underneath — the shape the "Enabled" row has.
        let words = format!("Signed in as {signed_in_as}");
        line(ui, |ui| {
            ui.horizontal(|ui| {
                field_label(ui, "Sign in instead");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if secondary_button_enabled(ui, "Sign out", true) {
                        *action = Some(Action::SignOutPortal);
                    }
                });
            });
            ui.add_space(theme::space::XXS);
            help_line(ui, &words);
        });
        return;
    }

    // The field starts on the address the last sign-in saved. What is typed is
    // what gets posted, so a field somebody emptied stays empty.
    let mut email = if state.portal.email_draft.is_empty() {
        state.settings.portal_email.clone()
    } else {
        state.portal.email_draft.clone()
    };
    let signing_in = state.portal.signing_in;
    let ready = !email.trim().is_empty()
        && !state.portal.password_draft.is_empty()
        && !signing_in;
    line(ui, |ui| {
        let label = field_label(ui, "Sign in instead");
        ui.add_space(theme::space::M);
        let field = portal_field(ui, &mut email, PORTAL_EMAIL_HINT).labelled_by(label.id);
        if field.changed() {
            // An email is not a secret, and a typo is worth keeping to be fixed.
            state.portal.email_draft = email.clone();
            state.portal.signin_error = None;
        }
        ui.add_space(theme::space::M);
        ui.horizontal(|ui| {
            // Right to left, as the token row has it: "Sign in" on the card's
            // edge, the masked field taking what is left.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let pressed = primary_button(ui, "Sign in", ready);
                let field_width = (ui.available_width() - theme::space::XXXL).max(60.0);
                ui.scope(|ui| {
                    ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
                    // Named by its hint, the way the composer's passphrase field
                    // is: one row, one name above it, and the field says what it
                    // wants inside itself.
                    ui.add(
                        TextEdit::singleline(&mut state.portal.password_draft)
                            .password(true)
                            .hint_text(PORTAL_PASSWORD_HINT)
                            .margin(Margin::symmetric(theme::space::L as i8, theme::space::M as i8))
                            .desired_width(field_width),
                    );
                });
                if pressed {
                    let email = email.trim().to_string();
                    let password = state.portal.password_draft.clone();
                    if !email.is_empty() && !password.is_empty() {
                        *action = Some(Action::SignInPortal {
                            email,
                            password: Secret::new(password),
                        });
                    }
                }
            });
        });
        ui.add_space(theme::space::XXS);
        match state.portal.signin_error.clone() {
            // The app's own sentence, where the note was: a refused sign-in has
            // something to say and the note does not.
            Some(reason) => reason_line(ui, &reason),
            None if signing_in => help_line(ui, SIGNING_IN),
            None => help_line(ui, PASSWORD_NOTE),
        }
    });
}

/// "One-time code": the six digits that finish a sign-in Portal asked a second
/// factor for.
///
/// The row is drawn only while the app reports `needs_code`, which is one
/// password step away from the fields above it: Portal took the email and the
/// password, and the same session is waiting for this. It goes away — and the
/// digits with it — the moment the code is answered, either way.
///
/// The digits are not masked: six of them, read back against the phone that
/// shows them, is exactly what the row is for. They are still a [`Secret`] on
/// their way to the app, and the draft is emptied as soon as the answer lands.
fn portal_totp_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let signing_in = state.portal.signing_in;
    let ready = state.portal.code_draft.trim().len() == TOTP_DIGITS && !signing_in;
    line(ui, |ui| {
        let label = field_label(ui, "One-time code");
        ui.add_space(theme::space::M);
        ui.horizontal(|ui| {
            // Right to left, as the two rows above have it: "Sign in" on the
            // card's edge, the field taking what is left.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let pressed = primary_button(ui, "Sign in", ready);
                let field_width = (ui.available_width() - theme::space::XXXL).max(60.0);
                ui.scope(|ui| {
                    ui.style_mut().visuals.extreme_bg_color = theme::SURFACE;
                    let field = ui.add(
                        TextEdit::singleline(&mut state.portal.code_draft)
                            .hint_text(PORTAL_CODE_HINT)
                            .char_limit(TOTP_DIGITS)
                            .margin(Margin::symmetric(theme::space::L as i8, theme::space::M as i8))
                            .desired_width(field_width),
                    );
                    field.labelled_by(label.id);
                });
                if pressed {
                    let code = state.portal.code_draft.trim().to_string();
                    if code.len() == TOTP_DIGITS {
                        *action = Some(Action::SignInPortalTotp { code: Secret::new(code) });
                    }
                }
            });
        });
        // A code is six digits: anything else typed or pasted — a space, a dash,
        // a letter from a copied "123 456" — is dropped rather than sent.
        let digits: String = state.portal.code_draft.chars().filter(|c| c.is_ascii_digit()).collect();
        state.portal.code_draft = digits;
        ui.add_space(theme::space::XXS);
        match state.portal.code_error.clone() {
            // Portal's own sentence: a wrong code and a sign-in that must start
            // again do not read the same.
            Some(reason) => reason_line(ui, &reason),
            None if signing_in => help_line(ui, SIGNING_IN),
            None => help_line(ui, PORTAL_CODE_NOTE),
        }
    });
}

/// "Local folder": the folder the marker names go under, with the absolute path
/// it resolves to beneath it.
fn portal_root_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut text = state.portal.root_draft.clone().unwrap_or_else(|| state.settings.portal_root.clone());
    let home = state.home.clone();
    line(ui, |ui| {
        let label = field_label(ui, "Local folder");
        ui.add_space(theme::space::M);
        let response = portal_field(ui, &mut text, PORTAL_ROOT_HINT).labelled_by(label.id);
        if response.changed() {
            state.portal.root_draft = Some(text.clone());
            state.portal.root_error = None;
        }
        if response.lost_focus() {
            match portal_root_reason(&text) {
                None => {
                    let root = text.trim().to_string();
                    state.settings.portal_root = root.clone();
                    state.portal.root_draft = None;
                    state.portal.root_error = None;
                    *action = Some(Action::SetPortalRoot(root));
                }
                Some(reason) => {
                    state.portal.root_draft = Some(text.clone());
                    state.portal.root_error = Some(reason);
                }
            }
        }
        ui.add_space(theme::space::XXS);
        match state.portal.root_error.clone() {
            Some(reason) => reason_line(ui, &reason),
            None => {
                // The resolved path, so nobody has to guess where the folders go.
                let resolved = resolved_portal_root(
                    state.portal.root_draft.as_deref().unwrap_or(&state.settings.portal_root),
                    &home,
                );
                help_line(ui, &format!("Folders are created under {resolved}"));
            }
        }
    });
}

/// "Enabled": the label on the left, the box on the right, and what the state
/// means underneath — off is a promise that nothing is contacted.
fn portal_enabled_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let mut enabled = state.settings.portal_enabled;
    let every = state.settings.portal_every_secs;
    line_with(ui, CHECK_ROW_CONTROL, |ui| {
        ui.horizontal(|ui| {
            let label = field_label(ui, "Enabled");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().interact_size.y = CHECK_ROW_CONTROL;
                let boxed = ui.add(Checkbox::without_text(&mut enabled)).labelled_by(label.id);
                if boxed.changed() {
                    *action = Some(Action::SetPortalEnabled(enabled));
                }
            });
        });
        ui.add_space(theme::space::XXS);
        if enabled {
            let every = if every >= 5 { every } else { 30 };
            help_line(ui, &format!("Checked every {every} s in the background."));
        } else {
            help_line(ui, "Nothing is contacted while this is off.");
        }
    });
    state.settings.portal_enabled = enabled;
}

/// The status line: the last reconcile, at a glance.
fn portal_status_line(ui: &mut egui::Ui, state: &mut UiState) {
    let words = portal_line(&state.portal.status);
    line(ui, |ui| {
        field_label(ui, "Status");
        ui.add_space(theme::space::XXS);
        let colour = if words.failed { theme::ERROR_TEXT } else { theme::TEXT_2 };
        ui.add(theme::wrapping(
            RichText::new(&words.text)
                .font(theme::body())
                .line_height(Some(HELP_LINE))
                .color(colour),
        ))
        .on_hover_text(&words.text);
    });
}

/// "Reconcile now": the button, and why it cannot be pressed when it cannot.
fn portal_reconcile_line(ui: &mut egui::Ui, state: &mut UiState, action: &mut Option<Action>) {
    let status = &state.portal.status;
    let ready = status.configured();
    let reason = if status.url.trim().is_empty() {
        "Add the Portal address first."
    } else if !status.signed_in {
        "Add the access token first."
    } else {
        "Keeps one watch folder per live link, now."
    };
    line(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 150.0).max(80.0));
                ui.spacing_mut().interact_size.y = LABEL_LINE;
                field_label(ui, "Reconcile");
                ui.add_space(theme::space::XXS);
                help_line(ui, reason);
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if secondary_button_enabled(ui, "Reconcile now", ready) {
                    *action = Some(Action::ReconcileNow);
                }
            });
        });
    });
}

// ---------- the pieces of the layout ----------

/// `.set-title`: 14 px, 600, `--text`.
fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .font(theme::section())
            .line_height(Some(TITLE_LINE))
            .color(theme::TEXT)
            .strong(),
    );
}

/// `.set-line label`: 13 px, 500, `--text`.
fn field_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(
        RichText::new(text)
            .font(theme::body())
            .line_height(Some(LABEL_LINE))
            .color(theme::TEXT)
            .strong(),
    )
}

/// `.set-card`: `--panel`, 1 px `--border`, 12 px radius, no padding — the
/// lines pad themselves so the separators reach the card's edges.
fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    theme::panel_card_frame().show(ui, |ui| {
        // `.settings { max-width: 680px }` binds the card, and the lines inside
        // take the card's width so every right-aligned control ends on the same
        // edge.
        ui.set_max_width(ui.available_width().min(SETTINGS_MAX_WIDTH));
        ui.set_width(ui.available_width());
        add(ui);
    });
}

/// `.set-line + .set-line { border-top: 1px solid var(--border) }`: a hairline
/// that reaches both edges of the card, with no gap around it.
fn rule(ui: &mut egui::Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, theme::hairline(theme::BORDER));
}

/// `.set-line { padding: 14px 16px }`.
fn line(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    line_with(ui, theme::size::CONTROL_H, add);
}

/// `.set-line` whose tallest control is `control` px tall.
///
/// `Ui::horizontal` gives its row the height of `spacing.interact_size.y`, so a
/// row whose only control is a 15 px checkbox has to be told it is 19 px tall —
/// otherwise every toggle would be a 32 px button row.
fn line_with(ui: &mut egui::Ui, control: f32, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new().inner_margin(LINE_PADDING).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().interact_size.y = control;
        add(ui);
    });
}

/// `.btn.primary`: `--brand` fill, white text, 32 px tall. `enabled == false`
/// draws it at 40 %, like `.btn:disabled { opacity: .4 }`.
fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let mut clicked = false;
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = theme::BRAND;
            widgets.hovered.weak_bg_fill = theme::BRAND_HOVER;
            widgets.active.weak_bg_fill = theme::BRAND_HOVER;
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.hovered.bg_stroke = Stroke::NONE;
            widgets.active.bg_stroke = Stroke::NONE;
            widgets.inactive.fg_stroke = theme::hairline(theme::WHITE);
            widgets.hovered.fg_stroke = theme::hairline(theme::WHITE);
            widgets.active.fg_stroke = theme::hairline(theme::WHITE);
        }
        clicked = ui
            .add_enabled(
                enabled,
                egui::Button::new(
                    RichText::new(text)
                        .font(theme::body())
                        .color(theme::WHITE),
                )
                .min_size(vec2(0.0, theme::size::CONTROL_H)),
            )
            .clicked();
    });
    clicked
}

/// `.btn` that can be disabled (`.btn:disabled { opacity: .4 }`).
fn secondary_button_enabled(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let mut clicked = false;
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = theme::CARD;
            widgets.hovered.weak_bg_fill = theme::RAISED;
            widgets.active.weak_bg_fill = theme::RAISED;
            widgets.inactive.bg_stroke = theme::stroke_border_2();
            widgets.hovered.bg_stroke = theme::hairline(theme::BORDER_3);
            widgets.inactive.fg_stroke = theme::hairline(theme::TEXT_2);
            widgets.hovered.fg_stroke = theme::hairline(theme::TEXT);
            widgets.inactive.corner_radius = theme::R_S;
            widgets.hovered.corner_radius = theme::R_S;
            widgets.active.corner_radius = theme::R_S;
        }
        clicked = ui
            .add_enabled(
                enabled,
                egui::Button::new(RichText::new(text).font(theme::body()).color(theme::TEXT_2))
                    .min_size(vec2(0.0, theme::size::CONTROL_H)),
            )
            .clicked();
    });
    clicked
}

/// `a` in the stylesheet: 12.5 px, `#9a95ff`, lighter on hover.
fn link_button(ui: &mut egui::Ui, text: &str) -> bool {
    let mut clicked = false;
    ui.scope(|ui| {
        {
            let widgets = &mut ui.style_mut().visuals.widgets;
            widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            widgets.hovered.weak_bg_fill = Color32::TRANSPARENT;
            widgets.active.weak_bg_fill = Color32::TRANSPARENT;
            widgets.inactive.bg_stroke = Stroke::NONE;
            widgets.hovered.bg_stroke = Stroke::NONE;
            widgets.active.bg_stroke = Stroke::NONE;
            widgets.inactive.fg_stroke = theme::hairline(theme::LINK);
            widgets.hovered.fg_stroke = theme::hairline(theme::LINK_HOVER);
            widgets.active.fg_stroke = theme::hairline(theme::LINK);
        }
        clicked = ui
            .add(
                egui::Button::new(
                    RichText::new(text)
                        .font(theme::note())
                        .color(theme::LINK),
                )
                .min_size(vec2(0.0, 22.0)),
            )
            .clicked();
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Settings, UpdateState, User};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT as _, Queryable};

    fn state(settings: Settings, logged_in: bool) -> UiState {
        UiState {
            logged_in,
            settings,
            version: "0.9.3".to_string(),
            open_at_login: false,
            log_dir: "/Users/chaos/Library/Logs/FioTransfer".to_string(),
            home: "/Users/chaos".to_string(),
            ..UiState::default()
        }
    }

    fn configured() -> Settings {
        Settings {
            client_id: String::new(),
            redirect_port: 8765,
            streams: 16,
            segment_mb: 32,
            download_dir: "/Users/chaos/Downloads".to_string(),
            no_folder_logs: false,
            no_update_check: false,
            last_folder_label: String::new(),
            ..Settings::default()
        }
    }

    fn screen_harness(state: UiState) -> Harness<'static, UiState> {
        Harness::builder()
            .with_size(vec2(956.0, 2400.0))
            .build_state(
                |ctx, state: &mut UiState| {
                    crate::app::apply_style_once(ctx);
                    egui::CentralPanel::default().show(ctx, |ui| show(ui, state));
                },
                state,
            )
    }

    /// The toggle boxes in draw order, which is the order of the rows: "Open at
    /// login" and "Check for updates" (App), then "Save log with media"
    /// (Storage), then Portal's "Enabled". Each box's text label is drawn to its
    /// left, the way `.set-line` puts a `label` beside a `.check` input.
    fn toggle_boxes<'a>(harness: &'a Harness<'a, UiState>) -> Vec<egui_kittest::Node<'a>> {
        harness
            .get_all_by_role(egui::accesskit::Role::CheckBox)
            .collect()
    }

    #[test]
    fn signed_out_settings_shows_the_client_id_setup_and_every_section() {
        let harness = screen_harness(state(configured(), false));

        // `index.html`'s titles, in its order: account, App, Performance, Storage.
        for heading in ["Settings", "Frame.io account", "App", "Performance", "Storage"] {
            harness.get_by_label(heading);
        }
        harness.get_by_label("Not set up");
        harness.get_by_label("Sign in with Adobe");
        harness.get_by_label("Adobe Client ID");
        // Two "Save" buttons now (the Client ID's and the Portal token's): the
        // first is this row's.
        harness.get_all_by_label("Save").next().expect("a Save button");
        harness.get_by_label("Setup");
        harness.get_by_label("Copy");
        harness.get_by_label("Parallel streams");
        harness.get_by_label("16");
        harness.get_by_label("Chunk size");
    }

    #[test]
    fn the_app_card_has_the_login_version_and_update_rows() {
        let harness = screen_harness(state(configured(), true));

        harness.get_by_label("Open at login");
        harness.get_by_label("Version");
        harness.get_by_label("0.9.3");
        harness.get_by_label("Check now");
        harness.get_by_label("Check for updates");
    }

    #[test]
    fn the_storage_card_has_the_folder_the_log_toggle_and_the_app_log() {
        let harness = screen_harness(state(configured(), true));

        harness.get_by_label("Download location");
        harness.get_by_label("~/Downloads");
        harness.get_by_label("Change…");
        harness.get_by_label("Save log with media");
        harness.get_by_label("App log");
        harness.get_by_label("~/Library/Logs/FioTransfer");
        harness.get_by_label("Show in Finder");
    }

    #[test]
    fn the_about_line_names_the_build() {
        let harness = screen_harness(state(configured(), true));
        harness.get_by_label("FioTransfer 0.9.3");
    }

    #[test]
    fn the_version_number_falls_back_to_the_screen_version() {
        let mut ui_state = state(configured(), true);
        ui_state.update.current = "9.9.9".to_string();
        assert_eq!(update_current(&ui_state), "9.9.9");
        screen_harness(ui_state).get_by_label("9.9.9");

        let mut ui_state = state(configured(), true);
        ui_state.update.current = String::new();
        assert_eq!(update_current(&ui_state), "0.9.3");
        screen_harness(ui_state).get_by_label("0.9.3");
    }

    #[test]
    fn the_update_line_speaks_the_webview_words() {
        let mut ui_state = state(configured(), true);

        // Nothing checked yet: no help text, and the button offers a check.
        let idle = update_line(&ui_state);
        assert_eq!(idle.help, "");
        assert_eq!(idle.button, "Check now");
        assert!(idle.button_enabled);

        ui_state.update = UpdateState {
            current: "0.9.3".to_string(),
            state: "checking".to_string(),
            ..UpdateState::default()
        };
        let checking = update_line(&ui_state);
        assert_eq!(checking.help, "Checking…");
        assert!(!checking.button_enabled, "a check in flight disables the button");

        ui_state.update = UpdateState {
            latest: "0.9.5".to_string(),
            available: true,
            state: "idle".to_string(),
            ..UpdateState::default()
        };
        let available = update_line(&ui_state);
        assert_eq!(available.help, "0.9.5 available");
        assert_eq!(available.button, "Update");
        assert!(available.button_primary);

        ui_state.update.blocker = "download only".to_string();
        let blocked = update_line(&ui_state);
        assert_eq!(blocked.help, "0.9.5 available · download only");
        assert_eq!(blocked.button, "Download");
        assert!(!blocked.button_primary);

        ui_state.update = UpdateState {
            state: "downloading".to_string(),
            progress: 0.42,
            ..UpdateState::default()
        };
        let downloading = update_line(&ui_state);
        assert_eq!(downloading.help, "Downloading… 42%");
        assert!(!downloading.button_enabled);

        ui_state.update = UpdateState {
            blocker: "development build".to_string(),
            ..UpdateState::default()
        };
        let dev = update_line(&ui_state);
        assert_eq!(dev.help, "Development build");
        assert!(dev.is_dev());
        assert!(!dev.button_enabled, "a development build cannot update itself");
    }

    #[test]
    fn a_development_build_hides_the_update_toggle() {
        let mut ui_state = state(configured(), true);
        ui_state.update.blocker = "development build".to_string();
        let harness = screen_harness(ui_state);

        harness.get_by_label("Development build");
        assert!(
            harness.query_by_label("Check for updates").is_none(),
            "`renderUpdate` hides the auto-update row in a development build"
        );
        assert!(harness.query_by_label("Check now").is_none());
    }

    #[test]
    fn the_update_error_is_shown_under_the_version() {
        let mut ui_state = state(configured(), true);
        ui_state.update.error = "Could not reach the release page".to_string();
        let harness = screen_harness(ui_state);
        harness.get_by_label("Could not reach the release page");
    }

    #[test]
    fn signed_in_settings_offers_sign_out_and_names_the_user() {
        let mut settings = Settings::default();
        settings.client_id = "abc123".to_string();
        let mut ui_state = state(settings, true);
        ui_state.user = Some(User {
            id: "user-1".to_string(),
            name: "Ada Lovelace".to_string(),
            email: "ada@example.com".to_string(),
        });
        let harness = screen_harness(ui_state);

        harness.get_by_label("Signed in as Ada Lovelace · ada@example.com");
        harness.get_by_label("Sign out");
        assert!(harness.query_by_label("Sign in with Adobe").is_none());
    }

    #[test]
    fn a_client_id_turns_not_set_up_into_not_signed_in() {
        let mut settings = Settings::default();
        settings.client_id = "abc123".to_string();
        let harness = screen_harness(state(settings, false));

        harness.get_by_label("Not signed in");
        harness.get_by_label("Sign in with Adobe");
    }

    #[test]
    fn a_signed_in_user_without_a_name_still_reads_sensibly() {
        let mut ui_state = state(Settings::default(), true);
        ui_state.user = None;
        assert_eq!(sign_in_words(&ui_state), "Signed in");

        ui_state.user = Some(User {
            id: String::new(),
            name: "  ".to_string(),
            email: "ada@example.com".to_string(),
        });
        assert_eq!(sign_in_words(&ui_state), "Signed in · ada@example.com");

        ui_state.user = Some(User {
            id: String::new(),
            name: "Ada".to_string(),
            email: String::new(),
        });
        assert_eq!(sign_in_words(&ui_state), "Signed in as Ada");
    }

    #[test]
    fn the_three_toggles_write_their_settings() {
        let mut harness = screen_harness(state(Settings::default(), true));
        assert!(!harness.state().open_at_login);
        assert!(!harness.state().settings.no_update_check);
        assert!(!harness.state().settings.no_folder_logs);
        assert_eq!(toggle_boxes(&harness).len(), 4, "four toggle rows: the three above, and Portal's");

        // Open at login.
        {
            let boxes = toggle_boxes(&harness);
            if let Some(boxed) = boxes.first() {
                boxed.click();
            }
        }
        harness.run();
        assert!(harness.state().open_at_login);

        // Check for updates.
        {
            let boxes = toggle_boxes(&harness);
            if let Some(boxed) = boxes.get(1) {
                boxed.click();
            }
        }
        harness.run();
        assert!(harness.state().settings.no_update_check);

        // Save log with media.
        {
            let boxes = toggle_boxes(&harness);
            if let Some(boxed) = boxes.get(2) {
                boxed.click();
            }
        }
        harness.run();
        assert!(harness.state().settings.no_folder_logs);
    }

    #[test]
    fn the_streams_value_follows_the_slider_and_stays_in_range() {
        let mut settings = Settings::default();
        settings.streams = 16;
        let harness = screen_harness(state(settings, true));
        harness.get_by_label("16");

        // A stored value outside 1–128 is shown clamped, never dropped.
        assert_eq!(clamped_streams(0), STREAMS_MIN);
        assert_eq!(clamped_streams(999), STREAMS_MAX);
        assert_eq!(clamped_streams(64), 64);

        let mut settings = Settings::default();
        settings.streams = 999;
        let clamped = screen_harness(state(settings, true));
        clamped.get_by_label("128");
        assert_eq!(clamped.state().settings.streams, STREAMS_MAX);
    }

    #[test]
    fn a_segment_size_outside_the_range_is_clamped_not_dropped() {
        assert_eq!(clamped_segment(0), SEGMENT_MIN);
        assert_eq!(clamped_segment(4096), SEGMENT_MAX);
        assert_eq!(clamped_segment(256), 256);

        let mut settings = Settings::default();
        settings.segment_mb = 4096;
        let harness = screen_harness(state(settings, true));
        harness.get_by_label("Chunk size");
        assert_eq!(harness.state().settings.segment_mb, SEGMENT_MAX);
    }

    #[test]
    fn the_redirect_uri_is_built_from_the_port_and_falls_back() {
        assert_eq!(redirect_uri(8765), "https://localhost:8765/callback");
        assert_eq!(redirect_uri(0), "https://localhost:8765/callback");
        assert_eq!(redirect_uri(-4), "https://localhost:8765/callback");
        assert_eq!(redirect_uri(70000), "https://localhost:8765/callback");
        assert_eq!(redirect_uri(9000), "https://localhost:9000/callback");
        assert_eq!(display_path("", "/Users/chaos"), "—");
        assert_eq!(
            display_path("  /Users/chaos/Downloads ", "/Users/chaos"),
            "~/Downloads"
        );
        assert_eq!(display_path("/Volumes/Raid/x", ""), "/Volumes/Raid/x");
        assert_eq!(short_path("/Users/chaos", "/Users/chaos"), "~");
    }

    #[test]
    fn copy_reports_the_redirect_uri() {
        let seen: std::rc::Rc<std::cell::RefCell<Vec<Action>>> = Default::default();
        let sink = seen.clone();
        let mut harness = Harness::builder()
            .with_size(vec2(956.0, 2400.0))
            .build_state(
                move |ctx, state: &mut UiState| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if let Some(action) = show_with_actions(ui, state) {
                            sink.borrow_mut().push(action);
                        }
                    });
                },
                state(Settings::default(), true),
            );

        harness.get_by_label("Copy").click();
        harness.run();

        assert!(
            seen.borrow()
                .contains(&Action::CopyRedirect(redirect_uri(8765))),
            "actions seen: {:?}",
            seen.borrow()
        );
    }

    #[test]
    fn show_in_finder_and_check_now_report_their_actions() {
        let seen: std::rc::Rc<std::cell::RefCell<Vec<Action>>> = Default::default();
        let sink = seen.clone();
        let mut harness = Harness::builder()
            .with_size(vec2(956.0, 2400.0))
            .build_state(
                move |ctx, state: &mut UiState| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        if let Some(action) = show_with_actions(ui, state) {
                            sink.borrow_mut().push(action);
                        }
                    });
                },
                state(Settings::default(), true),
            );

        harness.get_by_label("Check now").click();
        harness.run();
        assert!(seen.borrow().contains(&Action::UpdateClicked), "{:?}", seen.borrow());

        harness.get_by_label("Show in Finder").click();
        harness.run();
        assert!(seen.borrow().contains(&Action::ShowLogs), "{:?}", seen.borrow());
    }

    #[test]
    fn saving_the_client_id_writes_the_field_and_asks_the_app_to_save() {
        let mut ui_state = state(Settings::default(), true);
        ui_state.settings.client_id = "  abc123  ".to_string();
        let mut harness = screen_harness(ui_state);

        harness.get_all_by_label("Save").next().expect("the Client ID's Save").click();
        harness.run();

        assert_eq!(harness.state().settings.client_id, "abc123");
        assert!(
            harness.state().requests.contains(&Request::SaveSettings),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn copy_and_change_ask_the_app() {
        let mut harness = screen_harness(state(Settings::default(), true));

        harness.get_by_label("Copy").click();
        harness.run();
        assert!(
            harness
                .state()
                .requests
                .contains(&Request::CopyText(redirect_uri(8765))),
            "requests: {:?}",
            harness.state().requests
        );

        harness.get_by_label("Change…").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::PickDownloadDir),
            "requests: {:?}",
            harness.state().requests
        );

        harness.get_by_label("Show in Finder").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::OpenLogs),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn sign_out_and_sign_in_ask_the_app() {
        let mut harness = screen_harness(state(Settings::default(), true));
        harness.get_by_label("Sign out").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::Logout),
            "requests: {:?}",
            harness.state().requests
        );

        // Signed out with a client ID: the sign-in button is live.
        let mut settings = Settings::default();
        settings.client_id = "abc123".to_string();
        let mut harness = screen_harness(state(settings, false));
        harness.get_by_label("Sign in with Adobe").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::Login),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn check_now_checks_and_update_installs() {
        let mut harness = screen_harness(state(Settings::default(), true));

        harness.get_by_label("Check now").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::CheckUpdate),
            "requests: {:?}",
            harness.state().requests
        );

        // An available release turns the same button into "Update".
        harness.state_mut().update.available = true;
        harness.state_mut().update.latest = "9.9.9".to_string();
        harness.run();
        harness.get_by_label("Update").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::InstallUpdate),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn the_toggles_ask_the_app_for_what_they_wrote() {
        let mut harness = screen_harness(state(Settings::default(), true));

        // "Open at login" is its own call, not a settings save.
        {
            let boxes = toggle_boxes(&harness);
            if let Some(boxed) = boxes.first() {
                boxed.click();
            }
        }
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::SetOpenAtLogin(true)),
            "requests: {:?}",
            harness.state().requests
        );

        // "Check for updates" writes `no_update_check` and saves it.
        harness.state_mut().requests.clear();
        {
            let boxes = toggle_boxes(&harness);
            if let Some(boxed) = boxes.get(1) {
                boxed.click();
            }
        }
        harness.run();
        assert!(harness.state().settings.no_update_check);
        assert!(
            harness.state().requests.contains(&Request::SaveSettings),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn an_empty_folder_and_log_shows_an_em_dash_for_each() {
        // The core's `Settings::default()` fills in `~/Downloads`; an unset
        // folder is the empty string, which is what an em dash stands for.
        let mut settings = Settings::default();
        settings.download_dir = String::new();
        let mut ui_state = state(settings, true);
        ui_state.log_dir = String::new();
        let harness = screen_harness(ui_state);

        harness.get_by_label("Download location");
        harness.get_by_label("App log");
        assert_eq!(
            harness.get_all_by_label("—").count(),
            2,
            "both the folder and the log path are unset"
        );
    }

    // ---------- the Portal section ----------

    /// The moment the Portal cases are drawn at, so "2 min ago" is a number and
    /// not a race with the clock.
    const NOW: i64 = 1_793_266_860_000;

    /// The screen with a Portal that is set up: an address, a token in the
    /// keychain, the default folder, the poller on, and a reconcile that ran two
    /// minutes before `NOW` over two links.
    fn portal_state() -> UiState {
        let mut ui_state = state(configured(), true);
        ui_state.settings.portal_url = "https://portal.example.com".to_string();
        ui_state.settings.portal_root = "/Users/chaos/Live_Work".to_string();
        ui_state.settings.portal_enabled = true;
        ui_state.portal.status = PortalView {
            enabled: true,
            signed_in: true,
            needs_code: false,
            // A bearer token names no admin: the sign-in row is the signed-out
            // one, and "Signed in as …" belongs to `session_state`.
            email: String::new(),
            url: "https://portal.example.com".to_string(),
            root: "/Users/chaos/Live_Work".to_string(),
            running: false,
            last_at: NOW - 120_000,
            links_seen: 2,
            ensured: 3,
            removed: 1,
            skipped: 0,
            error: String::new(),
            now_ms: NOW,
        };
        ui_state
    }

    /// The whole node tree's words: every label and every value kittest can
    /// reach, which is everything the screen renders as text.
    fn every_word(harness: &Harness<'_, UiState>) -> Vec<String> {
        use egui_kittest::kittest::NodeT as _;
        let mut words: Vec<String> = Vec::new();
        for node in harness.root().children_recursive() {
            let node = node.accesskit_node();
            if let Some(label) = node.label() {
                words.push(label);
            }
            if let Some(value) = node.value() {
                words.push(value);
            }
        }
        words
    }

    #[test]
    fn the_portal_section_is_the_last_card_and_names_its_five_rows() {
        let harness = screen_harness(portal_state());

        harness.get_by_label("Portal");
        harness.get_by_label("Server URL");
        harness.get_by_label("Access token");
        harness.get_by_label("Local folder");
        harness.get_by_label("Enabled");
        harness.get_by_label("Status");
        harness.get_by_label("Reconcile");
        harness.get_by_label("Reconcile now");
        // The address and the folder are what the settings hold, and the folder
        // says where the folders really go.
        assert!(every_word(&harness).contains(&"https://portal.example.com".to_string()));
        harness.get_by_label("Folders are created under /Users/chaos/Live_Work");
    }

    #[test]
    fn the_portal_folder_shows_the_resolved_absolute_path() {
        // A `~` in the setting is expanded the way `Settings::portal_root_path`
        // expands it, so the row never shows a path nobody can check.
        assert_eq!(resolved_portal_root("~/Live_Work", "/Users/chaos"), "/Users/chaos/Live_Work");
        assert_eq!(resolved_portal_root("/Volumes/Raid/Live", "/Users/chaos"), "/Volumes/Raid/Live");
        // Empty means the default, which is `~/Live_Work`.
        assert_eq!(resolved_portal_root("", "/Users/chaos"), "/Users/chaos/Live_Work");
        assert_eq!(resolved_portal_root("~", "/Users/chaos"), "/Users/chaos");
        assert_eq!(resolved_portal_root("~ada/Live", "/Users/chaos"), "~ada/Live", "another user's home is taken as given");
        assert_eq!(resolved_portal_root("/Users/chaos/", "/Users/chaos/"), "/Users/chaos/");

        let mut ui_state = portal_state();
        ui_state.settings.portal_root = "~/Live_Work".to_string();
        let harness = screen_harness(ui_state);
        harness.get_by_label("Folders are created under /Users/chaos/Live_Work");
    }

    #[test]
    fn a_bad_portal_url_is_refused_and_the_old_value_kept() {
        let mut harness = screen_harness(portal_state());

        // Type a scheme-less address over the one in the field, then commit it
        // the way the field commits: Enter, which is also a lost focus.
        harness.get_by_label("Server URL").focus();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_label("Server URL").type_text("portal.example.com");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        harness.get_by_label(PORTAL_URL_REASON);
        assert_eq!(
            harness.state().settings.portal_url,
            "https://portal.example.com",
            "a refused address is never the one in the settings"
        );
        assert!(
            !harness.state().requests.contains(&Request::SetPortalUrl("portal.example.com".to_string())),
            "nothing was asked for: {:?}",
            harness.state().requests
        );
        assert!(
            !harness.state().requests.iter().any(|r| matches!(r, Request::SetPortalUrl(_))),
            "no Portal URL was written: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn a_good_portal_url_is_committed_and_asks_the_app() {
        let mut harness = screen_harness(state(configured(), true));
        assert!(harness.state().settings.portal_url.is_empty());

        harness.get_by_label("Server URL").focus();
        harness.run();
        harness.get_by_label("Server URL").type_text("https://portal.example.com/");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert!(
            harness
                .state()
                .requests
                .contains(&Request::SetPortalUrl("https://portal.example.com".to_string())),
            "requests: {:?}",
            harness.state().requests
        );
        assert_eq!(harness.state().settings.portal_url, "https://portal.example.com");
        assert_eq!(harness.state().portal.url_draft, None, "the field shows the setting again");
        assert!(harness.query_by_label(PORTAL_URL_REASON).is_none());
    }

    #[test]
    fn the_portal_url_scheme_rule_is_the_apps_and_a_host_is_required() {
        assert_eq!(portal_url_reason(""), None, "empty is how Portal is switched off");
        assert_eq!(portal_url_reason("  https://portal.example.com  "), None);
        assert_eq!(portal_url_reason("http://192.168.1.9:8080"), None);
        for bad in ["portal.example.com", "ftp://portal.example.com", "file:///etc/passwd", "//portal.example.com"] {
            assert_eq!(portal_url_reason(bad).as_deref(), Some(PORTAL_URL_REASON), "{bad}");
        }
        assert!(portal_url_reason("https://").is_some(), "a scheme with no host is not an address");
        assert!(portal_url_reason("https:///").is_some());
        assert!(portal_url_reason("https://portal example.com").is_some(), "a space is not an address");
    }

    #[test]
    fn the_portal_folder_rule_keeps_the_disk_root_out() {
        assert_eq!(portal_root_reason(""), None, "empty means the default");
        assert_eq!(portal_root_reason("~/Live_Work"), None);
        assert_eq!(portal_root_reason("/Volumes/Raid/Live_Work"), None);
        assert!(portal_root_reason("/").is_some(), "the whole disk is not a mirror root");
        assert!(portal_root_reason("Live_Work").is_some(), "a relative path is not a root");
        assert!(portal_root_reason("/tmp/a\nb").is_some());
    }

    #[test]
    fn the_portal_toggle_asks_the_app_for_what_it_wrote() {
        let mut harness = screen_harness(state(configured(), true));
        assert!(!harness.state().settings.portal_enabled);

        // The four toggles in draw order: Open at login, Check for updates, Save
        // log with media, and Portal's own.
        let boxes = toggle_boxes(&harness);
        assert_eq!(boxes.len(), 4, "the Portal section adds the fourth toggle");
        if let Some(boxed) = boxes.get(3) {
            boxed.click();
        }
        harness.run();

        assert!(harness.state().settings.portal_enabled);
        assert!(
            harness.state().requests.contains(&Request::SetPortalEnabled(true)),
            "requests: {:?}",
            harness.state().requests
        );

        // And back off again.
        harness.state_mut().requests.clear();
        let boxes = toggle_boxes(&harness);
        if let Some(boxed) = boxes.get(3) {
            boxed.click();
        }
        harness.run();
        assert!(!harness.state().settings.portal_enabled);
        assert!(
            harness.state().requests.contains(&Request::SetPortalEnabled(false)),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn nothing_is_contacted_while_portal_is_off() {
        let harness = screen_harness(state(configured(), true));
        harness.get_by_label("Nothing is contacted while this is off.");

        let mut ui_state = portal_state();
        ui_state.settings.portal_enabled = true;
        let harness = screen_harness(ui_state);
        harness.get_by_label("Checked every 30 s in the background.");
    }

    #[test]
    fn reconcile_now_waits_for_an_address_and_a_token() {
        // Neither: the button is there, dead, and says which one is missing.
        let harness = screen_harness(state(configured(), true));
        let button = harness.get_by_label("Reconcile now");
        assert!(button.accesskit_node().is_disabled(), "no address and no token");
        harness.get_by_label("Add the Portal address first.");

        // An address but no token: the token is what is missing.
        let mut ui_state = state(configured(), true);
        ui_state.settings.portal_url = "https://portal.example.com".to_string();
        ui_state.portal.status.url = "https://portal.example.com".to_string();
        let harness = screen_harness(ui_state);
        assert!(harness.get_by_label("Reconcile now").accesskit_node().is_disabled());
        harness.get_by_label("Add the access token first.");

        // Both: the button is live.
        let harness = screen_harness(portal_state());
        let button = harness.get_by_label("Reconcile now");
        assert!(!button.accesskit_node().is_disabled(), "an address and a token are both set");
    }

    #[test]
    fn reconcile_now_asks_the_app() {
        let mut harness = screen_harness(portal_state());
        harness.get_by_label("Reconcile now").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::PortalReconcile),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn the_status_line_reads_a_finished_reconcile_as_its_numbers() {
        let words = portal_line(&portal_state().portal.status);
        assert_eq!(words.text, "2 min ago · 2 links, 3 folders kept in step, 1 removed");
        assert!(!words.failed);

        let harness = screen_harness(portal_state());
        harness.get_by_label("2 min ago · 2 links, 3 folders kept in step, 1 removed");
    }

    #[test]
    fn the_status_line_reads_a_failure_as_a_failure() {
        let mut ui_state = portal_state();
        ui_state.portal.status.error = "can't reach Portal: connection refused".to_string();
        // A failed report has no numbers, and the line must not invent any.
        ui_state.portal.status.links_seen = 0;
        ui_state.portal.status.ensured = 0;
        ui_state.portal.status.removed = 0;
        let words = portal_line(&ui_state.portal.status);
        assert!(words.failed, "a failed reconcile is drawn as bad news");
        assert_eq!(words.text, "Failed 2 min ago: can't reach Portal: connection refused");
        assert!(!words.text.contains("links"), "a failed run claims no folders: {}", words.text);

        let harness = screen_harness(ui_state);
        harness.get_by_label("Failed 2 min ago: can't reach Portal: connection refused");
        assert!(
            harness.query_by_label("2 min ago · 2 links, 3 folders kept in step, 1 removed").is_none(),
            "the old numbers are gone"
        );
    }

    #[test]
    fn the_status_line_says_what_is_missing_before_it_says_anything_else() {
        let mut status = PortalView::default();
        // Nothing at all: not set up, and it is not an error.
        let words = portal_line(&status);
        assert_eq!(words.text, "Not set up: no Portal address and no token yet.");
        assert!(!words.failed);

        // A token but no address, and the other way round: each names what is
        // missing, and both are reasons nothing can be reconciled.
        status.signed_in = true;
        assert_eq!(portal_line(&status).text, "No Portal address yet: add one above.");
        assert!(portal_line(&status).failed);

        status.signed_in = false;
        status.url = "https://portal.example.com".to_string();
        assert_eq!(
            portal_line(&status).text,
            "Not signed in yet: save an access token above, or sign in below."
        );
        assert!(portal_line(&status).failed);

        // Configured, and never run: no numbers to claim yet.
        status.signed_in = true;
        let words = portal_line(&status);
        assert_eq!(words.text, "Configured. Press Reconcile now, or switch it on to check every 30 s.");
        assert!(!words.failed);

        // A reconcile in flight says so.
        status.running = true;
        assert_eq!(portal_line(&status).text, "Reconciling…");

        // One link, one folder, nothing removed: singular words, and skipped
        // links named when there are any.
        let done = PortalView {
            running: false,
            last_at: NOW - 1000,
            links_seen: 1,
            ensured: 1,
            removed: 0,
            skipped: 2,
            now_ms: NOW,
            ..status
        };
        assert_eq!(
            portal_line(&done).text,
            "just now · 1 link, 1 folder kept in step, 0 removed, 2 skipped"
        );
    }

    #[test]
    fn the_relative_time_is_readable_at_a_glance() {
        assert_eq!(portal_when(0, NOW), "never");
        assert_eq!(portal_when(NOW - 900, NOW), "just now");
        assert_eq!(portal_when(NOW - 30_000, NOW), "under a minute ago");
        assert_eq!(portal_when(NOW - 60_000, NOW), "1 min ago");
        assert_eq!(portal_when(NOW - 120_000, NOW), "2 min ago");
        assert_eq!(portal_when(NOW - 3_600_000, NOW), "1 h ago");
        assert_eq!(portal_when(NOW - 5 * 3_600_000, NOW), "5 h ago");
        assert_eq!(portal_when(NOW - 86_400_000, NOW), "1 day ago");
        assert_eq!(portal_when(NOW - 3 * 86_400_000, NOW), "3 days ago");
        assert_eq!(portal_when(NOW + 60_000, NOW), "just now", "a clock that went backwards is not a negative age");
    }

    #[test]
    fn the_token_is_never_rendered_and_never_a_setting() {
        let mut harness = screen_harness(portal_state());
        let token = "sekret-token-9f2c1a";

        // Type it into the field: what is drawn is the mask, not the token.
        harness.get_by_label("Access token").focus();
        harness.run();
        harness.get_by_label("Access token").type_text(token);
        harness.run();

        for word in every_word(&harness) {
            assert!(!word.contains(token), "a rendered label carries the token: {word:?}");
        }

        // Saving it asks for the keychain, and the token is nowhere in the
        // settings that get written to settings.json.
        harness.get_all_by_label("Save").last().expect("the token's Save").click();
        harness.run();
        assert!(
            harness
                .state()
                .requests
                .contains(&Request::SetPortalToken(Secret::new(token))),
            "requests: {:?}",
            harness.state().requests
        );
        let saved = serde_json::to_string(&harness.state().settings).expect("the settings serialise");
        assert!(!saved.contains(token), "the token reached the settings: {saved}");
        // ... and it is not in the request's own Debug either, so a failure
        // message cannot print it.
        let printed = format!("{:?}", harness.state().requests);
        assert!(!printed.contains(token), "a request printed the token: {printed}");
    }

    #[test]
    fn forget_asks_the_app_and_save_does_nothing_with_an_empty_field() {
        let mut harness = screen_harness(portal_state());
        harness.get_by_label("Forget").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::ForgetPortalToken),
            "requests: {:?}",
            harness.state().requests
        );

        // No token typed: Save is dead, so an empty field cannot overwrite the
        // token that is already stored.
        harness.state_mut().requests.clear();
        let save = harness.get_all_by_label("Save").last().expect("the token's Save");
        assert!(save.accesskit_node().is_disabled(), "nothing typed, nothing to save");
        save.click();
        harness.run();
        assert!(
            !harness.state().requests.iter().any(|r| matches!(r, Request::SetPortalToken(_))),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn a_stored_token_says_so_and_an_empty_slot_says_not_set() {
        // The placeholder is the field's hint: "stored in your keychain" when one
        // is set, "not set" when not. Neither is ever the token.
        assert_eq!(TOKEN_STORED, "stored in your keychain");
        assert_eq!(TOKEN_MISSING, "not set");
        // `hint_text` is painted, not an accesskit node, so the row says it in
        // its help line too, which is what a screen reader and these tests read.
        let harness = screen_harness(portal_state());
        harness.get_by_label("A token is stored in your keychain, never in settings.json.");

        let harness = screen_harness(state(configured(), true));
        harness.get_by_label("No token is stored yet. Paste one and press Save.");
    }

    /// The screen with a Portal session stored: the same set-up as
    /// `portal_state`, but the credential is Portal's own sign-in, which names
    /// the admin it belongs to.
    fn session_state() -> UiState {
        let mut ui_state = portal_state();
        ui_state.settings.portal_email = "ops@nightfall.tv".to_string();
        ui_state.portal.status.email = "ops@nightfall.tv".to_string();
        ui_state
    }

    #[test]
    fn a_signed_out_screen_offers_the_email_and_the_password() {
        let mut ui_state = portal_state();
        ui_state.settings.portal_email = "ops@nightfall.tv".to_string();
        let harness = screen_harness(ui_state);

        // The row is named, and the address the last sign-in used is already in
        // its field.
        harness.get_by_label("Sign in instead");
        assert!(
            every_word(&harness).contains(&"ops@nightfall.tv".to_string()),
            "the field is prefilled from settings.portal_email"
        );
        // Two masked fields now: the access token's, and this row's password.
        assert_eq!(
            harness.get_all_by_role(egui::accesskit::Role::PasswordInput).count(),
            2,
            "the token field and the password field"
        );
        // The promise the row makes, in the same `.help` line as the rows above.
        harness.get_by_label(PASSWORD_NOTE);
        assert!(
            harness.query_by_label("Signed in as ops@nightfall.tv").is_none(),
            "nobody is signed in yet"
        );

        // Nothing typed in the password yet, so "Sign in" waits.
        let button = harness.get_by_label("Sign in");
        assert!(button.accesskit_node().is_disabled(), "an empty password is not a sign-in");
    }

    #[test]
    fn the_password_is_masked_never_rendered_and_never_printed() {
        let password = "s3kret-P0rtal-pass";
        let mut ui_state = portal_state();
        ui_state.settings.portal_email = "ops@nightfall.tv".to_string();
        let mut harness = screen_harness(ui_state);

        harness
            .get_all_by_role(egui::accesskit::Role::PasswordInput)
            .last()
            .expect("the sign-in password field")
            .focus();
        harness.run();
        harness
            .get_all_by_role(egui::accesskit::Role::PasswordInput)
            .last()
            .expect("the sign-in password field")
            .type_text(password);
        harness.run();

        // What is drawn is the mask: no rendered word carries the password.
        for word in every_word(&harness) {
            assert!(!word.contains(password), "a rendered label carries the password: {word:?}");
        }
        // And it is not a setting, so it never reaches settings.json.
        let saved = serde_json::to_string(&harness.state().settings).expect("the settings serialise");
        assert!(!saved.contains(password), "the password reached the settings: {saved}");

        // Both fields filled: "Sign in" posts the address and a `Secret`.
        let button = harness.get_by_label("Sign in");
        assert!(!button.accesskit_node().is_disabled(), "an address and a password");
        button.click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::SignInPortal {
                email: "ops@nightfall.tv".to_string(),
                password: Secret::new(password),
            }),
            "requests: {:?}",
            harness.state().requests
        );
        // A `{:?}` of the request prints the address and the mask, never the
        // password, so no toast or log line can leak it.
        let printed = format!("{:?}", harness.state().requests);
        assert!(!printed.contains(password), "a request printed the password: {printed}");
        assert!(printed.contains("ops@nightfall.tv"), "the address is not a secret: {printed}");
    }

    #[test]
    fn a_sign_in_in_flight_waits_rather_than_posting_twice() {
        let mut ui_state = portal_state();
        ui_state.portal.email_draft = "ops@nightfall.tv".to_string();
        ui_state.portal.password_draft = "s3kret".to_string();
        ui_state.portal.signing_in = true;
        let harness = screen_harness(ui_state);

        let button = harness.get_by_label("Sign in");
        assert!(button.accesskit_node().is_disabled(), "one sign-in at a time");
        harness.get_by_label(SIGNING_IN);
        assert!(
            harness.query_by_label(PASSWORD_NOTE).is_none(),
            "while it is out, the row says what it is doing"
        );
    }

    #[test]
    fn a_refused_sign_in_says_why_under_the_fields() {
        let mut ui_state = portal_state();
        ui_state.portal.email_draft = "ops@nightfall.tv".to_string();
        ui_state.portal.signin_error =
            Some("Portal refused the sign-in (401): Invalid email or password".to_string());
        let harness = screen_harness(ui_state);

        // The app's sentence, where the note was, and the address it is about
        // still in the field.
        harness.get_by_label("Portal refused the sign-in (401): Invalid email or password");
        assert!(harness.query_by_label(PASSWORD_NOTE).is_none(), "the reason takes the note's place");
        assert!(every_word(&harness).contains(&"ops@nightfall.tv".to_string()));
        assert_eq!(
            harness.get_all_by_role(egui::accesskit::Role::PasswordInput).count(),
            2,
            "the fields are still there to retry with"
        );
    }

    /// The screen the moment after a password step Portal answered with
    /// `totp_required`: nothing is signed in, and the code row is what finishes
    /// the sign-in.
    fn needs_code_state() -> UiState {
        let mut ui_state = portal_state();
        ui_state.settings.portal_email = "ops@nightfall.tv".to_string();
        ui_state.portal.status.needs_code = true;
        ui_state.portal.signin_error =
            Some("Portal wants a one-time code to finish this sign-in: type the six digits from your authenticator app".to_string());
        ui_state
    }

    #[test]
    fn a_sign_in_that_wants_a_code_asks_for_six_digits() {
        let harness = screen_harness(needs_code_state());

        // The row, named, and the note under it: which code, and that it is not
        // kept. The password row is still there — Portal's pending step expires,
        // and a fresh password sign-in is how a user gets past that.
        harness.get_by_label("One-time code");
        harness.get_by_label(PORTAL_CODE_NOTE);
        // Nothing typed yet: the row's "Sign in" waits. (The password row has one
        // too, and both are disabled with nothing typed.)
        let buttons = harness.get_all_by_label("Sign in");
        assert!(
            buttons.into_iter().all(|b| b.accesskit_node().is_disabled()),
            "neither sign-in can be pressed with nothing typed"
        );
        // Nothing signed in, and no "Signed in as …" to be seen.
        assert!(harness.query_by_label("Signed in as ops@nightfall.tv").is_none());
    }

    #[test]
    fn the_six_digits_are_sent_as_a_secret_and_never_stored() {
        let mut ui_state = needs_code_state();
        ui_state.portal.code_draft = "135790".to_string();
        let mut harness = screen_harness(ui_state);

        // The row's own button — the Portal card draws it after the password
        // row's, and it is the enabled one.
        let button = harness
            .get_all_by_label("Sign in")
            .max_by_key(|b| b.rect().top() as i32)
            .expect("the code row's Sign in");
        assert!(!button.accesskit_node().is_disabled(), "six digits are a code");
        button.click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::SignInPortalTotp { code: Secret::new("135790") }),
            "requests: {:?}",
            harness.state().requests
        );
        // A `{:?}` of the request prints the mask, never the code: no toast, no
        // log line and no panic message can carry it.
        let printed = format!("{:?}", harness.state().requests);
        assert!(!printed.contains("135790"), "a request printed the code: {printed}");
        // And it is not a setting.
        let saved = serde_json::to_string(&harness.state().settings).expect("the settings serialise");
        assert!(!saved.contains("135790"), "the code reached the settings: {saved}");
    }

    #[test]
    fn a_code_row_takes_six_digits_and_a_refusal_reads_under_it() {
        let mut ui_state = needs_code_state();
        // What the shell leaves after Portal refuses a code: the row, the
        // sentence, and an empty field to type the next six digits into.
        ui_state.portal.signin_error = None;
        ui_state.portal.code_error =
            Some("Portal refused the one-time code (401): Invalid code".to_string());
        let harness = screen_harness(ui_state);

        harness.get_by_label("Portal refused the one-time code (401): Invalid code");
        assert!(harness.query_by_label(PORTAL_CODE_NOTE).is_none(), "the reason takes the note's place");
        harness.get_by_label("One-time code");
    }

    #[test]
    fn a_code_row_is_not_drawn_when_portal_is_not_asking() {
        // A signed-in session, and a plain bearer token: neither is waiting for a
        // code, so neither screen has the row.
        for ui_state in [portal_state(), session_state()] {
            let harness = screen_harness(ui_state);
            assert!(
                harness.query_by_label("One-time code").is_none(),
                "no code was asked for, so no row"
            );
        }
    }

    #[test]
    fn a_stored_session_says_who_it_is_and_offers_sign_out() {
        let mut harness = screen_harness(session_state());

        harness.get_by_label("Signed in as ops@nightfall.tv");
        // A session names its admin, so the two fields are not drawn: only the
        // access token's masked field is left.
        assert_eq!(
            harness.get_all_by_role(egui::accesskit::Role::PasswordInput).count(),
            1,
            "a stored session does not ask for a password"
        );
        assert!(harness.query_by_label("Sign in").is_none(), "nothing to sign in to");
        // The Frame.io row has a "Sign out" too: this one is the Portal card's,
        // which is drawn last.
        harness.get_all_by_label("Sign out").last().expect("the Portal sign-out").click();
        harness.run();
        assert!(
            harness.state().requests.contains(&Request::SignOutPortal),
            "requests: {:?}",
            harness.state().requests
        );
    }

    #[test]
    fn a_signed_in_session_counts_as_configured() {
        // The reconcile button asks for a credential, and a session is one: the
        // status that precedes it is the same either way.
        let view = session_state().portal.status;
        assert!(view.configured(), "an address and a session");
        let harness = screen_harness(session_state());
        assert!(!harness.get_by_label("Reconcile now").accesskit_node().is_disabled());
    }

    /// The Portal card is drawn by the same pieces as the cards above it: the
    /// labels start on the card's own left padding, the fields are the same
    /// height, and no new colour or font is named. This is the check that the
    /// section did not drift from the visual language.
    #[test]
    fn the_portal_card_is_laid_out_like_the_cards_above_it() {
        let harness = screen_harness(portal_state());

        let title = harness.get_by_label("Portal").rect();
        let above = harness.get_by_label("Storage").rect();
        assert!((title.left() - above.left()).abs() < 0.5, "the section title sits where the others do");

        let url_label = harness.get_by_label("Server URL").rect();
        let client_label = harness.get_by_label("Adobe Client ID").rect();
        assert!((url_label.left() - client_label.left()).abs() < 0.5, "the same card padding");

        // Fields: the Portal address, the folder, the token and the sign-in
        // email are the height of the Client ID field (`.input`, 32 px), not a
        // new one.
        let fields: Vec<f32> = harness
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .map(|node| node.rect().height())
            .collect();
        assert!(fields.len() >= 4, "the Client ID field and the three Portal ones: {}", fields.len());
        let client = fields[0];
        assert!(client > 0.0);
        for height in &fields[1..] {
            assert!((height - client).abs() < 1.0, "a field is {height} px, the Client ID's is {client}");
        }
        // The two masked ones, the access token's and the sign-in password's.
        let masked: Vec<f32> = harness
            .get_all_by_role(egui::accesskit::Role::PasswordInput)
            .map(|node| node.rect().height())
            .collect();
        assert_eq!(masked.len(), 2, "the token field and the password field");
        for height in masked {
            assert!((height - client).abs() < 1.0, "a masked field is {height} px, the Client ID's is {client}");
        }
    }


    #[test]
    fn the_portal_section_draws_with_populated_status() {
        let mut harness = screen_harness(portal_state());
        harness.snapshot("settings_portal");
    }

    #[test]
    fn a_very_long_path_is_truncated_not_wrapped() {
        let long = format!("/Users/chaos/{}", "deep-folder/".repeat(60));
        let mut settings = Settings::default();
        settings.download_dir = long.clone();
        let harness = screen_harness(state(settings, true));
        // The label carries the whole path, shortened to `~` by `shortPath`.
        harness.get_by_label(short_path(&long, "/Users/chaos").as_str());
    }
    #[test]
    fn a_configured_card_says_what_to_press_and_whether_the_poller_is_on() {
        let mut status = PortalView::default();
        status.url = "https://portal.example.com".to_string();
        status.signed_in = true;
        status.root = "/tmp/Live_Work".to_string();
        status.enabled = false;
        assert_eq!(
            portal_line(&status).text,
            "Configured. Press Reconcile now, or switch it on to check every 30 s."
        );
        status.enabled = true;
        assert_eq!(portal_line(&status).text, "Configured. Nothing has run yet: press Reconcile now.");
        assert!(!portal_line(&status).failed);
    }

}
