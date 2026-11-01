# Working on the Rust build

A port of [FioTransfer](../FioTransfer) (Go + Wails) to Rust: the same Frame.io
V4 API, the same transfer engine, the same history store, the same share-link
providers, the same updater.

**One app.** `FioTransfer` is an egui application (`egui-ui/`, package
`fiotransfer-egui`) drawing the same four screens, and it runs the real engine
through `fiotransfer-core` (`core/`). There is no webview: the WKWebView shell
that used to be the macOS bundle is retired, and so is the HTML/CSS/JS front end
it rendered. The menu bar item, the "Download from Link…" panel and the Finder
services are the Go app's AppKit code (`sys/mac/*.m`), compiled by
`sys/build.rs` and connected to the egui shell by `egui-ui/src/menubar.rs`.
Closing the window hides the app; quitting is the menu's (or ⌘Q's). The root
package is now only the command line (`fio`) and the measuring harness
(`fio-bench`).

```
cargo run -p fiotransfer-egui          # the app
make app                               # build FioTransfer.app (ad-hoc signed, version "dev")
make dmg                               # universal .dmg/.zip in dist/ (scripts/package.sh, as CI)
make cli                               # build bin/fio
make bench                             # build bin/fio-bench
make test                              # cargo test --workspace
```

## Portal

FioTransfer can take its watch folders from [Portal](../portal-traefik) instead of being configured by
hand. Portal marks a project's Frame.io folder with a `.turbosort` file naming the `Live_Work` folder it
belongs to; FioTransfer reads Portal's upload links and keeps a **mirrored** watch at
`~/Live_Work/<that name>` for each one. Media still moves only between FioTransfer and Frame.io — Portal
never carries a byte of it.

Connect in the app (**Settings → Portal**) or from a terminal:

```sh
fio portal signin https://your-portal you@yourdomain   # password, or a one-time code if you use one
fio portal reconcile                                   # what it did, and what it refused
```

A folder that already holds media is baselined rather than re-downloaded or re-uploaded, and when a link
disappears its folder is **unwatched, not deleted**. Full guide, including every refusal message and what
it means: [`docs/PORTAL.md`](docs/PORTAL.md).

## The command line

`fio` is also how a headless install sets the Portal connection up and checks it — `fio portal status | signin <url> <email> | signout | reconcile [--url URL] | token <token> | token --clear` — so a machine reached over ssh, or a cron job, needs no window for either.

## Where the pieces live now

| Piece | Where |
| --- | --- |
| Frame.io API, auth, history, share links, the engine, watch folders, updates, the `App` | `core/` (`fiotransfer-core`) |
| the four screens, the shell and its platform seam | `egui-ui/` (`fiotransfer-egui`) |
| `fio` (upload, download, watch, history, export) and `fio-bench` | `src/bin/` |
| the `libc` calls and the AppKit shims the CLI needs | `sys/` |
| the frozen webview front end, kept only as the bridge test's contract | `tests/contract/app.js` |

`fiotransfer-core` has no window and no webview dependency — no `wry`, no `tao`,
no Objective-C — which is what makes one app possible: `egui-ui` depends on it
directly, and so do the CLI and the bench. Every `window.go.main.App[method]`
call the old front end made is a plain Rust method on `core::App`; the JSON
dispatch that answered those names is still in `src/app.rs`, because
`tests/prop_bridge.rs` holds it to the frozen `app.js` call list.

## Verification

### The user interface: pixel for pixel

This was measured while the port still had a webview, and it is kept because it
is what the current screens were laid out from. Screen capture is not available
to a background session, so both builds photographed **their own webview**
(`takeSnapshotWithConfiguration:`): a build with `-tags shot` for Go, `FIO_SHOT`
for Rust. Both were driven to the same view with the same JavaScript, in a
1180×780 window, under the same `HOME`. The webview shell has since been
retired; the egui app draws the same measurements, and
`cargo test -p fiotransfer-egui --test snapshots` renders its four screens
headlessly at 1180×780 into `/tmp/egui-shots`.

`shasum` of the four views, Go versus Rust, 1,180 × 780 = 920,400 pixels each:

| View | Differing pixels |
| --- | --- |
| Transfers | **0** |
| Watch Folders | **0** |
| History | **0** |
| Settings | **0** |

The DOM agrees too: `document.body` is the same HTML (the only difference found
was the download folder in the title attribute, when the two builds ran under
different `HOME`s).

### The numbers

Measured on the same machine (macOS 26, Apple silicon), same isolated `HOME`,
same file set, same settings, both builds ad-hoc signed, both stripped.

| | Go | Rust | |
| --- | --- | --- | --- |
| Executable | 15,649,760 B (14.9 MiB) | **8,647,184 B (8.2 MiB)** | **-44.7%** |
| `.app` bundle | 15 MB | **8.4 MB** | -44% |
| Idle RSS (app process) | 119.5 MB | **103.2 MB** | **-13.6%** |
| Upload, link-limited to 250 MB/s | 249.4 / 251.8 MB/s | 250.1 / 245.4 MB/s | parity |
| Upload, 1 stream, 64 MiB parts | 1,538 MB/s | 1,477 MB/s | -4% |
| Upload, loopback, 4 streams | 8,302 MB/s | **11,481 MB/s** | **+38%** |
| Upload, loopback, 8 streams | 9,995 MB/s | **10,501 MB/s** | +5% |
| Upload, loopback, 16 streams | 9,766 MB/s | 9,667 MB/s | -1% |
| Upload, loopback, 8 GiB, 16 streams | 9,632 MB/s | **11,151 MB/s** | **+16%** |
| Upload, loopback, two endpoints | 8,192 MB/s | **11,281 MB/s** | **+38%** |
| Upload after a 50% interruption | 2.15 GB re-sent | **1.17 GB re-sent** | **-45%** |
| Download, 16 streams, 64 MiB segments | 4,204 MB/s | **5,177 MB/s** | **+23%** |
| Download after a ~50% interruption | 1,879 MB re-fetched | **1,073 MB re-fetched** | **-43%** |
| Unit tests | 4,655 lines of Go tests | 61 tests | |

RSS is the app process after 14 s, three runs each; the WebKit helper is a
separate XPC process with the same footprint on both sides, so it is left out of
both numbers. Sizes are the release builds the Makefile makes.

The upload numbers come from `bin/fio-bench`, which drives each build's real
engine against one sink process (`../bench/sink`, a stand-in for Frame.io's API
and storage) with 8 × 256 MiB files unless the row says otherwise. Read them as
follows:

* **Link-limited (250 MB/s): the two are the same.** Both saturate the link; the
  remaining difference is noise. This is the case that matters for a transfer
  app over the internet.
* **On loopback, Rust is ahead nearly everywhere**: +38% at 4 streams, +16% on
  the 8 GiB run, +38% when parts spread over two endpoints. The exception is the
  shortest run at 16 streams (-1%), where both are pinned against the sink's own
  ceiling and the difference is noise.
* **Go degrades as endpoints are added** (9.8 GB/s on one, 8.2 on two) while
  Rust climbs (11.3 on two). Frame.io hands parts to several storage hosts in
  practice, so this is the row that matters most.
* **An interrupted upload is much cheaper in Rust**: the second attempt sends
  only the missing parts (see *Resumable uploads* below).

The Python stand-in (`../bench/fake_frameio.py`) is slower and noisier than the
sink; its numbers are not quoted above.

### What was not verified

* Sign-in against Adobe IMS (no credentials here): the PKCE flow, the local
  CA/trust step and the Keychain store are implemented, but only the pieces
  that do not need a real account were exercised.
* Anything against the real Frame.io API: every request path was exercised
  against the stand-in servers only.
* The menu bar item, the download panel and the Finder services: the AppKit
  code is the Go app's, whose layout was checked then; the app was launched
  with them, but nobody has looked at them on screen since the port. The file
  and folder panels go through the OS (`osascript`, `zenity`, PowerShell) and
  were exercised as far as a background session can.
* The app was never signed in against Adobe from this session, so the screens
  were rendered from the fixture state, not from a live account.
* The Go test suite (4,655 lines) was not ported. 53 cheap unit tests were
  (paths, timings, escaping, naming, the CSV shape, resume sidecars).


## Resumable uploads

Parts are what make an upload resumable, so the engine keeps a ledger of them.

While a file is uploading, `~/Library/Application Support/FioTransfer/uploads/`
holds one JSON header per file in flight — the Frame.io file id, the part layout,
the presigned part URLs, the file's size and mtime — plus an append-only `.done`
list naming the parts already on Frame.io. Each finished part appends one line;
that is the whole write cost.

An upload that stops on a failure, a cancel, a quit or a change is deleted from
Frame.io and its ledger dropped, as the Go app did (`tests/upload_discard.rs`).
Only a crash leaves the ledger: the next attempt at the same file, to the same
folder, with the same size and mtime, resumes while it's fresh — only the missing
parts are sent, and the log says so (`9/32 parts already on Frame.io`).

* **Freshness.** Presigned URLs expire. A ledger older than
  `FIO_UPLOAD_RESUME_SECS` (default one hour) is not trusted.
* **Dead URLs.** If a resumed part answers 403, the file is started again from
  nothing, once, and the log records why. No user action needed.
* **Changed files.** A different size or mtime means the parts no longer describe
  the file: the ledger is dropped and the stale Frame.io copy deleted.
* **Different destination.** Same: dropped and deleted, so nothing is orphaned.
* **Housekeeping.** `clean_up` deletes unfinished uploads whose owner is gone —
  except when a fresh ledger still needs one.

Measured, interrupting an 8 × 256 MiB upload halfway and running it again
(2.15 GiB total, 8 streams, 8 MiB parts):

| | bytes re-sent | time |
| --- | --- | --- |
| Go (no resume) | 2,147,483,648 | 0.222 s |
| Rust (resume) | **1,174,405,120** | **0.119 s** |

That is 45% fewer bytes and half the time for a single interruption. On a real
link, which is what makes a 12 GB render worth interrupting at all, the saving is
the same fraction: the second attempt sends only what is missing.


## Resumable downloads

Downloads resume at **1 MiB block granularity**, not at segment granularity.

`.fiopart` is the target file's bytes so far; `.fiostate` records, per segment,
whether it is done and how many bytes of it are already in the part file:

```json
{"size":268435456,"seg_size":67108864,"done":[true,false,false,false],
 "filled":[67108864,3145728,0,0]}
```

`filled` is optional: a sidecar written by the Go app (or by an earlier build)
has no such field and is still read, resuming at segment boundaries. The Go app
ignores the extra field, so the two can share sidecars. A sidecar with no
progress at all is written exactly as before.

Why the extra field matters: with 64 MiB segments and 16 streams, an
interruption at 50% leaves 16 segments half-written. Recording only whole
segments throws all of that away — measured, the Go engine re-fetches 92% of a
file it had already half downloaded. Recording bytes per segment re-fetches only
what is missing: total bytes served over the two attempts equals the file's size
exactly.

The engine now writes progress per 1 MiB block (throttled to one state save a
second), so a killed process loses at most a second of transfer and never a
whole segment.

Uploads cannot go finer than a part: an S3 multipart part must be sent in one
request, so the part is the smallest resumable unit there — which is why the
upload ledger records parts and the download ledger records blocks.

## Layout

```
core/src/api.rs        Frame.io V4 client (same routes, limits and retry rules)
core/src/auth.rs       Adobe IMS PKCE, localhost https callback
core/src/localcert.rs  the local CA macOS trusts, name-constrained to localhost
core/src/config.rs     settings.json under ~/Library/Application Support/FioTransfer
core/src/host.rs       what a front end must supply: panels, clipboard, window, quit
core/src/history/      SQLite history, watch rows, unfinished uploads, monthly log
core/src/share/        Frame.io shares and review links, Smash, WeTransfer, Dropbox,
                       Google Drive, Portal
core/src/portal/        the Portal client and the watch-folder reconciler its
                       .turbosort markers drive (mirrored watch folders)
core/src/xfer/         the transfer engine: manager, upload, download, watch folders
core/src/update.rs     GitHub releases, checksums, bundle swap, relaunch
core/src/app.rs        the object every front end drives (every app.js call, as Rust)
egui-ui/src/app.rs     the shell: sidebar, central panel, toasts
egui-ui/src/shell.rs   the App on a tokio runtime, and the channels to the screens
egui-ui/src/platform.rs the file panels, the clipboard and the dialogs, per OS
egui-ui/src/views/     the four screens
egui-ui/src/model.rs   UiState, and Request — what a screen asks the app to do
src/app.rs             the JSON bridge the old front end called (contract test only)
src/bin/fio.rs         the CLI
src/bin/bench.rs       the measuring harness
sys/                   the libc calls the CLI and the platform seam need
tests/contract/app.js  the webview front end, frozen as the bridge contract
```
