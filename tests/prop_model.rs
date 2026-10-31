
//! Property tests for the job model the UI sees — `Job`/`File` counters,
//! `Job::view()`, `Manager::snapshot()` — plus the filesystem helpers next to
//! the engine (`scan_paths`, and the download naming helpers, which live in
//! `src/xfer/download.rs`'s own test module).
//!
//! The method: every property is checked against an independent reference
//! model that recomputes from the plain data (the file list, the op trace),
//! never against a production helper. Where the implementation keeps derived
//! state — the per-status counters — the oracle is a full recount of the file
//! list, so a drift in the incremental bookkeeping shows up as a disagreement.
//!
//! Run with:
//!   cd /Users/chaos/work/proptest-model && \
//!   CARGO_TARGET_DIR=/Users/chaos/work/proptest-target \
//!   cargo test --offline --test prop_model -- --nocapture

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use proptest::collection::vec as pvec;
use proptest::prelude::*;

use fiotransfer::api::Client;
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history;
use fiotransfer::xfer::download::{sidecar, truncate_utf8};
use fiotransfer::xfer::manager::{new_tuned, scan_paths, skip_name, Tune};
use fiotransfer::xfer::{File, Job, Manager, Status};

// ---------- a manager to drive, offline ----------

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

/// A manager whose history store, log and download dirs are all inside a
/// tempdir, so nothing touches the real user profile.
struct Env {
    rt: tokio::runtime::Runtime,
    m: Arc<Manager>,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}

fn env() -> Env {
    let rt = rt();
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().to_path_buf();
    std::fs::create_dir_all(base.join("hist")).expect("hist dir");
    std::fs::create_dir_all(base.join("logs")).expect("log dir");
    let dl = base.join("dl").to_string_lossy().into_owned();
    std::fs::create_dir_all(&dl).expect("download dir");
    let m = rt.block_on(async {
        let api = Arc::new(Client::new(AuthManager::new_static("t")));
        let hist = Arc::new(history::Store::open(&base.join("hist"), &base.join("logs")).expect("store"));
        new_tuned(
            api,
            hist,
            Box::new(move || {
                let mut s = Settings::default();
                s.streams = 4;
                // No log files next to the media: the tests own their temp trees.
                s.no_folder_logs = true;
                s.download_dir = dl.clone();
                s
            }),
            Tune::default(),
        )
    });
    Env { rt, m, dir }
}

/// Makes a job with `sizes.len()` files attached and (not yet) published.
fn job_with_files(m: &Arc<Manager>, sizes: &[i64]) -> (Arc<Job>, Vec<Arc<File>>) {
    let j = m.new_job("download");
    let mut files = Vec::with_capacity(sizes.len());
    for (i, size) in sizes.iter().enumerate() {
        let f = Arc::new(File::new(i, &format!("f{}.mov", i), &format!("f{}.mov", i), &format!("/tmp/f{}.mov", i), *size));
        f.attach(&j);
        files.push(f);
    }
    (j, files)
}

// ---------- the model ----------

/// What the file list says the counters should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Counts {
    done: i64,
    failed: i64,
    skipped: i64,
    canceled: i64,
    active: i64,
    other: i64, // resolving / queued / partial: no counter of their own
    total: i64,
}

fn recount(files: &[Arc<File>]) -> Counts {
    let mut c = Counts { total: files.len() as i64, ..Counts::default() };
    for f in files {
        match f.status() {
            Status::Done => c.done += 1,
            Status::Failed => c.failed += 1,
            Status::Skipped => c.skipped += 1,
            Status::Canceled => c.canceled += 1,
            Status::Running => c.active += 1,
            _ => c.other += 1,
        }
    }
    c
}

impl Counts {
    /// The counters as the job holds them.
    fn stored(j: &Job) -> Counts {
        Counts {
            done: j.n_done.load(Ordering::Relaxed) as i64,
            failed: j.n_failed.load(Ordering::Relaxed) as i64,
            skipped: j.n_skipped.load(Ordering::Relaxed) as i64,
            canceled: j.n_canceled.load(Ordering::Relaxed) as i64,
            active: j.n_active.load(Ordering::Relaxed) as i64,
            other: 0,
            total: j.files().len() as i64,
        }
    }
}

// ---------- generators ----------

fn status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Queued),
        Just(Status::Resolving),
        Just(Status::Running),
        Just(Status::Done),
        Just(Status::Partial),
        Just(Status::Failed),
        Just(Status::Canceled),
        Just(Status::Skipped),
    ]
}

/// One thing the engine can do to one file. Byte amounts are `u32` so a whole
/// history can never overflow the byte counters by accident.
#[derive(Debug, Clone, Copy)]
enum Act {
    Set(Status),
    Add(u32),
    Credit(u32),
    Unwind(u32),
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        5 => status().prop_map(Act::Set),
        2 => (0u32..8000).prop_map(Act::Add),
        1 => (0u32..8000).prop_map(Act::Credit),
        1 => (0u32..8000).prop_map(Act::Unwind),
    ]
}

/// A whole file's op trace picks its own index, so shrinking can drop files.
fn history(sizes: Vec<i64>) -> impl Strategy<Value = Vec<(usize, Act)>> {
    let n = sizes.len();
    pvec((0usize..n.max(1), act()), 0..14)
}

fn any_sizes() -> impl Strategy<Value = Vec<i64>> {
    pvec(0i64..12_000, 1..6)
}

// ---------- generators: progress that sums to exactly the file size ----------

/// A partition of `size` into parts, plus, for each part, whether it is
/// credited (resume) or added (network) and whether it is later unwound (a
/// failed attempt that will be redone). Replaying such a trace leaves
/// `done == size` exactly, which a random trace cannot promise.
fn exact_plan(size: i64, cuts: Vec<u16>, kinds: Vec<bool>, unwinds: Vec<bool>) -> Vec<(usize, Act)> {
    let mut bounds: Vec<i64> = vec![0];
    for c in cuts {
        let p = (size as i128 * (c as i128 % 1001) / 1000) as i64;
        bounds.push(p.min(size).max(0));
    }
    bounds.push(size);
    bounds.sort_unstable();
    let mut parts = Vec::new();
    for w in bounds.windows(2) {
        parts.push(w[1] - w[0]);
    }
    let mut out: Vec<(usize, Act)> = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        let n = *p as u32;
        let kind = kinds.get(i).copied().unwrap_or(false);
        let emit = |b: bool| if b { Act::Add(n) } else { Act::Credit(n) };
        out.push((0, emit(kind)));
        if unwinds.get(i).copied().unwrap_or(false) && *p > 0 {
            // A failed attempt: the bytes are taken back and moved again.
            out.push((0, Act::Unwind(n)));
            out.push((0, emit(kind)));
        }
    }
    let net: i64 = out
        .iter()
        .map(|(_, a)| match a {
            Act::Add(x) | Act::Credit(x) => *x as i64,
            Act::Unwind(x) => -(*x as i64),
            _ => 0,
        })
        .sum();
    debug_assert_eq!(net, size, "the plan must sum to the file size");
    out
}

fn exact_histories() -> impl Strategy<Value = (Vec<i64>, Vec<(usize, Act)>)> {
    pvec((0i64..5000, pvec(0u16..1000, 0..4), pvec(any::<bool>(), 0..5), pvec(any::<bool>(), 0..5)), 1..5)
        .prop_map(|plans| {
            let mut sizes = Vec::new();
            let mut ops = Vec::new();
            for (i, (size, cuts, kinds, unwinds)) in plans.into_iter().enumerate() {
                sizes.push(size);
                for (idx, a) in exact_plan(size, cuts, kinds, unwinds) {
                    ops.push((if idx == 0 { i } else { idx }, a));
                }
            }
            (sizes, ops)
        })
}


// ---------- the job counters ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// The incremental per-status counters must equal a full recount of the
    /// file list after any history of `set_status` calls — repeated statuses,
    /// terminal→non-terminal attempts, and every status including the ones
    /// with no counter of their own (`Partial`, `Resolving`, `Queued`).
    ///
    /// Some status changes are made between `File::attach` and
    /// `Job::set_files`, which is where the seeding of the counts meets a
    /// counter that has already moved (see `publishing_the_file_list_counts_
    /// each_file_once` in `src/xfer/mod.rs`).
    #[test]
    fn per_status_counters_agree_with_the_file_list((sizes, ops, cut) in any_sizes().prop_flat_map(|s| {
        let h = history(s.clone());
        (Just(s), h, 0usize..8)
    })) {
        let e = env();
        let (j, files) = job_with_files(&e.m, &sizes);
        let n = files.len();
        let cut = if ops.is_empty() { 0 } else { cut % (ops.len() + 1) };
        for (idx, a) in &ops[..cut] {
            if let Act::Set(s) = a {
                files[*idx % n].set_status(*s, "");
            }
        }
        j.set_files(files.clone());
        for (idx, a) in &ops[cut..] {
            if let Act::Set(s) = a {
                files[*idx % n].set_status(*s, "");
            }
        }
        let model = recount(&files);
        let stored = Counts::stored(&j);
        prop_assert_eq!((stored.done, stored.failed, stored.skipped, stored.canceled, stored.active),
                        (model.done, model.failed, model.skipped, model.canceled, model.active),
                        "counters drifted from the file list; files={:?} ops={:?}",
                        files.iter().map(|f| f.status()).collect::<Vec<_>>(), ops);
        // ... and `view()` maps them the way the UI reads them.
        let v = j.view();
        prop_assert_eq!(v.files_total, n as i64);
        prop_assert_eq!(v.files_done, model.done + model.skipped);
        prop_assert_eq!(v.files_failed, model.failed);
        prop_assert_eq!(v.files_active, model.active);
        prop_assert!(v.files_done + v.files_failed + v.files_active <= v.files_total,
                     "{} + {} + {} > {}: files={:?}", v.files_done, v.files_failed, v.files_active,
                     v.files_total, files.iter().map(|f| f.status()).collect::<Vec<_>>());
        prop_assert!(stored.done >= 0 && stored.failed >= 0 && stored.skipped >= 0
                     && stored.canceled >= 0 && stored.active >= 0, "a counter went negative: {:?}", stored);
        // The history row is built from the same counters.
        let rec = j.record();
        prop_assert_eq!(rec.files_total, n as i64);
        prop_assert_eq!(rec.files_done, model.done + model.skipped);
        prop_assert_eq!(rec.files_failed, model.failed);
    }

    /// Progress that sums to exactly the file sizes must leave `done == size`
    /// on the file, `job.done` equal to the sum over the files, and a meter
    /// that counted only what crossed the network (adds, not credits, and not
    /// the unwind of a failed attempt).
    #[test]
    fn byte_counters_exact_when_the_history_sums_to_the_size((sizes, ops) in exact_histories()) {
        let e = env();
        let (j, files) = job_with_files(&e.m, &sizes);
        let n = files.len();
        let mut total = 0i64;
        for s in &sizes { j.add_total(*s); total += *s; }
        j.set_files(files.clone());
        let mut net = 0i64;
        for (idx, a) in &ops {
            let f = &files[*idx % n];
            match a {
                Act::Add(x) => { f.add(*x as i64); net += *x as i64; }
                Act::Credit(x) => f.credit(*x as i64),
                Act::Unwind(x) => f.unwind(*x as i64),
                Act::Set(s) => f.set_status(*s, ""),
            }
        }
        let v = j.view();
        for (i, (f, want)) in files.iter().zip(sizes.iter()).enumerate() {
            prop_assert_eq!(f.done.load(Ordering::Relaxed), *want, "file {} done; sizes={:?}", i, sizes);
        }
        let sum: i64 = files.iter().map(|f| f.done.load(Ordering::Relaxed)).sum();
        prop_assert_eq!(j.done.load(Ordering::Relaxed), sum, "job.done must be the sum of the files");
        prop_assert_eq!(v.done, sum);
        prop_assert_eq!(v.total, total);
        prop_assert_eq!(j.meter.total(), net, "the meter counts bytes off the network only");
        prop_assert!(v.done >= 0 && v.done <= v.total, "done {} total {}", v.done, v.total);
    }

    /// `elapsed`, `avg_speed`, `eta` and `speed` must always be finite and
    /// non-negative, whatever the history looks like, and a terminal job must
    /// promise no more time.
    #[test]
    fn views_are_finite_and_non_negative((sizes, ops, job_st) in any_sizes().prop_flat_map(|s| {
        let h = history(s.clone());
        (Just(s), h, 0usize..16)
    })) {
        let e = env();
        let (j, files) = job_with_files(&e.m, &sizes);
        let n = files.len();
        j.set_files(files.clone());
        e.m.add(&j);
        for (idx, a) in &ops {
            let f = &files[*idx % n];
            match a {
                Act::Add(x) => f.add(*x as i64),
                Act::Credit(x) => f.credit(*x as i64),
                Act::Unwind(x) => f.unwind(*x as i64),
                Act::Set(s) => f.set_status(*s, ""),
            }
        }
        let all = [Status::Queued, Status::Resolving, Status::Running, Status::Done,
                   Status::Partial, Status::Failed, Status::Canceled, Status::Skipped];
        j.set_status(all[job_st % all.len()]);
        let v = j.view();
        prop_assert!(v.elapsed.is_finite() && v.elapsed >= 0.0, "elapsed {}", v.elapsed);
        prop_assert!(v.avg_speed.is_finite() && v.avg_speed >= 0.0, "avg_speed {}", v.avg_speed);
        prop_assert!(v.eta.is_finite() && v.eta >= 0.0, "eta {}", v.eta);
        prop_assert!(v.speed.is_finite() && v.speed >= 0.0, "speed {}", v.speed);
        if v.status.terminal() {
            prop_assert_eq!(v.eta, 0.0, "a finished job promises no more time");
        }
        let snap = e.m.snapshot();
        prop_assert_eq!(snap.jobs.len(), 1);
        prop_assert_eq!(snap.jobs[0].id.clone(), v.id.clone());
        prop_assert_eq!(snap.jobs[0].files_done, v.files_done);
        prop_assert_eq!(snap.jobs[0].done, v.done);
    }

    /// The engine's own ending: `finish` must leave a terminal job whose every
    /// file is terminal, and it must pick the final status from the *file
    /// list*, not from a counter that may have drifted.
    #[test]
    fn finish_reports_a_terminal_job_and_leaves_every_file_terminal(
        (sizes, ops, cancel, twice, fail_first) in any_sizes().prop_flat_map(|s| {
            let h = history(s.clone());
            (Just(s), h, any::<bool>(), any::<bool>(), any::<bool>())
        })
    ) {
        let e = env();
        let (j, files) = job_with_files(&e.m, &sizes);
        let n = files.len();
        j.set_files(files.clone());
        j.set_status(Status::Running);
        for (idx, a) in &ops {
            if let Act::Set(s) = a {
                files[*idx % n].set_status(*s, "");
            }
        }
        // the model is written from the file list *before* the ending runs
        let pre: Vec<Status> = files.iter().map(|f| f.status()).collect();
        if cancel {
            j.cx.cancel();
        }
        if fail_first {
            j.fail("boom");
        }
        e.rt.block_on(async { j.finish().await });
        let after = recount(&files);
        // the reference: settle the list by hand, then decide from a recount
        let expect = if fail_first {
            if cancel { Status::Canceled } else { Status::Failed }
        } else {
            model_outcome(&pre, cancel)
        };
        prop_assert_eq!(j.status(), expect, "files={:?}", files.iter().map(|f| f.status()).collect::<Vec<_>>());
        prop_assert!(j.status().terminal(), "a finished job must report a terminal status");
        // The invariant from the UI's side: no terminal job with a live file.
        for (i, f) in files.iter().enumerate() {
            prop_assert!(f.status().terminal(), "file {} is {} in a {} job", i, f.status(), j.status());
        }
        // ... and the counts the view shows still describe the file list.
        let v = j.view();
        prop_assert_eq!(v.files_done, after.done + after.skipped);
        prop_assert_eq!(v.files_failed, after.failed);
        prop_assert_eq!(v.files_active, 0);
        prop_assert!(v.files_done + v.files_failed <= v.files_total, "{:?}", v);
        if twice {
            let first = j.status();
            e.rt.block_on(async { j.finish().await });
            prop_assert_eq!(j.status(), first, "finishing twice must not change the outcome");
        }
    }

    /// The snapshot lists jobs newest-first with no duplicates, and
    /// `clear_finished` removes exactly the terminal ones.
    #[test]
    fn snapshot_is_newest_first_and_clear_finished_removes_exactly_the_terminal_jobs(
        ops in pvec(prop_oneof![
            3 => (1usize..4, any::<bool>()).prop_map(|(k, term)| JobAct::Add(k, term)),
            1 => Just(JobAct::Clear),
            1 => (0usize..12, status()).prop_map(|(k, s)| JobAct::SetLast(k, s)),
        ], 0..18)
    ) {
        let e = env();
        let mut model: Vec<(String, Status)> = Vec::new();
        for op in &ops {
            match *op {
                JobAct::Add(k, term) => {
                    for _ in 0..k {
                        let j = e.m.new_job(if model.len() % 2 == 0 { "upload" } else { "download" });
                        *j.title.lock().unwrap() = format!("job {}", model.len());
                        j.set_status(if term { Status::Done } else { Status::Running });
                        e.m.add(&j);
                        model.push((j.id.clone(), j.status()));
                    }
                }
                JobAct::Clear => {
                    e.m.clear_finished();
                    model.retain(|(_, s)| !s.terminal());
                }
                JobAct::SetLast(k, s) => {
                    if !model.is_empty() {
                        let i = k % model.len();
                        let j = e.m.job(&model[i].0).expect("job is still listed");
                        j.set_status(s);
                        model[i].1 = s;
                    }
                }
                _ => {}
            }
            let snap = e.m.snapshot();
            let want: Vec<&String> = model.iter().rev().map(|(id, _)| id).collect();
            let got: Vec<&String> = snap.jobs.iter().map(|v| &v.id).collect();
            prop_assert_eq!(got, want, "snapshot must be the list newest-first; ops={:?}", ops);
            let mut seen = std::collections::HashSet::new();
            for v in &snap.jobs { prop_assert!(seen.insert(v.id.clone()), "duplicate job {} in the snapshot", v.id); }
            for (i, (_id, st)) in model.iter().rev().enumerate() {
                prop_assert_eq!(snap.jobs[i].status, *st);
            }
        }
    }

    /// `prune_watch_jobs` keeps the newest `KEEP_WATCH_JOBS` finished watch
    /// batches and drops everything else it is allowed to drop.
    #[test]
    fn prune_keeps_the_newest_watch_batches(
        ops in pvec(prop_oneof![
            3 => (1usize..3, any::<bool>()).prop_map(|(k, watch)| JobAct::AddWatch(k, watch)),
            1 => Just(JobAct::Prune),
        ], 0..26)
    ) {
        let e = env();
        let mut model: Vec<(String, bool, Status)> = Vec::new(); // id, is_watch, status
        for op in &ops {
            match *op {
                JobAct::AddWatch(k, watch) => {
                    for i in 0..k {
                        let j = e.m.new_job("upload");
                        if watch { *j.watch_id.lock().unwrap() = format!("w{}", model.len() + i); }
                        j.set_status(if model.len() % 5 == 0 { Status::Running } else { Status::Done });
                        e.m.add(&j);
                        model.push((j.id.clone(), watch, j.status()));
                    }
                }
                JobAct::Prune => {
                    e.m.prune_watch_jobs();
                    // the reference rule, applied to the model's insertion order
                    let mut keep = vec![true; model.len()];
                    let mut n = 0usize;
                    for i in (0..model.len()).rev() {
                        if model[i].1 && model[i].2.terminal() {
                            n += 1;
                            if n > fiotransfer::xfer::KEEP_WATCH_JOBS { keep[i] = false; }
                        }
                    }
                    let mut i = 0;
                    model.retain(|_| { let k = keep[i]; i += 1; k });
                }
                _ => {}
            }
            let snap = e.m.snapshot();
            let want: Vec<&String> = model.iter().rev().map(|(id, _, _)| id).collect();
            let got: Vec<&String> = snap.jobs.iter().map(|v| &v.id).collect();
            prop_assert_eq!(got, want, "prune must drop only older watch batches");
        }
    }
}

#[derive(Debug, Clone)]
enum JobAct {
    Add(usize, bool),
    AddWatch(usize, bool),
    Clear,
    SetLast(usize, Status),
    Prune,
}

// ---------- a reference model for the engine's ending ----------

/// What `settle_files` does to one file: terminal files are left alone, the
/// rest are canceled on a cancel and failed otherwise.
fn settle(s: Status, cancel: bool) -> Status {
    if s.terminal() {
        s
    } else if cancel {
        Status::Canceled
    } else {
        Status::Failed
    }
}

/// The outcome rule, written out from the contract rather than read off a
/// counter: a canceled job is `canceled`, a job with no failures is `done`, a
/// job with some progress is `partial`, and one with nothing but failures is
/// `failed`. (The choice between the last two is policy, not bookkeeping; this
/// model is used to check that the *counts* feeding the choice are right.)
fn model_outcome(statuses: &[Status], cancel: bool) -> Status {
    let s: Vec<Status> = statuses.iter().map(|x| settle(*x, cancel)).collect();
    let n = |want: Status| s.iter().filter(|x| **x == want).count();
    if cancel && (n(Status::Canceled) > 0 || s.is_empty()) {
        Status::Canceled
    } else if n(Status::Failed) == 0 {
        Status::Done
    } else if n(Status::Done) + n(Status::Skipped) > 0 {
        Status::Partial
    } else {
        Status::Failed
    }
}

// ---------- the filesystem walk ----------

/// The names a walk must skip, written out from `skip_name`'s doc comment and
/// the folder-log prefix — an independent list, so a change to one side shows.
/// The narrow rule: only the stamp this app writes is a log, so a user file
/// called "FioTransfer notes.txt" is uploaded like any other.
fn is_stamped_log(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("FioTransfer ") else { return false };
    let Some(stem) = rest.strip_suffix(".txt") else { return false };
    ["Watch ", "Upload ", "Download "].iter().any(|kind| {
        stem.strip_prefix(kind)
            .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ' '))
            .unwrap_or(false)
    })
}

fn reference_skip(name: &str) -> bool {
    const META: [&str; 8] = [
        ".DS_Store",
        "Thumbs.db",
        "desktop.ini",
        ".localized",
        ".Spotlight-V100",
        ".Trashes",
        ".fseventsd",
        ".TemporaryItems",
    ];
    META.contains(&name)
        || name.starts_with("._")
        || name.ends_with(".fiopart")
        || name.ends_with(".fiostate")
        || name.ends_with(".fiostate.tmp")
        || is_stamped_log(name)
}

fn reference_walk(root: &Path, base: &Path, out: &mut Vec<(String, i64)>) {
    let mut ents: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("a generated directory is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    ents.sort();
    for p in ents {
        let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if reference_skip(&name) {
            continue;
        }
        let md = std::fs::metadata(&p).expect("metadata of a generated entry");
        if md.is_dir() {
            reference_walk(&p, base, out);
        } else {
            let rel = p.strip_prefix(base).expect("under the base").to_string_lossy().replace('\\', "/");
            out.push((rel, md.len() as i64));
        }
    }
}

const SKIP_POOL: [&str; 15] = [
    ".DS_Store", "._resource", "Thumbs.db", "desktop.ini", ".localized", "a.fiopart", "a.fiostate",
    "a.fiostate.tmp", ".Spotlight-V100", ".Trashes", ".fseventsd", ".TemporaryItems",
    "FioTransfer Watch 2026-01-01.txt",
    "FioTransfer Upload 2026-01-01 100000.txt",
    "FioTransfer Download 2026-01-01 100000.txt",
];
const SAFE_POOL: [&str; 13] = [
    "a.mov", "b.txt", "clip 1.mov", "é.mov", "日本語.mov", "x.🙂", ".hidden", "..x", "x.fiopartX",
    ".DS_Store2", "FioTransfer log.txt.bak", "tab\there", "sp ace ",
];

fn skipped_names_pick(i: usize) -> String {
    SKIP_POOL[i % SKIP_POOL.len()].to_string()
}
fn safe_name_pick(i: usize) -> String {
    SAFE_POOL[i % SAFE_POOL.len()].to_string()
}

fn safe_names() -> impl Strategy<Value = String> {
    (0usize..SAFE_POOL.len()).prop_map(|i| SAFE_POOL[i].to_string())
}

fn skipped_names() -> impl Strategy<Value = String> {
    (0usize..SKIP_POOL.len()).prop_map(|i| SKIP_POOL[i].to_string())
}

fn any_name() -> impl Strategy<Value = String> {
    prop_oneof![3 => safe_names(), 2 => skipped_names()]
}

/// One entry of a generated tree: which directory it goes in (modulo the
/// directories made so far), its name, whether it is a directory, its size.
#[derive(Debug, Clone, Copy)]
struct Ent {
    at: usize,
    name: u8,
    skipped: bool,
    dir: bool,
    size: u16,
}

fn any_tree() -> impl Strategy<Value = Vec<Ent>> {
    pvec((0usize..8, 0u8..24, any::<bool>(), any::<bool>(), 0u16..3000), 0..12).prop_map(|v| {
        v.into_iter()
            .map(|(at, name, skipped, dir, size)| Ent { at, name, skipped, dir, size })
            .collect()
    })
}

/// Builds the tree on disk; returns the root and the reference walk.
fn build_tree(spec: &[Ent], root: &Path) -> Vec<(String, i64)> {
    use std::collections::HashSet;
    let mut dirs: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut used: HashSet<(PathBuf, String)> = HashSet::new();
    for e in spec {
        let parent = dirs[e.at % dirs.len()].clone();
        let name = if e.skipped {
            skipped_names_pick(e.name as usize)
        } else {
            safe_name_pick(e.name as usize)
        };
        if !used.insert((parent.clone(), name.clone())) {
            continue; // the same name twice in one directory: the second create fails
        }
        let p = parent.join(&name);
        if e.dir {
            if std::fs::create_dir(&p).is_ok() {
                dirs.push(p);
            }
        } else {
            let body = vec![b'x'; e.size as usize];
            if std::fs::write(&p, &body).is_ok() {
                // The file is ours, and its size is the one just written.
            }
        }
    }
    // The walk's relative paths start at the scanned folder's own name, because
    // that is the folder created on Frame.io.
    let base = root.parent().unwrap_or(root).to_path_buf();
    let mut out = Vec::new();
    reference_walk(root, &base, &mut out);
    out
}


// ---------- the walk over generated trees ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// `skip_name` must agree with the list in its doc comment on every name,
    /// including the near misses (`x.fiopartX`, `.DS_Store2`, `FioTransfer
    /// log.txt.bak`) that a sloppier rule would swallow.
    #[test]
    fn skip_name_agrees_with_its_documented_list(name in any_name()) {
        prop_assert_eq!(skip_name(&name), reference_skip(&name), "name {:?}", name);
    }

    /// `scan_paths` on a directory must yield every regular, non-skipped file
    /// exactly once: never a directory, relative paths with '/' and no
    /// leading separator, and sizes read from the disk.
    #[test]
    fn scan_paths_matches_a_reference_walk(spec in any_tree()) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("tree");
        std::fs::create_dir(&root).expect("root dir");
        let want = build_tree(&spec, &root);
        let got = scan_paths(&[root.to_string_lossy().into_owned()]).expect("scan a generated tree");
        let mut got_set: Vec<(String, i64)> = got.iter().map(|s| (s.rel.clone(), s.size)).collect();
        got_set.sort();
        let mut want_set = want.clone();
        want_set.sort();
        prop_assert_eq!(got_set, want_set, "spec {:?}", spec);
        // every entry is a file on disk, with that size, and named as a relative path
        let base = root.parent().unwrap().to_path_buf();
        for s in &got {
            let p = PathBuf::from(&s.local);
            let md = std::fs::metadata(&p).expect("the yielded path exists");
            prop_assert!(md.is_file(), "yielded a directory: {:?}", s.rel);
            prop_assert_eq!(md.len() as i64, s.size, "size of {:?}", s.rel);
            prop_assert!(!s.rel.is_empty() && !s.rel.starts_with('/') && !s.rel.starts_with("./"),
                         "rel path {:?}", s.rel);
            prop_assert!(!s.rel.contains('\\') && !s.rel.split('/').any(|c| c.is_empty() || c == "." || c == ".."),
                         "rel path {:?}", s.rel);
            prop_assert!(!reference_skip(s.rel.rsplit('/').next().unwrap_or("")), "yielded a skipped name {:?}", s.rel);
            prop_assert_eq!(&s.local, &base.join(&s.rel).to_string_lossy().into_owned(), "rel {:?}", s.rel);
        }
        // no duplicates: one entry per file
        let mut rels: Vec<&String> = got.iter().map(|s| &s.rel).collect();
        rels.sort();
        rels.dedup();
        prop_assert_eq!(rels.len(), got.len());
        // and a second walk says the same thing
        let again = scan_paths(&[root.to_string_lossy().into_owned()]).expect("scan twice");
        prop_assert_eq!(again.len(), got.len());
    }

    /// A file named on the command line is yielded by its base name, unless it
    /// is a skipped or hidden-temp name — which yields nothing, not an error.
    #[test]
    fn scan_paths_on_a_named_file_yields_its_base_name(name in any_name(), size in 0u16..4000) {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("named").join(&name);
        std::fs::create_dir_all(p.parent().unwrap()).expect("parent");
        std::fs::write(&p, vec![b'y'; size as usize]).expect("write");
        let got = scan_paths(&[p.to_string_lossy().into_owned()]).expect("scan a named file");
        if reference_skip(&name) {
            prop_assert!(got.is_empty(), "a skipped name must not be uploaded: {:?}", got.iter().map(|s| &s.rel).collect::<Vec<_>>());
        } else {
            prop_assert_eq!(got.len(), 1);
            prop_assert_eq!(&got[0].rel, &name);
            prop_assert_eq!(got[0].size, size as i64);
        }
    }

    /// Several roots scan in the order given, each with its own relative base,
    /// and nothing is dropped.
    #[test]
    fn scan_paths_over_several_roots_is_the_sum_of_its_parts(spec in any_tree(), spec2 in any_tree()) {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).expect("a");
        std::fs::create_dir(&b).expect("b");
        let want_a = build_tree(&spec, &a);
        let want_b = build_tree(&spec2, &b);
        let got = scan_paths(&[a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned()])
            .expect("scan two roots");
        let mut got_set: Vec<(String, i64)> = got.iter().map(|s| (s.rel.clone(), s.size)).collect();
        got_set.sort();
        let mut want: Vec<(String, i64)> = want_a.into_iter().chain(want_b).collect();
        want.sort();
        prop_assert_eq!(got_set, want);
        // each root's entries keep that root's name in the relative path
        let n_a = got.iter().filter(|s| s.rel.starts_with("a/")).count();
        prop_assert!(n_a <= got.len());
    }
}

/// Names near the file-name limit, the interesting side of `sidecar`.
fn sidecar_names() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("a.mov".to_string()),
        Just(format!("{}.mov", "x".repeat(251))),
        Just(format!("{}.mov", "x".repeat(242))),
        Just(format!("{}.mov", "x".repeat(243))),
        Just("é".repeat(130)),
        Just("y".repeat(255)),
        Just("🎬 take 1.mov".to_string()),
        Just("a.tar.gz".to_string()),
    ]
}

// ---------- what the generators actually reach ----------

/// Prints the shape of the generated histories, so a green run is not mistaken
/// for coverage. `cargo test --offline --test prop_model generator_reach --
/// --nocapture`.
#[test]
fn generator_reach() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::BTreeMap;

    let mut runner = TestRunner::deterministic();

    // --- the job-model histories ---
    let mut files: BTreeMap<usize, usize> = BTreeMap::new();
    let mut st: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut cases, mut t2n, mut repeated, mut unwinds, mut over_total) = (0, 0, 0, 0, 0);
    let mut outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let strat = any_sizes().prop_flat_map(|s| {
        let h = history(s.clone());
        (Just(s), h, any::<bool>())
    });
    for _ in 0..300 {
        let (sizes, ops, cancel) = strat.new_tree(&mut runner).expect("a case").current();
        cases += 1;
        *files.entry(sizes.len()).or_default() += 1;
        let mut cur: Vec<Status> = vec![Status::Queued; sizes.len()];
        let mut done = 0i64;
        let mut has_unwind = false;
        for (idx, a) in &ops {
            let i = idx % sizes.len();
            match a {
                Act::Set(s) => {
                    *st.entry(s.as_str()).or_default() += 1;
                    if cur[i].terminal() && !s.terminal() { t2n += 1; }
                    if cur[i] == *s { repeated += 1; }
                    cur[i] = *s;
                }
                Act::Add(x) => done += *x as i64,
                Act::Credit(x) => done += *x as i64,
                Act::Unwind(x) => { done -= *x as i64; has_unwind = true; }
            }
        }
        if has_unwind { unwinds += 1; }
        if done > sizes.iter().sum::<i64>() { over_total += 1; }
        *outcomes.entry(model_outcome(&cur, cancel).as_str().to_string()).or_default() += 1;
    }
    println!("\n[job model] {} histories", cases);
    println!("  files per job: {:?}", files);
    println!("  file statuses set: {:?}", st);
    println!("  histories with a terminal->non-terminal move: {}", t2n);
    println!("  repeated set_status to the same value: {}", repeated);
    println!("  histories with an unwind: {}, ending above the total size: {}", unwinds, over_total);
    println!("  outcome the ending model picks: {:?}", outcomes);

    // --- the trees ---
    let (mut trees, mut empty, mut with_files, mut with_dirs, mut with_skipped, mut multibyte, mut collisions) =
        (0, 0, 0, 0, 0, 0, 0);
    let strat = any_tree();
    for _ in 0..200 {
        let spec = strat.new_tree(&mut runner).expect("a tree").current();
        trees += 1;
        if spec.is_empty() { empty += 1; }
        let mut names: Vec<(usize, String)> = Vec::new();
        for e in &spec {
            let name = if e.skipped { skipped_names_pick(e.name as usize) } else { safe_name_pick(e.name as usize) };
            if !name.is_ascii() { multibyte += 1; }
            if e.dir { with_dirs += 1; } else if e.skipped { with_skipped += 1; } else { with_files += 1; }
            if names.contains(&(e.at, name.clone())) { collisions += 1; }
            names.push((e.at, name));
        }
    }
    println!("\n[trees] {} specs: {} empty, {} files, {} dirs, {} skipped names, {} multibyte names, {} duplicate names dropped",
             trees, empty, with_files, with_dirs, with_skipped, multibyte, collisions);

    // --- the names ---
    let (mut drawn, mut hashed, mut cut, mut multi) = (0, 0, 0, 0);
    let strat = sidecar_names();
    for _ in 0..300 {
        let name = strat.new_tree(&mut runner).expect("a name").current();
        drawn += 1;
        if !name.is_ascii() { multi += 1; }
        let p = format!("/tmp/{}", name);
        if sidecar(&p) != p { hashed += 1; }
        if truncate_utf8(&name, 40) != name { cut += 1; }
    }
    println!("\n[names] {} drawn: {} multibyte, {} hashed by sidecar, {} cut by truncate_utf8",
             drawn, multi, hashed, cut);
}

// ---------- the one view rule a random history cannot reach ----------

/// `eta` is only computed once the meter has a real rate, which needs two
/// samples more than 200 ms apart (`Meter::sample`). A history generator would
/// have to sleep to reach it, so this is deterministic: give the job a rate,
/// push `done` past `total`, and check the estimate stays sane.
#[test]
fn eta_is_never_negative_even_when_progress_overshoots() {
    let e = env();
    let (j, files) = job_with_files(&e.m, &[100]);
    j.set_files(files.clone());
    j.add_total(100);
    files[0].add(10_000);
    let _ = j.meter.sample(); // seeds the meter: rate 0
    std::thread::sleep(std::time::Duration::from_millis(260));
    files[0].add(300_000);
    let rate = j.meter.sample(); // 300 kB in ~0.26 s
    assert!(rate > 1.0, "the meter needs a real rate for this check: {}", rate);
    let v = j.view();
    assert!(v.speed > 1.0, "speed {}", v.speed);
    // nothing left to move: no estimate
    assert_eq!(v.eta, 0.0, "done {} total {}", v.done, v.total);
    // more moved than the size promised (a resume credited twice, a file that
    // grew): the estimate must not go negative
    files[0].add(500_000);
    let v = j.view();
    assert_eq!(v.done, 810_000);
    assert_eq!(v.total, 100);
    assert!(v.eta.is_finite(), "eta {}", v.eta);
    assert!(v.eta >= 0.0, "eta {} with done {} > total {}", v.eta, v.done, v.total);
    assert!(v.avg_speed.is_finite() && v.avg_speed >= 0.0, "avg_speed {}", v.avg_speed);
}
