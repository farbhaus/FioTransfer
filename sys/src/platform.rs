//! The `libc` calls the app needs, one safe function each.
//!
//! Each of these has no safe `std` equivalent: the hostname, the machine's
//! time zone, a mount's flags, a write permission, a `setsid()` that outlives
//! the app, `strerror`, `isatty` and the process' SIGINT disposition. The
//! macOS-only one (`statfs` with `MNT_LOCAL`) is gated on the target; the
//! others are POSIX, so they are gated on `unix`.

#[cfg(unix)]
use std::ffi::CString;

/// A local wall-clock time, the way Go's `time.Now()` reports it: the
/// machine's zone, DST included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalParts {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// This machine's name, for the local CA's common name; "localhost" when the
/// system call fails.
#[cfg(unix)]
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return "localhost".to_string();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// This machine's name; "localhost" where there is no `gethostname`.
#[cfg(not(unix))]
pub fn hostname() -> String {
    "localhost".to_string()
}

/// Unix seconds as local wall-clock fields; `None` when the conversion fails.
#[cfg(unix)]
pub fn local_time(secs: i64) -> Option<LocalParts> {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return None;
    }
    Some(LocalParts {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    })
}

/// Unix seconds as local wall-clock fields; `None` where there is no
/// `localtime_r`.
#[cfg(not(unix))]
pub fn local_time(_secs: i64) -> Option<LocalParts> {
    None
}

/// `MNT_LOCAL` in `<sys/mount.h>`.
#[cfg(target_os = "macos")]
const MNT_LOCAL: u32 = 0x1000;

/// Reports whether `path` is on a network share (SMB, NFS, AFP, WebDAV…).
///
/// The mount flags are only visible through `statfs`, which has no safe `std`
/// wrapper.
#[cfg(target_os = "macos")]
pub fn is_network_volume(path: &str) -> bool {
    let Ok(c) = CString::new(path) else {
        return false;
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return false;
    }
    st.f_flags & MNT_LOCAL == 0
}

/// Reports whether `path` is on a network share; false where the mount flags
/// cannot be read.
#[cfg(not(target_os = "macos"))]
pub fn is_network_volume(_path: &str) -> bool {
    false
}

/// Whether `path` can be written to: W_OK, as Go's `syscall.Access(dir, 2)`
/// asks. A path with an interior NUL is not writable.
#[cfg(unix)]
pub fn is_writable(path: &str) -> bool {
    let Ok(c) = CString::new(path) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), 2) == 0 }
}

/// Whether `path` can be written to; false where there is no `access`.
#[cfg(not(unix))]
pub fn is_writable(_path: &str) -> bool {
    false
}

/// Makes the process this command starts outlive the app: its own session, so
/// quitting the app doesn't kill it.
#[cfg(unix)]
pub fn pre_exec_detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
    };
}

/// Makes the helper outlive the app.
///
/// Nothing to do here: what `setsid()` buys on unix is a session of the child's
/// own, and Windows has no sessions. A child process there is not killed when its
/// parent exits, which is the property the caller wants; and the one thing that
/// does still reach it — a Ctrl+C typed at the shared console — is not a
/// detached helper's problem here because there is no helper to start: the only
/// caller (`update::relaunch`) runs a `/bin/sh` script, and the Windows build
/// never takes that path (its installer owns updates).
#[cfg(not(unix))]
pub fn pre_exec_detach(_cmd: &mut std::process::Command) {}

/// One lock around `strerror`: for a code it does not know, the C library
/// formats the message into a single process-wide static buffer, so two threads
/// reading two different codes see each other's text unless the call and the
/// copy both happen under this lock.
#[cfg(unix)]
static STRERROR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The `strerror` text for an OS error code, which is what Go printed. Empty
/// when `strerror` has nothing to say.
///
/// Thread-safe: `strerror` is not, for a code it does not know (see
/// [`STRERROR_LOCK`]), so the call and the copy are made under the lock.
/// `strerror_r` is not the portable answer here — Darwin's XSI variant answers
/// non-zero for an unknown code, so `-1` and `1000` would lose their
/// "Unknown error: N" text.
#[cfg(unix)]
pub fn strerror_text(code: i32) -> String {
    let _guard = STRERROR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: strerror returns a static string for any int, and the copy below
    // is made while the lock that protects that buffer is held.
    let p = unsafe { libc::strerror(code) };
    if p.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The `strerror` text; empty where there is no `strerror`.
#[cfg(not(unix))]
pub fn strerror_text(_code: i32) -> String {
    String::new()
}

/// Whether `fd` is a terminal.
#[cfg(unix)]
pub fn is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

/// Whether `fd` is a terminal; false where there is no `isatty`.
#[cfg(not(unix))]
pub fn is_tty(_fd: i32) -> bool {
    false
}

/// Whether the process was started with SIGINT ignored: asks the disposition,
/// which resets it to the default in the same call.
#[cfg(unix)]
pub fn sigint_ignored() -> bool {
    unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) == libc::SIG_IGN }
}

/// Whether SIGINT was ignored; false where there is no `signal`.
#[cfg(not(unix))]
pub fn sigint_ignored() -> bool {
    false
}
