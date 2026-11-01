//! Models shared by the `sys` property suite.
//!
//! Everything here is written from the outside in: no `CStr`, no
//! `to_string_lossy`, no `localtime_r`, no `statfs` flag arithmetic that copies
//! the crate's own expression. Where a model has to call the OS (the file
//! system, the time zone database) it asks a *different* question: the mount's
//! file-system type name rather than its flags, the civil-date algorithm rather
//! than the C library's field mapping.

#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::path::Path;

// ---------- UTF-8, the way `to_string_lossy` would do it, by hand ----------

/// Decode `bytes` with every maximal ill-formed subsequence replaced by one
/// U+FFFD. Written from `str::from_utf8`'s error report alone, so it shares no
/// code with `CStr::to_string_lossy`.
pub fn lossy(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&rest[..valid]).expect("valid prefix"));
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        rest = &rest[valid + n..];
                    }
                    None => {
                        out.push('\u{FFFD}');
                        return out;
                    }
                }
            }
        }
    }
}

/// The bytes before the first NUL: what a C string built from this buffer
/// holds.
pub fn before_nul(b: &[u8]) -> &[u8] {
    match b.iter().position(|&x| x == 0) {
        Some(i) => &b[..i],
        None => b,
    }
}

/// The expected answer for `from_c_str` on a NUL-terminated copy of `bytes`.
pub fn lossy_c_prefix(bytes: &[u8]) -> String {
    lossy(before_nul(bytes))
}

// ---------- civil dates (Howard Hinnant's algorithms) ----------

/// Days since 1970-01-01 for a proleptic Gregorian `y-m-d`.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // Mar = 0
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse: the `(y, m, d)` a day number names.
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The local wall-clock reading of `LocalParts`, as seconds on the same
/// absolute scale as the epoch seconds that produced it: equal to the input
/// plus the zone's offset.
pub fn local_as_epoch(p: &fiotransfer_sys::platform::LocalParts) -> i64 {
    days_from_civil(p.year as i64, p.month as i64, p.day as i64) * 86_400
        + p.hour as i64 * 3_600
        + p.minute as i64 * 60
        + p.second as i64
}

// ---------- the file system, asked differently ----------

/// The mounted file-system type name at `path` (`statfs`'s `f_fstypename`),
/// which is what a human reads in `mount` output; `None` when the call fails.
pub fn fstypename(path: &str) -> Option<String> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(st.f_fstypename.as_ptr()) };
    Some(name.to_string_lossy().into_owned())
}

/// Network file-system type names, from `mount(2)`'s list of remote types.
const NETWORK_FS: &[&str] = &["nfs", "smbfs", "afpfs", "webdav", "cifs", "mvfs", "ftp", "osxfuse"];
/// Local ones that can show up on this machine.
const LOCAL_FS: &[&str] = &["apfs", "hfs", "devfs", "autofs", "msdos", "exfat", "tmpfs", "cd9660"];

/// Whether `path` is on a network share, decided from the file-system's name
/// rather than from the mount flags `is_network_volume` looks at.
pub fn network_oracle(path: &str) -> Option<bool> {
    let name = fstypename(path)?;
    if NETWORK_FS.contains(&name.as_str()) {
        Some(true)
    } else if LOCAL_FS.contains(&name.as_str()) {
        Some(false)
    } else {
        None
    }
}

/// Whether `path` can be written to, decided by doing the write: open the file
/// for writing, or for a directory, create a file in it. `None` when the answer
/// cannot be had that way.
pub fn writable_oracle(path: &str) -> Option<bool> {
    if path.contains('\0') {
        return Some(false);
    }
    let p = Path::new(path);
    match std::fs::metadata(p) {
        Err(_) => Some(false),
        Ok(md) if md.is_dir() => {
            let probe = p.join(format!(".prop-write-probe-{}", std::process::id()));
            let ok = File::create(&probe).is_ok();
            let _ = std::fs::remove_file(&probe);
            Some(ok)
        }
        Ok(_) => Some(OpenOptions::new().write(true).open(p).is_ok()),
    }
}

// ---------- the generators, in one place ----------
//
// They live here so that the case-coverage probe (`tests/coverage_tmp.rs`, kept
// out of the suite) exercises exactly the generators the properties use.

use proptest::prelude::*;

/// A NUL-terminated copy of `bytes`, read the way a shim's argument is read.
pub fn buffer_for_c(bytes: &[u8]) -> Vec<u8> {
    let mut v = bytes.to_vec();
    v.push(0);
    v
}

/// Byte content a shim can actually put behind a `const char *`: mostly small,
/// with the awkward shapes mixed in — empty, a lone NUL, invalid UTF-8,
/// truncations, a huge buffer, and valid text.
pub fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        8 => proptest::collection::vec(any::<u8>(), 0..48),
        2 => any::<String>().prop_map(|s| s.into_bytes()),
        2 => proptest::collection::vec(0x80u8..=0xff, 0..24),
        1 => proptest::collection::vec(any::<u8>(), 0..4),
        1 => Just(Vec::new()),
        1 => Just(vec![0u8]),
        1 => proptest::collection::vec(any::<u8>(), 100_000..=100_000),
    ]
}

/// Text that certainly has an interior NUL: `any::<String>()` almost never
/// produces one (proptest's default character set skips it), so the NUL cases
/// are built rather than waited for.
pub fn arb_string_with_nul() -> impl Strategy<Value = String> {
    (any::<String>(), any::<String>(), proptest::collection::vec(0usize..64, 0..4)).prop_map(
        |(a, b, spots)| {
            let mut chars: Vec<char> =
                a.chars().chain(std::iter::once('\0')).chain(b.chars()).collect();
            for spot in spots {
                let i = spot % chars.len();
                chars[i] = '\0';
            }
            chars.into_iter().collect()
        },
    )
}

/// Epoch seconds the app can hold: the awkward exact values first, then
/// anything at all.
pub fn arb_secs() -> impl Strategy<Value = i64> {
    prop_oneof![
        3 => prop_oneof![
            Just(0i64), Just(-1), Just(1), Just(-86_400), Just(86_400),
            Just(i32::MIN as i64), Just(i32::MAX as i64),
            Just(i32::MAX as i64 + 1), Just(i32::MIN as i64 - 1),
            Just(i64::MAX / 2), Just(i64::MIN / 2), Just(i64::MAX), Just(i64::MIN),
            Just(2_147_483_647), Just(-2_208_988_800), Just(253_402_300_799),
            Just(-63_107_232_000),
        ],
        4 => -2208988800i64..=4102444800i64, // 1900 .. 2100
        3 => any::<i64>(),
        2 => any::<i32>().prop_map(|v| v as i64),
    ]
}

/// A path segment: real directory names, arbitrary text, arbitrary characters.
pub fn arb_segment() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::sample::select(vec![
            "", ".", "..", "tmp", "dev", "null", "private", "Volumes", "FioTransfer",
            "\u{65e5}\u{672c}\u{8a9e}", "a b", "with-dash", "x\0y", "no-such-file", "prop-sys",
        ]).prop_map(|s| s.to_string()),
        2 => any::<String>(),
        1 => proptest::collection::vec(any::<char>(), 0..12).prop_map(|v| v.into_iter().collect()),
    ]
}

/// A path string, rooted or relative, short or over `PATH_MAX`.
pub fn arb_path() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => any::<String>(),
        3 => (
            prop::option::of(prop::sample::select(vec!["/tmp", "/dev", "/private/tmp", "/Volumes", "/", "."])),
            proptest::collection::vec(arb_segment(), 0..5)
        ).prop_map(|(root, segs)| {
            let mut s = String::from(root.unwrap_or(""));
            for (i, seg) in segs.iter().enumerate() {
                if i > 0 || !s.is_empty() { s.push('/'); }
                s.push_str(seg);
            }
            s
        }),
        1 => Just(String::new()),
        1 => Just("a".repeat(5_000)),
        1 => Just(format!("/tmp/{}", "b".repeat(300))),
    ]
}
