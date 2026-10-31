//! Is a QuickTime/MXF file still being written?
//!
//! Renders are written front to back and finished last: a QuickTime/MP4 file
//! gets its movie index (moov) at the end, an MXF file its footer and random
//! index pack. Until then the file can't be played. That holds even where its
//! size and timestamp can't be trusted: on an SMB share, the view of a file
//! another machine is writing can stand still for longer than the settle time,
//! and the server may keep its mtime until the writer closes it.

use std::io::{Read, Seek, SeekFrom};

/// What `media_state` learned about a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaFormat {
    /// Nothing to check; size and mtime decide.
    #[default]
    Unknown,
    /// It has the parts its writer adds last.
    Finished,
    /// It is missing them: the writer hasn't closed the file yet.
    Unfinished,
}

/// Checks a QuickTime/MP4 or MXF file of the given size for the parts its
/// writer adds last.
pub fn media_state(path: &str, size: i64) -> MediaFormat {
    let Ok(mut fh) = std::fs::File::open(path) else {
        return MediaFormat::Unknown;
    };
    let mut head = [0u8; 16];
    let n = read_at(&mut fh, &mut head, 0).unwrap_or(0);
    if n < head.len() || size < head.len() as i64 {
        return MediaFormat::Unknown;
    }
    if quick_time_atom(&head[4..8]) {
        return quick_time_state(&mut fh, size);
    }
    if mxf_key(&head, MXF_HEADER) {
        return mxf_state(&mut fh, size);
    }
    MediaFormat::Unknown
}

fn quick_time_atom(typ: &[u8]) -> bool {
    matches!(typ, b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip" | b"pnot")
}

/// Walks the top-level atoms. A finished file has a moov, and no atom runs past
/// the end of the file.
fn quick_time_state<R: Read + Seek>(r: &mut R, size: i64) -> MediaFormat {
    let done = |moov: bool| {
        if moov {
            MediaFormat::Finished
        } else {
            MediaFormat::Unfinished
        }
    };
    let mut h = [0u8; 16];
    let mut moov = false;
    let mut off: i64 = 0;
    while off < size {
        let want = std::cmp::min(16, size - off) as usize;
        let n = read_at(r, &mut h[..want], off).unwrap_or(0);
        if n < 8 {
            // A partial atom header: the next atom is still arriving. Balanced
            // against the cost of waiting — an upload that starts early sends a
            // truncated file, while waiting is capped by the stall limit.
            return MediaFormat::Unfinished;
        }
        let mut length = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as i64;
        let mut hdr: i64 = 8;
        match length {
            0 => length = size - off, // runs to the end of the file
            1 => {
                // 64-bit length
                if n < 16 {
                    return MediaFormat::Unfinished; // the extended header is still arriving
                }
                length = i64::from_be_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]]);
                if length == 0 {
                    // Reserved for a >4 GiB atom whose length isn't written yet.
                    return MediaFormat::Unfinished;
                }
                hdr = 16;
            }
            _ => {}
        }
        if length < hdr {
            // A length below its own header is a writer mid-update.
            return MediaFormat::Unfinished;
        }
        if &h[4..8] == b"moov" {
            moov = true;
        }
        if length > size - off {
            return MediaFormat::Unfinished; // the atom isn't all there yet
        }
        off += length;
    }
    done(moov)
}

/// MXF keys: 06.0E.2B.34.02.05.01.01.0D.01.02.01.01, then the pack kind.
const MXF_PREFIX: [u8; 13] = [0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01];

const MXF_HEADER: u8 = 0x02; // header partition pack
const MXF_FOOTER: u8 = 0x04; // footer partition pack
const MXF_RIP: u8 = 0x11; // random index pack

fn mxf_key(k: &[u8], kind: u8) -> bool {
    k.len() >= 14 && k[..13] == MXF_PREFIX && k[13] == kind
}

/// Looks for what an MXF writer adds when it closes the file: a random index
/// pack at the very end, or the footer's offset in the header partition.
fn mxf_state<R: Read + Seek>(r: &mut R, size: i64) -> MediaFormat {
    let mut b = [0u8; 64];
    if size >= 20 {
        if read_at(r, &mut b[..4], size - 4).is_ok() {
            // The pack ends with its own length, key included.
            let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64;
            if n >= 20 && n <= size && read_at(r, &mut b[..16], size - n).is_ok() && mxf_key(&b[..16], MXF_RIP) {
                return MediaFormat::Finished;
            }
        }
    }
    let n = read_at(r, &mut b, 0).unwrap_or(0);
    let mut val: usize = 17; // the pack follows its key and BER length
    if b[16] >= 0x80 {
        val += (b[16] & 0x7f) as usize;
    }
    if n < val + 32 {
        return MediaFormat::Unfinished;
    }
    // Major and minor version, KAG size, this and the previous partition: then the footer.
    if val + 32 > b.len() {
        return MediaFormat::Unfinished;
    }
    let footer = i64::from_be_bytes([b[val + 24], b[val + 25], b[val + 26], b[val + 27], b[val + 28], b[val + 29], b[val + 30], b[val + 31]]);
    if footer > 0 && footer <= size - 16 && read_at(r, &mut b[..16], footer).is_ok() && mxf_key(&b[..16], MXF_FOOTER) {
        return MediaFormat::Finished;
    }
    MediaFormat::Unfinished
}

/// Go's `ReaderAt.ReadAt`: fills `buf` from `off`, returning what it read
/// before the end of the file.
fn read_at<R: Read + Seek>(r: &mut R, buf: &mut [u8], off: i64) -> std::io::Result<usize> {
    r.seek(SeekFrom::Start(off.max(0) as u64))?;
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// An atom: 32-bit length, type, payload.
    fn atom(typ: &[u8; 4], payload: usize) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&((8 + payload) as u32).to_be_bytes());
        v.extend_from_slice(typ);
        v.extend(std::iter::repeat(0u8).take(payload));
        v
    }

    #[test]
    fn quicktime_finished_needs_a_moov() {
        let mut f = atom(b"ftyp", 8);
        f.extend(atom(b"mdat", 32));
        f.extend(atom(b"moov", 16));
        let size = f.len() as i64;
        assert_eq!(quick_time_state(&mut Cursor::new(f.clone()), size), MediaFormat::Finished);
        // The mdat's length not filled in yet (an extended length of zero) is unfinished.
        let mut un = f;
        un[16..20].copy_from_slice(&1u32.to_be_bytes());
        let size = un.len() as i64;
        assert_eq!(quick_time_state(&mut Cursor::new(un), size), MediaFormat::Unfinished);
    }

    #[test]
    fn quicktime_without_a_moov_is_unfinished() {
        let mut f = atom(b"ftyp", 8);
        f.extend(atom(b"mdat", 32));
        let size = f.len() as i64;
        assert_eq!(quick_time_state(&mut Cursor::new(f), size), MediaFormat::Unfinished);
    }

    #[test]
    fn quicktime_atom_past_the_end_is_unfinished() {
        // The writer reserved the mdat but hasn't written its bytes yet.
        let mut f = atom(b"ftyp", 8);
        f.extend_from_slice(&1_000_000u32.to_be_bytes());
        f.extend_from_slice(b"mdat");
        let size = f.len() as i64;
        assert_eq!(quick_time_state(&mut Cursor::new(f), size), MediaFormat::Unfinished);
    }

    #[test]
    fn mxf_without_a_footer_or_rip_is_unfinished() {
        let mut head = Vec::new();
        head.extend_from_slice(&MXF_PREFIX);
        head.push(MXF_HEADER);
        head.extend_from_slice(&[0x05, 0x00]); // the rest of the 16-byte key
        head.extend_from_slice(&[0x83, 0x00, 0x00]); // BER length, long form
        head.extend(std::iter::repeat(0u8).take(64));
        let mut f = head.clone();
        f.extend(std::iter::repeat(0xaa).take(200));
        let size = f.len() as i64;
        assert_eq!(mxf_state(&mut Cursor::new(f.clone()), size), MediaFormat::Unfinished);

        // A RIP at the very end, ending with its own length, means finished.
        let mut rip = Vec::new();
        rip.extend_from_slice(&MXF_PREFIX);
        rip.push(MXF_RIP);
        rip.extend(std::iter::repeat(0u8).take(24));
        let total = (rip.len() + 4) as u32; // the pack ends with its own length, key included
        rip.extend_from_slice(&total.to_be_bytes());
        f.extend(rip);
        let size = f.len() as i64;
        assert_eq!(mxf_state(&mut Cursor::new(f), size), MediaFormat::Finished);
    }

    #[test]
    fn media_state_of_a_missing_file_is_unknown() {
        assert_eq!(media_state("/nonexistent/does-not-exist.mov", 100), MediaFormat::Unknown);
    }
}
