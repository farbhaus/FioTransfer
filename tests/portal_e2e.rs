//! The Portal card's "Reconcile now", pressed, against a live Portal.
//!
//! Everything about Portal was proven at one end or the other: `tests/portal_live.rs`
//! calls `App::portal_reconcile` directly, and `egui-ui/src/views/settings.rs`
//! draws the card over a hand-built `PortalView`. Neither crosses the seam
//! between them — the button pushes `Request::PortalReconcile`,
//! `egui-ui/src/shell.rs` answers it with `App::portal_reconcile`, and the
//! sentence that comes back is drawn in the card. A wrong wiring there leaves
//! the button decorative and every existing test green, which is why this file
//! exists: it presses the real button, in the real window, with the real shell,
//! and reads the words the card put on screen.
//!
//! Skipped unless the live variables are set, exactly as `tests/portal_live.rs`
//! is, so a normal `cargo test` never touches the network:
//!
//!   PORTAL_LIVE_URL=http://127.0.0.1:18080 \
//!   PORTAL_LIVE_TOKEN=… \
//!     cargo test --offline -p fiotransfer --test portal_e2e -- --nocapture
//!
//! ## Why these cases live in this package and not in `egui-ui/tests/`
//!
//! The window they drive needs a `HOME` of its own: the shell builds its app
//! from `config::load()`, and pressing the button reaches `App::set_portal`,
//! which writes `settings.json` and keeps the token under `config::dir()`.
//! A test that did not move `HOME` would write into the developer's own
//! settings and keychain fallback. `std::env::set_var` is `unsafe` in edition
//! 2024 — which `egui-ui` is, under `#![forbid(unsafe_code)]` — so the world has
//! to be built where calling it is safe. This package is `edition = "2021"`,
//! and it already owns the live gating, the world and the "one live case at a
//! time" lock that `tests/portal_live.rs` uses. The harness itself is not new:
//! `egui` and `egui_kittest` are the two crates `egui-ui/Cargo.toml` already
//! depends on for `tests/snapshots.rs`, added here as dev-dependencies of the
//! test that needs them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable};
use fiotransfer_egui::model::{UiState, View};
use fiotransfer_egui::views::settings::{portal_line, PortalLine};
use fiotransfer_egui::{FioApp, Shell};

// ---------- the live host ----------

/// The base URL and the bearer token, from the environment and from nowhere
/// else — the same pair `tests/portal_live.rs` reads.
fn live() -> Option<(String, String)> {
    match (std::env::var("PORTAL_LIVE_URL"), std::env::var("PORTAL_LIVE_TOKEN")) {
        (Ok(u), Ok(t)) if !u.trim().is_empty() && !t.trim().is_empty() => Some((u, t)),
        _ => {
            eprintln!("skipped: PORTAL_LIVE_URL / PORTAL_LIVE_TOKEN not set");
            None
        }
    }
}

/// The base URL alone, for the case that carries its own (wrong) credential —
/// the same gate `tests/portal_live.rs`'s refused-token case has.
fn live_url() -> Option<String> {
    match std::env::var("PORTAL_LIVE_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u),
        _ => {
            eprintln!("skipped: PORTAL_LIVE_URL not set");
            None
        }
    }
}

/// One live case at a time: [`Window::new`] sets `HOME` and `FIO_NO_KEYCHAIN` for
/// the whole process, so two cases at once would share — and clobber — the folder
/// the stored credential lives in.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- the window ----------

/// A window over the real app, and the folders it was pointed at.
///
/// `home` is a temporary `HOME`: the app's settings, its history database, its
/// log and its stored token all live under it, so nothing here can reach the
/// real ones. `root` is the Portal root the seeded instance's marker is mirrored
/// into — `<temp>/media/Live_Work`, the setting the card shows.
struct Window {
    /// Everything the app may write. Held for its `Drop`, which deletes the tree
    /// once the case — and the app behind it — has finished with it.
    home: tempfile::TempDir,
    /// `<home>/media/Live_Work`: the local root the card is configured with.
    root: PathBuf,
    /// The frame and the app behind it.
    harness: Harness<'static, FioApp>,
}

impl Window {
    /// Builds the headless window the way `tests/portal_live.rs` builds its
    /// headless app — a temporary `HOME`, `FIO_NO_KEYCHAIN=1`, the history store
    /// and the watcher opened by the app's own startup — and configures Portal
    /// with the live URL, a stored bearer token and a root inside the temp dir.
    ///
    /// The credential is stored *before* the shell is started, so the token the
    /// card is showing is the one `App::start` read for itself: the only thing
    /// that can then flip `signed_in` is the app's own startup having reached
    /// its `refresh_portal_credential`, which is what says the history store and
    /// the watcher are open too (see [`Window::wait_for_the_app`]).
    fn new(url: &str, token: &str) -> Window {
        Window::build(url, Some(token))
    }

    /// The same window with **no** credential stored, so the card draws its
    /// "Sign in instead" row — the path a person takes, as opposed to a token.
    fn unsigned(url: &str) -> Window {
        Window::build(url, None)
    }

    fn build(url: &str, token: Option<&str>) -> Window {
        Window::build_with(url, token, false, 30)
    }

    /// The same window with the background poller configured: `enabled` and the
    /// interval, which the other cases deliberately keep off.
    fn build_with(url: &str, token: Option<&str>, enabled: bool, every_secs: u64) -> Window {
        let home = tempfile::tempdir().expect("a temporary HOME");
        std::env::set_var("HOME", home.path());
        std::env::set_var("FIO_NO_KEYCHAIN", "1");

        let root = home.path().join("media").join("Live_Work");
        std::fs::create_dir_all(&root).expect("the local root");

        // Settings on disk, which is where the shell's own app reads them.
        fiotransfer::config::update(|s| {
            s.portal_url = url.trim().trim_end_matches('/').to_string();
            s.portal_root = root.to_string_lossy().into_owned();
            // Off for the button cases, so a background reconcile cannot answer
            // for a press; on for the poller's own case.
            s.portal_enabled = enabled;
            s.portal_every_secs = every_secs as i64;
        })
        .expect("settings.json in the temporary HOME");
        if let Some(t) = token {
            fiotransfer::portal::set_token(t).expect("the token is stored under the temporary HOME");
        }

        // The screen the window opens on, before any of the app has been read.
        let state = UiState { view: View::Settings, ..Default::default() };
        // The shell loads its own app from disk (`config::load`), and starts it on
        // its own runtime.
        let shell = Shell::start(UiState::default());
        let harness = Harness::builder()
            .with_size(egui::vec2(
                fiotransfer_egui::WINDOW_WIDTH,
                // Tall enough that the whole Settings page — the Portal card is
                // its last one — is on screen, so the button can be pressed.
                2400.0,
            ))
            .with_pixels_per_point(1.0)
            .build_state(|ctx, app: &mut FioApp| app.draw(ctx), FioApp::with_shell(state, shell));
        Window { home, root, harness }
    }

    /// One frame of the real window: the shell pumps the app, then the screens
    /// draw. `step` rather than `run`, because the shell always asks for the
    /// next frame (its tick moves a transfer's progress) and `run` would only
    /// stop at its step limit.
    fn frame(&mut self) {
        self.harness.step();
    }

    /// Frames, with real time passing between them, until `done` holds.
    ///
    /// The app's own work runs on the shell's tokio runtime — its startup, and
    /// every reconcile — so a test that only stepped frames as fast as it could
    /// would never let any of it finish.
    fn pump_until(&mut self, budget: Duration, mut done: impl FnMut(&FioApp) -> bool) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if done(self.harness.state()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.frame();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The status line as the card draws it — not the model's copy of it.
    ///
    /// A sentence wider than the card wraps, and egui then hands AccessKit the
    /// whole sentence *and* the part that wrapped, so the copies are deduped and
    /// the longest is taken. They must all be the same sentence: the check at the
    /// end is that each shorter copy is a prefix of the longest, so a card that
    /// drew two different status lines cannot pass as one.
    fn line_on_screen(&self) -> String {
        let words = self.screen_words();
        let mut found: Vec<String> = words.into_iter().filter(|w| is_status_line(w)).collect();
        found.sort_by_key(|w| w.chars().count());
        found.dedup();
        assert!(!found.is_empty(), "the card drew no status line at all");
        let longest = found.last().cloned().expect("non-empty");
        for shorter in &found[..found.len() - 1] {
            assert!(
                longest.starts_with(shorter.trim_end()),
                "the card drew two status lines: {:?} then {:?}",
                shorter,
                longest
            );
        }
        longest
    }

    /// Every label and every value the screen rendered as text.
    fn screen_words(&self) -> Vec<String> {
        let mut words = Vec::new();
        for node in self.harness.root().children_recursive() {
            let node = node.accesskit_node();
            if let Some(label) = node.label() {
                words.push(label.to_string());
            }
            if let Some(value) = node.value() {
                words.push(value.to_string());
            }
        }
        words
    }

    /// The status line's own words and verdict, from the app through the shell.
    fn line(&self) -> PortalLine {
        portal_line(&self.harness.state().state.portal.status)
    }

    /// Presses `Reconcile now` the way a person does: by its label.
    fn press_reconcile_now(&mut self) {
        let button = self.harness.get_by_label(RECONCILE);
        assert!(
            !button.accesskit_node().is_disabled(),
            "the button is decorative: the card says {:?}",
            self.line().text
        );
        button.click();
        self.frame();
    }

    /// Waits for the app's own startup, which `Shell::start` spawns on the
    /// runtime while the first frames are already drawing.
    ///
    /// The signal is a real one and not a sleep: the token is stored before the
    /// shell exists, so the card can only say "a credential is stored" once
    /// `App::start` has reached its own `refresh_portal_credential` — and that
    /// line runs after the history store and the watcher have been opened. A
    /// reconcile that outran it would answer "history isn't open" and change
    /// nothing, which is a flaky test, not a failure of the app.
    ///
    /// The card only re-reads the app's status when the shell handles a request
    /// (`read_state`), so each attempt commits the Server URL field with the
    /// value it already has — the field's own "commit on Enter" — which the
    /// shell answers with `App::set_portal` and a full re-read. Nothing about
    /// the setting changes.
    fn wait_for_the_app(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.harness.state().state.portal.status.signed_in {
                return;
            }
            assert!(Instant::now() < deadline, "the shell never read a stored token");
            self.commit_the_url_field();
            self.pump_until(Duration::from_millis(200), |a| a.state.portal.status.signed_in);
        }
    }

    /// Clicks the "Server URL" field and presses Enter, which is the commit
    /// `views::settings::portal_url_line` waits for.
    fn commit_the_url_field(&mut self) {
        {
            let field = self
                .harness
                .get_all_by_label("Server URL")
                .find(|n| n.accesskit_node().role() == egui::accesskit::Role::TextInput)
                .expect("the Server URL field");
            field.click();
        }
        self.frame();
        self.harness.key_press(egui::Key::Enter);
        self.frame();
    }

    /// The watch folders the app itself holds, as `<path>` strings.
    fn watch_paths(&self) -> Vec<String> {
        self.harness.state().state.watches.iter().map(|w| w.local.clone()).collect()
    }
}

impl Drop for Window {
    /// Stops the app's background work — its runtime, its watcher, its history
    /// store — before the folders it was pointed at go away.
    fn drop(&mut self) {
        if let Some(shell) = self.harness.state_mut().shell.as_mut() {
            shell.shutdown();
        }
    }
}

/// The button's label, as both the screen and the test name it.
const RECONCILE: &str = "Reconcile now";

/// Whether a rendered string is the Portal card's status line.
///
/// One sentence, and it is the only one the card, the page or the sidebar draws
/// with any of these openings: "Configured…" and "Not set up…" before anything
/// has run, "Failed …" after a failure, and a finished run's "<when> · …".
fn is_status_line(text: &str) -> bool {
    text.contains(" kept in step")
        || text.starts_with("Configured.")
        || text.starts_with("Failed ")
        || text.starts_with("Not set up:")
        || text.starts_with("Not signed in yet:")
        || text.starts_with("No Portal address yet:")
}

/// The folder the seeded "Farbhaus" link is mirrored into.
fn farbhaus(root: &Path) -> PathBuf {
    root.join("Farbhaus")
}

// ---------- the cases ----------

/// `Reconcile now` does the work, and the card says what it did: one watch
/// folder made from the one link that carries a usable marker, the hostile
/// marker and the account-less link skipped, and nothing removed. Pressing it
/// again changes nothing, and the card says that too.
#[test]
fn pressing_reconcile_now_makes_the_watch_and_then_reports_it_is_stable() {
    let _serial = one_at_a_time();
    let Some((url, token)) = live() else { return };
    let mut w = Window::new(&url, &token);

    // The card is on the Settings screen, waiting to be pressed: "Configured."
    // with no numbers, because nothing has run yet.
    w.wait_for_the_app();
    let before = w.line();
    println!("before: {} ({:?})", before.text, w.line_on_screen());
    assert!(!before.failed, "nothing has failed yet: {}", before.text);
    assert!(
        before.text.starts_with("Configured."),
        "the pre-reconcile sentence: {}",
        before.text
    );
    assert!(
        before.text.contains(RECONCILE),
        "the card says what to press: {}",
        before.text
    );
    assert_eq!(w.line_on_screen(), before.text, "the card draws the model's sentence");
    assert!(!farbhaus(&w.root).exists(), "nothing has been created yet");

    // Press it. The view pushes `Request::PortalReconcile`, the shell answers it
    // with `App::portal_reconcile`, and the status line comes back through
    // `read_portal` — so the words the card shows are the app's own report.
    w.press_reconcile_now();
    assert!(
        w.pump_until(Duration::from_secs(30), |a| {
            let s = &a.state.portal.status;
            s.last_at > 0 && !s.running
        }),
        "the button did nothing: the app never reported a reconcile. Card: {:?}",
        w.line().text
    );

    let first = w.line();
    println!("after the first press: {}", first.text);
    assert_eq!(w.line_on_screen(), first.text, "the card draws the report it was given");
    assert!(!first.failed, "the reconcile failed: {}", first.text);
    assert!(!first.text.contains("Failed"), "{}", first.text);
    assert!(first.text.starts_with("just now") || first.text.contains("ago"), "{}", first.text);
    // The numbers: three links read, one watch folder made, nothing removed,
    // two links skipped (the hostile marker and the one with no account).
    assert!(first.text.contains("3 links"), "three links were read: {}", first.text);
    assert!(
        first.text.contains("1 folder kept in step"),
        "one watch folder was made: {}",
        first.text
    );
    assert!(first.text.contains("0 removed"), "nothing was removed: {}", first.text);
    assert!(first.text.contains("2 skipped"), "two links were skipped: {}", first.text);

    // The watch folder is on disk, under the root the card is configured with.
    let folder = farbhaus(&w.root);
    assert!(folder.is_dir(), "{} was not created", folder.display());
    assert!(
        w.watch_paths().iter().any(|p| Path::new(p) == folder),
        "the watch list does not hold {}: {:?}",
        folder.display(),
        w.watch_paths()
    );

    // A second press: the same single watch, and no work left to do.
    let first_at = w.harness.state().state.portal.status.last_at;
    w.press_reconcile_now();
    assert!(
        w.pump_until(Duration::from_secs(30), |a| {
            let s = &a.state.portal.status;
            s.last_at > first_at && !s.running
        }),
        "the second press did nothing: the app never reported a second reconcile. Card: {:?}",
        w.line().text
    );

    let second = w.line();
    println!("after the second press: {}", second.text);
    assert_eq!(w.line_on_screen(), second.text, "the card draws the second report");
    assert!(!second.failed, "the second reconcile failed: {}", second.text);
    assert!(second.text.contains("3 links"), "{}", second.text);
    assert!(
        second.text.contains("0 folders kept in step"),
        "the second run had nothing to do: {}",
        second.text
    );
    assert!(second.text.contains("0 removed"), "{}", second.text);
    assert!(second.text.contains("2 skipped"), "{}", second.text);
    assert!(folder.is_dir(), "the folder is still there");
    assert!(
        w.watch_paths().iter().any(|p| Path::new(p) == folder),
        "one watch, still: {:?}",
        w.watch_paths()
    );
}

/// The other half: a token Portal refuses. The card says the reconcile failed,
/// and shows no numbers — a run that did nothing must not read as a run that
/// found nothing to do.
#[test]
fn pressing_reconcile_now_with_a_refused_token_reports_the_failure_and_no_counts() {
    let _serial = one_at_a_time();
    let Some(url) = live_url() else { return };
    // A credential is stored — the button is live — and Portal will not accept it.
    let mut w = Window::new(&url, "definitely-not-the-token");

    w.wait_for_the_app();
    // A token is stored, so the card offers the button even though it is wrong:
    // the check is "a credential is stored", not "the credential works".
    assert!(
        !w.harness.get_by_label(RECONCILE).accesskit_node().is_disabled(),
        "a stored token arms the button: {}",
        w.line().text
    );

    w.press_reconcile_now();
    assert!(
        w.pump_until(Duration::from_secs(30), |a| {
            let s = &a.state.portal.status;
            s.last_at > 0 && !s.running
        }),
        "the button did nothing: the app never reported a reconcile. Card: {:?}",
        w.line().text
    );

    let line = w.line();
    println!("after the refused press: {}", line.text);
    assert_eq!(w.line_on_screen(), line.text, "the card draws the failure it was given");
    assert!(line.failed, "a failed reconcile is bad news: {}", line.text);
    assert!(line.text.starts_with("Failed "), "{}", line.text);
    assert!(line.text.contains("refused the token"), "{}", line.text);
    assert!(line.text.contains("401"), "Portal's status is the message: {}", line.text);
    // No counts: a run that failed has none to show.
    assert!(!line.text.contains("kept in step"), "the failure shows a count: {}", line.text);
    assert!(!line.text.contains(" links"), "the failure shows a count: {}", line.text);
    assert!(!line.text.contains("removed"), "the failure shows a count: {}", line.text);
    let status = &w.harness.state().state.portal.status;
    assert_eq!(status.ensured, 0, "nothing was made");
    assert_eq!(status.removed, 0, "nothing was removed");
    assert_eq!(status.links_seen, 0, "no links were read");

    // And nothing reached the disk: not the folder, not the root either.
    assert!(!farbhaus(&w.root).exists(), "a folder was made without a working credential");
    assert!(w.watch_paths().is_empty(), "watches: {:?}", w.watch_paths());
    // The refused token, and every setting the press wrote, are in the temporary
    // HOME: pressing the button cannot have touched the real one.
    assert!(
        fiotransfer::config::dir().starts_with(w.home.path()),
        "the app wrote outside the temporary HOME: {}",
        fiotransfer::config::dir().display()
    );
}

/// The wording the two cases above rely on, held to the card's own rules with no
/// live host and no window: a finished run is its numbers, a failed one is its
/// reason and no numbers. (The card's own tests cover the rest of the sentence;
/// this is the pair this file asserts against.)
#[test]
fn the_sentences_the_live_cases_assert_on_are_the_cards_own() {
    use fiotransfer_egui::model::PortalView;
    let mut status = PortalView {
        enabled: false,
        signed_in: true,
        url: "http://127.0.0.1:18080".to_string(),
        root: "/tmp/Live_Work".to_string(),
        last_at: 1_000,
        now_ms: 1_400, // "just now": 0..=4 s
        links_seen: 3,
        ensured: 1,
        removed: 0,
        skipped: 2,
        ..Default::default()
    };
    assert_eq!(
        portal_line(&status).text,
        "just now · 3 links, 1 folder kept in step, 0 removed, 2 skipped"
    );
    status.ensured = 0;
    assert_eq!(
        portal_line(&status).text,
        "just now · 3 links, 0 folders kept in step, 0 removed, 2 skipped"
    );
    status.error = "Portal refused the token (401). It may be wrong".to_string();
    status.ensured = 1;
    let failed = portal_line(&status);
    assert!(failed.failed);
    assert_eq!(failed.text, "Failed just now: Portal refused the token (401). It may be wrong");
    assert!(!failed.text.contains("kept in step"));
}

/// The app refreshes the stored credential as it starts, on the shell's own
/// runtime — not in response to anything the window does. The card has to notice
/// that by itself: on a real launch its first frame can beat the app's startup, and
/// a card that then says "Not signed in yet" with the button disabled looks like a
/// broken connection with a valid credential sitting in the keychain.
#[test]
fn the_card_reads_the_credential_without_the_user_touching_anything() {
    let Some((url, token)) = live() else { return };
    let _g = one_at_a_time();
    let mut w = Window::new(&url, &token);

    // No click, no field commit, no reconcile: frames and real time only.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let line = w.line().text.clone();
        if line.starts_with("Configured") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the card never read the stored credential on its own: it still says {:?}",
            line
        );
        w.frame();
        std::thread::sleep(Duration::from_millis(5));
    }

    let button = w.harness.get_by_label(RECONCILE);
    assert!(
        !button.accesskit_node().is_disabled(),
        "the card read the credential but Reconcile now is still disabled"
    );
}

/// The path a person takes: no token, no command line — type the admin's email and
/// password into the card, press Sign in, and then press Reconcile now. It is the
/// credential route `docs/PORTAL.md` recommends wherever the deployment has no
/// `PORTAL_API_TOKEN`, and until this case existed it had only ever been driven
/// through the core, never through the window.
#[test]
fn signing_in_on_the_card_creates_the_folders() {
    let Some(url) = live_url() else { return };
    let (email, password) = match (
        std::env::var("PORTAL_LIVE_EMAIL"),
        std::env::var("PORTAL_LIVE_PASSWORD"),
    ) {
        (Ok(e), Ok(p)) if !e.trim().is_empty() && !p.trim().is_empty() => (e, p),
        _ => {
            eprintln!("skipped: PORTAL_LIVE_EMAIL / PORTAL_LIVE_PASSWORD not set");
            return;
        }
    };
    let _g = one_at_a_time();
    let mut w = Window::unsigned(&url);

    // The card has to have read the app before the rows are there to type into.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let words = w.screen_words();
        if words.iter().any(|s| s.contains("Sign in instead")) {
            break;
        }
        assert!(Instant::now() < deadline, "the card never drew its sign-in row: {:?}", words);
        w.frame();
        std::thread::sleep(Duration::from_millis(5));
    }

    // The masked fields are not in the accessibility tree (egui does not expose a
    // password field's node as a text input), so the keystrokes into them are the
    // view's own kittest cases' business. What is *not* covered anywhere else is the
    // hop the button makes: the view pushes `Request::SignInPortal`, the shell
    // answers it against the app, and the card re-reads. That is what this pushes.
    {
        let state = &mut w.harness.state_mut().state;
        state.ask(fiotransfer_egui::model::Request::SignInPortal {
            email: email.clone(),
            password: fiotransfer_egui::model::Secret::new(password.clone()),
        });
    }
    w.frame();

    assert!(
        w.screen_words().iter().any(|s| s == "Sign in"),
        "the card drew no Sign in button at all"
    );

    // The app's own sign-in runs on the shell's runtime; wait for the card to say so.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let words = w.screen_words();
        if words.iter().any(|s| s.starts_with("Signed in as ")) {
            assert!(
                words.iter().any(|s| s.contains(&email)),
                "signed in, but not as the address that was typed: {:?}",
                words
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the card never reported a session after Sign in: {:?}",
            words
        );
        w.frame();
        std::thread::sleep(Duration::from_millis(10));
    }

    // And the password is nowhere on screen.
    assert!(
        !w.screen_words().iter().any(|s| s.contains(&password)),
        "the password was rendered"
    );

    // Now the button, with a session instead of a token.
    w.press_reconcile_now();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if w.watch_paths().iter().any(|p| p.ends_with("Farbhaus")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a session-authenticated reconcile created no watch: {:?}",
            w.line().text
        );
        w.frame();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(farbhaus(&w.root).is_dir(), "the mirrored folder is not on disk");
}

/// The poller is what makes "Portal controls the folders" true between visits: turn
/// it on and the app reconciles by itself, with nobody pressing anything. Every
/// other case here keeps it off so a background run cannot answer for a button.
#[test]
fn the_poller_reconciles_by_itself() {
    let Some((url, token)) = live() else { return };
    let _g = one_at_a_time();
    let mut w = Window::build_with(&url, Some(&token), true, 5);

    // The app's own startup has to have finished before the poller exists at all —
    // it is spawned at the end of `App::start`. Wait for the card to read the
    // credential, which only happens after that.
    let startup = Instant::now() + Duration::from_secs(10);
    loop {
        if w.line().text.starts_with("Configured") {
            break;
        }
        assert!(Instant::now() < startup, "the app never finished starting: {:?}", w.line().text);
        w.frame();
        std::thread::sleep(Duration::from_millis(10));
    }

    // No click anywhere. Only frames and real time, until the watch appears.
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if w.watch_paths().iter().any(|p| p.ends_with("Farbhaus")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the poller never reconciled on its own: {:?}",
            w.line().text
        );
        w.frame();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(farbhaus(&w.root).is_dir(), "the polled reconcile left no folder on disk");
}
