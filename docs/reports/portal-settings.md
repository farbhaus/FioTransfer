# Portal in the Settings screen

Branch `portal-ui`, cut from `origin/consolidate` (the branch of PR #12) at
`fd419b0`. Worktree `/tmp/portal-ui`, `CARGO_TARGET_DIR=/tmp/target-portal-ui`,
every cargo command `--offline`. No PR was opened: this joins #12.

The screen the user gets is a fifth card, **Portal**, drawn below Storage and
above the `.about` line. Nothing that was already there moved, changed colour or
changed height: the card is the same `.set-title` + `.set-card` + `.set-line`
stack, the fields are the `.input` the Client ID row already uses (32 px, surface
fill, 10 px either side), the help lines are the 12 px `--text-3` lines the
Version row already draws, and the only colour the section adds is
`theme::ERROR_TEXT`, which "Check now" already uses for a failed update.

## The six rows

| Row | What it is | Request |
| --- | --- | --- |
| **Server URL** | Text field bound to `portal_url`, committed on Enter or on focus loss. A value that is not `http(s)://` + a host is refused: the reason is drawn under the field, the field keeps what was typed so it can be fixed, and `settings.portal_url` keeps the value it already had. | `SetPortalUrl(String)` |
| **Access token** | Password field (`.password(true)`), placeholder `stored in your keychain` when one is stored and `not set` when not, with **Save** and **Forget**. The help line under it says which of the two states it is in. | `SetPortalToken(Secret)`, `ForgetPortalToken` |
| **Local folder** | Text field bound to `portal_root`, same commit rule, with the **resolved absolute path** under it ("Folders are created under /Users/chaos/Live_Work"). Empty means the default `~/Live_Work`. | `SetPortalRoot(String)` |
| **Enabled** | The same checkbox row as "Save log with media". Under it: "Nothing is contacted while this is off." / "Checked every 30 s in the background." | `SetPortalEnabled(bool)` |
| **Status** | One sentence from `portal_status()`: "Not set up: no Portal address and no token yet." / "No access token yet: …" / "Failed 2 min ago: can't reach Portal: …" (error colour) / "2 min ago · 2 links, 3 folders kept in step, 1 removed". | — |
| **Reconcile now** | Button, disabled with the reason under it ("Add the Portal address first." / "Add the access token first.") until there is both a URL and a token. | `PortalReconcile` |

A failed reconcile can never read as a success: `portal_line` returns the numbers
**only** when the last report has no error, and the failing branches never name a
link count.

### Where the token goes

The token is not a setting, and the section keeps it that way:

* the field is a `TextEdit` with `.password(true)`, so what is drawn is egui's
  mask; the accesskit text runs carry the mask, not the token, which is what
  `the_token_is_never_rendered_and_never_a_setting` greps for;
* the request carries it as `model::Secret`, whose `Debug` prints `Secret(…)` —
  a token cannot reach a log line through `{:?}` of a `Request`;
* the shell hands it to `App::set_portal`, which calls `portal::set_token`
  (Keychain, or the 0600 file), and then empties the field.

## Files

| File | Change |
| --- | --- |
| `core/src/app.rs` | `App::forget_portal_token()` — clears the token **and** the cached `signed_in` flag, so the status line says so at once. Four lines; `portal::clear_token()` alone left `portal_signed_in` true until the next reconcile. |
| `egui-ui/src/model.rs` | `PortalView` (a copy of the core `PortalStatus` plus `now_ms`, the way `UpdateState` copies `UpdateStatus`), `PortalUi` (status + the three drafts + the two refusal reasons), `Secret`, and six `Request` variants. |
| `egui-ui/src/model_browser.rs` | the five Portal `Settings` fields for the browser capture, same names and defaults as the core's. |
| `egui-ui/src/shell.rs` | answers the six requests through `App::set_portal` (one call owns the URL, the root, the flag and the token), `read_portal` on every state read, and the `portal` event the app already emitted. |
| `egui-ui/src/views/settings.rs` | the card, its six row builders, `portal_url_reason`, `portal_root_reason`, `resolved_portal_root`, `portal_when`, `portal_line`, and 19 tests. |
| `egui-ui/src/fixture.rs` | the fixture's Portal state: configured, a token stored, and a reconcile two minutes before a pinned `now`, so the status line reads the same in both captures. |
| `tests/prop_portal.rs` | two file-level tests: the URL save writes no token to `settings.json`, and "Forget" changes no setting. |

No new dependency, no `unsafe` (`#![forbid(unsafe_code)]` still holds), and
nothing here touches transfers, the Space screen or the delete path.

## Tests

`cargo test --offline --workspace`, before and after this branch:

| | passed | failed | ignored |
| --- | --- | --- | --- |
| before (`origin/consolidate`) | 487 | 0 | 4 |
| after (`portal-ui`) | 508 | 0 | 4 |

+21 tests, no test changed except where the fourth toggle row and the second
"Save" button made an old count wrong:

* `the_three_toggles_write_their_settings` → the four toggle rows (the Portal
  one is last), same assertions;
* `signed_out_settings_shows_the_client_id_setup_and_every_section` and
  `saving_the_client_id_writes_the_field_and_asks_the_app_to_save` now take the
  **first** "Save" (`get_all_by_label("Save").next()`): there are two now, the
  Client ID's and the token's, and the token's is the last.

kittest (`views::settings::tests`), in the idiom `views::space` uses:

* `the_portal_section_is_the_last_card_and_names_its_five_rows` — the card and
  its eight words, the address in the field, the resolved folder line;
* `a_bad_portal_url_is_refused_and_the_old_value_kept` — types
  `portal.example.com` over the stored URL (Cmd+A, type, Enter), gets the reason
  under the field, and asserts `settings.portal_url` is unchanged **and** that no
  `SetPortalUrl` was ever asked for;
* `a_good_portal_url_is_committed_and_asks_the_app` — the same typing with a good
  value: `SetPortalUrl("https://portal.example.com")` (trailing slash trimmed),
  the draft cleared, no reason line;
* `the_portal_toggle_asks_the_app_for_what_it_wrote` — off → on → off, and the
  `SetPortalEnabled(true)` / `(false)` the shell expects;
* `reconcile_now_waits_for_an_address_and_a_token` and `reconcile_now_asks_the_app`
  — the button is `is_disabled()` with neither, with a URL and no token, and live
  with both; a click asks for `PortalReconcile`;
* `the_status_line_reads_a_finished_reconcile_as_its_numbers` and
  `the_status_line_reads_a_failure_as_a_failure` — "2 min ago · 2 links, 3
  folders kept in step, 1 removed", and for a failure "Failed 2 min ago: can't
  reach Portal: connection refused" with no link count in it;
* `the_token_is_never_rendered_and_never_a_setting` — types a token, walks
  **every** accesskit label and value in the tree and finds it in none, then
  asserts the serialised `Settings` (what `settings.json` holds) and `{:?}` of the
  requests do not contain it either;
* `forget_asks_the_app_and_save_does_nothing_with_an_empty_field`;
* `the_portal_card_is_laid_out_like_the_cards_above_it` — the title's left edge
  matches "Storage"'s, the labels start on the same padding, and the three Portal
  fields are the height of the Client ID field;
* unit tests for `portal_url_reason`, `portal_root_reason`,
  `resolved_portal_root`, `portal_when` and `portal_line`.

File level (`tests/prop_portal.rs`, where `HOME` is the case's own temp dir and
`FIO_NO_KEYCHAIN=1`):

* `saving_the_url_alone_never_writes_the_token_into_settings_json` — stores a
  token, then saves a **new URL with an empty token** (exactly what the field's
  commit passes), then reads `config::path()` back: no token, the new URL, and
  the token still in the keychain stand-in. Also sweeps every `*.json` in the
  app's data folder;
* `forgetting_the_token_leaves_the_settings_alone` — `forget_portal_token()`
  clears the token and the `signed_in` the status line reads, and leaves
  `settings.json` byte for byte as it was.

## Snapshots

**The brief's `egui-ui/tests/snapshots/settings_*.png` do not exist on this
branch**, so there was nothing to re-bless. What exists:

* `egui-ui/tests/snapshots/` holds `space.png`, `space-empty.png` and the two
  `sidebar_*.png` (kittest goldens, compared with a pixel diff). All four are
  **unchanged** — `git status` shows none of them as modified, which is the
  "the four other screens did not change" check;
* `egui-ui/tests/snapshots/settings_portal.png` is **new**: the whole Settings
  screen at 956x2400 with the Portal card populated (2 links, 3 folders kept in
  step, 1 removed, "2 min ago"), drawn by
  `the_portal_section_draws_with_populated_status`;
* `cargo test -p fiotransfer-egui --test snapshots` writes the six window-size
  artefacts to `/tmp/egui-shots/`. I rendered them from a second worktree at
  `origin/consolidate` (`/tmp/portal-base`) and compared md5s:

  | artefact | base | this branch |
  | --- | --- | --- |
  | `transfers.png` | `0f13d185…` | `0f13d185…` same |
  | `space.png` | `a31044c5…` | `a31044c5…` same |
  | `space-empty.png` | `57a86f0d…` | `57a86f0d…` same |
  | `watch.png` | `18895697…` | `18895697…` same |
  | `history.png` | `3be94539…` | `3be94539…` same |
  | `settings.png` | `b9dc3d1e…` | `b9dc3d1e…` same |

  **`settings.png` did not change either**, and the reason is honest rather than
  convenient: that render is the 1180x780 window, and the Portal card sits below
  the fold, inside the shell's `ScrollArea`. The section is only in the
  2400 px golden above. A snapshot of the window *scrolled* to the card would
  need a synthetic wheel event; I did not add one, so the in-window look of the
  card is unchecked by a golden (see "not verified").

## What I could not verify

* **The card as a user first sees it in the 780 px window.** It is below the
  fold; the golden that covers it renders the whole page at 2400 px. I have no
  way to look at a PNG in this session (no vision), so every check here is
  numeric or textual: accesskit labels, node rects, md5s. The pixel *look* of the
  new card is asserted only through the shape checks in
  `the_portal_card_is_laid_out_like_the_cards_above_it`.
* **A folder picker.** The brief asked the row to *show* the resolved absolute
  path; I also made the field editable, so `portal_root` is reachable without
  `settings.json`. There is no "Change…" panel: `App::pick_download_dir` titles
  its dialog "Download to…", and adding a core method for one dialog was more
  than this brief asked for.
* **The shell's handlers, end to end.** `Shell` needs a real `App` (tokio,
  rusqlite, the Keychain), so no test drives `Request::SetPortalUrl` into
  `settings.json`. The two halves are covered instead: the view's tests assert
  the exact `Request` each row pushes, and `tests/prop_portal.rs` asserts what
  `App::set_portal` / `App::forget_portal_token` do to the file — with the same
  empty-token call the shell makes.
* **The browser capture** (`egui-ui/src/wasm.rs`) compiles for
  `wasm32-unknown-unknown` as a library
  (`cargo check --target wasm32-unknown-unknown -p fiotransfer-egui --lib`: clean,
  with the five new Portal `Settings` fields in `model_browser.rs`). The
  *binary* target fails on this target both here and on `origin/consolidate`
  (it needs the `native` feature), so that failure is pre-existing; Trunk builds
  the cdylib.
* **`Request` still derives `Debug`.** The token is wrapped in `Secret` so it
  prints as `Secret(…)`, but the *draft* lives in `UiState::portal.token_draft`
  as a plain `String` (a `TextEdit` needs one), and `UiState` derives `Debug`.
  Nothing debug-prints `UiState` today; if that ever changes, that field is the
  one to redact.
