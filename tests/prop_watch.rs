//! Property tests for the watch folders' public surface: the direction table, the
//! folder log (name and body), and where the log goes.
//!
//! The private decision machinery (the walk, temp names, the batch key, the
//! backoff, the remote dedupe, the settle/known-row rules, the remote listing) is
//! tested next to it, in `src/xfer/watch.rs`'s `mod props`:
//!   cargo test --lib --offline xfer::watch::props
//!
//! This file:
//!   cargo test --test prop_watch --offline

use std::sync::Arc;

use proptest::collection::vec as pvec;
use proptest::prelude::*;

use fiotransfer::api::Client;
use fiotransfer::auth::AuthManager;
use fiotransfer::config::Settings;
use fiotransfer::history;
use fiotransfer::history::watch::{
    valid_direction, valid_on_change, Watch, DIR_DOWNLOAD, DIR_MIRROR, DIR_UPLOAD, ON_CHANGE_KEEP,
    ON_CHANGE_OVERWRITE,
};
use fiotransfer::limiter::human_bytes;
use fiotransfer::xfer::folderlog::{
    dir_of, folder_log_text, is_folder_log, log_dirs, write_folder_log, FOLDER_LOG_PREFIX,
};
use fiotransfer::xfer::manager::{new_manager, skip_name};
use fiotransfer::xfer::{File, Manager, Status};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

/// A manager over a throwaway history store; no network, no user profile.
fn manager(rt: &tokio::runtime::Runtime, folder_logs: bool) -> (Arc<Manager>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(history::Store::open(dir.path(), dir.path()).expect("store"));
    let api = Arc::new(Client::new(AuthManager::new_static("test-token")));
    let mut s = Settings::default();
    s.no_folder_logs = !folder_logs;
    let _g = rt.enter();
    let m = new_manager(api, store, Box::new(move || s.clone()));
    (m, dir)
}

fn status_of(k: usize) -> Status {
    const STATUSES: [Status; 6] = [
        Status::Done,
        Status::Skipped,
        Status::Failed,
        Status::Canceled,
        Status::Running,
        Status::Queued,
    ];
    STATUSES[k % STATUSES.len()]
}

fn label_of(s: Status) -> &'static str {
    match s {
        Status::Done => "OK",
        Status::Skipped => "SKIPPED",
        Status::Failed => "FAILED",
        Status::Canceled => "CANCELED",
        Status::Running => "ACTIVE",
        Status::Queued => "PENDING",
        _ => "",
    }
}

// ---------- the direction table ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// valid_direction accepts exactly the three documented names, and nothing
    /// else — including the empty string a default Watch carries.
    #[test]
    fn valid_direction_is_exactly_the_documented_set(d in ".{0,10}") {
        let want = matches!(d.as_str(), "upload" | "download" | "mirror");
        prop_assert_eq!(valid_direction(&d), want, "direction {:?}", d);
    }

    /// uploads()/downloads() agree with the documented table for every direction,
    /// and a watch always takes one side or the other.
    #[test]
    fn uploads_and_downloads_match_the_table(d in ".{0,10}") {
        let w = Watch { direction: d.clone(), ..Watch::default() };
        prop_assert!(
            w.uploads() || w.downloads(),
            "{:?}: a watch takes files from at least one side",
            d
        );
        match d.as_str() {
            DIR_UPLOAD => {
                prop_assert!(w.uploads() && !w.downloads(), "upload: local -> Frame.io only");
            }
            DIR_DOWNLOAD => {
                prop_assert!(!w.uploads() && w.downloads(), "download: Frame.io -> local only");
            }
            DIR_MIRROR => {
                prop_assert!(w.uploads() && w.downloads(), "mirror: both ways");
            }
            // Rejected by add/update before it is stored; it reads as an upload.
            _ => {
                prop_assert!(!valid_direction(&d));
                prop_assert!(w.uploads() && !w.downloads(), "{:?} reads as an upload", d);
            }
        }
    }

    /// The rule for changed files: overwrite or keep, nothing else.
    #[test]
    fn valid_on_change_is_exactly_the_documented_set(c in ".{0,10}") {
        let want = matches!(c.as_str(), "overwrite" | "keep");
        prop_assert_eq!(valid_on_change(&c), want, "rule {:?}", c);
        prop_assert_eq!(c == ON_CHANGE_OVERWRITE, c == "overwrite");
        prop_assert_eq!(c == ON_CHANGE_KEEP, c == "keep");
    }
}

// ---------- the folder log ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// The log a finished upload writes is named once and rendered once: the text
    /// is the body folder_log_text gives, it lists every file in path order with
    /// its status and size, the header counts agree with the files, and a second
    /// write changes nothing.
    #[test]
    fn upload_log_name_and_body_are_stable(
        title in "[A-Za-z0-9 ]{1,12}",
        rows in pvec(("[a-z]{1,5}(\\.mov)?", 0i64..5_000_000, 0usize..6, "[A-Za-z0-9_-]{0,10}", "[A-Za-z0-9_-]{0,10}"), 0..5),
        urls in pvec(any::<bool>(), 3),
    ) {
        let rt = rt();
        let (m, _h) = manager(&rt, true);
        let dir = tempfile::tempdir().expect("log dir");
        let dir_s = dir.path().to_string_lossy().into_owned();

        let j = m.new_job("upload");
        *j.title.lock().unwrap() = title.clone();
        *j.source.lock().unwrap() = dir_s.clone();
        *j.dest.lock().unwrap() = "Project / Folder".to_string();
        *j.paths.lock().unwrap() = vec![dir_s.clone()];
        // The list is published first: set_files seeds the job's per-status
        // counts from the files it is given (see Job::set_files).
        let mut files: Vec<Arc<File>> = rows
            .iter()
            .enumerate()
            .map(|(i, (rel, size, _, _, _))| Arc::new(File::new(i, rel, rel, &format!("{}/{}", dir_s, rel), *size)))
            .collect();
        for f in &files {
            f.attach(&j);
        }
        j.set_files(files.clone());
        // (path, size, status, error, note, url)
        let mut want: Vec<(String, i64, Status, String, String, String)> = Vec::new();
        for (i, ((rel, size, st, err, note), f)) in rows.iter().zip(files.iter()).enumerate() {
            f.set_status(status_of(*st), "");
            f.set_error(err);
            f.set_note(note);
            let url = if *urls.get(i % urls.len()).unwrap_or(&false) {
                format!("https://next.frame.io/project/x/{}", i)
            } else {
                String::new()
            };
            *f.view_url.lock().unwrap() = url.clone();
            want.push((rel.clone(), *size, status_of(*st), err.clone(), note.clone(), url));
        }
        prop_assert_eq!(log_dirs(&j), vec![dir_s.clone()], "the log goes in the folder the upload came from");

        let body = folder_log_text(&j, &log_dirs(&j));
        write_folder_log(&j, false);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        prop_assert_eq!(names.len(), 1, "one log file, got {:?}", names);
        let name = names[0].clone();
        prop_assert!(name.starts_with(FOLDER_LOG_PREFIX), "name {:?}", name);
        prop_assert!(name.ends_with(".txt"), "name {:?}", name);
        let head = &name[FOLDER_LOG_PREFIX.len()..];
        prop_assert!(head.starts_with("Upload "), "name {:?}", name);
        let stamp = &head["Upload ".len()..head.len() - 4];
        let parts: Vec<&str> = stamp.split(' ').collect();
        prop_assert_eq!(parts.len(), 2, "stamp {:?}", stamp);
        let d: Vec<&str> = parts[0].split('-').collect();
        prop_assert_eq!(d.len(), 3, "stamp {:?}", stamp);
        prop_assert_eq!(d[0].len(), 4);
        prop_assert_eq!(d[1].len(), 2);
        prop_assert_eq!(d[2].len(), 2);
        let t: Vec<char> = parts[1].chars().collect();
        prop_assert_eq!(t.len(), 6, "stamp {:?}", stamp);
        prop_assert!(t.iter().all(|c| c.is_ascii_digit()), "stamp {:?}", stamp);
        // the watcher must never upload its own log
        prop_assert!(is_folder_log(&name), "name {:?}", name);
        prop_assert!(skip_name(&name), "the log would be uploaded back: {:?}", name);

        let text = std::fs::read_to_string(dir.path().join(&name)).expect("read log");
        prop_assert_eq!(&text, &body, "the file holds the rendered body");

        // The body: header, then one row per file in path order.
        prop_assert!(
            text.starts_with(&format!("FioTransfer Upload\n{}\n\n", "=".repeat(60))),
            "header:\n{}",
            text
        );
        prop_assert!(text.contains(&format!("\nName         {}\n", title)), "name:\n{}", text);
        prop_assert!(text.contains("\nStatus       In progress\n"), "status:\n{}", text);
        prop_assert!(text.contains(&format!("\nFrom         {}\n", dir_s)), "from:\n{}", text);
        prop_assert!(
            text.contains("\nTo           Frame.io / Project / Folder\n"),
            "to:\n{}",
            text
        );
        let head = format!("\n{:<10} {:>12}  {}\n", "STATUS", "SIZE", "FILE");
        prop_assert!(text.contains(&head), "row header {:?} missing from:\n{}", head, text);
        let sep = "-".repeat(60);
        let rows_text: Vec<&str> = text
            .lines()
            .skip_while(|l| *l != sep)
            .skip(1)
            .collect();
        prop_assert_eq!(rows_text.len(), want.len(), "one row per file");

        // Rows are sorted by path; the sort is stable, so equal paths keep their
        // order.
        let mut order: Vec<usize> = (0..want.len()).collect();
        order.sort_by(|a, b| want[*a].0.cmp(&want[*b].0));
        for (row, idx) in rows_text.iter().zip(order) {
            let (rel, size, st, err, note, url) = &want[idx];
            let got = parse_row(row);
            prop_assert_eq!(&got[0], label_of(*st), "status of {:?} in row {:?}", rel, row);
            prop_assert_eq!(&got[1], &human_bytes(*size), "size of {:?} in row {:?}", rel, row);
            prop_assert_eq!(&got[2], rel, "path in row {:?}", row);
            // The error wins over the note, and the URL comes after either.
            let says = if !err.is_empty() { err.clone() } else { note.clone() };
            prop_assert_eq!(&got[3], &says, "note of {:?} in row {:?}", rel, row);
            prop_assert_eq!(&got[4], url, "url of {:?} in row {:?}", rel, row);
        }

        // The header's counts are the files', and a second write is a no-op.
        let done = want.iter().filter(|(_, _, s, _, _, _)| matches!(s, Status::Done | Status::Skipped)).count();
        prop_assert!(
            text.contains(&format!("Files        {} of {}", done, want.len())),
            "counts: {:?}",
            text.lines().find(|l| l.starts_with("Files"))
        );
        write_folder_log(&j, false);
        let again = std::fs::read_to_string(dir.path().join(&name)).expect("read log");
        prop_assert_eq!(again, text, "a second write changed the log");
        let count = std::fs::read_dir(dir.path()).expect("read dir").count();
        prop_assert_eq!(count, 1, "a second write made another file");
    }

    /// A watch folder's log is per day and appends one block per finished batch,
    /// and nothing at all is written before the batch is final.
    #[test]
    fn watch_log_appends_one_block_per_batch(
        title in "[A-Za-z0-9 ]{1,8}",
        rows in pvec("[a-z]{1,5}\\.mov", 0..3),
        batches in 1usize..4,
    ) {
        let rt = rt();
        let (m, _h) = manager(&rt, true);
        let dir = tempfile::tempdir().expect("log dir");
        let dir_s = dir.path().to_string_lossy().into_owned();
        let j = m.new_job("download");
        *j.title.lock().unwrap() = title.clone();
        *j.source.lock().unwrap() = "Project / Folder".to_string();
        *j.dest.lock().unwrap() = dir_s.clone();
        *j.log_root.lock().unwrap() = dir_s.clone();
        *j.watch_id.lock().unwrap() = "w1".to_string();
        j.watch_log.store(true, std::sync::atomic::Ordering::Relaxed);
        let files: Vec<Arc<File>> = rows
            .iter()
            .enumerate()
            .map(|(i, rel)| Arc::new(File::new(i, rel, rel, &format!("{}/{}", dir_s, rel), 10)))
            .collect();
        for f in &files {
            f.attach(&j);
        }
        j.set_files(files.clone());
        for f in &files {
            f.set_status(Status::Done, "");
        }

        // Not final yet: a watch logs its batch only once it has finished.
        write_folder_log(&j, false);
        prop_assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 0, "a log was written early");

        let body = folder_log_text(&j, &log_dirs(&j));
        for _ in 0..batches {
            write_folder_log(&j, true);
        }
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        prop_assert_eq!(names.len(), 1, "one log per day, got {:?}", names);
        let name = names[0].clone();
        prop_assert!(name.starts_with(&format!("{}Watch ", FOLDER_LOG_PREFIX)), "name {:?}", name);
        let text = std::fs::read_to_string(dir.path().join(&name)).expect("read");
        let want = format!("{}\n\n", body).repeat(batches);
        prop_assert_eq!(text, want, "each finished batch is one block");
        // The watch log names Frame.io as the source.
        prop_assert!(body.contains("From         Frame.io / Project / Folder"), "body:\n{}", body);
    }
}

/// parse_row splits one log row into [label, size, path], plus the trailing
/// error or URL when there is one. The row is `{:<10} {:>12}  {path}`, then
/// "  — err" or "  <url>".
fn parse_row(line: &str) -> [String; 5] {
    // Columns are padded by character (Go's %-10s and Rust's {:<10} both count
    // runes), and the "—" in an error note is three bytes, so index by character.
    let c: Vec<char> = line.chars().collect();
    assert!(c.len() >= 25, "row {:?} is too short", line);
    let label: String = c[0..10].iter().collect();
    let size: String = c[11..23].iter().collect();
    let mut rest: String = c[25..].iter().collect();

    // "  — err" or "  — note" after the path, then the view URL after either.
    let mut url = String::new();
    if let Some(i) = rest.find("  http") {
        url = rest[i + 2..].to_string();
        rest.truncate(i);
    }
    let mut says = String::new();
    let dash = "  — ";
    if let Some(i) = rest.find(dash) {
        says = rest[i + dash.len()..].to_string();
        rest.truncate(i);
    }
    [label.trim().to_string(), size.trim().to_string(), rest, says, url]
}

// ---------- where the log goes ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// A download's log goes beside the media (log_root, else dest); an upload's
    /// goes in each folder it was taken from, once.
    #[test]
    fn log_dirs_follows_the_job(
        n in 1usize..4,
        as_dirs in pvec(any::<bool>(), 3),
        root in any::<bool>(),
    ) {
        let rt = rt();
        let (m, _h) = manager(&rt, true);
        let mut paths: Vec<String> = Vec::new();
        let mut want: Vec<String> = Vec::new();
        for i in 0..n {
            let d = tempfile::tempdir().expect("dir");
            let ds = d.path().to_string_lossy().into_owned();
            std::fs::write(d.path().join("a.mov"), b"x").expect("file");
            let is_dir = *as_dirs.get(i % as_dirs.len()).unwrap_or(&true);
            let p = if is_dir { ds.clone() } else { format!("{}/a.mov", ds) };
            if !want.contains(&ds) {
                want.push(ds.clone());
            }
            paths.push(p);
            std::mem::forget(d); // keep it for the length of the case
        }
        let j = m.new_job("upload");
        *j.paths.lock().unwrap() = paths.clone();
        prop_assert_eq!(log_dirs(&j), want, "upload paths {:?}", paths);

        let dest = paths[0].clone();
        let root_s = tempfile::tempdir().expect("root");
        let root_p = root_s.path().to_string_lossy().into_owned();
        let d = m.new_job("download");
        *d.dest.lock().unwrap() = dest.clone();
        if root {
            *d.log_root.lock().unwrap() = root_p.clone();
            prop_assert_eq!(log_dirs(&d), vec![root_p.clone()]);
        } else {
            prop_assert_eq!(log_dirs(&d), vec![dest.clone()]);
        }
    }
}

// ---------- dir_of ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// dir_of is the folder a path is in: it never ends with a separator unless it
    /// is the root, it is a prefix of the path, and taking the folder of a file in
    /// a folder gives that folder back.
    #[test]
    fn dir_of_is_the_folder_a_path_is_in(
        dir in "/[A-Za-z0-9]{1,6}(/[A-Za-z0-9]{1,6}){0,2}",
        name in "[A-Za-z0-9]{1,8}\\.[a-z]{2,4}",
    ) {
        let file = format!("{}/{}", dir, name);
        prop_assert_eq!(dir_of(&file), dir.clone(), "dir_of({:?})", file);
        let d = dir_of(&dir);
        prop_assert!(d == "/" || !d.ends_with('/'), "dir_of({:?}) = {:?}", dir, d);
        let under = if d == "/" { dir.starts_with('/') } else { dir == d || dir.starts_with(&format!("{}/", d)) };
        prop_assert!(under, "dir_of({:?}) = {:?} is not a prefix", dir, d);
        // (Of a file in that folder the folder is given back; the folder of the
        // folder itself is one level up, which the prefix rule above covers.)
    }
}
