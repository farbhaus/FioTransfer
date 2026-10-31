//! The browser entry point: [`crate::app::FioApp`] on a `<canvas>`.
//!
//! This is a measurement apparatus, not a shipping target. The native window and
//! the webview front end cannot be compared fairly — different window chrome,
//! different rasteriser, different pixel ratio — so this build puts the same
//! shell on a canvas that a headless Chromium can capture at 1180x780 with a
//! device pixel ratio of 1, exactly like it captures `frontend/dist`.
//!
//! ## What selects the screen
//!
//! Both parameters are optional; anything missing or unrecognised falls back to
//! the default, so a bare `index.html` still draws a real screen. Nothing here
//! panics on a bad query string.
//!
//! ```text
//! ?view=transfers|watch|history|settings   which screen (default: transfers)
//! ?fixture=1                               the populated fixture from
//!                                          `src/fixture.rs`, the same state
//!                                          `tests/snapshots.rs` renders
//! ```
//!
//! ## How the capture knows it is ready
//!
//! Two signals, both readable from the page without touching wasm:
//!
//! * `<canvas id="fio-canvas">` carries `data-frames="<n>"`, the number of
//!   frames drawn so far. `bench/wasm-shots.sh` waits for a few frames.
//! * `document.body` carries `data-fio-error="<message>"` if startup failed, and
//!   the same message is written to the console and into the page.
#![forbid(unsafe_code)]

#![cfg(target_arch = "wasm32")]

use eframe::wasm_bindgen::prelude::*;

use crate::model::{UiState, View};
use crate::FioApp;

/// The id `web/index.html` gives the canvas this module draws into.
pub const CANVAS_ID: &str = "fio-canvas";

/// The toast lifetime the fixture asks for (`Toast::seconds` in
/// `src/fixture.rs`): a capture holds the toasts at this age so a slow boot does
/// not screenshot a stack that has already aged out.
const TOAST_SECONDS: f32 = 3.5;

/// What the URL asked for. `Default` is the empty state on the Transfers screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub view: View,
    pub fixture: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            view: View::Transfers,
            fixture: false,
        }
    }
}

impl Options {
    /// `?view=` and `?fixture=` out of a query string, with or without the
    /// leading `?`. Unknown keys and unknown values are ignored, so a capture
    /// link can carry anything else it likes.
    pub fn from_query(query: &str) -> Self {
        let mut options = Options::default();
        for pair in query.trim_start_matches('?').split('&') {
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None => (pair, ""),
            };
            match key {
                "view" => {
                    if let Some(view) = view_from_param(value) {
                        options.view = view;
                    }
                }
                // `?fixture=1`, `?fixture=true`, `?fixture=yes`, and a bare
                // `?fixture` all mean the same thing.
                "fixture" => {
                    options.fixture = matches!(value, "" | "1" | "true" | "yes" | "on");
                }
                _ => {}
            }
        }
        options
    }
}

/// The screen name the URL uses, matching `View::ALL` order. `None` for anything
/// else, so a typo falls back to the default screen instead of failing.
fn view_from_param(value: &str) -> Option<View> {
    match value.to_ascii_lowercase().as_str() {
        "transfers" => Some(View::Transfers),
        "space" => Some(View::Space),
        "watch" => Some(View::Watch),
        "history" => Some(View::History),
        "settings" => Some(View::Settings),
        _ => None,
    }
}

/// `#[wasm_bindgen(start)]`: the module `.wasm` runs this as soon as it is
/// instantiated, which is when the module script the page loads executes — by
/// then the `<canvas>` is in the document.
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    let options = Options::from_query(&query_string().unwrap_or_default());
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(err) = run(options).await {
            fail(&format!("fiotransfer-egui: {err:?}"));
        }
    });
    Ok(())
}

/// `location.search`, or nothing at all if the page has no location.
fn query_string() -> Option<String> {
    web_sys::window()?.location().search().ok()
}

/// Start eframe on `#fio-canvas` with the state `options` asks for.
async fn run(options: Options) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("this page has no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("this window has no document"))?;
    let canvas = document
        .get_element_by_id(CANVAS_ID)
        .ok_or_else(|| JsValue::from_str("index.html has no canvas with this id"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str("the element is not a canvas"))?;

    let state = if options.fixture {
        crate::fixture::fixture_state(options.view)
    } else {
        let mut state = UiState::default();
        state.view = options.view;
        state
    };

    // Dithering off: `egui_kittest`/wgpu does not dither, so leaving it on would
    // put noise between the two captures that is not the UI. Everything else is
    // eframe's default (WebGL2 where the context allows it, then WebGL1).
    let web_options = eframe::WebOptions {
        dithering: false,
        ..Default::default()
    };

    // The runner owns the app: its request-animation-frame closures hold the
    // only other handle, and dropping it here would stop the page. Leak it so it
    // lives as long as the canvas does — this build runs one app per page.
    let runner: &'static eframe::WebRunner = Box::leak(Box::new(eframe::WebRunner::new()));
    runner
        .start(
            canvas.clone(),
            web_options,
            Box::new(move |cc| {
                // The theme and Inter, the same call `main.rs` makes before the
                // first frame.
                crate::app::apply_style_once(&cc.egui_ctx);
                Ok(Box::new(Capture::new(FioApp::new(state), canvas.clone())))
            }),
        )
        .await
}

/// The shell, plus the two things a capture needs from the browser side.
struct Capture {
    app: FioApp,
    canvas: web_sys::HtmlCanvasElement,
    frames: u32,
}

impl Capture {
    fn new(app: FioApp, canvas: web_sys::HtmlCanvasElement) -> Self {
        Capture {
            app,
            canvas,
            frames: 0,
        }
    }
}

impl eframe::App for Capture {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // `FioApp::draw` ages the toast stack by wall time. A capture must not
        // depend on how long the machine took to boot, so hold each toast at the
        // age the fixture gave it: the native snapshot draws the same stack.
        for toast in &mut self.app.state.toasts {
            toast.seconds = toast.seconds.max(TOAST_SECONDS);
        }
        self.app.draw(ctx);

        self.frames += 1;
        let _ = self.canvas.set_attribute("data-frames", &self.frames.to_string());
    }

    /// `body { background: var(--base) }`, through the same code the native
    /// build uses.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        eframe::App::clear_color(&self.app, visuals)
    }
}

/// Report a startup failure where both a person and the capture script can see
/// it: the page, the console, and an attribute on `<body>`.
fn fail(message: &str) {
    web_sys::console::error_1(&JsValue::from_str(message));
    let Some(body) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.body())
    else {
        return;
    };
    let _ = body.set_attribute("data-fio-error", message);
    body.set_inner_html(&format!(
        "<pre style=\"margin:0;padding:12px;color:#ff6b6b;background:#000;\
         font:13px monospace;white-space:pre-wrap\">{}</pre>",
        escape_html(message)
    ));
}

/// Just enough escaping for the failure text, which can quote a JS value.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
