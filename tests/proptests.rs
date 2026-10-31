//! Property tests, built the way `docs/proptest-praxis.md` describes: a
//! separate, obviously-correct reference model for each piece of machinery that
//! is worth doubting, and generated operation histories fed to both.
//!
//! Everything here is a discovery suite: it is allowed to be slow and blunt. The
//! regression tests for anything it finds belong next to the code.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proptest::collection::{hash_set, vec as pvec};
use proptest::prelude::*;
use proptest::sample::Index;

use fiotransfer::api::route_key;
use fiotransfer::limiter::Limiter;
use fiotransfer::share::safe_name;
use fiotransfer::xfer::bufpool::BufPool;
use fiotransfer::xfer::manager::{base_name, dirs_for, parent_dir, skip_name};
use fiotransfer::xfer::uploadstate::{Ledger, PartState, UploadState};

// ---------- the limiter: a counter is the whole model ----------

/// A limit, a number of tasks, and a random order in which they finish.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// The invariant the whole transfer engine leans on: no more streams in
    /// flight than the setting allows, ever, and every permit comes back.
    #[test]
    fn limiter_never_exceeds_its_limit_and_returns_every_permit(limit in 1usize..12, tasks in 1usize..40) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Arc::new(Limiter::new(limit));
            let held = Arc::new(AtomicUsize::new(0));
            let worst = Arc::new(AtomicUsize::new(0));
            let mut handles = Vec::new();
            for i in 0..tasks {
                let l = limiter.clone();
                let held = held.clone();
                let worst = worst.clone();
                handles.push(tokio::spawn(async move {
                    let cx = tokio_util::sync::CancellationToken::new();
                    let permit = l.acquire(&cx).await.expect("a permit");
                    let now = held.fetch_add(1, Ordering::SeqCst) + 1;
                    worst.fetch_max(now, Ordering::SeqCst);
                    // Vary how long each task keeps its permit.
                    for _ in 0..(i % 5) {
                        tokio::task::yield_now().await;
                    }
                    held.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                }));
            }
            for h in handles {
                let _ = h.await;
            }
            prop_assert!(worst.load(Ordering::SeqCst) <= limit, "at most {} at once", limit);
            prop_assert_eq!(limiter.active(), 0, "every permit returned");
            Ok(())
        });
        checked?;
    }

    /// A waiter that is cancelled must not consume capacity: the classic
    /// notify-based deadlock leaves `active` high and starves everything after.
    #[test]
    fn a_cancelled_waiter_does_not_eat_a_permit(limit in 1usize..6, waiters in 1usize..10) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Arc::new(Limiter::new(limit));
            let held: Vec<_> = {
                let mut v = Vec::new();
                for _ in 0..limit {
                    let cx = tokio_util::sync::CancellationToken::new();
                    v.push(limiter.acquire(&cx).await.expect("a permit"));
                }
                v
            };
            prop_assert_eq!(limiter.active(), limit);
            // Everyone waits, then is cancelled.
            let mut waiting = Vec::new();
            for _ in 0..waiters {
                let l = limiter.clone();
                let cx = tokio_util::sync::CancellationToken::new();
                let c = cx.clone();
                waiting.push((tokio::spawn(async move { l.acquire(&cx).await.is_ok() }), c));
            }
            tokio::task::yield_now().await;
            for (_, c) in &waiting {
                c.cancel();
            }
            for (h, _) in waiting {
                prop_assert!(!h.await.expect("task"), "a cancelled acquire must fail");
            }
            prop_assert_eq!(limiter.active(), limit, "cancelled waiters took nothing");
            drop(held);
            prop_assert_eq!(limiter.active(), 0);
            // And the limiter still works afterwards.
            let cx = tokio_util::sync::CancellationToken::new();
            let p = limiter.acquire(&cx).await.expect("a permit after the storm");
            prop_assert_eq!(limiter.active(), 1);
            drop(p);
            Ok(())
        });
        checked?;
    }

    /// Raising and lowering the limit while streams are in flight must be safe.
    ///
    /// The first version of this property acquired `l` permits per step while
    /// keeping the earlier ones, so it asked for more than the limit and then
    /// waited for ever — a hang in the *test*, not in the limiter (an acquire
    /// blocks while the limit is reached, which is the whole point). Found by
    /// this suite refusing to finish. The history below never holds more than
    /// the current limit, and releases down to it when it shrinks.
    #[test]
    fn changing_the_limit_mid_flight_keeps_the_count_sane(limits in pvec(1usize..10, 1..6)) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Arc::new(Limiter::new(limits[0]));
            let mut permits = Vec::new();
            for l in &limits {
                limiter.set_limit(*l);
                while permits.len() < *l {
                    let cx = tokio_util::sync::CancellationToken::new();
                    permits.push(limiter.acquire(&cx).await.expect("a permit"));
                }
                while permits.len() > *l {
                    permits.pop();
                }
                prop_assert_eq!(limiter.limit(), *l);
                prop_assert_eq!(limiter.active(), permits.len(), "active tracks what is held");
                prop_assert!(limiter.active() <= *l, "never above the limit");
            }
            drop(permits);
            prop_assert_eq!(limiter.active(), 0);
            Ok(())
        });
        checked?;
    }
}

// ---------- the buffer pool: allocation history is the risk ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// Two live buffers must never be the same memory, and a buffer handed out
    /// again must not carry the previous user's bytes as "its own" length.
    #[test]
    fn pooled_buffers_are_never_aliased(size in 1usize..64, takes in 1usize..12) {
        let pool = BufPool::new(size.max(1));
        let mut live = Vec::new();
        let mut seen: Vec<usize> = Vec::new();
        for i in 0..takes {
            let mut b = pool.take();
            let want = 1 + (i % size.max(1));
            b.set_len(want);
            b.as_mut_slice().fill(b'x');
            let ptr = b.as_mut_slice().as_ptr() as usize;
            prop_assert!(!seen.contains(&ptr), "buffer {:p} handed out twice while alive", b.as_mut_slice().as_ptr());
            seen.push(ptr);
            live.push(b);
        }
        // Each buffer still holds exactly the bytes it was told to.
        for b in &live {
            prop_assert!(b.as_ref().iter().all(|c| *c == b'x'));
        }
    }

    /// Recycling is what makes the pool worth having: many takes, few buffers.
    #[test]
    fn the_pool_recycles_buffers(takes in 2usize..40) {
        let pool = BufPool::new(16);
        for _ in 0..takes {
            let mut b = pool.take();
            b.set_len(16);
            let bytes = b.into_bytes();
            drop(bytes); // as the socket does once it has written them
        }
        let (made, reused) = pool.stats();
        prop_assert!(made <= 2, "made {} buffers for {} takes", made, takes);
        prop_assert_eq!(reused, takes - made, "every later take came from the pool");
    }
}

// ---------- the upload ledger: a set of finished parts ----------

fn arb_parts(n: usize) -> Vec<PartState> {
    (0..n)
        .map(|i| PartState { off: (i as i64) * 8, size: 8, url: format!("http://x/{}", i) })
        .collect()
}

fn state_for(source: &str, parts: usize, created: i64) -> UploadState {
    UploadState {
        source: source.to_string(),
        file_id: "file-1".into(),
        account_id: "acct".into(),
        folder_id: "folder".into(),
        name: "a.bin".into(),
        size: (parts as i64) * 8,
        mtime_ms: 7,
        created_at: created,
        view_url: "http://view/1".into(),
        parts: arb_parts(parts),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// What a resumed upload sends is exactly the parts it did not have: the
    /// model is the set of marked parts, nothing more.
    #[test]
    fn resuming_reports_exactly_the_parts_that_finished(parts in 1usize..40, marks in hash_set(0usize..40, 0..8)) {
        let dir = tempfile::tempdir().expect("temp dir");
        let src = dir.path().join("a.bin");
        std::fs::write(&src, b"x").expect("source");
        let source = src.to_string_lossy().into_owned();
        let ledger = Ledger::in_dir(dir.path(), &source);
        let now = fiotransfer::xfer::uploadstate::now_ms();
        let st = state_for(&source, parts, now);
        ledger.save(&st).expect("save");
        let mut model = vec![false; parts];
        for m in &marks {
            if *m < parts {
                ledger.mark_done(*m);
                model[*m] = true;
            }
        }
        let (back, done) = ledger.load(st.size, st.mtime_ms, now).expect("a resumable upload");
        prop_assert_eq!(&back.parts, &st.parts, "the part plan is unchanged");
        prop_assert_eq!(done, model, "only the parts that finished are skipped");
        ledger.remove();
    }

    /// Whatever happens to the file, an upload that no longer describes it is
    /// never resumed: a changed size, a changed mtime, or an expired window.
    #[test]
    fn a_changed_file_is_never_resumed(dsize in -2i64..3, dmtime in -2i64..3, age in 0u64..7200) {
        let dir = tempfile::tempdir().expect("temp dir");
        let src = dir.path().join("b.bin");
        std::fs::write(&src, b"y").expect("source");
        let source = src.to_string_lossy().into_owned();
        let ledger = Ledger::in_dir(dir.path(), &source);
        let now = fiotransfer::xfer::uploadstate::now_ms();
        let st = state_for(&source, 3, now);
        ledger.save(&st).expect("save");
        let size = st.size + dsize;
        let mtime = st.mtime_ms + dmtime;
        let at = now + (age as i64) * 1000;
        let loaded = ledger.load(size, mtime, at);
        let changed = dsize != 0 || dmtime != 0;
        if changed {
            prop_assert!(loaded.is_none(), "a changed file must not resume ({:?} {:?})", dsize, dmtime);
        } else if age as u64 > fiotransfer::xfer::uploadstate::resume_window().as_secs() {
            prop_assert!(loaded.is_none(), "a stale upload must not resume");
        }
        ledger.remove();
    }

    /// Housekeeping must keep exactly the Frame.io copies that a live ledger
    /// still needs, no more and no fewer.
    #[test]
    fn keeps_file_agrees_with_the_ledgers_that_exist(ids in hash_set(0u64..8, 0..6)) {
        let dir = tempfile::tempdir().expect("temp dir");
        let now = fiotransfer::xfer::uploadstate::now_ms();
        let mut made = Vec::new();
        for id in &ids {
            let src = dir.path().join(format!("f{}.bin", id));
            std::fs::write(&src, b"z").expect("source");
            let source = src.to_string_lossy().into_owned();
            let ledger = Ledger::in_dir(dir.path(), &source);
            let mut st = state_for(&source, 2, now);
            st.file_id = format!("file-{}", id);
            ledger.save(&st).expect("save");
            made.push((id, ledger));
        }
        for id in 0..8u64 {
            let expected = ids.iter().any(|i| *i == id);
            prop_assert_eq!(
                fiotransfer::xfer::uploadstate::keeps_file_in(dir.path(), &format!("file-{}", id), now),
                expected,
                "keeps_file(file-{})",
                id
            );
        }
        for (_, l) in made {
            l.remove();
        }
    }
}

// ---------- parsers, printers and path arithmetic ----------

/// The naive reading of a URL route, independent of the token-bucket key code:
/// take the path, drop the scheme and host, and replace anything that looks
/// like an id with `:id`.
fn reference_route_key(method: &str, url: &str) -> String {
    let mut path = url.to_string();
    if let Some(scheme) = path.find("://") {
        let rest = &path[scheme + 3..];
        path = match rest.find('/') {
            Some(slash) => rest[slash..].to_string(),
            None => "/".to_string(),
        };
    }
    if let Some(cut) = path.find(|c| c == '?' || c == '#') {
        path = path[..cut].to_string();
    }
    let looks_like_id = |s: &str| s.len() >= 16 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let segs: Vec<&str> = path.split('/').collect();
    let out: Vec<&str> = segs.iter().map(|s| if looks_like_id(s) { ":id" } else { *s }).collect();
    format!("{} {}", method, out.join("/"))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn route_keys_mask_ids_the_same_way_as_the_reference(
        method in prop_oneof![Just("GET"), Just("POST"), Just("DELETE"), Just("PUT")],
        host in "[a-z.]{3,12}",
        segs in pvec(prop_oneof![
            "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
            "[0-9a-f]{16}",
            "[a-z]{1,8}",
            Just("".to_string()),
        ], 0..6),
        query in prop_oneof![Just(String::new()), "[a-z]=[0-9]{1,3}".prop_map(|q| format!("?{}", q))],
    ) {
        let url = format!("https://{}/{}", host, segs.join("/")) + &query;
        let got = route_key(method, &url);
        let want = reference_route_key(method, &url);
        prop_assert_eq!(got, want, "url was {}", url);
    }

    /// Path arithmetic, against the obvious string model.
    #[test]
    fn relative_paths_are_split_like_the_reference(rels in pvec("[a-z]{1,6}", 0..5), name in "[a-z]{1,6}") {
        let rel = if rels.is_empty() { name.clone() } else { format!("{}/{}", rels.join("/"), name) };
        prop_assert_eq!(base_name(&rel), name.as_str(), "base of {}", rel);
        let want_parent = rels.join("/");
        prop_assert_eq!(parent_dir(&rel), want_parent, "parent of {}", rel);
    }

    /// `dirs_for` must name every ancestor of every file, once, parents before
    /// children — that order is what makes folder creation work on Frame.io.
    #[test]
    fn dirs_for_lists_each_ancestor_once_parents_first(rels in pvec(pvec("[a-z]{1,5}", 1..4), 0..4)) {
        let rels: Vec<String> = rels.iter().map(|p| format!("{}/f.mov", p.join("/"))).collect();
        let dirs = dirs_for(&rels);
        let mut want: Vec<String> = Vec::new();
        for r in &rels {
            let mut d = parent_dir(r);
            while !d.is_empty() {
                if !want.contains(&d) {
                    want.push(d.clone());
                }
                d = parent_dir(&d);
            }
        }
        let mut got = dirs.clone();
        let mut sorted = want.clone();
        got.sort();
        sorted.sort();
        prop_assert_eq!(&got, &sorted, "same set of directories");
        // parents before children
        for (i, d) in dirs.iter().enumerate() {
            let depth = d.matches('/').count();
            for later in &dirs[i + 1..] {
                prop_assert!(later.matches('/').count() >= depth, "{} came before {}", d, later);
            }
            let parent = parent_dir(d);
            if !parent.is_empty() {
                prop_assert!(dirs[..i].contains(&parent), "parent {} of {} came later", parent, d);
            }
        }
    }

    /// A downloaded name must never be able to leave the directory it is
    /// written into: no separators, no `..`, never empty.
    #[test]
    fn safe_names_cannot_escape_a_directory(name in ".{0,40}") {
        let out = safe_name(&name);
        prop_assert!(!out.is_empty(), "safe_name({:?}) was empty", name);
        prop_assert!(!out.contains('/'), "safe_name({:?}) = {:?} has a separator", name, out);
        prop_assert!(!out.contains('\0'), "safe_name({:?}) = {:?} has a NUL", name, out);
        prop_assert!(out != "." && out != "..", "safe_name({:?}) = {:?} is a directory", name, out);
        prop_assert!(!out.contains("../") && !out.ends_with("/.."), "safe_name({:?}) = {:?} escapes", name, out);
    }

    /// Metadata files are skipped and ordinary media is not: a watch folder
    /// that uploaded `.DS_Store` would be a bug users see.
    #[test]
    fn metadata_files_are_skipped_and_media_is_not(stem in "[A-Za-z0-9 _-]{1,12}", ext in "[a-z0-9]{1,4}") {
        let media = format!("{}.{}", stem, ext);
        prop_assert!(!skip_name(&media), "{} should upload", media);
        prop_assert!(skip_name(".DS_Store"));
        let sidebar = format!("._{}", media);
        prop_assert!(skip_name(&sidebar), "{} should be skipped", sidebar);
        let part = format!("{}.fiopart", media);
        prop_assert!(skip_name(&part), "{} should be skipped", part);
        let state = format!("{}.fiostate", media);
        prop_assert!(skip_name(&state), "{} should be skipped", state);
        // The upload ledger is not written next to the media (it lives in the
        // app's own directory), so a media name ending in .fioup is still a
        // media name. Checking the opposite of what I first assumed.
        let ledgerish = format!("{}.fioup", media);
        prop_assert!(!skip_name(&ledgerish), "{} is not a sidecar of ours", ledgerish);
    }
}

// ---------- the CSV export is a printer, so test it as one ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Export, then parse with a reference CSV reader: every field comes back
    /// exactly as it went in, whatever quotes, commas or newlines it holds.
    #[test]
    fn exported_csv_round_trips_fields(
        title in ".{0,30}",
        source in ".{0,30}",
        err in ".{0,20}",
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fiotransfer::history::Store::open(dir.path(), dir.path()).expect("store");
        let t = fiotransfer::history::Transfer {
            id: "t1".into(),
            kind: "upload".into(),
            title: title.clone(),
            source: source.clone(),
            dest: "dest".into(),
            status: "done".into(),
            error: err.clone(),
            created_at: 1,
            started_at: 2,
            finished_at: 3,
            total_bytes: 10,
            done_bytes: 10,
            files_total: 1,
            files_done: 1,
            ..Default::default()
        };
        let f = fiotransfer::history::File {
            transfer_id: "t1".into(),
            idx: 0,
            rel_path: "a.bin".into(),
            local_path: "/tmp/a.bin".into(),
            size: 10,
            status: "done".into(),
            ..Default::default()
        };
        store.start(&t, &[f]).expect("start");
        let out = dir.path().join("out.csv");
        store.export_csv(&out).expect("export");
        let text = std::fs::read_to_string(&out).expect("read");
        let rows = parse_csv(&text);
        prop_assert_eq!(rows[0][0].as_str(), "transfer_id", "header");
        prop_assert_eq!(rows[1][2].as_str(), title.as_str(), "title survives quoting");
        prop_assert_eq!(rows[1][7].as_str(), source.as_str(), "source survives quoting");
        prop_assert_eq!(rows[1][13].as_str(), err.as_str(), "error survives quoting");
    }
}

/// A CSV reader that only knows RFC 4180 — the model the export is checked
/// against, deliberately independent of the writer.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            '\r' => {}
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}
