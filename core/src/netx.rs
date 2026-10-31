
//! HTTP clients tuned for bulk transfer, and the retry timing shared by
//! everything that talks to Frame.io.

use rand::RngExt;
use std::sync::OnceLock;
use std::time::Duration;

/// Used for presigned S3/CDN transfers. It forces HTTP/1.1 so that N parallel
/// streams really are N TCP connections (HTTP/2 would multiplex them onto one
/// connection and cap throughput), and uses large socket buffers.
pub fn data_client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .http1_only()
            .pool_max_idle_per_host(256)
            .pool_idle_timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(20))
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .read_timeout(Duration::from_secs(120)) // stall detection is the callers' job
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .build()
            .expect("data client")
    })
}

/// Used for JSON API calls.
pub fn api_client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("api client")
    })
}

/// The wait before retry number `attempt` (counting from 0): exponential from
/// 500ms, capped at 15s, with ±25% jitter so streams that failed together don't
/// all retry at the same moment.
pub fn backoff(attempt: u32) -> Duration {
    let shift = attempt.min(5);
    let base = 500u64.saturating_mul(1u64 << shift).min(15_000);
    let d = base * 3 / 4;
    let jitter = rand::rng().random_range(0..(base / 2 + 1));
    Duration::from_millis(d + jitter)
}

/// Waits for `d`, or until the token is cancelled.
pub async fn sleep(cx: &tokio_util::sync::CancellationToken, d: Duration) -> Result<(), Cancelled> {
    tokio::select! {
        _ = tokio::time::sleep(d) => Ok(()),
        _ = cx.cancelled() => Err(Cancelled),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "context canceled")
    }
}
impl std::error::Error for Cancelled {}

// ---- Errors shared across the crates ----

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub body: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame.io API error {}: {}", self.status, self.body)
    }
}
impl std::error::Error for ApiError {}

pub fn trim_body(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b).trim().to_string();
    if s.chars().count() > 400 {
        let mut out: String = s.chars().take(400).collect();
        out.push('…');
        out
    } else {
        s
    }
}

/// Renders an error the way the Go app's `err.Error()` does, for the UI.
pub fn err_text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}
