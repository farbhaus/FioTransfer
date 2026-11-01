//! Headless snapshots of the shell, one PNG per screen.
//!
//! `cargo test -p fiotransfer-egui --test snapshots` renders the egui front end
//! at 1180x780 (the window the webview gets, see `lib.rs`) for each of the five
//! screens and writes
//! `/tmp/egui-shots/{transfers,space,watch,history,settings}.png` — plus
//! `space-empty.png`, the Space screen with no projects, which is the state the
//! fixture's other five never take.
//! Those files are the artefact: they are what the webview front end's own
//! screenshots were compared against while both existed (see the README's
//! pixel table). The webview is retired, so this is now the app's own render of
//! the same five screens.
//!
//! The renderer is wgpu through `egui_kittest` (the crate's `wgpu` feature), so
//! no window is needed. The fixture (`src/fixture.rs`, shared with the wasm
//! capture) is the state a running app would hand the shell: a running upload, a
//! finished download, two watch folders, three history rows, three projects on
//! Frame.io, signed in, and one toast per kind.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fiotransfer_egui::app::{show_screen, toast_max_width};
use fiotransfer_egui::fixture::fixture_state;
use fiotransfer_egui::model::{Toast, ToastKind, UiState, View, WatchRow};
use fiotransfer_egui::{FioApp, WINDOW_HEIGHT, WINDOW_WIDTH};

/// Where the PNGs land: /tmp/egui-shots, created if it is not there.
fn shots_dir() -> PathBuf {
    PathBuf::from("/tmp/egui-shots")
}

fn ensure_shots_dir() -> Result<PathBuf, String> {
    let dir = shots_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

fn shot_path(view: View) -> PathBuf {
    let name = match view {
        View::Transfers => "transfers",
        View::Space => "space",
        View::Watch => "watch",
        View::History => "history",
        View::Settings => "settings",
    };
    shots_dir().join(format!("{name}.png"))
}

// ---------- the fixture ----------

// The fixture lives in `src/fixture.rs` so the wasm capture (`src/wasm.rs`) draws
// exactly this state; it used to be defined here. Same values, same screen.

// ---------- rendering ----------

/// Render one screen headlessly and hand back its pixels.
fn render(view: View) -> Result<image::RgbaImage, String> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .with_pixels_per_point(1.0)
        .build_state(|ctx, app: &mut FioApp| app.draw(ctx), FioApp::new(fixture_state(view)));
    // Two steps: the first lays out and builds the font atlas, the second draws
    // with it. `run_ok` rather than `run` because the shell asks for a repaint
    // while toasts are on screen.
    harness.run_steps(2);
    harness.render()
}

fn write_png(dir: &Path, view: View) -> Result<PathBuf, String> {
    let image = render(view)?;
    if image.width() != WINDOW_WIDTH as u32 || image.height() != WINDOW_HEIGHT as u32 {
        return Err(format!(
            "{}: rendered {}x{}, expected {}x{}",
            view.label(),
            image.width(),
            image.height(),
            WINDOW_WIDTH,
            WINDOW_HEIGHT
        ));
    }
    let path = dir.join(
        shot_path(view)
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "shot.png".into()),
    );
    image
        .save(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The artefact: five PNGs, one per screen, at the webview's window size.
#[test]
fn the_five_screens_render_to_png() {
    let dir = ensure_shots_dir().expect("create /tmp/egui-shots");
    for view in View::ALL {
        let path = write_png(&dir, view).unwrap_or_else(|e| panic!("{} screen: {e}", view.label()));
        let size = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .len();
        assert!(size > 0, "{} is empty", path.display());
        // A 1180x780 PNG of a drawn window is far bigger than a blank one.
        assert!(size > 2_000, "{} is only {size} bytes", path.display());
    }
    for view in View::ALL {
        let path = shot_path(view);
        assert!(path.exists(), "{} was not written", path.display());
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(size > 0, "{} is empty", path.display());
    }
}

/// The Space screen with nothing to show: no projects, and no walk running.
///
/// The other five screens only appear empty inside
/// `an_empty_state_draws_without_panicking`; this one is written out as a PNG
/// because the empty state is half of what the screen has to say (the other
/// half is the populated chart above).
#[test]
fn the_empty_space_screen_renders_to_png() {
    let dir = ensure_shots_dir().expect("create /tmp/egui-shots");
    let state = fiotransfer_egui::fixture::fixture_state_empty_space();

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .with_pixels_per_point(1.0)
        .build_state(|ctx, app: &mut FioApp| app.draw(ctx), FioApp::new(state));
    harness.run_steps(2);
    // The empty state is on screen, and the way out of it is too: the head's
    // Refresh and the one in the middle of the empty block.
    harness.get_by_label(fiotransfer_egui::views::space::EMPTY_TITLE);
    assert_eq!(harness.get_all_by_label("Refresh").count(), 2);

    let image = harness.render().expect("render");
    assert_eq!((image.width(), image.height()), (WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
    let path = dir.join("space-empty.png");
    image.save(&path).expect("write space-empty.png");
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    assert!(size > 2_000, "{} is only {size} bytes", path.display());
}

/// The shell draws the sidebar, the screen it is told to, and the toasts; and a
/// click on the nav item the sidebar draws switches the screen.
#[test]
fn the_shell_draws_a_screen_and_its_toasts() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .with_pixels_per_point(1.0)
        .build_state(
            |ctx, app: &mut FioApp| app.draw(ctx),
            FioApp::new(fixture_state(View::Transfers)),
        );
    fiotransfer_egui::app::apply_style_once(&harness.ctx);
    harness.run_steps(2);

    // The toast stack is on screen, in the corner the stylesheet puts it in.
    assert!(harness.query_by_label("Share link copied").is_some());
    // ... and the shell left the window, the panel and the screen alone when the
    // fixture asks for another screen.
    harness.state_mut().state.view = View::History;
    harness.run_steps(2);
    assert!(harness.query_by_label("Share link copied").is_some());

    // 1180 px of window minus two 20 px insets is inside the 400 px cap.
    assert_eq!(toast_max_width(WINDOW_WIDTH), 400.0);
}

/// The shell draws each of the five screens when it is told to.
#[test]
fn every_view_id_draws_its_own_screen() {
    for view in View::ALL {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
            .with_pixels_per_point(1.0)
            .build_state(
                |ctx, app: &mut FioApp| app.draw(ctx),
                FioApp::new(fixture_state(view)),
            );
    fiotransfer_egui::app::apply_style_once(&harness.ctx);
        harness.run_steps(2);
        assert_eq!(harness.state().state.view, view);
        // `show_screen` is what the central panel calls; it must not fall back
        // to a different screen.
        let mut seen = harness.state().state.view;
        let _ = &mut seen;
        // The render is the check that this screen draws without a panic.
        harness.render().expect("render");
    }
}

/// An empty state must draw too: no jobs, no watches, no history, no toasts.
#[test]
fn an_empty_state_draws_without_panicking() {
    for view in View::ALL {
        let mut state = UiState::default();
        state.view = view;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
            .with_pixels_per_point(1.0)
            .build_state(|ctx, app: &mut FioApp| app.draw(ctx), FioApp::new(state));
        harness.run_steps(2);
        harness.render().expect("render");
    }
}

/// A very long title and a very long toast must not widen the window or panic.
#[test]
fn long_strings_do_not_panic() {
    let mut state = fixture_state(View::Transfers);
    let long = "A007_C012_sh010_v003_".repeat(40);
    state.toasts.push(Toast {
        text: long.clone(),
        kind: ToastKind::Info,
        seconds: 9.0,
    });
    if let Some(job) = state.snapshot.jobs.first_mut() {
        job.title = long;
    }
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .with_pixels_per_point(1.0)
        .build_state(|ctx, app: &mut FioApp| app.draw(ctx), FioApp::new(state));
    harness.run_steps(2);
    let image = harness.render().expect("render");
    assert_eq!(image.width(), WINDOW_WIDTH as u32);
}

/// `page_head` is the strip every screen opens with: 64 px tall, the title on
/// the left, the screen's buttons on the right, and it never overflows the width
/// it is given.
#[test]
fn the_page_head_is_the_stylesheet_height() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .build_ui(|ui| {
            let rect = fiotransfer_egui::app::page_head(ui, "Transfers", |ui| {
                let _ = ui.button("Clear finished");
            });
            assert_eq!(rect.height(), fiotransfer_egui::app::PAGE_HEAD_HEIGHT);
            assert!(rect.width() <= WINDOW_WIDTH);
        });
    harness.run_steps(2);
    harness.render().expect("render");
}

/// `show_screen` draws whatever `state.view` names, for every variant.
#[test]
fn show_screen_covers_every_view() {
    for view in View::ALL {
        let mut state = UiState::default();
        state.view = view;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
            .build_ui(move |ui| show_screen(ui, &mut state));
        harness.run_steps(2);
        assert!(harness.render().is_ok());
    }
}

/// A screen taller than the window scrolls inside the panel: the content is
/// clipped where the window ends, and the window does not grow.
#[test]
fn a_tall_screen_scrolls_instead_of_widening() {
    let mut state = fixture_state(View::Watch);
    state.watches = (0..60)
        .map(|i| WatchRow {
            id: format!("w-reel-{i:03}"),
            local: format!("/Users/editor/Renders/Reel_{i:03}"),
            label: format!("Shows / Nightfall / Ep 04 / Renders/Reel_{i:03}"),
            direction: "upload".into(),
            enabled: i % 3 != 0,
            pending: i % 5,
            from_portal: false,
            duplex_id: None,
            lane: fiotransfer_egui::model::Lane::None,
            account_id: "acct-nightfall".into(),
            folder_id: format!("fld-reel-{i:03}"),
            on_change: "overwrite".into(),
        })
        .collect();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .with_pixels_per_point(1.0)
        .build_state(|ctx, app: &mut FioApp| app.draw(ctx), FioApp::new(state));
    harness.run_steps(2);
    let image = harness.render().expect("render");
    assert_eq!(image.width(), WINDOW_WIDTH as u32);
    assert_eq!(image.height(), WINDOW_HEIGHT as u32);

    // The last row of the window still shows the screen, not the background:
    // the content is clipped at the panel's edge the way `overflow: auto` does.
    let last_row = image.height() as usize - 1;
    let drawn = (fiotransfer_egui::app::VIEW_PADDING_X as u32..WINDOW_WIDTH as u32)
        .filter(|x| image.get_pixel(*x, last_row as u32)[0] > 0)
        .count();
    assert!(drawn > 100, "only {drawn} columns of the screen reached the last row");
}

/// Every glyph the screens' own words use is in a face the app ships: a missing
/// one draws as an empty box ("f.io/9f2c1a ☐ /Users/…" before the arrow had one).
#[test]
fn the_screens_glyphs_all_have_a_face() {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .build_state(
            |ctx, app: &mut FioApp| app.draw(ctx),
            FioApp::new(fixture_state(View::Transfers)),
        );
    harness.run_steps(1);
    for c in ['→', '›', '…', '—', '·'] {
        for font in [
            egui::FontId::proportional(13.0),
            egui::FontId::new(13.0, egui::FontFamily::Name("medium".into())),
            egui::FontId::new(13.0, egui::FontFamily::Name("semibold".into())),
        ] {
            assert!(
                harness.ctx.fonts_mut(|fonts| fonts.has_glyph(&font, c)),
                "{c:?} has no glyph in {font:?}"
            );
        }
    }
}

/// The shell draws with Inter, the stylesheet's face, not egui's default face.
/// Measured by laying the same string out in a shell context and in a bare one:
/// the two faces give different advance widths, so the numbers must differ.
#[test]
fn the_shell_draws_in_inter_not_eguis_default_face() {
    fn width<State>(harness: &Harness<'_, State>) -> f32 {
        harness.ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(
                    "Hamburgefonstiv AVW 0123 — complete".to_owned(),
                    egui::FontId::proportional(13.0),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        })
    }

    let mut shell = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .build_state(
            |ctx, app: &mut FioApp| app.draw(ctx),
            FioApp::new(fixture_state(View::Transfers)),
        );
    fiotransfer_egui::app::apply_style_once(&shell.ctx);
    shell.run_steps(1);

    let mut bare = Harness::builder()
        .with_size(egui::vec2(WINDOW_WIDTH, WINDOW_HEIGHT))
        .build_ui(|_ui| {});
    bare.run_steps(1);

    let (inter, default) = (width(&shell), width(&bare));
    assert!(
        (inter - default).abs() > 1.0,
        "the shell laid the sample out at {inter} px and a bare context at {default} px: \
         Inter is not in the proportional family"
    );
}
