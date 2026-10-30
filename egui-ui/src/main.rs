//! FioTransfer's egui front end: the app itself.
//!
//! `cargo run -p fiotransfer-egui` opens the same five screens as the webview
//! front end, over the same engine (`fiotransfer_core::App`), with the same
//! strings. The shell that drives it is [`fiotransfer_egui::Shell`]: it owns the
//! runtime, answers every request a screen pushes, and hands the desktop's
//! panels and clipboard to the engine.
//!
//! Two arguments run it with no backend at all, which is how the screens are
//! looked at, captured and compared without touching Frame.io:
//!
//! ```text
//! cargo run -p fiotransfer-egui -- --fixture
//! cargo run -p fiotransfer-egui -- --snapshot /tmp/snapshot.json
//! ```
//!
//! `--fixture` draws `src/fixture.rs`, the state `tests/snapshots.rs` renders and
//! the browser capture draws with `?fixture=1`. `--snapshot <file>` seeds the
//! Transfers list from the app's own snapshot JSON (`{"jobs":[…],"speed":…,
//! "active_streams":…,"stream_limit":…}`), which is enough to look at a real
//! queue without a bridge. Either one leaves the shell out: with a backend
//! running, the file would be replaced by the live queue on the first frame.

#![forbid(unsafe_code)]

use fiotransfer_egui::app::apply_style_once;
use fiotransfer_egui::model::{Snapshot, UiState, View};
use fiotransfer_egui::{FioApp, Shell, MIN_HEIGHT, MIN_WIDTH, WINDOW_HEIGHT, WINDOW_WIDTH};

fn main() -> eframe::Result<()> {
    let launch = launch_from_args(std::env::args().skip(1));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
            .with_min_inner_size([MIN_WIDTH, MIN_HEIGHT])
            .with_title("FioTransfer"),
        ..Default::default()
    };
    eframe::run_native(
        "FioTransfer",
        options,
        Box::new(move |cc| {
            apply_style_once(&cc.egui_ctx);
            Ok(Box::new(app(launch)))
        }),
    )
}

/// What the window opens with, and whether the app runs behind it.
struct Launch {
    state: UiState,
    /// `false` for `--fixture` and `--snapshot`: a window with no backend.
    backend: bool,
}

/// The window: the real app when `launch` asked for one, a pure renderer when it
/// did not. The runtime is built here — on the main thread, before the first
/// frame — and the app's own startup is spawned onto it.
fn app(launch: Launch) -> FioApp {
    if !launch.backend {
        return FioApp::new(launch.state);
    }
    let shell = Shell::start(launch.state.clone());
    FioApp::with_shell(launch.state, shell)
}

/// What the arguments asked for. Bad input is reported and ignored: this is a
/// front end, it never takes the window down over a missing file.
fn launch_from_args(args: impl Iterator<Item = String>) -> Launch {
    let mut state = UiState::default();
    let mut backend = true;
    let args: Vec<String> = args.collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fixture" => {
                // The state `tests/snapshots.rs` renders, screen for screen.
                state = fiotransfer_egui::fixture::fixture_state(View::Transfers);
                backend = false;
            }
            "--snapshot" => {
                i += 1;
                backend = false;
                match args.get(i) {
                    Some(path) => match read_snapshot(path) {
                        Ok(snapshot) => {
                            state.snapshot = snapshot;
                            state.logged_in = true;
                        }
                        Err(err) => eprintln!("fiotransfer-egui: {path}: {err}"),
                    },
                    None => eprintln!("fiotransfer-egui: --snapshot needs a file"),
                }
            }
            other => eprintln!("fiotransfer-egui: unknown argument {other}"),
        }
        i += 1;
    }
    Launch { state, backend }
}

fn read_snapshot(path: &str) -> Result<Snapshot, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state one launch argument list produces.
    fn state_from_args(args: impl IntoIterator<Item = &'static str>) -> UiState {
        launch_from_args(args.into_iter().map(str::to_string)).state
    }

    fn backend_from_args(args: impl IntoIterator<Item = &'static str>) -> bool {
        launch_from_args(args.into_iter().map(str::to_string)).backend
    }

    #[test]
    fn a_missing_snapshot_file_is_reported_not_fatal() {
        let state = state_from_args(["--snapshot", "/nonexistent/fio-snapshot.json"]);
        assert!(state.snapshot.jobs.is_empty());
        assert!(!state.logged_in);
    }

    #[test]
    fn a_snapshot_file_seeds_the_job_list() {
        let path = std::env::temp_dir().join(format!(
            "fiotransfer-egui-test-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"jobs":[{"id":"a","kind":"upload","title":"clip.mov","status":"running",
                "total":10,"done":5}],"speed":1.0,"active_streams":1,"stream_limit":16}"#,
        )
        .expect("write the fixture");
        let args = ["--snapshot".to_string(), path.display().to_string()];
        let state = launch_from_args(args.into_iter()).state;
        let _ = std::fs::remove_file(&path);
        assert_eq!(state.snapshot.jobs.len(), 1);
        assert!(state.logged_in);
        assert_eq!(state.snapshot.jobs[0].progress(), Some(0.5));
    }

    #[test]
    fn junk_in_a_snapshot_file_does_not_panic() {
        let path = std::env::temp_dir().join(format!(
            "fiotransfer-egui-junk-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "not json at all").expect("write the fixture");
        let args = ["--snapshot".to_string(), path.display().to_string()];
        let state = launch_from_args(args.into_iter()).state;
        let _ = std::fs::remove_file(&path);
        assert!(state.snapshot.jobs.is_empty());
    }

    #[test]
    fn a_bare_launch_runs_the_app_and_a_fixture_launch_does_not() {
        // The default is the real app: `main` gives it a shell.
        assert!(backend_from_args([]));
        // `--fixture` and `--snapshot` are a window over a given state, with no
        // engine behind it (a live queue would overwrite it on the first frame).
        assert!(!backend_from_args(["--fixture"]));
        assert!(!backend_from_args(["--snapshot", "/tmp/nothing.json"]));
    }

    #[test]
    fn a_fixture_launch_is_the_state_the_snapshots_render() {
        let state = state_from_args(["--fixture"]);
        let expected = fiotransfer_egui::fixture::fixture_state(View::Transfers);
        assert_eq!(state.snapshot.jobs.len(), expected.snapshot.jobs.len());
        assert_eq!(state.history.len(), expected.history.len());
        assert_eq!(state.view, View::Transfers);
        assert!(state.logged_in);
    }

    #[test]
    fn an_unknown_argument_is_ignored() {
        let launch = launch_from_args(["--nonsense".to_string()].into_iter());
        assert!(launch.backend, "the app still runs");
        assert!(launch.state.snapshot.jobs.is_empty());
    }
}
