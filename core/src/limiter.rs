
//! Caps the number of concurrent HTTP streams across all transfers, and
//! measures throughput.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::netx::Cancelled;

/// A resizable counting limiter: `acquire` waits while `limit` streams are
/// already running. The limit can be changed while transfers are running.
pub struct Limiter {
    state: Mutex<(usize, usize)>, // (limit, active)
    notify: Notify,
}

pub struct Permit<'a>(&'a Limiter);

impl Limiter {
    pub fn new(n: usize) -> Self {
        Limiter {
            state: Mutex::new((n.max(1), 0)),
            notify: Notify::new(),
        }
    }

    pub fn set_limit(&self, n: usize) {
        let n = n.max(1);
        self.state.lock().unwrap().0 = n;
        self.notify.notify_waiters();
    }

    pub fn limit(&self) -> usize {
        self.state.lock().unwrap().0
    }

    pub fn active(&self) -> usize {
        self.state.lock().unwrap().1
    }

    pub async fn acquire(&self, cx: &CancellationToken) -> Result<Permit<'_>, Cancelled> {
        loop {
            // Register for a wake-up *before* looking at the count. Waking
            // waiters is not a broadcast to future listeners: a release that
            // lands between reading the count and enrolling here would
            // otherwise be lost, and the task would sleep until the next
            // release — or for ever if there is none.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut st = self.state.lock().unwrap();
                if st.1 < st.0 {
                    st.1 += 1;
                    return Ok(Permit(self));
                }
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = cx.cancelled() => return Err(Cancelled),
            }
            if cx.is_cancelled() {
                return Err(Cancelled);
            }
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().1 -= 1;
        self.notify.notify_waiters();
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Throughput with an exponentially-weighted moving average.
pub struct Meter {
    total: AtomicI64,
    inner: Mutex<MeterInner>,
}

struct MeterInner {
    last_at: Option<Instant>,
    last_tot: i64,
    rate: f64,
    peak: f64,
    smoothed: bool,
}

impl Default for Meter {
    fn default() -> Self {
        Meter {
            total: AtomicI64::new(0),
            inner: Mutex::new(MeterInner {
                last_at: None,
                last_tot: 0,
                rate: 0.0,
                peak: 0.0,
                smoothed: false,
            }),
        }
    }
}

impl Meter {
    pub fn add(&self, n: i64) {
        self.total.fetch_add(n, Ordering::Relaxed);
    }

    pub fn total(&self) -> i64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Updates and returns the current rate; call periodically (~2/sec).
    pub fn sample(&self) -> f64 {
        let mut m = self.inner.lock().unwrap();
        let now = Instant::now();
        let tot = self.total.load(Ordering::Relaxed);
        let Some(last_at) = m.last_at else {
            m.last_at = Some(now);
            m.last_tot = tot;
            return 0.0;
        };
        let dt = now.duration_since(last_at).as_secs_f64();
        if dt < 0.2 {
            return m.rate;
        }
        let mut inst = (tot - m.last_tot) as f64 / dt;
        if inst < 0.0 {
            inst = 0.0;
        }
        if !m.smoothed {
            m.rate = inst;
            m.smoothed = true;
        } else {
            m.rate = 0.6 * m.rate + 0.4 * inst;
        }
        if m.rate > m.peak {
            m.peak = m.rate;
        }
        m.last_at = Some(now);
        m.last_tot = tot;
        m.rate
    }

    pub fn rate(&self) -> f64 {
        self.inner.lock().unwrap().rate
    }

    pub fn peak(&self) -> f64 {
        self.inner.lock().unwrap().peak
    }
}

/// Bytes as the UI words them (`fmtBytes` in the frontend, `humanBytes` in Go).
pub fn human_bytes(n: i64) -> String {
    if n <= 0 {
        return "0 B".to_string();
    }
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 || v >= 100.0 {
        format!("{:.0} {}", v, units[i])
    } else if v >= 10.0 {
        format!("{:.1} {}", v, units[i])
    } else {
        format!("{:.2} {}", v, units[i])
    }
}

pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs_f64().round() as u64;
    let (h, m) = (s / 3600, s % 3600 / 60);
    if h > 0 {
        format!("{}h {:02}m", h, m)
    } else if m > 0 {
        format!("{}m {:02}s", m, s % 60)
    } else {
        format!("{}s", s)
    }
}
