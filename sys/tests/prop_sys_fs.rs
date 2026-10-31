//! `platform::is_writable`, `platform::is_network_volume` and
//! `platform::strerror_text`: the `libc` calls the app makes about the file
//! system and about OS errors.
//!
//! Each is total — every path string in the app eventually reaches it, including
//! the empty one, a 5000-byte one and one with a NUL in it — and each has an
//! oracle that does not copy the crate's own expression:
//!
//! * `is_writable` (W_OK) is checked against *doing the write*: open the file
//!   for writing, or create a file in the directory.
//! * `is_network_volume` (mount flags) is checked against the mount's
//!   file-system *type name* (`apfs`, `devfs`, … versus `nfs`, `smbfs`, …), the
//!   same distinction `mount(8)` prints for a human.
//! * `strerror_text` is checked for being non-empty, stable, and free of the
//!   static-buffer smell (the same code read from eight threads at once).
//!
//! unix only: every one of these is a `libc` call, and on Windows the crate
//! answers "no" to all three without asking anything (`is_writable` is false,
//! `is_network_volume` is false, `strerror_text` is empty), so there is no
//! behaviour here to check against an oracle.

#![cfg(unix)]

mod common;

use std::fs::{self, OpenOptions};
use std::sync::Mutex;
use std::os::unix::fs::PermissionsExt;

use fiotransfer_sys::platform::{is_network_volume, is_writable, strerror_text};
use proptest::prelude::*;

use common::arb_path;

// ---------- paths ----------

/// Paths that are worth naming explicitly, whatever the generator says.
const CORPUS: &[&str] = &[
    "",
    ".",
    "..",
    "/",
    "/tmp",
    "/tmp/",
    "/private/tmp",
    "/dev",
    "/dev/null",
    "/dev/zero",
    "/nonexistent-prop-sys",
    "/tmp/does-not-exist/deeper",
    "relative-name",
    "./relative-name",
    "../relative-name",
    "with a space",
    "日本語/ファイル",
    "\u{1F600}",
    "nul\0inside",
    "/tmp/\0",
    "//tmp//",
    "/tmp/./../tmp/",
    "/System",
    "/Applications",
    "/Volumes",
];

// ---------- strerror ----------

/// `strerror` is not required to be thread-safe, and on this libc it is not:
/// unknown codes are formatted into one process-wide static buffer. Every test
/// that reads it without racing takes this lock, so the parallel test harness
/// cannot turn one test's read into another test's failure. The race itself has
/// its own test below, which is where the finding lives.
static ERRNO_LOCK: Mutex<()> = Mutex::new(());

fn errno_lock() -> std::sync::MutexGuard<'static, ()> {
    ERRNO_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    /// Every errno gets text, including the ones that name nothing: the app
    /// prints this string to a user, and an empty line is a lost error.
    #[test]
    fn strerror_text_is_never_empty(code in prop_oneof![
        4 => -4096i32..4096,
        2 => any::<i32>(),
        1 => Just(0i32),
        1 => Just(i32::MAX),
        1 => Just(i32::MIN),
    ]) {
        let _g = errno_lock();
        let s = strerror_text(code);
        prop_assert!(!s.is_empty(), "empty text for errno {code}");
        prop_assert!(s.is_ascii(), "non-ASCII text for errno {code}: {s:?}");
        prop_assert_eq!(&strerror_text(code), &s, "unstable text for {}", code);
    }
}

#[test]
fn every_errno_up_to_four_thousand_has_text() {
    // The sweep the app's own error paths live in, checked one by one.
    let _g = errno_lock();
    for code in -1000i32..=4000 {
        let s = strerror_text(code);
        assert!(!s.is_empty(), "empty text for errno {code}");
    }
}

/// Finding F1 of the sweep, now a regression test: `strerror_text` used to be
/// correct for the codes `strerror` knows — they are `static` strings in the
/// table — but for a code it does not know the C library formats
/// "Unknown error: N" into a **single process-wide static buffer**
/// (`strerrbuf` in Darwin's `libc`), returns a pointer to it, and the crate
/// copied from that pointer afterwards. Two threads asking about two different
/// unknown codes therefore read each other's answer, and the app's
/// `login::os_message` — called from the error paths of concurrent transfers —
/// could name somebody else's errno.
///
/// The fix is one mutex around the call and the copy (`STRERROR_LOCK` in
/// `sys/src/platform.rs`). This test fails without it: eight threads, 600
/// unknown codes each, and every answer must be the text for the code it asked
/// about, byte for byte.
#[test]
fn strerror_text_is_thread_safe_for_unknown_codes() {
    let _g = errno_lock();
    // What each code answers on one thread is the answer every thread must get.
    let codes: Vec<i32> = (0..600).map(|i| 700 + i).collect();
    let want: Vec<String> = codes.iter().map(|c| strerror_text(*c)).collect();
    for (c, w) in codes.iter().zip(want.iter()) {
        assert_eq!(w, &format!("Unknown error: {c}"), "code {c} on one thread");
    }
    let want = std::sync::Arc::new(want);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let want = want.clone();
            std::thread::spawn(move || {
                let mut wrong = Vec::new();
                for i in 0..600usize {
                    let code = 700 + i as i32;
                    let got = strerror_text(code);
                    if got != want[i] {
                        wrong.push((code, got, want[i].clone()));
                    }
                }
                wrong
            })
        })
        .collect();
    let mut wrong = Vec::new();
    for h in handles {
        wrong.extend(h.join().expect("no thread panicked"));
    }
    assert!(
        wrong.is_empty(),
        "{} of 4800 answers named another thread's errno, e.g. {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(3)]
    );
}

/// The same question answered one thread at a time: nothing wrong. It is the
/// concurrency, not the code path, that breaks it.
#[test]
fn strerror_text_of_unknown_codes_is_correct_on_one_thread() {
    let _g = errno_lock();
    for code in 300..307 {
        assert_eq!(strerror_text(code), format!("Unknown error: {code}"));
    }
}
// ---------- is_writable ----------

/// The deterministically interesting file-system shapes: writable file,
/// read-only file, writable and read-only directory, a dangling link, a symlink
/// to a writable file, a path through a file, and a missing name.
#[test]
fn is_writable_agrees_with_doing_the_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();

    let writable = d.join("writable.txt");
    fs::write(&writable, b"x").unwrap();
    let readonly = d.join("readonly.txt");
    fs::write(&readonly, b"x").unwrap();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o444)).unwrap();
    let writeonly = d.join("writeonly.txt");
    fs::write(&writeonly, b"x").unwrap();
    fs::set_permissions(&writeonly, fs::Permissions::from_mode(0o200)).unwrap();
    let closed_dir = d.join("closed");
    fs::create_dir(&closed_dir).unwrap();
    fs::set_permissions(&closed_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let dangling = d.join("dangling");
    std::os::unix::fs::symlink(d.join("no-such-target"), &dangling).unwrap();
    let link = d.join("link-to-writable");
    std::os::unix::fs::symlink(&writable, &link).unwrap();
    let through_file = writable.join("inside");

    let cases: Vec<(String, bool)> = vec![
        (writable.to_string_lossy().into_owned(), true),
        (readonly.to_string_lossy().into_owned(), false),
        (writeonly.to_string_lossy().into_owned(), true),
        (d.to_string_lossy().into_owned(), true),
        (closed_dir.to_string_lossy().into_owned(), false),
        (dangling.to_string_lossy().into_owned(), false),
        (link.to_string_lossy().into_owned(), true),
        (through_file.to_string_lossy().into_owned(), false),
        (d.join("missing").to_string_lossy().into_owned(), false),
        (String::new(), false),
        ("nul\0inside".to_string(), false),
        ("/dev/null".to_string(), true),
        ("/nonexistent-prop-sys".to_string(), false),
        ("a".repeat(5_000), false),
    ];
    for (path, want) in cases {
        assert_eq!(is_writable(&path), want, "is_writable({path:?})");
        let oracle = common::writable_oracle(&path);
        assert_eq!(oracle, Some(want), "the oracle itself, for {path:?}");
    }
    fs::set_permissions(&closed_dir, fs::Permissions::from_mode(0o755)).ok();
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    /// Total for any string, and it answers the same question the write answers.
    #[test]
    fn is_writable_matches_the_write_it_names(path in arb_path()) {
        if path.contains('\0') {
            // Already pinned above and below: no C string, no write.
            prop_assert!(!is_writable(&path));
            return Ok(());
        }
        let got = is_writable(&path);
        if let Some(want) = common::writable_oracle(&path) {
            prop_assert_eq!(got, want, "is_writable({:?})", path);
        }
    }

    /// Same for the mount question, decided from the file-system's name.
    #[test]
    fn is_network_volume_matches_the_mounts_filesystem_type(path in arb_path()) {
        if path.contains('\0') {
            prop_assert!(!is_network_volume(&path));
            return Ok(());
        }
        let got = is_network_volume(&path);
        match common::network_oracle(&path) {
            Some(want) => prop_assert_eq!(got, want, "is_network_volume({:?}) (type {:?})", path, common::fstypename(&path)),
            None => prop_assert!(!got, "is_network_volume({:?}) is true but no mount names it a network type", path),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    /// The true cases, generated rather than named: a file that exists, with a
    /// random permission, in a directory the test just made. The answer must
    /// follow the owner's write bit, which is the only thing `access(W_OK)`
    /// and an actual `open` both look at for a regular file.
    #[test]
    fn is_writable_follows_the_owner_write_bit(
        raw in common::arb_segment(),
        mode in prop::sample::select(vec![0o644u32, 0o444, 0o600, 0o200, 0o400, 0o755]),
    ) {
        let name: String = raw.chars().map(|c| if c == '/' || c == '\0' { 'x' } else { c }).collect();
        let name = if name.is_empty() || name == "." || name == ".." { format!("prop{name}") } else { name };
        let dir = tempfile::tempdir().expect("tempdir");
        // Long names are a file-system limit, not a question about writing.
        let name = if name.len() > 200 { name[..200].to_string() } else { name };
        let path = dir.path().join(&name);
        // APFS refuses some code points outright (EILSEQ, e.g. U+3347A): those
        // paths never exist, so there is nothing to ask `is_writable` about.
        if fs::write(&path, b"x").is_err() {
            return Ok(());
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
        let p = path.to_string_lossy().into_owned();
        let want = mode & 0o200 != 0;
        prop_assert_eq!(is_writable(&p), want, "is_writable({:?}) with mode {:o}", p, mode);
        prop_assert_eq!(common::writable_oracle(&p), Some(want), "oracle for {:?} mode {:o}", p, mode);
    }
}

#[test]
fn the_corpus_is_total_and_pinned_where_it_is_known() {
    for path in CORPUS {
        // Neither call may panic on any of these.
        let w = is_writable(path);
        let n = is_network_volume(path);
        if path.contains('\0') || path.is_empty() {
            assert!(!w, "is_writable({path:?})");
            assert!(!n, "is_network_volume({path:?})");
        }
        if matches!(*path, "/tmp" | "/private/tmp" | "/dev" | "/dev/null" | "/" | "/System" | "/Applications") {
            // Every one of these is on this machine's local APFS root, or on
            // devfs, which is local too.
            assert!(!n, "is_network_volume({path:?})");
        }
        if *path == "/dev/null" || *path == "/dev/zero" {
            assert!(w, "is_writable({path:?})");
        }
        if *path == "/nonexistent-prop-sys" || path.contains("does-not-exist") {
            assert!(!w, "is_writable({path:?})");
        }
        if let Some(oracle) = common::writable_oracle(path) {
            assert_eq!(w, oracle, "is_writable({path:?})");
        }
        if let Some(oracle) = common::network_oracle(path) {
            assert_eq!(n, oracle, "is_network_volume({path:?}) type {:?}", common::fstypename(path));
        }
    }
}

#[test]
fn the_network_volume_answer_is_false_for_a_local_volume_in_this_workspace() {
    // The workspace itself, a tempdir, and the crate's own source file.
    let here = std::env::current_dir().expect("cwd");
    for p in [
        here.to_string_lossy().into_owned(),
        here.join("src").to_string_lossy().into_owned(),
        tempfile::tempdir().unwrap().path().to_string_lossy().into_owned(),
        "/tmp".to_string(),
        "/dev/null".to_string(),
    ] {
        assert!(!is_network_volume(&p), "is_network_volume({p:?})");
        assert!(
            matches!(common::fstypename(&p).as_deref(), Some("apfs") | Some("devfs")),
            "type of {p:?} is {:?}",
            common::fstypename(&p)
        );
        let _ = OpenOptions::new().write(true).open(&p); // touch the path once, cheaply
    }
}
