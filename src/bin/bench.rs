
//! Measures the Rust transfer engine's throughput against the same local
//! stand-in for Frame.io that the Go bench uses (work/bench/fake_frameio.py),
//! so the two numbers are comparable.
//!
//!   fio-bench -base http://127.0.0.1:18080 -files 8 -mb 256 -streams 16

use std::sync::Arc;
use std::time::{Duration, Instant};

use fiotransfer::api::Client;
use fiotransfer::auth::AuthManager;
use fiotransfer::config;
use fiotransfer::history;
use fiotransfer::xfer::{self, manager as xfer_manager, Status, UploadOptions};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut base = "http://127.0.0.1:18080".to_string();
    let mut dir = "/tmp/fio-bench/data".to_string();
    let mut files = 8usize;
    let mut mb = 64usize;
    let mut streams = 16i32;
    let mut mode = "upload".to_string();
    let mut out = "/tmp/fio-bench/dl".to_string();
    let mut link = "https://next.frame.io/share/11111111-2222-4333-8444-555555555555".to_string();
    let mut seg = 64i32;
    let mut gql = String::new();
    let mut cancel_after_mb: i64 = 0;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-base" => {
                base = argv.get(i + 1).cloned().unwrap_or(base);
                i += 2;
            }
            "-dir" => {
                dir = argv.get(i + 1).cloned().unwrap_or(dir);
                i += 2;
            }
            "-files" => {
                files = argv.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(files);
                i += 2;
            }
            "-mb" => {
                mb = argv.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(mb);
                i += 2;
            }
            "-cancel-after-mb" => {
                cancel_after_mb = argv.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0);
                i += 2;
            }
            "-mode" => { mode = argv.get(i + 1).cloned().unwrap_or(mode); i += 2; }
            "-out" => { out = argv.get(i + 1).cloned().unwrap_or(out); i += 2; }
            "-link" => { link = argv.get(i + 1).cloned().unwrap_or(link); i += 2; }
            "-seg" => { seg = argv.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(seg); i += 2; }
            "-gql" => { gql = argv.get(i + 1).cloned().unwrap_or(gql); i += 2; }
            "-streams" => {
                streams = argv.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(streams);
                i += 2;
            }
            _ => i += 1,
        }
    }

    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Ok(n) = std::env::var("FIO_BENCH_THREADS") {
        if let Ok(n) = n.parse::<usize>() {
            builder.worker_threads(n);
        }
    }
    let rt = builder.build().expect("runtime");
    let _g = rt.enter();

    let mut settings = config::Settings::default();
    settings.client_id = "bench".into();
    settings.streams = streams;
    settings.segment_mb = 64;
    settings.download_dir = "/tmp/fio-bench/dl".into();

    let paths = match make_files(&dir, files, mb) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bench: {}", e);
            std::process::exit(1);
        }
    };

    if !gql.is_empty() {
        std::env::set_var("FIO_GRAPHQL_URL", &gql);
    }
    let auth = AuthManager::new_static("bench-token");
    let mut client = Client::new(auth);
    // Point the client at the fake server.
    client.base_url = base.clone();
    let client = Arc::new(client);

    let h = match history::Store::open(
        std::path::Path::new("/tmp/fio-bench/db"),
        std::path::Path::new("/tmp/fio-bench/logs"),
    ) {
        Ok(h) => Arc::new(h),
        Err(e) => {
            eprintln!("bench: {}", e);
            std::process::exit(1);
        }
    };
    let cell = Arc::new(std::sync::Mutex::new(settings.clone()));
    // The settle and verify timings are shortened so the number is the transfer
    // itself, exactly as the Go bench does.
    let tune = xfer_manager::Tune {
        upload_settle: Duration::from_millis(0),
        verify_delays: vec![Duration::from_millis(1)],
        change_check: Duration::from_secs(1),
        stall_limit: Duration::from_secs(600),
    };
    let m = xfer_manager::new_tuned(client, h, Box::new(move || cell.lock().unwrap().clone()), tune);

    let started = Instant::now();
    let started_job = if mode == "get" {
        let _ = std::fs::create_dir_all(&out);
        let mut s2 = settings.clone();
        s2.segment_mb = seg;
        m.set_streams(streams);
        let _ = s2;
        m.start_download(xfer::DownloadOptions { link: link.clone(), passphrase: String::new(), dest_dir: out.clone(), subfolder: false })
    } else {
        m.start_upload(UploadOptions {
            paths,
            account_id: "acct-1".into(),
            folder_id: "folder-1".into(),
            dest_label: "bench".into(),
        })
    };
    let j = match started_job {
        Ok(j) => j,
        Err(e) => {
            eprintln!("bench: {}", e);
            std::process::exit(1);
        }
    };
    let mut view = None;
    let mut last = String::new();
    loop {
        let tp = Instant::now();
        let snap = m.snapshot();
        if std::env::var("FIO_TRACE").is_ok() {
            eprintln!("[bench] poll +{:.0}ms took {:.1}ms jobs={}", started.elapsed().as_secs_f64() * 1000.0, tp.elapsed().as_secs_f64()*1000.0, snap.jobs.len());
        }
        if let Some(v) = snap.jobs.iter().find(|v| v.id == j.id) {
            let now = format!("{:?} {} done={}/{} files={}/{} streams={}", v.status, v.phase, v.done, v.total, v.files_done, v.files_total, snap.active_streams);
            if now != last {
                eprintln!("[bench] +{:.0}ms {}", started.elapsed().as_secs_f64() * 1000.0, now);
                last = now;
            }
            if cancel_after_mb > 0 && v.done >= cancel_after_mb * 1_000_000 {
                eprintln!("[bench] cancelling at {} bytes, as asked", v.done);
                m.cancel(&j.id);
                cancel_after_mb = 0;
            }
            if matches!(v.status, Status::Done | Status::Partial | Status::Failed | Status::Canceled) {
                view = Some(v.clone());
                break;
            }
        } else {
            eprintln!("[bench] job {} not in the list yet", j.id);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let secs = started.elapsed().as_secs_f64();
    let v = view.expect("a finished job");
    println!(
        "RESULT {}",
        serde_json::json!({
            "impl": "rust",
            "mode": mode,
            "files": v.files_total,
            "failed": v.files_failed,
            "bytes": v.done,
            "seconds": secs,
            "mbps": v.done as f64 / secs / 1e6,
            "streams": streams,
            "status": v.status.as_str(),
            "error": v.error,
        })
    );
    if v.status != Status::Done {
        std::process::exit(1);
    }
}

fn make_files(dir: &str, n: usize, mb: usize) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut paths = Vec::new();
    let buf = vec![0u8; 1 << 20];
    for i in 0..n {
        let p = format!("{}/bench-{:02}.bin", dir, i);
        let want = (mb << 20) as u64;
        let ok = std::fs::metadata(&p).map(|m| m.len() == want).unwrap_or(false);
        if !ok {
            use std::io::Write;
            let mut f = std::fs::File::create(&p).map_err(|e| e.to_string())?;
            for _ in 0..mb {
                f.write_all(&buf).map_err(|e| e.to_string())?;
            }
        }
        paths.push(p);
    }
    Ok(paths)
}
