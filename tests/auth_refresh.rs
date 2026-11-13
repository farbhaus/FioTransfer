//! Refreshing the Adobe session when several calls need it at once.
//!
//! Adobe rotates refresh tokens: once one is spent, a second refresh with it is
//! refused, and a refused refresh signs the app out. The Go app held its lock
//! across the refresh, so concurrent callers waited for one refresh and used
//! its token. The port let every caller refresh on its own, so a burst of API
//! calls near the token's expiry signed the user out (found 2026-11-13).
//!
//! The stub is IMS: it answers a refresh with the current refresh token by
//! issuing the next one, and refuses any other with 400 `invalid_grant`.
//! `HOME` moves to a scratch folder and the Keychain is off, so the session is
//! the 0600 file there.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fiotransfer::auth::AuthManager;
use tokio_util::sync::CancellationToken;

/// Starts the rotating IMS stub and points the app at it.
async fn rotating_ims() -> Arc<AtomicUsize> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::env::set_var("FIO_IMS_BASE", format!("http://{}", l.local_addr().unwrap()));
    let current = Arc::new(Mutex::new("r1".to_string()));
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let (cur, n) = (current.clone(), n.clone());
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match c.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(k) => req.extend_from_slice(&buf[..k]),
                    }
                    let s = String::from_utf8_lossy(&req);
                    if let Some(i) = s.find("\r\n\r\n") {
                        let len: usize = s
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                            .unwrap_or(0);
                        if req.len() >= i + 4 + len {
                            break;
                        }
                    }
                }
                let s = String::from_utf8_lossy(&req).to_string();
                let body = s.split("\r\n\r\n").nth(1).unwrap_or("").replace("%5F", "_");
                let sent = body.split('&').find_map(|kv| kv.strip_prefix("refresh_token=")).unwrap_or("").to_string();
                n.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(150)).await; // IMS takes a moment
                let (status, out) = {
                    let mut g = cur.lock().unwrap();
                    if sent == *g {
                        let next = format!("r{}", g[1..].parse::<u32>().unwrap() + 1);
                        *g = next.clone();
                        ("200 OK", format!(r#"{{"access_token":"a-{next}","refresh_token":"{next}","expires_in":3600}}"#))
                    } else {
                        ("400 Bad Request", r#"{"error":"invalid_grant"}"#.to_string())
                    }
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
                let _ = c.write_all(resp.as_bytes()).await;
                let _ = c.shutdown().await;
            });
        }
    });
    calls
}

/// A session whose access token expires in a minute: inside the refresh margin.
fn expiring_session() -> Arc<AuthManager> {
    let home = tempfile::tempdir().unwrap().keep();
    std::env::set_var("HOME", &home);
    std::env::set_var("FIO_NO_KEYCHAIN", "1");
    let dir = home.join("Library/Application Support/FioTransfer");
    std::fs::create_dir_all(&dir).unwrap();
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 60;
    std::fs::write(dir.join("token.json"), format!(r#"{{"access_token":"a1","refresh_token":"r1","expiry":{exp}}}"#)).unwrap();
    let auth = AuthManager::new(|| "client".to_string());
    assert!(auth.logged_in());
    auth
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_share_one_refresh_and_stay_signed_in() {
    let calls = rotating_ims().await;
    let auth = expiring_session();

    let mut hs = Vec::new();
    for _ in 0..8 {
        let a = auth.clone();
        hs.push(tokio::spawn(async move { a.token(&CancellationToken::new()).await }));
    }
    for h in hs {
        assert_eq!(h.await.unwrap().as_deref(), Ok("a-r2"), "every caller gets the refreshed token");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one refresh, not one per caller");
    assert!(auth.logged_in(), "still signed in");

    // 401s that arrive together after the refresh: the token they were for is
    // already replaced, so they don't refresh again either.
    let mut hs = Vec::new();
    for _ in 0..4 {
        let a = auth.clone();
        hs.push(tokio::spawn(async move { a.force_refresh(&CancellationToken::new()).await }));
    }
    for h in hs {
        assert!(h.await.unwrap().is_ok());
    }
    assert!(auth.logged_in(), "concurrent 401s don't sign the user out");
    assert!(calls.load(Ordering::SeqCst) <= 2, "at most one more refresh for the burst of 401s");
}
