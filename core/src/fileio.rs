//! The file-system calls that differ between unix and Windows, one function
//! each, so that no caller has to branch on the platform.
//!
//! Three things differ, and only three:
//!
//! * a **positioned read** — `pread(2)` on unix, `seek_read` on Windows;
//! * **"is this the same file?"** — `(st_dev, st_ino)` on unix, the volume serial
//!   number and file index on Windows. That one needs the platform's own system
//!   call, so it lives in `fiotransfer_sys::fs` and is handed back here;
//! * the **unix mode bits** — 0600 for the files that hold a token, 0700 for a
//!   temporary folder. Windows has no mode bits at all; what protects those files
//!   there is the access control list they inherit from the folder they are
//!   created in (see [`mode`]).
//!
//! Nothing here writes `unsafe`: this crate cannot, and the Windows identity call
//! is the `sys` crate's.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::path::Path;

pub use fiotransfer_sys::fs::FileId;

/// Reads up to `buf.len()` bytes starting at `off`, without moving the file's
/// own read position: `pread(2)` on unix, `seek_read` on Windows. The count is
/// what was read, so a short count means the file ended.
#[cfg(unix)]
pub fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, off)
}

/// Reads up to `buf.len()` bytes starting at `off`, without moving the file's
/// own read position: `pread(2)` on unix, `seek_read` on Windows. The count is
/// what was read, so a short count means the file ended.
#[cfg(windows)]
pub fn read_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, off)
}

/// Fills all of `buf` from `off`, or fails: the file ended first
/// (`UnexpectedEof`), or the read itself failed.
pub fn read_exact_at(file: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        match read_at(file, &mut buf[done..], off + done as u64) {
            Ok(0) => {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer"));
            }
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// What "the same file" is on this platform: `(device, inode)` on unix,
/// `(volume serial number, file index)` on Windows. See [`FileId`] for what the
/// answer means and what the fallback misses.
pub fn file_id(path: &Path) -> FileId {
    fiotransfer_sys::fs::file_id(path)
}

/// Asks for the file `opts` creates to be `mode`: 0600 for the ones that hold a
/// token, so only their owner can read them.
///
/// Windows has no mode bits and nothing here can set an ACL, so the file is
/// created exactly as `opts` says and the protection is the one the folder
/// already carries: the token files live in `%APPDATA%\FioTransfer` (see
/// `config::dir`), a per-user folder whose ACL admits that user and an
/// administrator and nobody else. So the protection is not dropped on Windows —
/// it is the folder's, not the file's, and this call has nothing to add. A file
/// put somewhere else (`FIO_*`/`HOME` overrides) inherits that folder's ACL
/// instead, which is only as private as wherever the caller pointed it.
#[cfg(unix)]
pub fn mode(opts: &mut OpenOptions, mode: u32) {
    use std::os::unix::fs::OpenOptionsExt;
    opts.mode(mode);
}

/// Asks for the file `opts` creates to be `mode`; on Windows the folder's ACL is
/// what protects it. See the unix definition of this function for why.
#[cfg(not(unix))]
pub fn mode(_opts: &mut OpenOptions, _mode: u32) {}

/// Asks for the folder `builder` creates to be `mode`: 0700 where it is
/// temporary and holds an update.
///
/// Windows has no mode bits; a folder created there inherits its parent's ACL,
/// which for `%APPDATA%`/`%TEMP%` is the user's own. See [`mode`].
#[cfg(unix)]
pub fn dir_mode(builder: &mut DirBuilder, mode: u32) {
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(mode);
}

/// Asks for the folder `builder` creates to be `mode`; on Windows it inherits the
/// parent folder's ACL. See the unix definition of this function for why.
#[cfg(not(unix))]
pub fn dir_mode(_builder: &mut DirBuilder, _mode: u32) {}
