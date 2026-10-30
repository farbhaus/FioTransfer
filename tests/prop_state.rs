//! Property tests for the state that survives an interruption: the resumable
//! upload ledger (`xfer::uploadstate`), the downloader's block-level resume
//! state (`xfer::download`, in that file's own module because it is private)
//! and the SQLite history store.
//!
//! Each target gets an independent reference model — a set for the finished
//! parts, a `Vec<bool>` of 1 MiB blocks for a download, a `BTreeMap` for the
//! store — and generated operation histories fed to both. Anything the model
//! cannot express is a blind spot and is named in the report.
//!
//! Run with:
//!   cargo test --offline --test prop_state
//!
//! The ledger normally writes into the app's profile directory, so every case
//! here works through `Ledger::in_dir` / `keeps_file_in` in its own temp dir.

use std::collections::{BTreeMap, BTreeSet};

use proptest::collection::{hash_set, vec as pvec};
use proptest::prelude::*;

use fiotransfer::history::unfinished::{Replaced, Unfinished};
use fiotransfer::history::watch::{
    Watch, WatchFile, WatchRemote, DIR_MIRROR, ON_CHANGE_KEEP, WATCH_CANCELED, WATCH_DOWNLOADED,
    WATCH_FAILED, WATCH_QUEUED, WATCH_UPLOADED, WATCH_WAITING,
};
use fiotransfer::history::{File, Store, Transfer};
use fiotransfer::xfer::uploadstate::{keeps_file_in, now_ms, resume_window, Ledger, PartState, UploadState};

// ---------- isolating the upload ledger from the real user profile ----------
//
// `Ledger::in_dir` and `keeps_file_in` take the directory, so every case here
// works in its own temp dir: nothing is written into the user's profile and no
// two cases can see each other's ledgers.

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        Sandbox { dir: tempfile::tempdir().expect("temp dir") }
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

/// An upload ledger for `source`, with parts of the given sizes laid end to end.
fn state(source: &str, file_id: &str, sizes: &[i64], created_at: i64) -> UploadState {
    let mut off = 0;
    let parts: Vec<PartState> = sizes
        .iter()
        .map(|s| {
            let p = PartState { off, size: *s, url: format!("http://x/{}", off) };
            off += *s;
            p
        })
        .collect();
    UploadState {
        source: source.to_string(),
        file_id: file_id.to_string(),
        account_id: "acct".into(),
        folder_id: "folder".into(),
        name: "a.bin".into(),
        size: off,
        mtime_ms: 1234,
        created_at,
        view_url: "http://view/1".into(),
        parts,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// What a resumed upload skips is exactly the set of parts it marked
    /// finished — not one more, not one fewer — and the plan it resumes with is
    /// byte-for-byte the one it saved, because the presigned URLs belong to
    /// those offsets.
    #[test]
    fn a_resume_skips_exactly_the_parts_that_finished(
        sizes in pvec(1i64..40, 0..12),
        marks in pvec(0usize..14, 0..16),
    ) {
        let sb = Sandbox::new();
        let src = sb.path().join("a.mov").to_string_lossy().into_owned();
        let at = now_ms();
        let st = state(&src, "file-1", &sizes, at);
        let ledger = Ledger::in_dir(sb.path(), &src);
        prop_assert!(!ledger.exists(), "a fresh source has no ledger");
        ledger.save(&st).expect("save");
        prop_assert!(ledger.exists(), "a saved ledger exists");

        // The model: a set of finished indices. Re-marking is a no-op, and an
        // index outside the plan cannot exist.
        let mut model = vec![false; sizes.len()];
        for m in &marks {
            if *m < sizes.len() {
                ledger.mark_done(*m);
                model[*m] = true;
            } else {
                ledger.mark_done(*m); // must be ignored, not clamped onto a real part
            }
        }
        let (back, done) = ledger.load(st.size, st.mtime_ms, at).expect("a resumable upload");
        prop_assert_eq!(&back.parts, &st.parts, "the part plan round-trips unchanged");
        prop_assert_eq!(back.size, st.size);
        prop_assert_eq!(back.mtime_ms, st.mtime_ms);
        prop_assert_eq!(back.created_at, st.created_at);
        prop_assert_eq!(back.source.as_str(), st.source.as_str());
        prop_assert_eq!(back.file_id.as_str(), st.file_id.as_str());
        prop_assert_eq!(back.account_id.as_str(), st.account_id.as_str());
        prop_assert_eq!(back.folder_id.as_str(), st.folder_id.as_str());
        prop_assert_eq!(back.name.as_str(), st.name.as_str());
        prop_assert_eq!(back.view_url.as_str(), st.view_url.as_str());
        prop_assert_eq!(&done, &model, "a resume skips exactly the parts that finished");
    }

    /// A file that changed cannot resume: the parts Frame.io holds describe
    /// bytes that are no longer there. Same for a ledger older than any sane
    /// presigned URL.
    #[test]
    fn a_changed_file_or_an_old_ledger_is_never_resumed(
        sizes in pvec(1i64..40, 1..5),
        dsize in -2i64..3,
        dmtime in -2i64..3,
        marks in pvec(0usize..6, 0..6),
    ) {
        let sb = Sandbox::new();
        let src = sb.path().join("b.mov").to_string_lossy().into_owned();
        let at = now_ms();
        let st = state(&src, "file-2", &sizes, at);
        let ledger = Ledger::in_dir(sb.path(), &src);
        ledger.save(&st).expect("save");
        for m in &marks {
            if *m < sizes.len() {
                ledger.mark_done(*m);
            }
        }
        if dsize != 0 {
            prop_assert!(ledger.load(st.size + dsize, st.mtime_ms, at).is_none(), "size changed");
        }
        if dmtime != 0 {
            prop_assert!(ledger.load(st.size, st.mtime_ms + dmtime, at).is_none(), "mtime changed");
        }
        // The control case, so the assertions above are about the change and not
        // about a ledger that never loads at all.
        prop_assert!(ledger.load(st.size, st.mtime_ms, at).is_some(), "the unchanged file resumes");
        // Two hours older than the window: whatever the window is, this is out.
        let late = at + (resume_window().as_secs() as i64 + 3600) * 1000;
        prop_assert!(ledger.load(st.size, st.mtime_ms, late).is_none(), "an old ledger is not resumed");
    }

    /// An upload saved inside the resume window must still resume. The window is
    /// a `Duration` (default one hour, `FIO_UPLOAD_RESUME_SECS`), and the ledger
    /// timestamps are milliseconds, so the model is `age <= window` in one unit.
    #[test]
    fn a_ledger_inside_the_resume_window_still_resumes(
        age_ms in prop_oneof![
            Just(0i64),
            Just(1_000i64),
            Just(60_000i64),
            Just(3_600_000i64),
            0i64..(resume_window().as_millis() as i64 * 2 + 2),
        ],
    ) {
        let sb = Sandbox::new();
        let src = sb.path().join("c.mov").to_string_lossy().into_owned();
        let at = now_ms();
        let st = state(&src, "file-3", &[4, 4], at - age_ms);
        let ledger = Ledger::in_dir(sb.path(), &src);
        ledger.save(&st).expect("save");
        let want = age_ms <= resume_window().as_millis() as i64;
        let got = ledger.load(st.size, st.mtime_ms, at).is_some();
        prop_assert_eq!(got, want, "age {} ms, window {:?}", age_ms, resume_window());
    }

    /// Housekeeping deletes the Frame.io copy of an interrupted upload unless a
    /// live ledger still names it. The model is a set of (file id, age) pairs on
    /// disk: an id is kept exactly when some fresh ledger names it.
    #[test]
    fn keeps_file_names_exactly_the_files_a_live_ledger_needs(
        live in hash_set(0u64..6, 0..5),
        expired in hash_set(0u64..6, 0..5),
    ) {
        let sb = Sandbox::new();
        let at = now_ms();
        let old = at - (resume_window().as_secs() as i64 + 3600) * 1000;
        let mut srcs = Vec::new();
        for id in live.iter() {
            let src = sb.path().join(format!("live-{}.mov", id)).to_string_lossy().into_owned();
            let st = state(&src, &format!("file-{}", id), &[4, 4], at);
            Ledger::in_dir(sb.path(), &src).save(&st).expect("save");
            srcs.push(src);
        }
        // A stale ledger for the same ids: it exists on disk, but it is past the
        // window, so housekeeping must not be held back by it.
        for id in expired.iter() {
            let src = sb.path().join(format!("old-{}.mov", id)).to_string_lossy().into_owned();
            let st = state(&src, &format!("file-{}", id), &[4, 4], old);
            Ledger::in_dir(sb.path(), &src).save(&st).expect("save");
            srcs.push(src);
        }
        for id in 0..6u64 {
            let want = live.contains(&id);
            prop_assert_eq!(keeps_file_in(sb.path(), &format!("file-{}", id), at), want, "keeps_file(file-{})", id);
        }
        prop_assert!(!keeps_file_in(sb.path(), "file-nobody", at), "an id no ledger names is not kept");
    }

    /// A new attempt at the same file starts from nothing: the marks of the
    /// earlier attempt (a different file id, a different part plan) are gone.
    #[test]
    fn saving_a_fresh_attempt_forgets_the_earlier_marks(
        first in pvec(1i64..20, 1..6),
        marks1 in pvec(0usize..8, 1..8),
        second in pvec(1i64..20, 0..6),
        marks2 in pvec(0usize..8, 0..8),
    ) {
        let sb = Sandbox::new();
        let src = sb.path().join("d.mov").to_string_lossy().into_owned();
        let at = now_ms();
        let a = state(&src, "file-old", &first, at);
        let b = state(&src, "file-new", &second, at);
        let ledger = Ledger::in_dir(sb.path(), &src);
        ledger.save(&a).expect("save a");
        for m in &marks1 {
            if *m < first.len() {
                ledger.mark_done(*m);
            }
        }
        ledger.save(&b).expect("save b");
        // Straight after the save, nothing is done.
        let (mid, mid_done) = ledger.load(b.size, b.mtime_ms, at).expect("the new attempt resumes");
        prop_assert_eq!(mid.file_id.as_str(), "file-new");
        prop_assert_eq!(mid_done, vec![false; second.len()], "a fresh attempt starts with nothing done");
        let mut model = vec![false; second.len()];
        for m in &marks2 {
            if *m < second.len() {
                ledger.mark_done(*m);
                model[*m] = true;
            }
        }
        let (back, done) = ledger.load(b.size, b.mtime_ms, at).expect("the new attempt resumes");
        prop_assert_eq!(back.file_id.as_str(), "file-new");
        prop_assert_eq!(&back.parts, &b.parts);
        prop_assert_eq!(&done, &model, "only the new attempt's marks count");
    }

    /// Two source files never share a ledger, however close their names are:
    /// otherwise one file's parts would resume into another's bytes.
    #[test]
    fn two_sources_never_share_a_ledger(names in pvec("[a-zA-Z]{1,4}", 2..6)) {
        let sb = Sandbox::new();
        let at = now_ms();
        // Near-identical names: the same stem with different case, a trailing
        // space, a trailing dot, a suffix.
        let mut sources: BTreeSet<String> = BTreeSet::new();
        for n in &names {
            for v in [
                n.clone(),
                n.to_uppercase(),
                format!("{} ", n),
                format!("{}.", n),
                format!("{}.mov", n),
                format!("é{}", n),
            ] {
                sources.insert(sb.path().join(v).to_string_lossy().into_owned());
            }
        }
        let mut paths = BTreeSet::new();
        for src in &sources {
            prop_assert!(paths.insert(Ledger::in_dir(sb.path(), src).header_path().to_path_buf()), "two sources share a ledger: {}", src);
        }
        let sources: Vec<&String> = sources.iter().collect();
        // Distinct plans and distinct marks per source, then each source must
        // read back its own.
        let mut saved: Vec<(String, UploadState, Vec<bool>)> = Vec::new();
        for (i, src) in sources.iter().enumerate() {
            let n = i + 1;
            let sizes: Vec<i64> = (0..n).map(|k| 4 + k as i64).collect();
            let st = state(src, &format!("file-{}", i), &sizes, at);
            let ledger = Ledger::in_dir(sb.path(), src);
            ledger.save(&st).expect("save");
            let mut model = vec![false; n];
            for k in (0..n).step_by(2) {
                ledger.mark_done(k);
                model[k] = true;
            }
            saved.push(((*src).clone(), st, model));
        }
        for (src, st, model) in &saved {
            let (back, done) = Ledger::in_dir(sb.path(), src).load(st.size, st.mtime_ms, at).expect("its own ledger");
            prop_assert_eq!(back.file_id.as_str(), st.file_id.as_str(), "{} resumed another file's parts", src);
            prop_assert_eq!(&back.parts, &st.parts, "{} resumed another file's plan", src);
            prop_assert_eq!(&done, model, "{} resumed another file's marks", src);
        }
    }

    /// The finished-parts file is a plain text log, so a line that isn't a part
    /// index (a truncated write, a byte from a crash) must be ignored rather than
    /// read as part 0.
    #[test]
    fn the_done_file_is_read_as_a_set_of_part_indices(lines in pvec("[-0-9x .]{0,3}", 0..10)) {
        let sb = Sandbox::new();
        let src = sb.path().join("e.mov").to_string_lossy().into_owned();
        let at = now_ms();
        let sizes = vec![4i64, 4, 4];
        let st = state(&src, "file-5", &sizes, at);
        let ledger = Ledger::in_dir(sb.path(), &src);
        ledger.save(&st).expect("save");
        let body = lines.join("\n") + "\n";
        std::fs::write(ledger.header_path().with_extension("done"), &body).expect("done file");
        let mut model = vec![false; sizes.len()];
        for line in lines.iter() {
            if let Ok(i) = line.trim().parse::<usize>() {
                if i < sizes.len() {
                    model[i] = true;
                }
            }
        }
        let (_, done) = ledger.load(st.size, st.mtime_ms, at).expect("a resumable upload");
        prop_assert_eq!(&done, &model, "done file {:?}", body);
    }
}

// ---------- the history store: a map of rows plus a set of files ----------

/// Text that makes a CSV writer work: quotes, commas, both line endings, the
/// BOM, unicode, leading whitespace, the one string Go quotes for no visible
/// reason, and the empty field.
fn arb_text() -> impl Strategy<Value = String> {
    let atom = prop_oneof![
        Just(",".to_string()),
        Just("\"".to_string()),
        Just("\"\"".to_string()),
        Just("\n".to_string()),
        Just("\r".to_string()),
        Just("\r\n".to_string()),
        Just(" ".to_string()),
        Just("\t".to_string()),
        Just("\u{feff}".to_string()),
        Just("\\.".to_string()),
        Just("é".to_string()),
        Just("日本語".to_string()),
        Just("🙂".to_string()),
        Just("a b".to_string()),
        "[A-Za-z0-9_.-]{0,6}",
        Just(String::new()),
    ];
    pvec(atom, 0..4).prop_map(|v| v.concat())
}

fn arb_status() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("queued".to_string()),
        Just("running".to_string()),
        Just("resolving".to_string()),
        Just("done".to_string()),
        Just("failed".to_string()),
        Just("canceled".to_string()),
        Just("skipped".to_string()),
        "[a-z]{0,5}",
    ]
}

/// A short identifier-ish string. (A plain string literal cannot appear inside
/// `prop_compose!`'s argument list: the macro cannot tell it from an attribute.)
fn arb_slug() -> impl Strategy<Value = String> {
    "[a-z0-9]{0,6}".prop_map(|s| s)
}

fn arb_kind() -> impl Strategy<Value = String> {
    prop_oneof![Just("upload".to_string()), Just("download".to_string()), "[a-z]{0,4}".prop_map(|s| s)]
}

prop_compose! {
    fn arb_transfer()(
        kind in arb_kind(),
        title in arb_text(),
        source in arb_text(),
        dest in arb_text(),
        status in arb_status(),
        error in arb_text(),
        account in arb_slug(),
        folder in arb_slug(),
        share in arb_slug(),
        total in 0i64..1_000_000i64,
        done in 0i64..1_000_000i64,
        ftotal in 0i64..10i64,
        fdone in 0i64..10i64,
        ffailed in 0i64..10i64,
        started in 0i64..2_000_000_000i64,
        finished in 0i64..2_000_000_000i64,
        avg in 0i64..1_000_000i64,
        peak in 0i64..1_000_000i64,
        options in arb_text(),
        link in arb_text(),
        share_link in arb_text()
    ) -> Transfer {
        Transfer {
            id: String::new(),
            kind,
            title,
            source,
            dest,
            account_id: account,
            folder_id: folder,
            share_id: share,
            status,
            error,
            total_bytes: total,
            done_bytes: done,
            files_total: ftotal,
            files_done: fdone,
            files_failed: ffailed,
            created_at: 0,
            started_at: started,
            finished_at: finished,
            avg_bps: avg as f64 / 1000.0,
            peak_bps: peak as f64,
            options,
            link,
            share_link,
        }
    }
}

// `idx` is drawn from a range smaller than the file count so that a start with
// duplicate indices happens often: the primary key is (transfer_id, idx), and a
// start that trips it must leave nothing behind.
prop_compose! {
    fn arb_file()(
        idx in 0i64..3,
        rel in arb_text(),
        local in arb_text(),
        size in 0i64..1_000_000i64,
        status in arb_status(),
        error in arb_text(),
        note in arb_text(),
        parent in arb_slug(),
        remote in arb_slug(),
        view in arb_text(),
        asset in arb_slug(),
        fin in 0i64..1_000_000i64
    ) -> File {
        File {
            transfer_id: String::new(),
            idx,
            rel_path: rel,
            local_path: local,
            size,
            status,
            error,
            note,
            remote_parent: parent,
            remote_id: remote,
            view_url: view,
            asset_id: asset,
            finished_at: fin,
        }
    }
}

#[derive(Debug, Clone)]
enum Op {
    Start { slot: usize, t: Transfer, files: Vec<File> },
    Update { slot: usize, t: Transfer },
    UpdateFile { slot: usize, f: File },
    SetShareLink { slot: usize, link: String },
    Get { slot: usize },
    Files { slot: usize },
    List { query: String, kind: String, limit: i64 },
}

const SLOTS: usize = 3;

fn slot_id(slot: usize) -> String {
    format!("t{}", slot)
}

fn arb_op() -> impl Strategy<Value = Op> {
    let slot = 0usize..SLOTS;
    prop_oneof![
        4 => (slot.clone(), arb_transfer(), pvec(arb_file(), 0..4))
            .prop_map(|(slot, t, files)| Op::Start { slot, t, files }),
        3 => (slot.clone(), arb_transfer()).prop_map(|(slot, t)| Op::Update { slot, t }),
        2 => (slot.clone(), arb_file()).prop_map(|(slot, f)| Op::UpdateFile { slot, f }),
        1 => (slot.clone(), arb_text()).prop_map(|(slot, link)| Op::SetShareLink { slot, link }),
        1 => slot.clone().prop_map(|slot| Op::Get { slot }),
        1 => slot.clone().prop_map(|slot| Op::Files { slot }),
        2 => (pvec("[a-z0-9]{1,2}", 0..2), prop_oneof![Just("".to_string()), Just("upload".into()), Just("download".into()), Just("sideways".into())], -1i64..4)
            .prop_map(|(parts, kind, limit)| Op::List { query: parts.join(""), kind, limit }),
    ]
}

#[derive(Default, Clone)]
struct Model {
    transfers: BTreeMap<String, Transfer>,
    files: BTreeMap<String, BTreeMap<i64, File>>,
}

impl Model {
    fn rows(&self) -> Vec<Transfer> {
        self.transfers.values().cloned().collect()
    }

    fn files_of(&self, id: &str) -> Vec<File> {
        self.files
            .get(id)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }
}

/// The fields `Store::update` rewrites, in the store's own terms.
fn model_update(t: &mut Transfer, n: &Transfer) {
    t.title = n.title.clone();
    t.source = n.source.clone();
    t.dest = n.dest.clone();
    t.status = n.status.clone();
    t.error = n.error.clone();
    t.total_bytes = n.total_bytes;
    t.done_bytes = n.done_bytes;
    t.files_total = n.files_total;
    t.files_done = n.files_done;
    t.files_failed = n.files_failed;
    t.started_at = n.started_at;
    t.finished_at = n.finished_at;
    t.avg_bps = n.avg_bps;
    t.peak_bps = n.peak_bps;
    t.share_id = n.share_id.clone();
    t.options = n.options.clone();
    t.link = n.link.clone();
    if !n.share_link.is_empty() {
        t.share_link = n.share_link.clone();
    }
}

fn model_update_file(f: &mut File, n: &File) {
    f.local_path = n.local_path.clone();
    f.size = n.size;
    f.status = n.status.clone();
    f.error = n.error.clone();
    f.note = n.note.clone();
    f.remote_parent = n.remote_parent.clone();
    f.remote_id = n.remote_id.clone();
    f.view_url = n.view_url.clone();
    f.finished_at = n.finished_at;
}

/// SQLite's LIKE over ASCII, with a pattern built as `%q%` and no wildcards in
/// `q`: a case-insensitive substring test.
fn like(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    hay.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
}

fn model_list(m: &Model, query: &str, kind: &str, limit: i64) -> Vec<String> {
    let q = query.trim();
    let mut rows: Vec<&Transfer> = m
        .transfers
        .values()
        .filter(|t| {
            like(&t.title, q)
                || like(&t.source, q)
                || like(&t.dest, q)
                || like(&t.error, q)
                || m.files_of(&t.id).iter().any(|f| like(&f.rel_path, q))
        })
        .filter(|t| !(kind == "upload" || kind == "download") || t.kind == kind)
        .collect();
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let n = if limit <= 0 { 200 } else { limit as usize };
    rows.truncate(n);
    rows.iter().map(|t| t.id.clone()).collect()
}

fn debug<T: std::fmt::Debug>(v: &T) -> String {
    format!("{:?}", v)
}

/// Replays one operation against the store and the model, and checks that the
/// two agree afterwards through every read the store offers.
fn step(store: &Store, m: &mut Model, op: &Op, order: i64) -> Result<(), TestCaseError> {
    let id = match op {
        Op::Start { slot, .. }
        | Op::Update { slot, .. }
        | Op::UpdateFile { slot, .. }
        | Op::SetShareLink { slot, .. }
        | Op::Get { slot }
        | Op::Files { slot } => slot_id(*slot),
        Op::List { .. } => String::new(),
    };
    match op {
        Op::Start { t, files, .. } => {
            let mut t = t.clone();
            t.id = id.clone();
            t.created_at = order;
            let files: Vec<File> = files
                .iter()
                .map(|f| File { transfer_id: id.clone(), ..f.clone() })
                .collect();
            let mut seen = BTreeSet::new();
            let duplicate = files.iter().any(|f| !seen.insert(f.idx));
            let before = debug(&store.get(&id).expect("get"));
            match store.start(&t, &files) {
                Err(_) => {
                    prop_assert!(duplicate, "start failed for a plan with no duplicate index: {:?}", files);
                    // A start that fails must leave the store exactly as it was.
                    prop_assert_eq!(debug(&store.get(&id).expect("get")), before, "a failed start is atomic");
                }
                Ok(()) => {
                    prop_assert!(!duplicate, "a plan with duplicate indices must be rejected");
                    m.transfers.insert(id.clone(), t);
                    m.files.insert(id.clone(), files.into_iter().map(|f| (f.idx, f)).collect());
                }
            }
        }
        Op::Update { t, .. } => {
            let mut t = t.clone();
            t.id = id.clone();
            store.update(&t);
            if let Some(row) = m.transfers.get_mut(&id) {
                model_update(row, &t);
            }
        }
        Op::UpdateFile { f, .. } => {
            let mut f = f.clone();
            f.transfer_id = id.clone();
            store.update_file(&f);
            if let Some(row) = m.files.get_mut(&id).and_then(|fs| fs.get_mut(&f.idx)) {
                model_update_file(row, &f);
            }
        }
        Op::SetShareLink { link, .. } => {
            store.set_share_link(&id, link).expect("set share link");
            if let Some(row) = m.transfers.get_mut(&id) {
                row.share_link = link.clone();
            }
        }
        Op::Get { .. } => {
            let got = store.get(&id).expect("get");
            prop_assert_eq!(debug(&got), debug(&m.transfers.get(&id).cloned()), "get {}", id);
        }
        Op::Files { .. } => {
            let got = store.files(&id).expect("files");
            prop_assert_eq!(debug(&got), debug(&m.files_of(&id)), "files {}", id);
        }
        Op::List { query, kind, limit } => {
            let got: Vec<String> = store
                .list(query, kind, *limit)
                .expect("list")
                .into_iter()
                .map(|t| t.id)
                .collect();
            let want = model_list(m, query, kind, *limit);
            prop_assert_eq!(got, want, "list(query={:?}, kind={:?}, limit={})", query, kind, limit);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// A generated history of starts, progress updates, per-file updates, share
    /// links and the three reads, replayed against the store and against a plain
    /// map model, agreeing after every step.
    #[test]
    fn the_store_follows_an_insert_update_list_model(ops in pvec(arb_op(), 1..34)) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(dir.path(), dir.path()).expect("store");
        let mut m = Model::default();
        for (i, op) in ops.iter().enumerate() {
            step(&store, &mut m, op, i as i64 * 1000)?;
            // After every step, every slot reads back as the model says.
            for slot in 0..SLOTS {
                let id = slot_id(slot);
                prop_assert_eq!(
                    debug(&store.get(&id).expect("get")),
                    debug(&m.transfers.get(&id).cloned()),
                    "after op {} ({:?}): get {}", i, op, id
                );
                prop_assert_eq!(debug(&store.files(&id).expect("files")), debug(&m.files_of(&id)), "after op {}", i);
            }
        }
    }

    /// `MarkInterrupted` flags exactly the rows that were queued, running or
    /// resolving — transfers and files alike — stamps the transfers it touched,
    /// and is idempotent.
    #[test]
    fn mark_interrupted_flags_exactly_the_unfinished_rows(
        t_status in pvec(arb_status(), 1..6),
        f_status in pvec(arb_status(), 1..6),
        file_counts in pvec(0usize..3, 1..6),
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(dir.path(), dir.path()).expect("store");
        let unfinished = ["queued", "running", "resolving"];
        for (i, st) in t_status.iter().enumerate() {
            let id = slot_id(i);
            let n = file_counts[i % file_counts.len()];
            let files: Vec<File> = (0..n)
                .map(|k| File {
                    transfer_id: id.clone(),
                    idx: k as i64,
                    status: f_status[(i + k) % f_status.len()].clone(),
                    ..Default::default()
                })
                .collect();
            store
                .start(
                    &Transfer { id: id.clone(), kind: "upload".into(), status: st.clone(), created_at: i as i64, ..Default::default() },
                    &files,
                )
                .expect("start");
        }
        let before: Vec<(Transfer, Vec<File>)> = (0..t_status.len())
            .map(|i| {
                let id = slot_id(i);
                (store.get(&id).unwrap().unwrap(), store.files(&id).unwrap())
            })
            .collect();
        store.mark_interrupted();
        for (i, (t0, f0)) in before.iter().enumerate() {
            let id = slot_id(i);
            let t = store.get(&id).unwrap().unwrap();
            let files = store.files(&id).unwrap();
            if unfinished.contains(&t0.status.as_str()) {
                prop_assert_eq!(&t.status, "interrupted", "{} was {}", id, t0.status);
                prop_assert!(t.finished_at > 0, "{} was stamped", id);
            } else {
                prop_assert_eq!(&t.status, &t0.status, "{} was not running", id);
                prop_assert_eq!(t.finished_at, t0.finished_at, "{} was not stamped", id);
            }
            // The files are flagged on their own status, whatever the transfer's.
            for (f, g) in f0.iter().zip(files.iter()) {
                if unfinished.contains(&f.status.as_str()) {
                    prop_assert_eq!(&g.status, "interrupted", "file {}/{} was {}", id, f.idx, f.status);
                } else {
                    prop_assert_eq!(&g.status, &f.status, "file {}/{} was not running", id, f.idx);
                }
                prop_assert_eq!(g.finished_at, f.finished_at, "a file row's stamp is left alone");
            }
        }
        // Idempotent: a second pass finds nothing unfinished and changes nothing.
        let after: Vec<(Transfer, Vec<File>)> = (0..t_status.len())
            .map(|i| {
                let id = slot_id(i);
                (store.get(&id).unwrap().unwrap(), store.files(&id).unwrap())
            })
            .collect();
        store.mark_interrupted();
        for (i, (t0, f0)) in after.iter().enumerate() {
            let id = slot_id(i);
            prop_assert_eq!(debug(&store.get(&id).unwrap().unwrap()), debug(t0), "second pass changed {}", id);
            prop_assert_eq!(debug(&store.files(&id).unwrap()), debug(f0), "second pass changed {}'s files", id);
        }
    }
}

/// An RFC 4180 reader written from the grammar, not from the writer: fields
/// separated by commas, records by CRLF or LF, quotes doubled inside a quoted
/// field, and nothing else allowed outside quotes.
fn read_csv(text: &str) -> (Vec<Vec<String>>, Vec<String>) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut problems: Vec<String> = Vec::new();
    let mut chars = text.chars().peekable();
    // 0 = at the start of a field, 1 = inside an unquoted field,
    // 2 = inside a quoted field, 3 = just saw a quote inside a quoted field.
    let mut state = 0u8;
    while let Some(c) = chars.next() {
        match state {
            2 => {
                if c == '"' {
                    state = 3;
                } else {
                    field.push(c);
                }
            }
            3 => {
                if c == '"' {
                    field.push('"');
                    state = 2;
                } else {
                    // End of the quoted section: only a comma or a line end may
                    // follow, and a quote here closes the field.
                    state = 1;
                    match c {
                        ',' => {
                            row.push(std::mem::take(&mut field));
                            state = 0;
                        }
                        '\n' => {
                            row.push(std::mem::take(&mut field));
                            rows.push(std::mem::take(&mut row));
                            state = 0;
                        }
                        '\r' if chars.peek() == Some(&'\n') => {
                            chars.next();
                            row.push(std::mem::take(&mut field));
                            rows.push(std::mem::take(&mut row));
                            state = 0;
                        }
                        other => {
                            problems.push(format!("{:?} after a closing quote", other));
                            field.push(other);
                        }
                    }
                }
            }
            _ => match c {
                '"' if state == 0 => {
                    state = 2;
                }
                ',' => {
                    row.push(std::mem::take(&mut field));
                    state = 0;
                }
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    state = 0;
                }
                '\r' => {
                    problems.push("a bare CR outside quotes".to_string());
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    state = 0;
                }
                other => {
                    field.push(other);
                    state = 1;
                }
            },
        }
    }
    if state == 2 {
        problems.push("an unterminated quoted field".to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    (rows, problems)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Export is a printer, so check it against a reader that shares nothing
    /// with it: every field of every row comes back byte for byte, whatever
    /// quotes, commas, CRs, LFs, BOMs or unicode it holds, and no field can make
    /// the writer emit a bare CR outside quotes.
    #[test]
    fn exported_csv_round_trips_adversarial_fields(
        title in arb_text(),
        source in arb_text(),
        dest in arb_text(),
        status in arb_status(),
        terr in arb_text(),
        files in pvec((arb_text(), arb_text(), arb_text(), arb_status(), arb_text()), 0..3),
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(dir.path(), dir.path()).expect("store");
        let t = Transfer {
            id: "t1".into(),
            kind: "upload".into(),
            title: title.clone(),
            source: source.clone(),
            dest: dest.clone(),
            status: status.clone(),
            error: terr.clone(),
            created_at: 1,
            total_bytes: 10,
            ..Default::default()
        };
        let fs: Vec<File> = files
            .iter()
            .enumerate()
            .map(|(i, (rel, local, view, st, err))| File {
                transfer_id: "t1".into(),
                idx: i as i64,
                rel_path: rel.clone(),
                local_path: local.clone(),
                size: i as i64 * 7,
                status: st.clone(),
                error: err.clone(),
                view_url: view.clone(),
                ..Default::default()
            })
            .collect();
        store.start(&t, &fs).expect("start");
        let out = dir.path().join("out.csv");
        store.export_csv(&out).expect("export");
        let text = std::fs::read_to_string(&out).expect("read");
        let (rows, problems) = read_csv(&text);
        prop_assert!(problems.is_empty(), "the export is not RFC 4180: {:?} in {:?}", problems, text);
        prop_assert_eq!(rows.len(), 1 + fs.len().max(1), "one header plus one row per file");
        for (i, row) in rows.iter().enumerate() {
            prop_assert_eq!(row.len(), 15, "row {} has 15 fields: {:?}", i, row);
        }
        prop_assert_eq!(rows[0].len(), 15);
        // Fields that are not per file.
        prop_assert_eq!(&rows[0][0].as_str(), &"transfer_id", "header");
        if fs.is_empty() {
            // A transfer with no files still exports: the file columns are empty.
            prop_assert_eq!(rows[1][9].as_str(), "", "no file, no rel_path");
            prop_assert_eq!(rows[1][13].as_str(), terr.as_str(), "the transfer error stands in");
        } else {
            for (i, f) in fs.iter().enumerate() {
                let row = &rows[1 + i];
                prop_assert_eq!(row[2].as_str(), title.as_str(), "title row {}", i);
                prop_assert_eq!(row[7].as_str(), source.as_str(), "source row {}", i);
                prop_assert_eq!(row[8].as_str(), dest.as_str(), "dest row {}", i);
                prop_assert_eq!(row[5].as_str(), status.as_str(), "transfer status row {}", i);
                prop_assert_eq!(row[9].as_str(), f.rel_path.as_str(), "rel_path row {}", i);
                prop_assert_eq!(row[10].as_str(), f.local_path.as_str(), "local_path row {}", i);
                let want_size = f.size.to_string();
                prop_assert_eq!(row[11].as_str(), want_size.as_str(), "size row {}", i);
                prop_assert_eq!(row[12].as_str(), f.status.as_str(), "file status row {}", i);
                let want_err = if f.error.is_empty() { terr.as_str() } else { f.error.as_str() };
                prop_assert_eq!(row[13].as_str(), want_err, "error row {}", i);
                prop_assert_eq!(row[14].as_str(), f.view_url.as_str(), "view_url row {}", i);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Rows are what outlives the process: everything written — transfers, their
    /// files, unfinished uploads, replaced versions and watch rows — reads back
    /// identically after the store is closed and opened again, and nothing
    /// written by another store leaks in.
    #[test]
    fn rows_survive_a_close_and_reopen(
        transfers in pvec((arb_transfer(), pvec(arb_file(), 0..3)), 0..4),
        unfinished in pvec((arb_text(), "[a-z0-9]{1,4}", 0i64..99i64, arb_text()), 0..3),
        replaced in pvec(("[a-z0-9]{1,4}", "[a-z0-9]{1,4}", 0i64..99i64, arb_text()), 0..3),
        watch_files in pvec((arb_text(), arb_status(), 0i64..99i64, "[a-z0-9]{0,4}"), 0..4),
        watch_remotes in pvec(("[a-z0-9]{1,4}", arb_text(), arb_status(), 0i64..99i64), 0..4),
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let watch_id = "w1";
        let mut want: Vec<(Transfer, Vec<File>)> = Vec::new();
        {
            let store = Store::open(dir.path(), dir.path()).expect("store");
            for (i, (t, files)) in transfers.iter().enumerate() {
                let mut t = t.clone();
                t.id = slot_id(i);
                t.created_at = i as i64;
                let files: Vec<File> = files
                    .iter()
                    .enumerate()
                    .map(|(k, f)| File { transfer_id: t.id.clone(), idx: k as i64, ..f.clone() })
                    .collect();
                store.start(&t, &files).expect("start");
                want.push((t, files));
            }
            for (file_id, acct, pid, rel) in unfinished.iter() {
                store.add_unfinished(&Unfinished {
                    file_id: file_id.clone(),
                    account_id: acct.clone(),
                    transfer_id: "t0".into(),
                    rel_path: rel.clone(),
                    pid: *pid,
                    run: "run".into(),
                    created_at: 17,
                });
            }
            for (file_id, new_id, pid, rel) in replaced.iter() {
                store.add_replaced(&Replaced {
                    file_id: file_id.clone(),
                    account_id: "acct".into(),
                    new_id: new_id.clone(),
                    transfer_id: "t0".into(),
                    rel_path: rel.clone(),
                    pid: *pid,
                    run: "run".into(),
                    created_at: 19,
                });
            }
            store
                .add_watch(&Watch {
                    id: watch_id.into(),
                    local_path: "/tmp/in".into(),
                    account_id: "a".into(),
                    folder_id: "f".into(),
                    dest_label: "P / In".into(),
                    enabled: true,
                    created_at: 5,
                    direction: DIR_MIRROR.into(),
                    ignore_remote: false,
                    on_change: ON_CHANGE_KEEP.into(),
                    origin: String::new(),
                })
                .expect("watch");
            let wf: Vec<WatchFile> = watch_files
                .iter()
                .map(|(rel, st, updated, remote)| WatchFile {
                    rel_path: rel.clone(),
                    size: 3,
                    mtime: 4,
                    state: st.clone(),
                    attempts: 1,
                    error: String::new(),
                    remote_id: remote.clone(),
                    updated_at: *updated,
                })
                .collect();
            let wr: Vec<WatchRemote> = watch_remotes
                .iter()
                .map(|(id, rel, st, updated)| WatchRemote {
                    remote_id: id.clone(),
                    rel_path: rel.clone(),
                    size: 3,
                    state: st.clone(),
                    attempts: 1,
                    error: String::new(),
                    updated_at: *updated,
                })
                .collect();
            // Baseline first, then the real records: a baseline keeps whatever
            // record is already there, and a record overwrites.
            store.baseline_watch_files(watch_id, &wf).expect("baseline files");
            store.put_watch_files(watch_id, &wf).expect("put files");
            store.baseline_watch_remotes(watch_id, &wr).expect("baseline remotes");
            store.put_watch_remotes(watch_id, &wr).expect("put remotes");
            store.close();
        }
        let store = Store::open(dir.path(), dir.path()).expect("reopen");
        prop_assert_eq!(store.watches().expect("watches").len(), 1, "one watch survives");
        let w = &store.watches().expect("watches")[0];
        prop_assert_eq!(w.direction.as_str(), DIR_MIRROR);
        prop_assert_eq!(w.on_change.as_str(), ON_CHANGE_KEEP);
        prop_assert!(w.enabled);
        let got_files = store.watch_files(watch_id).expect("watch files");
        let mut want_files: BTreeMap<String, WatchFile> = BTreeMap::new();
        for f in watch_files.iter().map(|(rel, st, updated, remote)| WatchFile {
            rel_path: rel.clone(),
            size: 3,
            mtime: 4,
            state: st.clone(),
            attempts: 1,
            error: String::new(),
            remote_id: remote.clone(),
            updated_at: *updated,
        }) {
            want_files.insert(f.rel_path.clone(), f);
        }
        prop_assert_eq!(got_files.len(), want_files.len(), "every local watch file survives");
        for (rel, f) in &want_files {
            let g = got_files.get(rel).unwrap_or_else(|| panic!("{} is missing", rel));
            prop_assert_eq!(g.state.as_str(), f.state.as_str(), "state of {}", rel);
            // A row written with no stamp gets one from the store, and keeps it.
            if f.updated_at == 0 {
                prop_assert!(g.updated_at > 0, "the store stamps a new row ({})", rel);
            } else {
                prop_assert_eq!(g.updated_at, f.updated_at, "stamp of {}", rel);
            }
            prop_assert_eq!(g.remote_id.as_str(), f.remote_id.as_str(), "remote of {}", rel);
        }
        let got_remotes = store.watch_remotes(watch_id).expect("watch remotes");
        let mut want_remotes: BTreeMap<String, WatchRemote> = BTreeMap::new();
        for f in watch_remotes.iter().map(|(id, rel, st, updated)| WatchRemote {
            remote_id: id.clone(),
            rel_path: rel.clone(),
            size: 3,
            state: st.clone(),
            attempts: 1,
            error: String::new(),
            updated_at: *updated,
        }) {
            want_remotes.insert(f.remote_id.clone(), f);
        }
        prop_assert_eq!(got_remotes.len(), want_remotes.len(), "every remote watch file survives");
        for (id, f) in &want_remotes {
            let g = got_remotes.get(id).unwrap_or_else(|| panic!("{} is missing", id));
            prop_assert_eq!(g.state.as_str(), f.state.as_str(), "state of {}", id);
            prop_assert_eq!(g.rel_path.as_str(), f.rel_path.as_str(), "path of {}", id);
        }
        for (t, files) in &want {
            let g = store.get(&t.id).expect("get").expect("the transfer survives");
            prop_assert_eq!(debug(&g), debug(t), "transfer {}", t.id);
            prop_assert_eq!(debug(&store.files(&t.id).expect("files")), debug(files), "files of {}", t.id);
        }
        prop_assert!(store.get("nobody").expect("get").is_none(), "no invented rows");
        // Unfinished and replaced rows, by set of file id.
        // A second add for the same file id hands the row to a new owner: only
        // pid and run change, so account, transfer and path stay as first written.
        let mut want_un: BTreeMap<String, (String, i64, String)> = BTreeMap::new();
        for (file_id, acct, pid, rel) in unfinished.iter() {
            let row = want_un.entry(file_id.clone()).or_insert((acct.clone(), 0, rel.clone()));
            row.1 = *pid;
        }
        let got_un = store.unfinished_uploads().expect("unfinished");
        prop_assert_eq!(got_un.len(), want_un.len(), "one unfinished row per file id");
        for u in &got_un {
            let (acct, pid, rel) = want_un.get(&u.file_id).unwrap_or_else(|| panic!("{}", u.file_id));
            prop_assert_eq!(u.account_id.as_str(), acct.as_str());
            prop_assert_eq!(u.pid, *pid);
            prop_assert_eq!(u.rel_path.as_str(), rel.as_str());
        }
        let mut want_rep: BTreeMap<String, String> = BTreeMap::new();
        for (file_id, new_id, _pid, _rel) in replaced.iter() {
            want_rep.insert(file_id.clone(), new_id.clone());
        }
        let got_rep = store.replaced_files().expect("replaced");
        prop_assert_eq!(got_rep.len(), want_rep.len(), "one replaced row per file id");
        for r in &got_rep {
            prop_assert_eq!(r.new_id.as_str(), want_rep.get(&r.file_id).unwrap().as_str());
        }
    }

    /// Watch rows are a map keyed by (watch, path): a put replaces the whole
    /// record, a baseline keeps what is there, requeueing rewrites exactly the
    /// failed and canceled rows, and the counters follow from the map.
    #[test]
    fn watch_rows_follow_a_map_model(
        puts in pvec((0usize..4, arb_status(), 0i64..4), 0..6),
        baselines in pvec((0usize..4, arb_status(), 0i64..4), 0..4),
        requeue in pvec(0usize..4, 0..4),
        forget in hash_set(0usize..4, 0..3),
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Store::open(dir.path(), dir.path()).expect("store");
        let id = "w";
        let rel = |k: usize| format!("file-{}.mov", k);
        let mut model: BTreeMap<String, WatchFile> = BTreeMap::new();
        for (k, st, updated) in puts.iter() {
            let f = WatchFile {
                rel_path: rel(*k),
                size: 1,
                mtime: 2,
                state: st.clone(),
                attempts: 1,
                error: String::new(),
                remote_id: format!("r{}", k),
                updated_at: *updated + 1,
            };
            store.put_watch_files(id, &[f.clone()]).expect("put");
            model.insert(f.rel_path.clone(), f);
        }
        for (k, st, updated) in baselines.iter() {
            let f = WatchFile {
                rel_path: rel(*k),
                size: 9,
                mtime: 9,
                state: st.clone(),
                attempts: 9,
                error: "baseline".into(),
                remote_id: "base".into(),
                updated_at: *updated + 1,
            };
            store.baseline_watch_files(id, &[f.clone()]).expect("baseline");
            model.entry(f.rel_path.clone()).or_insert(f);
        }
        store.requeue_canceled(id);
        for f in model.values_mut() {
            if f.state == WATCH_CANCELED {
                f.state = WATCH_QUEUED.into();
                f.attempts = 0;
                f.error = String::new();
            }
        }
        // Explicitly requeue a few: only the failed and canceled ones move.
        let keys: Vec<String> = requeue.iter().map(|k| rel(*k)).collect();
        if !keys.is_empty() {
            store.requeue_watch_files(id, &keys).expect("requeue");
            for f in model.values_mut() {
                if keys.contains(&f.rel_path) && (f.state == WATCH_FAILED || f.state == WATCH_CANCELED) {
                    f.state = WATCH_QUEUED.into();
                    f.attempts = 0;
                    f.error = String::new();
                }
            }
        }
        let got = store.watch_files(id).expect("watch files");
        prop_assert_eq!(got.len(), model.len(), "one row per path");
        for (k, f) in &model {
            let g = got.get(k).unwrap_or_else(|| panic!("{} is missing", k));
            prop_assert_eq!(g.state.as_str(), f.state.as_str(), "state of {}", k);
            prop_assert_eq!(g.attempts, f.attempts, "attempts of {}", k);
            prop_assert_eq!(g.error.as_str(), f.error.as_str(), "error of {}", k);
            prop_assert_eq!(g.updated_at, f.updated_at, "stamp of {}", k);
        }
        let st = store.watch_stats(id);
        let count = |m: &BTreeMap<String, WatchFile>, s: &str| m.values().filter(|f| f.state == s).count() as i64;
        prop_assert_eq!(st.uploaded, count(&model, WATCH_UPLOADED), "uploaded");
        prop_assert_eq!(st.failed, count(&model, WATCH_FAILED), "failed");
        prop_assert_eq!(st.canceled, count(&model, WATCH_CANCELED), "canceled");
        let last = model.values().filter(|f| f.state == WATCH_UPLOADED).map(|f| f.updated_at).max().unwrap_or(0);
        prop_assert_eq!(st.last_transfer, last, "last transfer");
        // Forgetting a remote record drops only the failed, waiting and canceled
        // ones, and leaving a watch takes all three of its tables with it.
        let remotes: Vec<WatchRemote> = (0..4)
            .map(|k| WatchRemote {
                remote_id: format!("r{}", k),
                rel_path: rel(k),
                size: 1,
                state: [WATCH_DOWNLOADED, WATCH_FAILED, WATCH_WAITING, WATCH_CANCELED][k].into(),
                attempts: 0,
                error: String::new(),
                updated_at: 1,
            })
            .collect();
        store.put_watch_remotes(id, &remotes).expect("put remotes");
        let ids: Vec<String> = forget.iter().map(|k| format!("r{}", k)).collect();
        store.forget_watch_remotes(id, &ids).expect("forget");
        let got = store.watch_remotes(id).expect("remotes");
        for (k, r) in remotes.iter().enumerate() {
            let dropped = ids.contains(&r.remote_id)
                && (r.state == WATCH_FAILED || r.state == WATCH_WAITING || r.state == WATCH_CANCELED);
            prop_assert_eq!(!got.contains_key(&r.remote_id), dropped, "remote r{} state {}", k, r.state);
        }
        store.delete_watch(id);
        prop_assert!(store.watch_files(id).expect("files").is_empty(), "delete_watch takes the local rows");
        prop_assert!(store.watch_remotes(id).expect("remotes").is_empty(), "delete_watch takes the remote rows");
        prop_assert!(store.watches().expect("watches").is_empty(), "delete_watch takes the watch");
    }
}
