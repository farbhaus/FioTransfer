# Windows: `fiotransfer-core` compiles on `x86_64-pc-windows-msvc`

Branch **`win-port`**, worktree `/tmp/win-port`, base `origin/consolidate`
**fd419b0** (the branch PR #12 carries). CI run **40995042130**, job `windows`:
16 errors, all of them a unix-only API used without a `cfg`. macOS and Linux
were already green.

The compiler's list was not the whole job. Every unix-only call the crate makes
was found by grep first (`os::unix`, `.mode(`, `.dev()`, `.ino()`, `read_at`,
`pre_exec`, `PermissionsExt`, `MetadataExt`, `CommandExt`) and every hit is
accounted for below — including the ones that only had to be gated out of a test
target, and the ones that compile but do not yet *work* on Windows.

## 1. The shape of the fix

Two modules, one per side of the boundary, so no caller branches on the platform:

| module | holds | crate |
|---|---|---|
| `sys/src/fs.rs` (new) | `file_id`: "is this the same file?" — `(st_dev, st_ino)` on unix, `(volume serial, file index)` from `GetFileInformationByHandle` on Windows | `fiotransfer-sys` (the one crate allowed `unsafe`) |
| `core/src/fileio.rs` (new) | `read_at` / `read_exact_at` (positioned read), `mode` / `dir_mode` (0600/0700), `file_id` (re-exported) | `fiotransfer-core`, still `#![forbid(unsafe_code)]` |

Nothing outside `sys/` gained an `unsafe` block. `core` could not: it forbids
`unsafe_code` outright, so the Windows identity call had to live in `sys`.

`windows-sys` (0.61, already in the tree through `winit`) is a
`[target.'cfg(windows)'.dependencies]` of `sys` only. Its `BY_HANDLE_FILE_INFORMATION`
and `GetFileInformationByHandle` come from the generated SDK bindings, so the
struct layout is the C header's, not a hand-copied one — that matters because the
struct cannot be checked at runtime on this machine (§6).

## 2. Every site, and what Windows does instead

| site | was | now |
|---|---|---|
| `core/src/auth.rs:419` — the Adobe token file | `.mode(0o600)` | `fileio::mode(&mut opts, 0o600)`; on Windows no mode is set, the file inherits `%APPDATA%\FioTransfer`'s ACL |
| `core/src/portal/mod.rs:113` — the Portal token file | `.mode(0o600)` | same |
| `core/src/history/mod.rs:8,453` — the monthly transfer log | `.mode(0o644)` | same, 0644 |
| `core/src/update.rs:477` — `mkdtemp` for an update | `DirBuilder::mode(0o700)` | `fileio::dir_mode(&mut b, 0o700)`; the folder inherits its parent's ACL |
| `core/src/xfer/upload.rs:4,936` — the part reader | `FileExt::read_at` | `fileio::read_at` → `pread` on unix, `seek_read` on Windows |
| `core/src/xfer/manager.rs:1145,1150` — the walk's cycle guard | `(dev, ino)` in a `HashSet<(u64,u64)>` | `fileio::file_id` into a `HashSet<FileId>` |
| `core/src/xfer/watch.rs:914` — `same_device` for `/Volumes/…` | `MetadataExt::dev()` | `fileio::file_id(..)`, comparing the volume half |
| `core/src/xfer/manager.rs:1210` — the symlink-cycle test | `std::os::unix::fs::symlink` | `#[cfg(all(test, unix))]` on the module: Windows needs `SeCreateSymbolicLinkPrivilege` to build the fixture |
| `core/src/config.rs` — `home_dir`, `dir`, `log_dir` | macOS paths off `$HOME` | `%APPDATA%\FioTransfer` and `%LOCALAPPDATA%\FioTransfer\Logs` on Windows, `%USERPROFILE%` when `HOME` is unset (§4) |
| `core/src/secret.rs:17` — `disabled()` | unused off macOS | `#[cfg(target_os = "macos")]` (removes a warning; the other platforms never ask the keychain) |
| `src/bin/bodyprobe.rs:6,106,122` — the body probe | `FileExt::{read_at, read_exact_at}` | `fiotransfer_core::fileio::{read_at, read_exact_at}` |
| `sys/tests/prop_sys_{fs,process,time}.rs` | `PermissionsExt`, `libc::{getpgrp,statfs}`, `localtime_r` | `#![cfg(unix)]`: every case in them is a `libc` call, and the Windows crate answers "no" to all of them (§5) |
| `tests/prop_delete.rs` | a sandbox built from `std::fs::symlink` | `#![cfg(unix)]` (§5) |
| `sys/src/platform.rs:128` — `pre_exec_detach` | already `#[cfg(not(unix))]` empty | the empty Windows body is now documented: Windows has no sessions, a child outlives its parent anyway, and the only caller starts a `/bin/sh` script the Windows build never runs |

## 3. "Is this the same file?"

`sys/src/fs.rs` answers with one type and one function:

```rust
pub enum FileId {
    Fs { a: u64, b: u64 }, // (device, inode) / (volume serial, file index)
    Path(u64),             // a hash of the canonical path
}
pub fn file_id(path: &Path) -> FileId;
```

* **unix**: `std::fs::metadata` → `(dev, ino)`. The same pair as before, no
  behaviour change, and no `unsafe`.
* **Windows**: `OpenOptions::new().read(true).access_mode(0)
  .custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)` — `access_mode(0)` asks
  for a handle to *ask about*, not to read through, and `BACKUP_SEMANTICS` is
  what lets the same call open a **directory**, which is the case that matters
  (the walk asks this question about folders). Then
  `GetFileInformationByHandle` gives `dwVolumeSerialNumber` and
  `nFileIndexHigh/Low`. The handle is `std::fs::File`'s, so it is closed on drop.
* **the fallback**, used when either platform cannot answer: a hash of
  `canonicalize(path)`. It catches the ordinary case — the same folder reached
  by a relative and an absolute spelling — and misses the two that only the file
  system can see: two hard links to one file look like two files, and a file
  reached through a link, a Windows junction or an 8.3 short name looks like two
  files unless both spellings canonicalise the same.

Both callers degrade toward "do more, not less":

* the walk (`manager.rs`) uses `FileId` as a *second* guard against walking a
  tree twice. The first guard — a symlink is reported as a symlink and not
  followed — is unchanged, and it is the one that stops a cycle on Windows too
  (a directory junction is a reparse point, which `symlink_metadata` reports as
  a symlink).
* `same_device` (`watch.rs`) only ever *refuses* a download root, so a doubtful
  identity answers "not the same volume" and lets the download go ahead.

**Not caught by either**: a Windows file opened through a share path
(`\\server\share\x` vs `X:\x`) is the same file to the file system only when the
network redirector says so; `GetFileInformationByHandle` on a share handle
returns the server's volume serial, which is right, but if the handle cannot be
opened at all we fall back to the path hash and miss it.

## 4. The 0600 story, and why `config::dir` had to change with it

On Windows a token file is created with no mode at all, and the protection is the
folder's ACL. That claim is only true if the folder is the per-user one — and it
was not: `config::dir()` was `$HOME/Library/Application Support/FioTransfer`, and
Windows sets no `HOME`, so `home_dir()` fell back to `/` and the token and the
history database would have landed in `C:\Library\Application
Support\FioTransfer` — a directory at the root of the current drive, which a
standard user cannot create or write, and which is not the account's at all.

So `config.rs` now answers the Windows folders: `%APPDATA%\FioTransfer` for the
settings, the token and the history (roaming, per-user, ACL-protected) and
`%LOCALAPPDATA%\FioTransfer\Logs` for the monthly log (local, not roaming), with
`%USERPROFILE%` standing in for `HOME`. On unix the code path is unchanged, byte
for byte.

A file written somewhere else — the `HOME`/`FIO_*` overrides the tests use —
inherits *that* folder's ACL, so it is only as private as wherever the caller
pointed it. `fileio::mode` says so.

## 5. What was gated out instead of ported, and why

| target | why |
|---|---|
| `sys/tests/prop_sys_fs.rs` | `is_writable` is `access(2)`, `is_network_volume` is `statfs`, `strerror_text` is `strerror`. On Windows all three answer "no" without asking anything, so there is no behaviour left to check against an oracle. |
| `sys/tests/prop_sys_process.rs` | `pre_exec_detach` is `setsid()`; the test asks `ps` for the child's session and process group. Windows has neither. |
| `sys/tests/prop_sys_time.rs` | `local_time` is `localtime_r`, and the suite calls `tzset`. On Windows `local_time` answers `None`. |
| `tests/prop_delete.rs` | the sandbox is built with `std::fs::symlink` and the model records links as their own kind of entry. Windows needs a privilege to create one. |
| `core/src/xfer/manager.rs::scan_tests` | same: the one test builds a symlink cycle. |
| `src/bin/bodyprobe.rs` | a measurement harness, not shipped. |

`#![cfg(unix)]` means these suites run zero tests on Windows — visibly ("running
0 tests"), not silently skipped. None of them has been ported, and nothing in them
has been checked on Windows.

## 6. Verification, and exactly how far it goes

Toolchain: `rustup target add x86_64-pc-windows-msvc` on
**1.98.0-aarch64-apple-darwin**. Note the shell: this machine has
`/opt/homebrew/bin/rustc` (1.90, Homebrew) ahead of the rustup shims on the login
`PATH`, and a Homebrew cargo cannot see the rustup-only target. Every command
below was run with `PATH=$HOME/.cargo/bin:$PATH` and
`CARGO_TARGET_DIR=/tmp/target-win-port`.

```
git fetch -q origin
git worktree add -q /tmp/win-port -b win-port origin/consolidate
rustup target add x86_64-pc-windows-msvc
```

| # | command | result |
|---|---|---|
| 1 | `cargo check --offline -p fiotransfer-core --target x86_64-pc-windows-msvc` | **before**: `could not compile fiotransfer-core (lib) due to 16 previous errors` (E0433 x7, E0599 x9) — the same 16, in the same files, as CI run 40995042130. **after**: `Finished`, 4 warnings, all pre-existing |
| 2 | `cargo check --offline -p fiotransfer-core --all-targets --target x86_64-pc-windows-msvc` | `Finished` (no new warnings) |
| 3 | `cargo check --offline --workspace --all-targets --target x86_64-pc-windows-msvc` | `Finished`: core, sys, egui-ui, the CLI, every test target. Warnings: only the pre-existing ones |
| 4 | `cargo check --offline --all-targets` (native) | `Finished`: no errors, no new warnings |
| 5 | `cargo test --offline --workspace` (native) | **487 passed, 0 failed**, exit 0 (the brief said 486 before; the run here counts every target, `prop_sys_*` included, 30 test binaries) |

### The base moved during the work

`origin/consolidate` advanced by seven commits while this branch was being
written (`fd419b0..d775329`: the version bump to 1.4.0, the Portal settings card,
the Portal token and its tests). `win-port` is based on `fd419b0`, and merging it
into today's `origin/consolidate` is clean (`git merge-tree --write-tree` reports
no conflict; only `Cargo.lock` is touched on both sides). The merged tree was
built too:

| command (in a scratch worktree at the merge) | result |
|---|---|
| `cargo check --offline --workspace --all-targets --target x86_64-pc-windows-msvc` | `Finished`, exit 0 |
| `cargo test --offline --workspace` (native) | **508 passed, 0 failed**, exit 0 |

So the branch does not need a rebase to keep the Windows job green; the numbers
in run 5 above are the branch alone, these are the branch on top of the newer
base.

### The C dependency, and the one substitution in run 1–3

The Rust half is real: `rustc` for `x86_64-pc-windows-msvc` type-checked every
line of `core`, `sys` and `egui-ui` against the real Windows `std`. The **C**
half is not: this mac has no Windows SDK (`find / -name windows.h` → nothing), so
`aws-lc-sys` — `rustls`'s provider, in the tree through `reqwest` and
`rustls-platform-verifier` — fails building jitterentropy (`fatal error:
'windows.h' file not found`), and `ring` fails archiving afterwards.

`cargo check` never links, so runs 1–3 were made to pass that wall with a stub C
toolchain: `CC_x86_64_pc_windows_msvc=/tmp/faketools/cc` (writes an empty object
for every `-o`) and `AR_x86_64_pc_windows_msvc=/tmp/faketools/ar` (writes an empty
archive). Nothing in the repository refers to them, and no Rust source, feature,
version or vendored copy was changed to get past the build. What that means for
confidence: a Rust-level mistake in `core`/`sys`/`egui-ui` cannot hide, and a
C-level problem in `aws-lc-sys`/`ring` for Windows is simply not observed here —
the GitHub `windows` job with a real MSVC will be the first time those are
compiled.

## 7. Still unverified, and what still does not work on Windows

1. **Nothing has run on Windows.** No Windows binary was executed, no Windows
   test passed. The claims about behaviour are the Windows API's documented
   contract, not an observation.
2. **`GetFileInformationByHandle` has never been called.** The call, its
   arguments and the struct are compile-checked (and the struct is the SDK's);
   the *values* are not. A wrong answer there shows up as a walk that visits one
   folder twice, not as a crash — the identity is the second guard, not the
   first.
3. **`seek_read`** on a file opened `read(true)` is not exercised at all. The
   unix `pread` path is (upload tests, bodyprobe).
4. **The app's macOS-shaped runtime paths still compile and still do nothing on
   Windows.** This task was a compile port, and these are the next job:
   * `notify.rs` → `/usr/bin/osascript`: no notification banner on Windows.
   * `localcert.rs` → `/usr/bin/security`: the local CA for the `https://localhost`
     OAuth callback is never trusted, so Adobe sign-in would fail at the
     certificate.
   * `update.rs` → `ditto`, `xattr`, `codesign`, `plutil`, and it downloads
     `FioTransfer-<version>-macOS.zip` for a `.app` bundle: the self-updater is
     macOS-only on every platform. The Windows installer (NSIS) owns updates
     there, and the app should not offer its own.
   * `xfer/manager.rs` → `/usr/bin/caffeinate`: no sleep prevention during a
     transfer.
   * `login.rs` → `/usr/bin/open -g`: no app activation.
5. **`cargo packager --formats nsis` was not run.** It needs Windows (or a
   Windows-side toolchain); the CI job remains the first real test of the
   packaging step.
6. **The gated suites (§5)** run zero tests on Windows, and `cargo test` on
   Windows is not part of the `windows` job today — if it becomes one, those
   suites will pass by doing nothing, which is worth remembering when reading a
   green tick.

## 8. Commits (branch `win-port`, base `origin/consolidate` fd419b0)

| commit | what |
|---|---|
| `26226cf` `sys: file identity for Windows, and the module that holds it` | `sys/src/fs.rs`, `sys/src/lib.rs`, `sys/Cargo.toml`, `Cargo.lock` |
| `ebcd50f` `core: fileio, the four calls that differ between unix and Windows` | `core/src/fileio.rs`, `core/src/lib.rs`, the mode/read call sites (`auth`, `portal`, `history`, `update`, `xfer/upload`), `secret.rs` |
| `cc65084` `core: 'the same file' on both platforms, in the walk and in the volume check` | `xfer/manager.rs`, `xfer/watch.rs` |
| `81e6231` `core: the per-user folders on Windows` | `core/src/config.rs` |
| `6ee645f` `tests: the unix-only suites, and the positioned reads in bodyprobe` | `sys/tests/*`, `sys/src/platform.rs` (comment), `tests/prop_delete.rs`, `src/bin/bodyprobe.rs` |
| (this commit) `docs: the Windows port report` | `docs/reports/windows-port.md` |
