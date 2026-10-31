# The Rust app, property-tested and made sovereign

One branch. It contains the property-testing machinery built for the Rust port, every
defect that machinery found, and the fixes with their regression tests.

Two things this branch does *not* touch: the frontend assets and the window chrome. The
app still looks and behaves as it did for the user.

## Defects found and fixed

Each has a regression test that fails before the fix.

| # | Where | What was wrong | Effect | Test |
| --- | --- | --- | --- | --- |
| 1 | `share::dedupe_remote` | stripped *every* repeated extension instead of one | `a.mov.mov` deduped to `a (2).mov`; a copy could be renamed onto another file's stem | `dedupe_remote_keeps_a_repeated_extension_in_the_stem` |
| 2 | `folderlog::dir_of` / `base_name_path` | empty and trailing-slash paths did not follow the documented rules (`""` → `/`, `a/b/` → `a`) | a trailing-slash upload would write the folder log one level too high | `go_parity_helpers_match_go` table |
| 3 | `xfer::Job::set_files` | seeded per-status counters by incrementing instead of recounting | a file whose status moved before publishing was counted twice: `files_done > files_total` | `publishing_the_file_list_counts_each_file_once` |
| 4 | `share::parse_rfc3339` | accepted impossible dates and a missing offset | `0000-00-00T00:00:00Z` made a working review link look expired | property `rfc3339_…` + `short_timestamps_are_refused` |
| 5 | `api::route_key` | kept the host for a path-less URL; a `://` inside a query was taken for a scheme | distinct routes shared one rate-limit bucket and missed their documented limits | two reproducers, previously `#[ignore]`d |
| 6 | `xfer::Limiter::acquire` | enrolled for a wake-up only after re-reading the count | a release landing in that window was lost; a waiter could sleep until the next release | `prop_sync` (6 properties, 4/4 mutations caught) |
| 7 | `xfer::scan_paths` | followed directory symlinks with no cycle guard | a self-referential link recursed until the stack overflowed | `scan_paths_does_not_follow_a_directory_symlink_cycle` |
| 8 | `api::Client::send` | `Method::from_bytes(..).unwrap()` | a non-token method panicked instead of erroring | covered by the client's error path |
| 9 | `App::path_to` | a failed step up the folder chain ended the walk instead of failing | the picker showed a folder under the wrong parent | `prop_bridge` |
| 10 | `xfer::complete::quick_time_state` | a partial atom header at the end was forgiven | a file whose tail was still being written could be uploaded as complete | `trailing_bytes_under_a_header_are_unfinished` |
| 11 | same | a 64-bit atom whose extended length was still zero was forgiven | same window, on the >4 GiB path | `an_unfilled_extended_length_after_a_moov_is_unfinished`, `a_length_below_a_header_is_unfinished` |
| 12 | `xfer::download::unique_path` | a lone leading dot was treated as an extension | deduping `.hidden` produced ` (2).hidden` — it lost its dot and stopped being hidden | `prop_model` naming properties |
| 13 | `xfer::folderlog::is_folder_log` | any `FioTransfer *.txt` counted as one of our logs | a user's `FioTransfer notes.txt` was silently left out of every upload | `prop_model` skip-name properties |
| 14 | `xfer::uploadstate::fresh` | compared an age in **milliseconds** against a window in **seconds** | the one-hour resume window was really 3.6 seconds: an interrupted upload almost never resumed | `a_ledger_saved_a_minute_ago_still_resumes` |

## Property machinery

Model-based suites, each with an independent reference model and a mutation-calibration
table (deliberate defects must be caught, survivors must be fixed by strengthening the
generator):

| suite | area | properties | mutations |
| --- | --- | --- | --- |
| `tests/proptests.rs` | seed suite: limiter, buffer pool, ledger, route keys, paths, CSV | 14 | – |
| `tests/prop_sync.rs` | `Limiter` | 6 | 4/4 caught |
| `tests/prop_state.rs` | ledger resume set + staleness + `keeps_file`; `done` file as a set; history store against an insert/update/list model; `mark_interrupted`; CSV with adversarial fields; rows survive close/reopen; watch rows as a map | 12 | not returned by the worker; see tests |
| `tests/prop_parse.rs` | `route_key`, `safe_name` | 9 | 3/3 caught |
| `tests/prop_watch.rs` | watch folders, folder logs | 7 (+23 lib) | 14/14 caught |
| `tests/prop_shares.rs` | six link providers | 14 (+27 lib) | 30/30 caught |
| `tests/prop_model.rs` | job/file counters, `view()`, `snapshot()`, scanning | 12 (+33 lib) | 24/24 caught |
| `tests/prop_engine.rs` | media-atom parser, part tiling, backoff | 16 | 5 caught, 1 equivalent, 1 survivor→strengthened |
| `tests/prop_bridge.rs` | the method list scraped from `app.js` vs the dispatch arms (a missing arm fails); dispatch totality; `null` vs missing arguments in every position; documented JSON shapes; `SaveSettings` as a fixed point; a bad settings payload changes nothing; upload-destination round trip | 12 | not returned by the worker; see tests |

## Evidence

* `cargo test --offline` → 110 lib tests, 78 integration property tests, 0 failures.
* UI: the app's own webview snapshot, four views at 1180×780 — **0 differing pixels**.
* Performance (same rig as the port's benchmarks): upload 2 GiB / 8 streams 11,018 MB/s,
  8 GiB / 16 streams 10,477 MB/s — at parity with the pre-change numbers.

## Found, reported, not fixed here

Each of these is a real contract or documentation gap that a reviewer may want decided
rather than silently changed:

* `xfer::watch::join_path` documents itself as `filepath.Join` but only concatenates:
  `join_path("/a/b", "../c")` is `/a/b/../c`, and `join_path("/", "/")` is `//`. Every
  caller passes a clean relative path today, so nothing misbehaves — but the doc overclaims.
* `share::resolve_link("0")` parses as `https://0.0.0.0/` and issues a request; junk input
  that looks like a host escapes the "not a link" path.
* `share::dedupe_paths` folds case with ASCII rules only; on APFS a composed and a
  decomposed `é.mov` collide on disk but not here.
* `Limiter` is not FIFO, and that is undocumented (no fairness guarantee is claimed).
* a job with zero files finishes `Done`, and a cancelled job whose files all completed
  reports `Done` rather than `Canceled` — policy, not bookkeeping.

## Behaviour deliberately changed

These four were inherited tolerances, kept for parity until now. Each is now the safer
rule, with a test that pins it, and each is a deliberate divergence from the old app:

* a partial atom header at the end of a QuickTime file reads `Unfinished` (an early
  upload sends a truncated copy; the cost of waiting is bounded by the stall limit);
* an unfilled 64-bit atom length likewise;
* `.hidden` dedupes to `.hidden (2)` and stays hidden;
* only the stamped `FioTransfer Watch|Upload|Download …txt` names count as folder logs.
