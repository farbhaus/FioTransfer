//! "Is this the same file?": the one question about a file that the file
//! system answers differently on each platform.
//!
//! unix answers with `(st_dev, st_ino)` from `stat(2)` — the device the file is
//! on and its inode there. Windows answers with the pair
//! `GetFileInformationByHandle` returns: the serial number of the volume and the
//! file's index on it. The two are the same fact in two vocabularies, and they
//! carry the same fidelity: two hard links to one file are one file, and one file
//! reached by two spellings of its path (a junction, an 8.3 short name) is one
//! file.
//!
//! When the system will not answer — `stat` fails, or the handle cannot be
//! opened or queried — a hash of the canonical path stands in. That is weaker,
//! and [`FileId::Path`] says exactly what it misses.

use std::path::Path;

/// What "the same file" means on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileId {
    /// The file system's own answer: `(device, inode)` on unix,
    /// `(volume serial number, file index)` on Windows.
    Fs { a: u64, b: u64 },
    /// The file system would not answer, so a hash of the canonical path stands
    /// in. It gets the ordinary cases right — the same directory reached by the
    /// same path twice, or by a relative and an absolute spelling of it — and
    /// misses the two cases only the file system can see: two hard links to one
    /// file look like two files, and a file reached through a link (or a Windows
    /// junction, or an 8.3 short name) looks like two files unless both
    /// spellings canonicalise to the same path.
    Path(u64),
}

/// The identity of the file or folder at `path`.
#[cfg(unix)]
pub fn file_id(path: &Path) -> FileId {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(md) => FileId::Fs { a: md.dev(), b: md.ino() },
        Err(_) => FileId::Path(path_hash(path)),
    }
}

/// The identity of the file or folder at `path`.
#[cfg(windows)]
pub fn file_id(path: &Path) -> FileId {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS};

    // `access_mode(0)` asks for a handle we may only ask questions of, not read
    // through, and `FILE_FLAG_BACKUP_SEMANTICS` is what lets the same call open a
    // *folder* as well as a file — which is the case that matters here, since the
    // walk asks this question about directories.
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path);
    let Ok(f) = opened else {
        return FileId::Path(path_hash(path));
    };
    // `BY_HANDLE_FILE_INFORMATION` and the call's signature come from the SDK
    // bindings, so the layout is the C header's, not a hand-copied one.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `f` holds a valid handle for as long as it is alive, and `info` is
    // a correctly sized and aligned `BY_HANDLE_FILE_INFORMATION`.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    if ok == 0 {
        return FileId::Path(path_hash(path));
    }
    FileId::Fs {
        a: info.dwVolumeSerialNumber as u64,
        b: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    }
}

/// A hash of `path` once canonicalised: the stand-in identity, and the thing
/// [`FileId::Path`] is about.
fn path_hash(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut h);
    h.finish()
}
