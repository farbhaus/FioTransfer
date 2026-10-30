//! Is a path on a network volume?

/// Reports whether `dir` is on a network share (SMB, NFS, AFP, WebDAV…).
///
/// The mount flags are only visible through `statfs`, which has no safe std
/// wrapper; `fiotransfer_sys::platform` reads them.
pub fn is_network_volume(dir: &str) -> bool {
    fiotransfer_sys::platform::is_network_volume(dir)
}
