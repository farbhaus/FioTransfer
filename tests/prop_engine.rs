
//! Property tests for `media_state` in `src/xfer/complete.rs`: "is this
//! QuickTime/MXF file still being written?"
//!
//! Method: a separate reference model of the *format rules* (not of the code)
//! lives at the top of this file. Generators build byte strings from an atom
//! plan, so the shape of the file is controlled (which atom, what size form,
//! what payload, where the damage is) while the expected verdict comes from the
//! model alone. The parser under test reads the same bytes back off disk.
//!
//! Where the format rules do not decide the answer, the model says
//! `Ambiguous` and the property stands down; those regions are pinned down
//! explicitly in the characterization tests at the bottom, so a change to them
//! is still visible.
//!
//! Run: `cargo test --test prop_engine --offline`

use std::fs;

use proptest::prelude::*;

use fiotransfer::xfer::complete::{media_state, MediaFormat};

// ---------- the model: the format, written down independently ----------

/// The 4-byte types a QuickTime/MP4 file may start with (anything else is not
/// one, as far as the caller is concerned). From the format, not from the code.
const QT_FIRST_TYPES: [&[u8; 4]; 7] = [b"ftyp", b"moov", b"mdat", b"wide", b"free", b"skip", b"pnot"];

/// The SMPTE UL prefix shared by every MXF pack; the 14th byte names the pack.
const MXF_PREFIX: [u8; 13] = [0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01];
const MXF_HEADER: u8 = 0x02; // header partition pack
const MXF_FOOTER: u8 = 0x04; // footer partition pack
const MXF_RIP: u8 = 0x11; // random index pack

/// What the format rules say about a byte string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Model {
    /// Not a QuickTime or MXF file at all.
    Unknown,
    /// It has what its writer adds last.
    Finished,
    /// It does not: the writer has not closed it.
    Unfinished,
    /// The format rules (as this port states them) do not decide: a partial
    /// header, a nonsense length, a negative 64-bit length. The parser's answer
    /// here is a policy, pinned by the characterization tests instead.
    Ambiguous,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn model(bytes: &[u8]) -> Model {
    // A file has to be at least one 8-byte atom header plus one type field
    // before anything can be said about it.
    if bytes.len() < 16 {
        return Model::Unknown;
    }
    if QT_FIRST_TYPES.iter().any(|t| **t == bytes[4..8]) {
        return qt_model(bytes);
    }
    if bytes.len() >= 14 && bytes[..13] == MXF_PREFIX[..] && bytes[13] == MXF_HEADER {
        return mxf_model(bytes);
    }
    Model::Unknown
}

/// QuickTime: walk the top-level atoms. Finished means the walk lands exactly
/// on the end of the file and a `moov` went by; an atom that is not all there
/// means the writer is still filling it in.
fn qt_model(b: &[u8]) -> Model {
    let len = b.len() as u64;
    let verdict = |moov: bool| if moov { Model::Finished } else { Model::Unfinished };
    let mut off: u64 = 0;
    let mut moov = false;
    loop {
        if off == len {
            return verdict(moov);
        }
        let rem = len - off;
        if rem < 8 {
            // Less than a header: the format does not say whether this is a
            // writer mid-header or stray padding.
            return Model::Ambiguous;
        }
        let at = off as usize;
        let size32 = be32(&b[at..at + 4]) as u64;
        let (size, hdr) = match size32 {
            0 => (rem, 8),  // runs to the end of the file
            1 => {
                // 64-bit size, in the next eight bytes.
                if rem < 16 {
                    return Model::Ambiguous;
                }
                let big = be64(&b[at + 8..at + 16]);
                if big > i64::MAX as u64 {
                    // Not a size: a negative 64-bit length.
                    return Model::Ambiguous;
                }
                (big, 16)
            }
            n => (n, 8),
        };
        if size < hdr {
            // A length the writer has not filled in yet, or padding.
            return Model::Ambiguous;
        }
        if size > rem {
            // The atom is not all there yet.
            return Model::Unfinished;
        }
        if b[at + 4..at + 8] == b"moov"[..] {
            moov = true;
        }
        off += size;
    }
}

fn mxf_key_at(b: &[u8], at: usize, kind: u8) -> bool {
    at + 14 <= b.len() && b[at..at + 13] == MXF_PREFIX[..] && b[at + 13] == kind
}

/// MXF: the writer closes the file with a random index pack at the very end
/// (whose last four bytes are the pack's own length, key included) or with a
/// footer partition that the header partition's footer offset names.
fn mxf_model(b: &[u8]) -> Model {
    let size = b.len() as u64;
    if size >= 20 {
        let n = be32(&b[b.len() - 4..]) as u64;
        if n >= 20 && n <= size && mxf_key_at(b, (size - n) as usize, MXF_RIP) {
            return Model::Finished;
        }
    }
    if b.len() < 17 {
        return Model::Unfinished;
    }
    let head_len = b.len().min(64);
    // The key is 16 bytes, then the BER length; the value area starts after it.
    let mut val = 17usize;
    if b[16] >= 0x80 {
        val += (b[16] & 0x7f) as usize;
    }
    if head_len < val + 32 || val + 32 > 64 {
        return Model::Unfinished;
    }
    let footer = i64::from_be_bytes([
        b[val + 24], b[val + 25], b[val + 26], b[val + 27], b[val + 28], b[val + 29], b[val + 30],
        b[val + 31],
    ]);
    if footer > 0 && footer as u64 <= size - 16 && mxf_key_at(b, footer as usize, MXF_FOOTER) {
        return Model::Finished;
    }
    Model::Unfinished
}

// ---------- driving the parser under test ----------

/// A file on disk for one case. The parser takes a path and a size.
struct Probe {
    dir: tempfile::TempDir,
}

impl Probe {
    fn new() -> Probe {
        Probe { dir: tempfile::tempdir().expect("a temp dir") }
    }

    fn put(&self, bytes: &[u8]) -> String {
        let p = self.dir.path().join("probe.media");
        fs::write(&p, bytes).expect("write the probe file");
        p.to_string_lossy().into_owned()
    }

    fn state(&self, bytes: &[u8]) -> MediaFormat {
        let p = self.put(bytes);
        media_state(&p, bytes.len() as i64)
    }

    fn state_sized(&self, bytes: &[u8], size: i64) -> MediaFormat {
        let p = self.put(bytes);
        media_state(&p, size)
    }
}

// ---------- generators ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sz {
    /// A 4-byte size and a 4-byte type.
    Bits32,
    /// The extended marker, a 4-byte type and an 8-byte size.
    Bits64,
    /// A zero size: the atom runs to the end of the file.
    ToEof,
}

#[derive(Debug, Clone)]
struct PlannedAtom {
    typ: [u8; 4],
    payload: Vec<u8>,
    sz: Sz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tamper {
    None,
    /// The last atom's size field claims more than the file holds.
    OverrunLast,
    /// Drop this many bytes off the end of the file.
    TruncateTail(u8),
    /// The last atom's size field is a value below a header (2..=7).
    BadLength(u8),
}

#[derive(Debug, Clone)]
struct Plan {
    atoms: Vec<PlannedAtom>,
    tamper: Tamper,
}

fn atom_bytes(a: &PlannedAtom, forced_len: Option<u32>) -> Vec<u8> {
    let mut v = Vec::new();
    match (a.sz, forced_len) {
        (_, Some(n)) => {
            v.extend_from_slice(&n.to_be_bytes());
            v.extend_from_slice(&a.typ);
        }
        (Sz::Bits32, None) => {
            v.extend_from_slice(&((8 + a.payload.len()) as u32).to_be_bytes());
            v.extend_from_slice(&a.typ);
        }
        (Sz::Bits64, None) => {
            v.extend_from_slice(&1u32.to_be_bytes());
            v.extend_from_slice(&a.typ);
            v.extend_from_slice(&((16 + a.payload.len()) as u64).to_be_bytes());
        }
        (Sz::ToEof, None) => {
            v.extend_from_slice(&0u32.to_be_bytes());
            v.extend_from_slice(&a.typ);
        }
    }
    v.extend_from_slice(&a.payload);
    v
}

/// Lay the atoms out, then damage the file in the way the plan asks for.
fn build(p: &Plan) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last_at = 0usize;
    for a in &p.atoms {
        last_at = out.len();
        out.extend(atom_bytes(a, None));
    }
    match p.tamper {
        Tamper::None => {}
        Tamper::OverrunLast => {
            // Claim the rest of the file plus a bit: the atom has not landed.
            let rem = out.len() - last_at;
            let claim = (rem + 8 + (rem % 16)) as u32;
            out[last_at..last_at + 4].copy_from_slice(&claim.to_be_bytes());
        }
        Tamper::BadLength(n) => {
            out[last_at..last_at + 4].copy_from_slice(&(n as u32).to_be_bytes());
        }
        Tamper::TruncateTail(k) => {
            let keep = out.len().saturating_sub(k as usize).max(1);
            out.truncate(keep);
        }
    }
    out
}

/// A 4-byte type: mostly the ones a real file carries, sometimes anything.
fn any_type() -> impl Strategy<Value = [u8; 4]> {
    prop_oneof![
        4 => Just(*b"mdat"),
        4 => Just(*b"moov"),
        2 => Just(*b"ftyp"),
        2 => Just(*b"free"),
        1 => Just(*b"wide"),
        1 => Just(*b"skip"),
        1 => Just(*b"pnot"),
        1 => Just(*b"moof"),
        1 => Just(*b"MOOV"),
        2 => prop::array::uniform4(any::<u8>()),
    ]
}

/// The same, but a type a QuickTime file may legitimately start with.
fn first_type() -> impl Strategy<Value = [u8; 4]> {
    prop_oneof![
        4 => Just(*b"ftyp"),
        3 => Just(*b"mdat"),
        2 => Just(*b"moov"),
        1 => Just(*b"free"),
        1 => Just(*b"wide"),
        1 => Just(*b"skip"),
        1 => Just(*b"pnot"),
    ]
}

fn any_sz() -> impl Strategy<Value = Sz> {
    prop_oneof![8 => Just(Sz::Bits32), 2 => Just(Sz::Bits64), 1 => Just(Sz::ToEof)]
}

fn payload() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..24)
}

fn plan() -> impl Strategy<Value = Plan> {
    (
        first_type(),
        prop::collection::vec((any_type(), payload(), any_sz()), 0..4),
        any_sz(),
        payload(),
        prop_oneof![
            6 => Just(Tamper::None),
            2 => Just(Tamper::OverrunLast),
            2 => (1u8..=24).prop_map(Tamper::TruncateTail),
            1 => (2u8..8).prop_map(Tamper::BadLength),
        ],
    )
        .prop_map(|(t0, rest, s0, p0, tamper)| {
            let mut atoms = vec![PlannedAtom { typ: t0, payload: p0, sz: s0 }];
            atoms.extend(rest.into_iter().map(|(typ, payload, sz)| PlannedAtom { typ, payload, sz }));
            Plan { atoms, tamper }
        })
}

/// The same plan with every atom in one size form, so the two layouts can be
/// compared: the 64-bit form is the same file with eight more header bytes per
/// atom and nothing else changed.
fn plan_with(force: Option<Sz>) -> impl Strategy<Value = Plan> {
    plan().prop_map(move |mut p| {
        if let Some(sz) = force {
            for a in &mut p.atoms {
                a.sz = sz;
            }
        }
        p
    })
}

// ---------- the properties ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Arbitrary bytes, whatever size the caller claims the file is: the
    /// detector answers, it does not panic, and it never claims to know
    /// anything about a file too short to hold one atom header.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..300),
                                    delta in -40i64..40) {
        let p = Probe::new();
        let len = bytes.len() as i64;
        let got = p.state_sized(&bytes, (len + delta).max(-1));
        let honest = p.state(&bytes);
        if bytes.len() < 16 || len + delta < 16 {
            prop_assert_eq!(honest, MediaFormat::Unknown);
            prop_assert_eq!(got, MediaFormat::Unknown);
        }
        prop_assert!(matches!(got, MediaFormat::Unknown | MediaFormat::Finished | MediaFormat::Unfinished));
    }

    /// Atom lists laid out the way a writer lays them out: the parser and the
    /// model agree, and the model has an answer (nothing is ambiguous here).
    #[test]
    fn clean_quicktime_layouts_agree_with_the_model(plan in plan_with(None).prop_filter("undamaged", |p| p.tamper == Tamper::None)) {
        let bytes = build(&plan);
        let want = model(&bytes);
        prop_assume!(want != Model::Ambiguous);
        let p = Probe::new();
        prop_assert_eq!(p.state(&bytes), to_kind(want), "layout {:?}", plan);
    }

    /// The same, with one of the three plausible kinds of damage applied.
    /// Where the format rules still decide, the parser must match them.
    #[test]
    fn damaged_quicktime_layouts_agree_where_the_model_is_sure(plan in plan()) {
        let bytes = build(&plan);
        let want = model(&bytes);
        prop_assume!(want != Model::Ambiguous);
        let p = Probe::new();
        prop_assert_eq!(p.state(&bytes), to_kind(want), "layout {:?}", plan);
    }

    /// The contract the upload path leans on: no `moov`, not finished.
    #[test]
    fn a_file_without_a_moov_is_never_finished(plan in plan_with(None)
        .prop_filter("undamaged", |p| p.tamper == Tamper::None)
        .prop_filter("no moov", |p| p.atoms.iter().all(|a| &a.typ != b"moov")))
    {
        let bytes = build(&plan);
        prop_assume!(model(&bytes) != Model::Ambiguous);
        prop_assert!(Probe::new().state(&bytes) != MediaFormat::Finished, "layout {:?}", plan);
    }

    /// An atom whose declared size runs past the end of the file is a writer
    /// still filling it in, whatever came before it: never finished.
    #[test]
    fn an_atom_past_the_end_is_never_finished(plan in plan_with(Some(Sz::Bits32)).prop_filter("undamaged", |p| p.tamper == Tamper::None),
                                               over in 1u32..600) {
        let mut bytes = build(&plan);
        // A complete moov behind us, so "unfinished" can only come from the
        // header that follows and claims more than the file holds.
        bytes.extend(atom_bytes(&PlannedAtom { typ: *b"moov", payload: vec![0; 8], sz: Sz::Bits32 }, None));
        let p = Probe::new();
        prop_assert_eq!(p.state(&bytes), MediaFormat::Finished, "layout {:?}", plan);
        bytes.extend_from_slice(&((8 + over) as u32).to_be_bytes());
        bytes.extend_from_slice(b"mdat");
        prop_assert_ne!(p.state(&bytes), MediaFormat::Finished, "overrun {}", over);
    }

    /// Cutting the tail off a finished file leaves a final atom that is not all
    /// there: never finished. (Cut into a payload, so a whole header survives.)
    #[test]
    fn a_truncated_final_atom_is_never_finished(plan in plan_with(Some(Sz::Bits32)).prop_filter("undamaged", |p| p.tamper == Tamper::None),
                                                cut in 1usize..8) {
        let mut p0 = plan;
        // Give the file a moov at the very end, fully present, then cut into it.
        p0.atoms.push(PlannedAtom { typ: *b"moov", payload: vec![0u8; 16], sz: Sz::Bits32 });
        let whole = build(&p0);
        prop_assert_eq!(Probe::new().state(&whole), MediaFormat::Finished, "layout {:?}", p0);
        let cut_to = whole.len() - cut;
        let partial = &whole[..cut_to];
        prop_assert_ne!(Probe::new().state(partial), MediaFormat::Finished);
    }

    /// A 64-bit size can hold a number no 32-bit field could: an mdat of more
    /// than four gigabytes, reserved with its true size written and only its
    /// head on disk. That atom is not all there, so the file is unfinished - and
    /// a parser that reads the size's low half would see a small, complete atom
    /// and call it finished.
    #[test]
    fn a_reserved_atom_above_four_gigabytes_is_unfinished(plan in plan_with(Some(Sz::Bits32))
            .prop_filter("undamaged", |p| p.tamper == Tamper::None),
        present in 0usize..64) {
        let mut bytes = build(&plan);
        // A whole moov behind us: nothing but the reserved atom can make this
        // unfinished.
        bytes.extend(atom_bytes(&PlannedAtom { typ: *b"moov", payload: vec![0; 8], sz: Sz::Bits32 }, None));
        let declared = (1u64 << 32) + 16 + present as u64;
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(b"mdat");
        bytes.extend_from_slice(&declared.to_be_bytes());
        // The first `present` bytes of the payload have landed.
        bytes.extend(std::iter::repeat(0u8).take(present));
        prop_assert_eq!(model(&bytes), Model::Unfinished);
        prop_assert_eq!(Probe::new().state(&bytes), MediaFormat::Unfinished, "present {}", present);
    }

    /// Every atom in the 64-bit size form: the same verdict as the same file in
    /// the 32-bit form. The extended form is not a longer 32-bit size.
    #[test]
    fn the_extended_size_form_does_not_change_the_verdict(plan in plan_with(None).prop_filter("undamaged", |p| p.tamper == Tamper::None)) {
        let mut wide = plan.clone();
        let mut narrow = plan;
        for a in &mut wide.atoms { a.sz = Sz::Bits64; }
        for a in &mut narrow.atoms { a.sz = Sz::Bits32; }
        let w = build(&wide);
        let n = build(&narrow);
        // Under sixteen bytes neither layout is a file anyone could write.
        prop_assume!(n.len() >= 16);
        let (mw, mn) = (model(&w), model(&n));
        prop_assume!(mw != Model::Ambiguous && mn != Model::Ambiguous);
        prop_assert_eq!(mw, mn);
        let p = Probe::new();
        prop_assert_eq!(p.state(&w), to_kind(mw), "64-bit layout {:?}", wide);
        prop_assert_eq!(p.state(&n), to_kind(mn), "32-bit layout {:?}", narrow);
    }
}

fn to_kind(m: Model) -> MediaFormat {
    match m {
        Model::Finished => MediaFormat::Finished,
        Model::Unfinished => MediaFormat::Unfinished,
        Model::Unknown => MediaFormat::Unknown,
        Model::Ambiguous => panic!("an ambiguous verdict has no counterpart"),
    }
}

// ---------- MXF ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MxfTail {
    /// No index pack and no footer: the writer has not closed the file.
    Nothing,
    /// A random index pack at the end; `delta` corrupts the length it ends with.
    Rip { delta: i64 },
    /// A footer partition, with the header's footer offset either right or off
    /// by one byte.
    Footer { correct: bool },
}

fn mxf_keyed(kind: u8) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&MXF_PREFIX);
    v.push(kind);
    v.extend_from_slice(&[0x05, 0x00]);
    v
}

fn build_mxf(filler: &[u8], tail: MxfTail) -> Vec<u8> {
    let mut out = vec![0u8; 96];
    out[..16].copy_from_slice(&mxf_keyed(MXF_HEADER));
    out[16] = 0x83; // BER length, long form, three bytes
    out.extend_from_slice(filler);
    match tail {
        MxfTail::Nothing => {}
        MxfTail::Rip { delta } => {
            let mut rip = mxf_keyed(MXF_RIP);
            rip.extend_from_slice(&[0u8; 12]);
            let total = (rip.len() + 4) as i64 + delta;
            rip.extend_from_slice(&(total.max(0) as u32).to_be_bytes());
            out.extend_from_slice(&rip);
        }
        MxfTail::Footer { correct } => {
            let at = out.len();
            out.extend_from_slice(&mxf_keyed(MXF_FOOTER));
            out.extend_from_slice(&[0u8; 16]);
            let field = if correct { at as u64 } else { at as u64 + 1 };
            out[44..52].copy_from_slice(&field.to_be_bytes());
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// An MXF file is finished exactly when its writer's last pack is there:
    /// the random index pack at the end, or the footer the header points at.
    #[test]
    fn mxf_finished_follows_the_last_pack(filler in prop::collection::vec(any::<u8>(), 0..200),
                                          tail in prop_oneof![
                                              3 => Just(MxfTail::Nothing),
                                              3 => Just(MxfTail::Rip { delta: 0 }),
                                              2 => (-8i64..8).prop_map(|d| MxfTail::Rip { delta: d }),
                                              3 => Just(MxfTail::Footer { correct: true }),
                                              2 => Just(MxfTail::Footer { correct: false }),
                                          ]) {
        let bytes = build_mxf(&filler, tail);
        let want = model(&bytes);
        prop_assert!(want != Model::Ambiguous);
        prop_assert_eq!(Probe::new().state(&bytes), to_kind(want), "tail {:?} filler {}", tail, filler.len());
    }

    /// A RIP at the end means finished; a truncated one does not.
    #[test]
    fn mxf_truncation_is_never_finished(filler in prop::collection::vec(any::<u8>(), 0..120),
                                        cut in 1usize..16) {
        let whole = build_mxf(&filler, MxfTail::Rip { delta: 0 });
        prop_assert_eq!(Probe::new().state(&whole), MediaFormat::Finished);
        let keep = whole.len() - cut;
        prop_assert_ne!(Probe::new().state(&whole[..keep]), MediaFormat::Finished);
    }
}

// ---------- the small cases, and the tolerances, spelled out ----------

fn state_of(bytes: &[u8]) -> MediaFormat {
    Probe::new().state(bytes)
}

#[test]
fn a_file_too_short_to_hold_one_atom_is_unknown() {
    for n in 0..16usize {
        let mut b = vec![0u8; n];
        if n >= 8 {
            b[4..8].copy_from_slice(b"ftyp");
        }
        assert_eq!(state_of(&b), MediaFormat::Unknown, "{} bytes", n);
    }
    // Sixteen bytes is the shortest file anything can be said about: one whole
    // atom and nothing else, so no moov.
    let one = {
        let mut b = Vec::new();
        b.extend_from_slice(&16u32.to_be_bytes());
        b.extend_from_slice(b"ftyp");
        b.extend_from_slice(&[0u8; 8]);
        b
    };
    assert_eq!(one.len(), 16);
    assert_eq!(state_of(&one), MediaFormat::Unfinished);
}

#[test]
fn a_quicktime_file_that_is_not_one_is_unknown() {
    let mut b = vec![0u8; 64];
    b[4..8].copy_from_slice(b"junk");
    assert_eq!(state_of(&b), MediaFormat::Unknown);
    let mut m = vec![0u8; 64];
    m[..13].copy_from_slice(&MXF_PREFIX);
    m[13] = 0x03; // a partition pack kind that opens neither file
    assert_eq!(state_of(&m), MediaFormat::Unknown);
}

/// A partial atom header at the end means the next atom is still arriving, so
/// the file is not finished however complete the atoms before it are. (This
/// replaces a tolerance the port inherited: starting an upload on a file whose
/// tail is still being written sends a truncated copy, and the cost of waiting
/// is bounded by the stall limit.)
#[test]
fn trailing_bytes_under_a_header_are_unfinished() {
    let mut b = Vec::new();
    b.extend_from_slice(&8u32.to_be_bytes());
    b.extend_from_slice(b"ftyp");
    b.extend_from_slice(&24u32.to_be_bytes());
    b.extend_from_slice(b"moov");
    b.extend_from_slice(&[0u8; 16]);
    assert_eq!(state_of(&b), MediaFormat::Finished);
    for n in 1..8usize {
        let mut t = b.clone();
        t.extend(std::iter::repeat(0xab).take(n));
        assert_eq!(state_of(&t), MediaFormat::Unfinished, "{} stray bytes", n);
    }
    // Without a moov the same tail is unfinished, so the tolerance is not
    // "assume finished".
    let mut no_moov = Vec::new();
    no_moov.extend_from_slice(&16u32.to_be_bytes());
    no_moov.extend_from_slice(b"ftyp");
    no_moov.extend_from_slice(&[0u8; 8]);
    no_moov.extend_from_slice(&[0xab; 5]);
    assert_eq!(state_of(&no_moov), MediaFormat::Unfinished);
}

/// Characterization: a length below a header (2..=7) ends the walk, and with a
/// moov already behind it the file reads as finished.
#[test]
fn a_length_below_a_header_is_unfinished() {
    let mut b = Vec::new();
    b.extend_from_slice(&8u32.to_be_bytes());
    b.extend_from_slice(b"ftyp");
    b.extend_from_slice(&24u32.to_be_bytes());
    b.extend_from_slice(b"moov");
    b.extend_from_slice(&[0u8; 16]);
    for n in 2u32..8 {
        let mut t = b.clone();
        t.extend_from_slice(&n.to_be_bytes());
        t.extend_from_slice(b"mdat");
        assert_eq!(state_of(&t), MediaFormat::Unfinished, "length {}", n);
    }
}

/// A 64-bit mdat whose extended length is still zero: the writer reserved the
/// header and has not filled its size in. That shape only exists mid-write, so
/// it is unfinished — the old leniency let such a file be uploaded as complete.
#[test]
fn an_unfilled_extended_length_after_a_moov_is_unfinished() {
    let mut b = Vec::new();
    b.extend_from_slice(&8u32.to_be_bytes());
    b.extend_from_slice(b"ftyp");
    b.extend_from_slice(&24u32.to_be_bytes());
    b.extend_from_slice(b"moov");
    b.extend_from_slice(&[0u8; 16]);
    // An mdat with the extended marker and a zero 64-bit length.
    b.extend_from_slice(&1u32.to_be_bytes());
    b.extend_from_slice(b"mdat");
    b.extend_from_slice(&0u64.to_be_bytes());
    b.extend_from_slice(&[0xcd; 12]);
    assert_eq!(state_of(&b), MediaFormat::Unfinished);
}

#[test]
fn mxf_needs_its_pack_fields_before_it_can_say_anything_finished() {
    // The key alone, cut short of the partition fields.
    let mut b = mxf_keyed(MXF_HEADER);
    b.extend_from_slice(&[0u8; 8]);
    assert_eq!(state_of(&b), MediaFormat::Unfinished);
    // A zero footer offset and no RIP.
    let f = build_mxf(&[0u8; 32], MxfTail::Nothing);
    assert_eq!(state_of(&f), MediaFormat::Unfinished);
}
