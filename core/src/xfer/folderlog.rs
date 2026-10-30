//! The transfer log written next to the media: in the download folder, or in
//! the folder an upload was taken from.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::manager::kind_tag;
use super::{File, Job, Status};
use crate::limiter::{human_bytes, human_duration};

pub const FOLDER_LOG_PREFIX: &str = "FioTransfer ";

/// Whether a name is one of the logs this app writes next to media.
///
/// It has to be the stamp we produce — `FioTransfer Watch 2026-10-29.txt`,
/// `FioTransfer Upload 2026-10-29 143012.txt` — and not anything that merely
/// starts with the product name, or a user's `FioTransfer notes.txt` would be
/// silently left out of an upload.
pub fn is_folder_log(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(FOLDER_LOG_PREFIX) else { return false };
    let Some(stem) = rest.strip_suffix(".txt") else { return false };
    ["Watch ", "Upload ", "Download "].iter().any(|kind| {
        stem.strip_prefix(kind)
            .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ' '))
            .unwrap_or(false)
    })
}

/// log_dirs decides where this job's log goes.
pub fn log_dirs(j: &Job) -> Vec<String> {
    if j.kind == "download" {
        let root = j.log_root.lock().unwrap().clone();
        if !root.is_empty() {
            return vec![root];
        }
        return vec![j.dest.lock().unwrap().clone()];
    }
    let mut seen: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let add = |d: String, seen: &mut Vec<String>, dirs: &mut Vec<String>| {
        if !seen.iter().any(|x| *x == d) {
            seen.push(d.clone());
            dirs.push(d);
        }
    };
    let paths = j.paths.lock().unwrap().clone();
    for p in &paths {
        let mut d = dir_of(p);
        if let Ok(md) = std::fs::metadata(p) {
            if md.is_dir() {
                d = p.clone();
            }
        }
        add(d, &mut seen, &mut dirs);
    }
    if dirs.is_empty() {
        // retries without original paths: use the files' folders
        for f in j.files() {
            add(dir_of(&f.local_path()), &mut seen, &mut dirs);
        }
        if dirs.len() > 1 {
            dirs = vec![common_dir(&dirs)];
        }
    }
    dirs
}

fn common_dir(dirs: &[String]) -> String {
    let mut c = dirs[0].clone();
    for d in &dirs[1..] {
        while c != "/" && !format!("{}/", d).starts_with(&format!("{}/", c)) {
            c = dir_of(&c);
        }
    }
    c
}

/// write_folder_log (re)writes the log file. Failures are recorded in the app log.
pub fn write_folder_log(j: &Job, last: bool) {
    let Some(m) = j.mgr() else { return };
    if !(m.settings)().folder_logs() {
        return;
    }
    if j.watch_log.load(Ordering::Relaxed) && !last {
        return; // watch folders append each batch once it's finished
    }
    let dirs = log_dirs(j);
    let body = folder_log_text(j, &dirs);
    if j.watch_log.load(Ordering::Relaxed) {
        // Watch folders get one log per day; each finished batch is appended.
        let name = format!("{}Watch {}.txt", FOLDER_LOG_PREFIX, stamp(SystemTime::now(), "-", true));
        for d in &dirs {
            let p = Path::new(d).join(&name);
            let res = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .and_then(|mut fh| std::io::Write::write_all(&mut fh, format!("{}\n\n", body).as_bytes()));
            if let Err(e) = res {
                m.hist.log(&format!("WATCH    note     [{}] could not write log to {}: {}", j.id, d, e));
            }
        }
        return;
    }
    let name = {
        let mut g = j.log_name.lock().unwrap();
        if g.is_empty() {
            let verb = if j.kind == "upload" { "Upload" } else { "Download" };
            let created = j.state.lock().unwrap().created;
            *g = format!("{}{} {}.txt", FOLDER_LOG_PREFIX, verb, stamp(created, " ", false));
        }
        g.clone()
    };
    for d in &dirs {
        let p = Path::new(d).join(&name);
        if let Err(e) = std::fs::write(&p, body.as_bytes()) {
            m.hist.log(&format!("{} note     [{}] could not write log to {}: {}", kind_tag(&j.kind), j.id, d, e));
        }
    }
}

/// folder_log_text renders the log; dirs is where an upload was taken from.
pub fn folder_log_text(j: &Job, dirs: &[String]) -> String {
    let rec = j.record();
    let files = j.files();
    let mut b = String::new();
    macro_rules! w {
        ($($a:tt)*) => { b.push_str(&format!($($a)*)) };
    }

    let verb = if j.kind == "upload" { "Upload" } else { "Download" };
    let status = match rec.status.as_str() {
        "running" | "resolving" | "queued" => "In progress",
        "done" => "Completed",
        "partial" => "Completed with errors",
        "failed" => "Failed",
        "canceled" => "Canceled",
        other => other,
    };
    w!("FioTransfer {}\n", verb);
    w!("{}\n\n", "=".repeat(60));
    w!("Name         {}\n", rec.title);
    w!("Status       {}\n", status);
    if j.kind == "upload" {
        w!("From         {}\n", dirs.join(", "));
        w!("To           Frame.io / {}\n", rec.dest);
        if !rec.link.is_empty() {
            w!("Frame.io     {}\n", rec.link);
        }
    } else if !j.watch_id.lock().unwrap().is_empty() {
        w!("From         Frame.io / {}\n", rec.source);
        if !rec.link.is_empty() {
            w!("Frame.io     {}\n", rec.link);
        }
        w!("To           {}\n", rec.dest);
    } else {
        w!("Link         {}\n", rec.source);
        w!("To           {}\n", rec.dest);
    }
    w!("Started      {}\n", ts(rec.started_at));
    w!("Finished     {}\n", ts(rec.finished_at));
    if rec.finished_at > 0 && rec.started_at > 0 {
        w!("Duration     {}\n", human_duration(Duration::from_millis((rec.finished_at - rec.started_at).max(0) as u64)));
    }
    w!("Files        {} of {}", rec.files_done, rec.files_total);
    if rec.files_failed > 0 {
        w!(" ({} failed)", rec.files_failed);
    }
    w!("\n");
    w!("Size         {}\n", human_bytes(rec.total_bytes));
    if rec.avg_bps > 0.0 {
        w!("Avg speed    {}/s ({:.0} Mbit/s)\n", human_bytes(rec.avg_bps as i64), rec.avg_bps * 8.0 / 1e6);
    }
    if !rec.error.is_empty() {
        w!("Error        {}\n", rec.error);
    }
    w!("Transfer ID  {}\n\n", rec.id);

    let mut rows: Vec<&std::sync::Arc<File>> = files.iter().collect();
    rows.sort_by(|a, b| a.rel_path.cmp(&b.rel_path)); // stable: equal paths keep their order
    w!("{:<10} {:>12}  {}\n", "STATUS", "SIZE", "FILE");
    w!("{}\n", "-".repeat(60));
    for f in rows {
        let (st, e, note) = {
            let g = f.inner.lock().unwrap();
            (g.status, g.err.clone(), g.note.clone())
        };
        let url = f.view_url();
        let label = match st {
            Status::Done => "OK",
            Status::Skipped => "SKIPPED",
            Status::Failed => "FAILED",
            Status::Canceled => "CANCELED",
            Status::Running => "ACTIVE",
            Status::Queued => "PENDING",
            _ => "",
        };
        w!("{:<10} {:>12}  {}", label, human_bytes(f.size()), f.rel_path);
        if !e.is_empty() {
            w!("  — {}", e);
        } else if !note.is_empty() {
            w!("  — {}", note);
        }
        if !url.is_empty() {
            w!("  {}", url);
        }
        w!("\n");
    }
    b
}

/// The directory a path is in (Go's filepath.Dir): the path up to and
/// including its last separator, cleaned; a path with no separator is in ".".
/// Trailing separators are part of the path, so Dir("a/b/") is "a/b".
pub fn dir_of(p: &str) -> String {
    match p.rfind('/') {
        Some(i) => super::watch::clean(&p[..=i]),
        None => ".".to_string(),
    }
}

fn stamp(t: SystemTime, sep: &str, date_only: bool) -> String {
    let ms = t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let (y, mo, d, h, mi, s) = local_fields(ms / 1000);
    if date_only {
        return format!("{:04}{}{:02}{}{:02}", y, sep, mo, sep, d);
    }
    format!("{:04}-{:02}-{:02} {:02}{:02}{:02}", y, mo, d, h, mi, s)
}

fn ts(ms: i64) -> String {
    if ms == 0 {
        return "—".to_string();
    }
    let (y, mo, d, h, mi, s) = local_fields(ms.div_euclid(1000));
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
}

/// Local wall-clock fields for a Unix time, as Go's time.Format gives them: the
/// log is written in the user's time zone, so the C library's conversion is
/// used (the same one Go's time package calls).
fn local_fields(unix_secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    match fiotransfer_sys::platform::local_time(unix_secs) {
        Some(t) => (t.year, t.month, t.day, t.hour, t.minute, t.second),
        None => (1970, 1, 1, 0, 0, 0),
    }
}
