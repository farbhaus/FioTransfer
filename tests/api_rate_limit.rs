//! Frame.io's per-route limits hold when many calls want the same route at once.
//!
//! Go's `rate.Limiter.Wait` reserves a slot before it sleeps, so callers in a
//! queue leave one interval apart. The port computed a wait without reserving
//! anything: 20 concurrent status checks on the 5/s route went out within a
//! quarter of a second (2026-11-13), which Frame.io answers with 429s.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fiotransfer::api::Client;
use fiotransfer::auth::AuthManager;
use tokio_util::sync::CancellationToken;

const ACCT: &str = "0123456789abcdef0123456789abcdef";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_queue_on_the_route_limit() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let t0 = Instant::now();
    let seen: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match c.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                log.lock().unwrap().push(t0.elapsed());
                let body = r#"{"data":{"upload_complete":true}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = c.write_all(resp.as_bytes()).await;
                let _ = c.shutdown().await;
            });
        }
    });
    let mut c = Client::new(AuthManager::new_static("t"));
    c.base_url = base;
    let c = Arc::new(c);
    let mut hs = Vec::new();
    for i in 0..12 {
        let c = c.clone();
        hs.push(tokio::spawn(async move {
            c.file_status(&CancellationToken::new(), ACCT, &format!("{:032x}", i)).await.unwrap();
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
    let mut t = seen.lock().unwrap().clone();
    t.sort();
    // 5/s with a burst of 5, run at 90%: 5 at once, then one every ~222 ms.
    let first_second = t.iter().filter(|d| **d < Duration::from_secs(1)).count();
    assert!(first_second <= 9, "{first_second} calls in the first second on a 5/s route: {t:?}");
    assert!(t[11] >= Duration::from_millis(1400), "the 12th call waited its turn: {t:?}");
}
