# The "Portal" marker on the Watch Folders screen

Branch `portal-badge`, cut from `origin/main` at `2706131`. Worktree
`/tmp/portal-badge`, `CARGO_TARGET_DIR=/tmp/target-portal-badge`, every cargo
command `--offline`.

The screen could not say whose a watch folder was. That made the reconciler look
like a bug: delete one of Portal's watches and the next reconcile puts it back,
because Portal still asks for it, with nothing on screen saying the row was ever
Portal's. The fix is one muted word and the sentence that explains it — no new
colour, no new box, no transfer behaviour touched, and no way out of the screen
that would be a lie.

## What the user sees

| Row | Folder cell, second line | Tooltip (marker and the row) |
| --- | --- | --- |
| a watch the user made | `~/Movies/Rushes` | the whole path, as before |
| a watch Portal made | `Portal  ~/Live_Work/Ep 04` | "Kept in step by Portal. Remove the marker in Portal to drop it." |

`Portal` is a label of its own, drawn in the same `theme::detail()` (12 px) and
`theme::TEXT_3` the path beside it uses, so the row gains no colour and no font.
It is drawn *before* the path so truncation can never eat it.

The row keeps its Remove button. The sentence states the consequence instead:
`"Kept in step by Portal. Remove the marker in Portal to drop it."` Adding a
Portal-specific delete, or hiding the common one, would promise something the
reconciler undoes.

## Files

| File | Change |
| --- | --- |
| `core/src/xfer/watch.rs` | `WatchView.origin` — the record's own `origin`, copied in `Watcher::views` beside `id`, `link` and the counters. This is what puts it in the bridge payload: `src/app.rs`'s `Watches` arm is `ok(app.watches())`, i.e. `serde_json::to_value` of exactly these views. |
| `egui-ui/src/model.rs` | `WatchRow.from_portal: bool`, and `WatchRow::from_core` fills it from `portal::agent::link_of(&w.origin)` — the reconciler's own reading of ownership, not a second `starts_with("portal:")` that could drift. |
| `egui-ui/src/shell.rs` | unchanged; `read_watches` already builds every row with `WatchRow::from_core`, so the flag arrives with the rest of the row. |
| `egui-ui/src/views/watch.rs` | `PORTAL_MARKER`, `PORTAL_SENTENCE`, `portal_marker`, `path_line`; `strip` now returns its row `Response` so a Portal row can carry the sentence as a tooltip. |
| `egui-ui/src/fixture.rs`, `egui-ui/src/views/sidebar.rs`, `egui-ui/tests/snapshots.rs` | the `WatchRow` literals gain `from_portal: false`: every fixture row is still the user's, so no existing screen changes. |
| `tests/watch_portal_payload.rs` | new: the payload test (below). |
| `egui-ui/tests/snapshots/watch_portal.png` | new: the whole Watch screen at 956x780 with one user row and one Portal row. |

Nothing in `core/src/portal/`, the reconciler, `xfer::Watch` or the store was
changed: `origin` already existed, round-tripped and was written only by
`Watcher::set_origin`.

## The layout landmine this hit

The obvious implementation — wrap the name and the path in a `ui.horizontal` so
the marker can sit beside the path — moved the path **8.5 px down** on every row,
user rows included: `ui.horizontal` centres its line inside the space left under
the name. Measured with kittest rects: the path's centre sat at y 148.5 on a user
row and 214.0 on the next one, a pitch of 65.5 px where the row pitch is 57.

So the user's branch draws the plain label, exactly as before, and the Portal
branch allocates one line of exactly `row_height(detail)`. With that, both rows
sit on the 57 px pitch (path centres 148.5 and 205.5).

## Tests

* `a_portal_row_carries_the_marker_and_the_sentence` — the marker is a label of
  its own, vertically on the path's line and to its left, and hovering it puts
  the full sentence on screen (kittest finds `PORTAL_SENTENCE` as a node).
* `a_row_the_user_made_shows_no_marker_and_no_sentence` — no marker, and hovering
  the path brings no sentence up either; the four common actions are still there.
* `one_portal_row_and_one_user_row_in_the_same_list` — two rows, one of each: one
  marker, two Remove buttons, and the second row exactly `ROW_HEIGHT` below the
  first.
* `the_watch_screen_with_a_portal_row_renders_to_png` — `harness.snapshot("watch_portal")`.
* `tests/watch_portal_payload.rs::a_portal_origin_survives_the_watch_payload` — a
  real `Store` with `origin = portal:L42` and `origin = ""`, a real `Watcher` over
  it, `App::watches()`, the bridge's own `serde_json::to_value`, and then
  `WatchRow::from_core`: the payload's `origin` is `portal:L42` and `""`, and the
  rows come back `from_portal: true` and `false`.

## The snapshots

`cargo test -p fiotransfer-egui --test snapshots` renders the five screens (plus
`space-empty`) into `/tmp/egui-shots`. Run against the base worktree
(`/tmp/portal-base`, `origin/main` @ `2706131`) and against this branch, the six
PNGs are **byte-identical**, `watch.png` included:

| screen | md5, base and branch |
| --- | --- |
| transfers | `0f13d185d1039eee2a2c16bdc9f91a97` |
| space | `a31044c579a10e346eedb52baec3197f` |
| space-empty | `57a86f0d2e8156b2e1093158ff285319` |
| watch | `18895697a5647f0ba990fe5bc90c1cc7` |
| history | `3be945399d8a17afcb05f44c31109053` |
| settings | `b9dc3d1efd450cefd45d70ba73ed60ed` |

So the existing watch snapshot does not change when every row is user-made — as
required, since the fixture's two watches are the user's and the branch that
draws a user row is the old code path unchanged.

`egui-ui/tests/snapshots/watch_portal.png` (956x780, md5
`da0f828066e244ab1c8a233162e96c4f`) is the new whole-screen render, one user row
`Rushes`/`Rushes 2026` and one Portal row `Ep 04`/`Shows / Nightfall / Ep 04`.

## What could not be verified: the picture

This session has **no vision**: `attach_image` refuses
("deepseek-v4.1-flash does not support vision"), so nobody here has *looked* at
`watch_portal.png`. What is verified instead is arithmetic on its pixels, by
rendering the same screen twice — both rows the user's, then the second row
Portal's — and diffing (`/tmp/scratch-user.png` vs `/tmp/scratch-portal.png`):

* 961 pixels differ, and **all** of them lie in y 200..212, x 63..209 — the
  Portal row's detail line, where the marker pushes the path right;
* the user row above (y 124..163) differs in **0** pixels;
* the most common non-background colour in the marker's box is exactly
  `(125, 129, 152)` = `theme::TEXT_3`, anti-aliased — the marker adds no colour;
* both rows' detail lines sit on the same 57 px pitch (path centres 148.5 and
  205.5).

Whether the marker *reads* well at 12 px beside the path is a judgement this
session cannot make. Someone with eyes should look at
`egui-ui/tests/snapshots/watch_portal.png` before merging.

## Checks

| Command | Result |
| --- | --- |
| `cargo check --offline --all-targets` | clean; the warnings it prints are the ones already on `origin/main` |
| `cargo test --offline --workspace` | 579 passed, 0 failed, 33 test binaries |
| `cargo test --offline -p fiotransfer-egui --test snapshots` | 10 passed |
| `cargo test --offline --test watch_portal_payload` | 1 passed |
