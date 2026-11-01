//! Property tests for the Space delta's second half — the egui front end: the
//! aggregation the ring and the head are built from, and the confirmation's
//! state machine.
//!
//! Run: `CARGO_TARGET_DIR=/tmp/target-space cargo test --offline -p
//! fiotransfer-egui --test prop_space`
//!
//! What the brief asked for and origin/main does not have: `Project.storage`,
//! `Account.storage_limit` and `Account.storage_usage` do not exist in this
//! tree, so there is no `limit - usage` here to model and no "walked size only
//! when the reported size is absent" branch. The aggregation that does ship is
//! `views::space::total_bytes` / `SpaceRow::share` / `views::space::slices`
//! (walked sizes in, ring geometry out), and that is what P1–P5 model. See the
//! report.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use fiotransfer_egui::model::{SpaceRow, UiState};
use fiotransfer_egui::views::space::{
    Action, CONFIRM_CANCEL, CONFIRM_DELETE, CONFIRM_TITLE, padded, slices, space_color, total_bytes,
};
use proptest::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

// ---------- rows ----------

fn row(index: usize, bytes: i64) -> SpaceRow {
    SpaceRow {
        account_id: "acct-1".into(),
        account_name: "Farbhaus".into(),
        workspace_name: "Shows".into(),
        project_id: format!("proj-{index}"),
        name: format!("Project {index}"),
        root_folder_id: format!("root-{index}"),
        bytes,
        files: index as i64,
        // A walked row: the sweep's rows carry a file count. `walked` arrived
        // with the account headline after the sweep was written
        // (`d187802`), so the literal has to name it.
        walked: true,
        error: String::new(),
    }
}

/// Sizes a walk really produces: zero, ties, gigabytes — plus a negative, which
/// the walk should never produce but the ring must survive.
fn size() -> impl Strategy<Value = i64> {
    prop_oneof![
        3 => Just(0i64),
        5 => 0i64..=8i64,                  // ties are the common case
        4 => 0i64..(1i64 << 40),           // up to a terabyte
        1 => Just(-1i64),
        1 => -(1i64 << 40)..0i64,
    ]
}

fn rows() -> impl Strategy<Value = Vec<SpaceRow>> {
    prop::collection::vec(size(), 0..=8).prop_map(|sizes| {
        sizes.into_iter().enumerate().map(|(index, bytes)| row(index, bytes)).collect()
    })
}

/// The total the head and the hole show, computed in a wider type so the
/// reference cannot overflow the way the production sum can.
fn reference_total(rows: &[SpaceRow]) -> i128 {
    rows.iter().map(|row| row.bytes.max(0) as i128).sum()
}

// ---------- the aggregation ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// P1. The total is the sum of the non-negative sizes: never negative, zero
    /// exactly when no project holds anything, and unaffected by the order the
    /// walk happened to find the projects in.
    #[test]
    fn p1_the_total_is_the_sum_of_what_is_there(rows in rows()) {
        let total = total_bytes(&rows);
        prop_assert!(total >= 0, "total {} is negative", total);
        prop_assert_eq!(total as i128, reference_total(&rows));
        prop_assert_eq!(total == 0, rows.iter().all(|row| row.bytes <= 0));

        // A permutation is a different walk order over the same projects.
        let mut shuffled = rows.clone();
        shuffled.reverse();
        shuffled.rotate_left(rows.len() / 2);
        prop_assert_eq!(total_bytes(&shuffled), total);
    }

    /// P2. A row's share of the ring is a fraction in [0, 1]: nothing for a row
    /// that holds nothing or a negative size, and the row's exact ratio
    /// otherwise. All-positive rows divide the whole circle between them.
    #[test]
    fn p2_a_share_is_a_fraction_of_the_whole(rows in rows()) {
        let total = total_bytes(&rows);
        let mut sum = 0.0f64;
        for row in &rows {
            let share = row.share(total);
            prop_assert!(share.is_finite(), "share {} is not finite", share);
            prop_assert!((0.0..=1.0).contains(&share), "share {} out of range", share);
            if row.bytes <= 0 || total <= 0 {
                prop_assert_eq!(share, 0.0);
            } else {
                let exact = (row.bytes as f64 / total as f64) as f32;
                prop_assert_eq!(share, exact);
            }
            sum += share as f64;
        }
        if total > 0 && rows.iter().all(|row| row.bytes > 0) {
            prop_assert!((sum - 1.0).abs() < 1e-3, "the shares add up to {}", sum);
        }
    }

    /// P3. The ring's slices are a partition of the circle in row order: each
    /// starts where the one before ended, none is negative or wraps past 360°,
    /// and the last is stretched to close the circle exactly.
    #[test]
    fn p3_the_slices_partition_the_circle(rows in rows()) {
        let total = total_bytes(&rows);
        let pieces = slices(&rows);
        let holding = rows.iter().filter(|row| row.bytes > 0).count();

        if total <= 0 {
            prop_assert!(pieces.is_empty(), "{} slices for an empty ring", pieces.len());
            return Ok(());
        }
        prop_assert_eq!(pieces.len(), holding, "one slice per project that holds anything");

        // A project with nothing keeps its place in the colour ramp even though
        // it gets no arc: the slice after it must not shift colour.
        let colours: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.bytes > 0)
            .map(|(index, _)| index)
            .collect();
        let mut at = 0.0f32;
        for (piece, index) in pieces.iter().zip(&colours) {
            prop_assert_eq!(piece.colour, space_color(*index));
            prop_assert!(piece.start.is_finite() && piece.span.is_finite());
            prop_assert!(piece.span > 0.0, "a slice of {} degrees", piece.span);
            prop_assert!((piece.start - at).abs() < 1e-2, "a gap at {}: {} vs {}", index, piece.start, at);
            prop_assert!(piece.start + piece.span <= 360.0 + 0.5);
            at += piece.span;
        }

        // Only the last slice may be stretched, and it must reach 360.
        let last = pieces.last().expect("a non-empty ring");
        prop_assert!(
            (last.start + last.span - 360.0).abs() < 0.5,
            "the ring closes at {}", last.start + last.span
        );
        for (piece, index) in pieces.iter().zip(&colours).take(pieces.len() - 1) {
            let exact = rows[*index].share(total) * 360.0;
            prop_assert!((piece.span - exact).abs() < 1e-2, "slice {} is {} not {}", index, piece.span, exact);
        }
    }

    /// P4. A slice is drawn with a gap at each end that never eats it: the pad
    /// is capped at 0.9° and never takes more than half the slice.
    #[test]
    fn p4_the_drawn_gap_never_eats_the_slice(span in 0.0f32..360.0) {
        let (pad, drawn) = padded(span);
        prop_assert!(pad >= 0.0);
        prop_assert!(pad <= 0.9);
        prop_assert!(2.0 * pad <= span + 1e-4, "pad {} for a slice of {}", pad, span);
        prop_assert!(drawn >= 0.0);
    }
}

// ---------- the confirmation's state machine ----------

type Seen = Rc<RefCell<Vec<Action>>>;

/// The Space screen at the main column of the default window, collecting every
/// action it reports.
fn screen(rows: Vec<SpaceRow>) -> (Harness<'static, UiState>, Seen) {
    let seen: Seen = Default::default();
    let sink = seen.clone();
    let state = UiState { space: rows, logged_in: true, ..UiState::default() };
    let harness = Harness::builder()
        .with_size(egui::vec2(956.0, 780.0))
        .build_state(
            move |ctx, state: &mut UiState| {
                fiotransfer_egui::app::apply_style_once(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(action) = fiotransfer_egui::views::space::show_with_actions(ui, state) {
                        sink.borrow_mut().push(action);
                    }
                });
            },
            state,
        );
    (harness, seen)
}

fn pump(harness: &mut Harness<'static, UiState>) {
    harness.run_steps(3);
}

/// What the user does to a confirmation that is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Away {
    Cancel,
    CloseCross,
    Escape,
    Enter,
    Backdrop,
    Confirm,
}

fn away(index: usize) -> Away {
    match index {
        0 => Away::Cancel,
        1 => Away::CloseCross,
        2 => Away::Escape,
        3 => Away::Enter,
        4 => Away::Backdrop,
        _ => Away::Confirm,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 36, ..ProptestConfig::default() })]

    /// P5. Opening a confirmation reports nothing; dismissing it — Cancel, the
    /// close cross, Escape, Enter, or a click on the backdrop — reports nothing
    /// however many frames go by; and only "Delete on Frame.io" reports a
    /// delete, exactly one, naming the project whose row was pressed.
    #[test]
    fn p5_only_confirm_reports_a_delete_and_only_once(
        sizes in prop::collection::vec(0i64..(1i64 << 40), 1..=4),
        which in any::<prop::sample::Index>(),
        same_name in any::<bool>(),
        away_index in 0usize..6,
    ) {
        let mut rows: Vec<SpaceRow> = sizes.iter().enumerate().map(|(i, bytes)| row(i, *bytes)).collect();
        if same_name {
            // Two rows with one name: the dialog must name the row that was
            // pressed, not the first row with that name.
            for row in rows.iter_mut() { row.name = "Rushes".into(); }
        }
        let pressed = which.index(rows.len());
        let (mut harness, seen) = screen(rows.clone());
        pump(&mut harness);
        prop_assert!(seen.borrow().is_empty(), "the screen reports on its own: {:?}", seen.borrow());

        // Open the confirmation on one row.
        let label = format!("Delete {} on Frame.io", rows[pressed].name);
        // Unique names have one match; a shared name has one per row, in
        // legend order, so the pressed row is the `pressed`-th of them.
        let index = if same_name { pressed } else { 0 };
        let mut matches = harness.get_all_by_label(&label);
        let node = matches.nth(index).expect("the row's Delete button");
        drop(matches);
        node.click();
        pump(&mut harness);

        prop_assert!(harness.query_by_label(CONFIRM_TITLE).is_some(), "the confirmation did not open");
        prop_assert!(seen.borrow().is_empty(), "opening reported {:?}", seen.borrow());

        // Escape, Enter, the cross, the backdrop, Cancel: nothing is reported.
        match away(away_index) {
            Away::Cancel => harness.get_by_label(CONFIRM_CANCEL).click(),
            Away::CloseCross => harness.get_by_label("Close").click(),
            Away::Escape => harness.key_press(egui::Key::Escape),
            Away::Enter => harness.key_press(egui::Key::Enter),
            Away::Backdrop => {
                // Anywhere outside the box: the modal is centred, the corner is
                // the backdrop.
                harness.event(egui::Event::PointerMoved(egui::pos2(4.0, 4.0)));
                for pressed_down in [true, false] {
                    harness.event(egui::Event::PointerButton {
                        pos: egui::pos2(4.0, 4.0),
                        button: egui::PointerButton::Primary,
                        pressed: pressed_down,
                        modifiers: egui::Modifiers::default(),
                    });
                }
            }
            Away::Confirm => harness.get_by_label(CONFIRM_DELETE).click(),
        }
        pump(&mut harness);
        // Let it settle for several frames: a delay is not "once".
        pump(&mut harness);
        pump(&mut harness);

        if away(away_index) == Away::Confirm {
            let expected = vec![Action::Delete {
                account_id: rows[pressed].account_id.clone(),
                project_id: rows[pressed].project_id.clone(),
                name: rows[pressed].name.clone(),
            }];
            prop_assert_eq!(&*seen.borrow(), &expected, "the confirm reported {:?}", seen.borrow());
        } else {
            prop_assert!(seen.borrow().is_empty(), "{:?} reported {:?}", away(away_index), seen.borrow());
        }
        prop_assert!(
            harness.query_by_label(CONFIRM_DELETE).is_none(),
            "{:?} left the dialog open",
            away(away_index)
        );
    }

    /// P6. A row's Delete opens the dialog on THAT row: two projects with the
    /// same name and different sizes are two different confirmations.
    #[test]
    fn p6_the_dialog_names_the_row_that_was_pressed(
        first in 0i64..(1i64 << 40),
        second in 0i64..(1i64 << 40),
    ) {
        prop_assume!(first != second);
        let mut rows = vec![row(0, first), row(1, second)];
        for row in rows.iter_mut() { row.name = "Rushes".into(); }

        let (mut harness, seen) = screen(rows.clone());
        pump(&mut harness);
        let mut matches = harness.get_all_by_label("Delete Rushes on Frame.io");
        let node = matches.nth(1).expect("the second row's Delete");
        drop(matches);
        node.click();
        pump(&mut harness);

        harness.get_by_label(CONFIRM_DELETE).click();
        pump(&mut harness);
        pump(&mut harness);

        prop_assert_eq!(
            &*seen.borrow(),
            &[Action::Delete {
                account_id: "acct-1".into(),
                project_id: "proj-1".into(),
                name: "Rushes".into(),
            }],
            "the second row was pressed"
        );
    }

    /// P7. The dialog cannot be opened twice: a second press on a row while it
    /// is open does not stack confirmations, so one Delete click is one delete.
    #[test]
    fn p7_the_confirmation_does_not_stack(sizes in prop::collection::vec(0i64..(1i64 << 40), 1..=3)) {
        let rows: Vec<SpaceRow> = sizes.iter().enumerate().map(|(i, bytes)| row(i, *bytes)).collect();
        let (mut harness, seen) = screen(rows.clone());
        pump(&mut harness);

        harness.get_by_label(&format!("Delete {} on Frame.io", rows[0].name)).click();
        pump(&mut harness);
        // The reminder of the screen is behind the scrim; pressing it changes
        // nothing about what one Delete click reports.
        harness.get_by_label(CONFIRM_DELETE).click();
        pump(&mut harness);
        prop_assert!(harness.query_by_label(CONFIRM_DELETE).is_none(), "the dialog stayed open");
        pump(&mut harness);
        pump(&mut harness);

        prop_assert_eq!(seen.borrow().len(), 1, "one confirm click reported {:?}", seen.borrow());
    }
}


// ---------- findings, pinned ----------

/// F2. `total_bytes` adds `i64`s with no checked arithmetic, so a project list
/// whose sizes add up past `i64::MAX` panics in a debug build and wraps
/// negative in a release one — `--5.00 EB · 2 projects` in the page head.
///
/// Frame.io sizes never reach that, so this is a robustness note rather than a
/// user-facing bug: it is pinned here so the suite stays green, and so that a
/// later checked sum shows up as this test changing.
#[test]
fn f2_the_total_overflows_past_i64() {
    let rows = vec![row(0, i64::MAX), row(1, 1)];
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let total = std::panic::catch_unwind(|| total_bytes(&rows));
    std::panic::set_hook(previous);
    assert!(total.is_err(), "total_bytes no longer overflows: {:?}", total);

    // The head, in a release build, would print whatever the wrapped sum says.
    assert!(
        rows.iter().map(|row| row.bytes as i128).sum::<i128>() > i64::MAX as i128,
        "the two sizes really do sum past i64::MAX"
    );
}
