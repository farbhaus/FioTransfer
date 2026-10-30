# Portal → watch folders: the reconciler, and what it promises

What was built, why it is shaped the way it is, what the stub Portal could not
model, and what only the user's real host can answer.

Branch `portal-agent`, based on `origin/one-app` (`e33a6f2`) — see "The base"
below. Worktree `/Users/chaos/work/FioTransfer-portal`, its own
`CARGO_TARGET_DIR=/Users/chaos/work/fio-portal-target`.

## The model it implements

Portal publishes upload/download links and writes a `.turbosort` marker into a
link's Frame.io folder; the marker names the `Live_Work` folder the project
belongs to. For every link that carries such a marker, FioTransfer keeps one
watch folder, mirrored:

    <portal_root>/<turbosort marker>   <->   the link's Frame.io folder   (mirror)

* Nothing flows through Portal. The only traffic is `GET /api/upload-links`
  (plus `GET /api/sync-rules`, read-only, for reference). Every byte of media
  still goes FioTransfer <-> Frame.io.
* The local path is ours: `Settings::portal_root` (`~/Live_Work` by default) and
  the marker. Portal's `destination_path` / `path_template` are *not* used.
* Auth: the bearer token, stored under the Keychain item `portal_token` (with the
  same 0600 fallback file the Adobe session uses where there is no Keychain).
  It is never written to `settings.json`.
* Ownership: a watch made by the user has an empty `origin`; a watch Portal a)
  may edit and b) may remove has `origin = portal:<link id>`.

## Files

New:

    core/src/portal/mod.rs      the client, the tolerant row types, the token
    core/src/portal/agent.rs    the reconciler, the poller, the report
    tests/prop_portal.rs        properties and deterministic cases, against a stub Portal

Edited (small, rebase-friendly):

    core/src/config.rs          five non-secret settings, their defaults and normalisation
    core/src/app.rs             App::portal_status / portal_reconcile / set_portal, the poller start
    core/src/history/watch.rs   Watch::origin, in the schema and in every query
    core/src/history/mod.rs     the `origin` column on an older database
    core/src/xfer/watch.rs      Watcher::set_origin, one new method; `add` sets origin ""
    core/src/lib.rs             `pub mod portal;`
    src/app.rs                  three bridge arms (PortalStatus, PortalReconcile, SetPortal)
    src/lib.rs                  `portal` added to the re-export list
    tests/prop_state.rs         one line: the `Watch` literal now has `origin`

No transfer, Space or delete code changed. `#![forbid(unsafe_code)]` is untouched.

## What one reconcile does

1. Read `GET /api/upload-links` with the bearer token, 30 s timeout, six attempts
   with `netx::backoff` on a connect error, a timeout, 429 or a 5xx, and
   `Retry-After` honoured. A failed read **changes nothing** and is reported.
2. Create the local root if it is missing.
3. For each row with a non-empty `turbosort`:
   * the marker must be **exactly one path segment** — reject empty, `.`, `..`,
     anything with `/`, `\`, `%` or a control character, and anything over 255
     bytes. A rejected marker creates no directory and no watch, and appears in
     `Report::skipped` with the reason;
   * the row must carry a Frame.io account and folder. If it does not, it is
     skipped with a reason rather than guessed at;
   * one link, one folder: the rows are deduplicated by link id and folder, so a
     second link naming the same `Live_Work` folder is reported, not doubled;
   * create `root/<marker>` and add a **mirror** watch (`on_change` from
     `Settings::portal_on_change`, default `overwrite`, label the link's own
     name), then stamp `origin = portal:<link id>`.
4. A portal watch that is already right is left alone. One that is not is edited
   through `Watcher::update` (same watch id, same history).
5. Remove every portal watch whose link is gone, switched off, or no longer
   carries a marker. Never a watch with any other origin — a locally created
   watch is untouchable, and a link that names a folder a user watch already
   occupies is skipped and reported.
6. Nothing local is ever removed, and no file is ever created. Only directories
   under the root are created.

Second run with the same links: `ensured = 0`, `removed = 0`, and the watch list
(row ids included) is byte-identical.

## App surface

    App::portal_status()   -> PortalStatus { enabled, signed_in, url, root, running,
                                             last_at, links_seen, ensured, removed,
                                             skipped, error }
    App::portal_reconcile() -> Report      (manual trigger; one at a time)
    App::set_portal(url, token, root, enabled) -> PortalStatus

The poller starts with `App::start()` and sleeps `portal_every_secs` (default
30 s, floor 5 s, ceiling 24 h), reading the settings each round, so switching
Portal off or changing the interval takes effect within one interval.

## Tests

`cargo test --offline --test prop_portal` — 11 passing, 1 ignored, about 2 s. Sixteen
new unit tests sit in the two new modules (the marker rule, the origin round trip,
the planner, the tolerant reader, the envelope shapes, the error paths).

Three properties (32 cases each by default; 400 cases run clean):

| | property | cases |
|---|---|---|
| P1 | one mirrored watch per valid link at `root/<marker>`, one level under the root, labelled with the link's name; the user's watch is untouched; no file created, changed or removed; a second run changes nothing | links drawn from 0..6 rows, markers including `""`, `.`, `..`, `a/b`, `a\b`, `a%2f..`, a tab, 300 bytes; ids including `""`, `../L4`, `L/5`, `L%2f6` |
| P2 | after a link disappears, a marker is cleared, a link is switched off, a marker is renamed or a link is added: the watch set is the new set exactly, removals are the model's count | 32 × the same generator with a random mutation |
| P3 | an unsafe marker creates no folder, no file and no watch, and is reported | every marker the generator can make |

Plus, deterministically: a tolerant read (bare array, `null` where a string is
expected, a folder as a bare string, an unknown field, the bearer header on the
wire); a value of the wrong type failing the whole read rather than silently
dropping a link; 500 → retried → read, 401 → error and nothing changed; a
connection that closes with no reply → error and nothing changed; a user watch
on the folder a link names → skipped, untouched, not removed when the link goes
away; `on_change = keep` and a second root; `set_portal` writing the settings but
never the token; no token → an error and no call at all; an older `settings.json`
without the Portal fields still loading.

One `#[ignore]`d case: a refused connection costs six attempts with exponential
backoff (~30 s) and changes nothing. Run it with `-- --ignored`.

## Findings and decisions worth a look

1. **The account id is not in `/api/upload-links` as far as this crate can see.**
   The brief names `target_folder_id`, `target_folder_name`, `folder` and
   `turbosort`. A Frame.io watch also needs an *account* id, and none of those
   four carries one — unless `folder` is an object that does. The row therefore
   reads `target_account_id`, then `account_id`, then `folder.account_id`, and a
   row that carries none is **skipped with a reason** rather than guessed at.
   This is the one thing most likely to need a change once the real reply is
   seen; it is one function (`UploadLink::acct`) and one skip message.
2. **"Disabled" is not a documented field either.** A row is treated as off when
   `disabled` is true, `enabled` is `false`, or `status` is one of
   `disabled/inactive/archived/deleted/expired/paused`. If Portal spells it
   differently, that list is the one place to change.
3. **`#[serde(default)]` does not cover a present `null`** — the Space sweep found
   the same thing for Frame.io. Every string field also goes through
   `portal::nullable`, so `{"turbosort": null}` reads as empty instead of failing
   the page. A value of the *wrong type* (a number where a string belongs) still
   fails the read on purpose: silently dropping a link would drop its watch.
4. **The list envelope is not documented.** `upload_links` is read from a bare
   array, or from `{upload_links: []}`, `{data: []}`, `{items: []}`,
   `{results: []}`, `{rows: []}`, or one level down inside `data`. `links` is
   deliberately *not* searched: that is how Frame.io's envelopes carry pagination
   and reading a cursor as a list would be worse than saying so.
5. **`folder` is read as either a string (a name) or an object (id/name/account).**
   A name is not an id, so a link whose only folder information is a bare name is
   skipped for the missing folder id.
6. **`include_existing = false` for the watches Portal creates.** A mirrored watch
   starts by syncing *changes from now on*; what is already in the Frame.io
   folder is recorded as ignored, and what is already in the local folder is not
   uploaded. Since the folder is usually brand new, this is what keeps a first
   reconcile from pulling an entire project down unasked. Flipping it is one
   argument in `Watcher::add`.
7. **A user's watch outranks Portal.** If the user already watches
   `root/<marker>`, Portal's link is reported and skipped — it is not converted
   into a mirror and not removed later.
8. **A paused Portal watch stays paused.** `enabled` is not part of the match, so
   a user who switched a Portal watch off is not overruled every 30 s.
9. **The poller ignores `portal_enabled`; only the poller does.** The manual
   trigger works whether or not the background poller is on, which is what a
   "Reconcile now" button needs.
10. **The token has a 0600 fallback file**, `portal-token` under the app's data
    folder, exactly as `auth` keeps the Adobe session's `token.json`. On macOS
    the Keychain normally holds it. Without that fallback the whole path is
    untestable off-macOS and under `FIO_NO_KEYCHAIN=1`.
11. **`Watcher::add` does not take an origin**, so the reconciler stamps it just
    after with the new `Watcher::set_origin`. One new method rather than a
    signature change on a function the UI, the CLI and the Finder service all
    call.

## What the stub Portal could not model

The stub answers one request per connection with a canned body. It cannot show:

* whether the real `/api/upload-links` wraps its rows, and under what key;
* which fields a real row carries beyond the four the brief names (the account
  id above all);
* how large the real list is (the client reads it in one call; there is no
  pagination here because none is documented — if Portal paginates with
  `links.next` like Frame.io, that is unimplemented and would be visible as a
  short list);
* TLS, a real host name, redirects, or a proxy;
* how Portal names a link that is switched off;
* whether `folder` is an object or a string in practice.

## What needs the user's real host

1. `PORTAL_API_TOKEN` (a bearer token that Portal accepts) and the host URL, to
   see one real row and confirm or correct items 1, 2, 5 and the envelope in 4.
2. One real reconcile, to watch `Report::skipped` — a row that lands there is the
   fastest way to see a wrong assumption.
3. The real root: `~/Live_Work` may already hold project folders made by Portal's
   own worker. Because `include_existing = false`, the first run records what is
   already there as existing and does not move it; it also means a folder Portal
   filled earlier will look empty in the watch's view until something changes.

## Commands

    cd /Users/chaos/work/FioTransfer-portal
    export CARGO_TARGET_DIR=/Users/chaos/work/fio-portal-target

    cargo build   --offline -p fiotransfer-core
    cargo test    --offline -p fiotransfer-core --lib          # 128 passed (16 new)
    cargo test    --offline --test prop_portal                 # 11 passed, 1 ignored
    cargo test    --offline                                    # whole workspace
    PROPTEST_CASES=400 cargo test --offline --test prop_portal -- p1_ p2_ p3_
    cargo test    --offline --test prop_portal -- --ignored    # the 30 s refused-connection case
    cargo build   --offline --workspace

## The base

The brief said "branch off the latest `origin/main`", and also named
`core/src/portal/mod.rs`, `core/src/portal/agent.rs` and `core/src/config.rs`.
`origin/main` has no `core/` crate — it is the classic layout (`src/app.rs`, the
wry webview) — so both could not hold. The `core/` layout is `origin/one-app`
(`e33a6f2`), which is also what `/Users/chaos/work/FioTransfer-rs` has checked
out. The branch was made from `origin/one-app`; asked, the user confirmed that is
the layout the app is becoming and that nothing should be duplicated under
`src/portal/`.

Moving this to `origin/main` is a path rename (`core/src/portal` → `src/portal`)
plus the same four edits in `src/`; no logic changes.
