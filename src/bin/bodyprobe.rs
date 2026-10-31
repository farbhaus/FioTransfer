
//! A body-path probe: how fast can this machine PUT a file part over loopback
//! with different request-body strategies? Measurement harness, not shipped.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use futures::Stream;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut base = "http://127.0.0.1:18081".to_string();
    let mut file = "/tmp/fio-bench/data/bench-00.bin".to_string();
    let mut part_mb = 8usize;
    let mut streams = 16usize;
    let mut mode = "fresh".to_string();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-base" => { base = argv[i+1].clone(); i += 2 }
            "-file" => { file = argv[i+1].clone(); i += 2 }
            "-part" => { part_mb = argv[i+1].parse().unwrap(); i += 2 }
            "-streams" => { streams = argv[i+1].parse().unwrap(); i += 2 }
            "-mode" => { mode = argv[i+1].clone(); i += 2 }
            _ => i += 1,
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async move {
        let size = std::fs::metadata(&file).unwrap().len() as i64;
        let part = (part_mb as i64) << 20;
        let parts: Vec<(i64, i64)> = (0..((size + part - 1) / part)).map(|n| (n * part, ((size - n * part).min(part)))).collect();
        let fh = Arc::new(std::fs::File::open(&file).unwrap());
        let sent = Arc::new(AtomicI64::new(0));
        let inflight = Arc::new(tokio::sync::Semaphore::new(streams));
        let engine_limiter = Arc::new(fiotransfer::limiter::Limiter::new(streams));
        let started = Instant::now();
        let mut tasks = Vec::new();

        for (n, (off, len)) in parts.iter().cloned().enumerate() {
            let use_engine_limiter = mode == "limit";
            let permit = if use_engine_limiter { None } else { Some(inflight.clone().acquire_owned().await.unwrap()) };
            let url = format!("{}/s3/probe/{}", base, n);
            let fh = fh.clone();
            let sent = sent.clone();
            let mode = mode.clone();
            let lim = engine_limiter.clone();
            let cx = tokio_util::sync::CancellationToken::new();
            tasks.push(tokio::spawn(async move {
                let _p = permit;
                let _lp = if use_engine_limiter { Some(lim.acquire(&cx).await.unwrap()) } else { None };
                let body = match mode.as_str() {
                    "engine" | "fresh" if mode == "engine" => Body::Fresh(fh, off, len),
                    "pool" => Body::Pooled(Arc::new(pool::Pool::new(1 << 20, streams)), fh, off, len),
                    "engine" | "limit" => Body::Fresh(fh, off, len),
                    "whole" => {
                        let mut buf = vec![0u8; len as usize];
                        fiotransfer_core::fileio::read_exact_at(&fh, &mut buf, off as u64).unwrap();
                        Body::Bytes(Bytes::from(buf))
                    }
                    _ => Body::Fresh(fh, off, len),
                };
                let s = sent.clone();
                let stream = body.into_stream(s);
                let client = match mode.as_str() {
                    "engine" => fiotransfer::netx::data_client().clone(),
                    _ => reqwest::Client::builder().pool_max_idle_per_host(64).build().unwrap(),
                };
                let req = client.put(&url).body(reqwest::Body::wrap_stream(stream)).header("Content-Type", "application/octet-stream");
                let resp = req.send().await.unwrap();
                assert_eq!(resp.status().as_u16(), 200);
            }));
        }
        for t in tasks { t.await.unwrap(); }
        let secs = started.elapsed().as_secs_f64();
        println!("mode={} part={}MiB streams={} => {:.0} MB/s ({:.3}s, {} B)", mode, part_mb, streams, sent.load(Ordering::Relaxed) as f64 / secs / 1e6, secs, sent.load(Ordering::Relaxed));
    });
}

enum Body { Fresh(Arc<std::fs::File>, i64, i64), Pooled(Arc<pool::Pool>, Arc<std::fs::File>, i64, i64), Bytes(Bytes) }

impl Body {
    fn into_stream(self, sent: Arc<AtomicI64>) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
        match self {
            Body::Bytes(b) => { sent.fetch_add(b.len() as i64, Ordering::Relaxed); Box::pin(futures::stream::once(async move { Ok(b) })) }
            Body::Fresh(fh, off, len) => Box::pin(FreshStream { fh, pos: off, left: len, sent }),
            Body::Pooled(pool, fh, off, len) => Box::pin(PooledStream { pool, fh, pos: off, left: len, sent }),
        }
    }
}
use std::pin::Pin;

struct FreshStream { fh: Arc<std::fs::File>, pos: i64, left: i64, sent: Arc<AtomicI64> }
impl Stream for FreshStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let me = &mut *self;
        if me.left <= 0 { return std::task::Poll::Ready(None); }
        let want = (me.left as usize).min(1 << 20);
        let mut buf = vec![0u8; want];
        let got = fiotransfer_core::fileio::read_at(&me.fh, &mut buf, me.pos as u64).unwrap();
        buf.truncate(got);
        me.pos += got as i64; me.left -= got as i64;
        me.sent.fetch_add(got as i64, Ordering::Relaxed);
        std::task::Poll::Ready(Some(Ok(Bytes::from(buf))))
    }
}

struct PooledStream { pool: Arc<pool::Pool>, fh: Arc<std::fs::File>, pos: i64, left: i64, sent: Arc<AtomicI64> }
impl Stream for PooledStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let me = &mut *self;
        if me.left <= 0 { return std::task::Poll::Ready(None); }
        let want = (me.left as usize).min(me.pool.buf_size());
        let mut b = me.pool.take(want);
        let got = fiotransfer_core::fileio::read_at(&me.fh, b.as_mut_slice(), me.pos as u64).unwrap();
        b.set_len(got);
        me.pos += got as i64; me.left -= got as i64;
        me.sent.fetch_add(got as i64, Ordering::Relaxed);
        std::task::Poll::Ready(Some(Ok(b.into_bytes())))
    }
}

mod pool {
    use super::*;
    pub struct Pool { size: usize, idle: Mutex<Vec<Vec<u8>>> }
    impl Pool {
        pub fn new(size: usize, _cap: usize) -> Self { Pool { size, idle: Mutex::new(Vec::new()) } }
        pub fn buf_size(&self) -> usize { self.size }
        pub fn take(&self, want: usize) -> Buf {
            let mut g = self.idle.lock().unwrap();
            let v = match g.pop() { Some(v) => v, None => Vec::with_capacity(self.size) };
            Buf { data: Some(v), len: want }
        }
        fn give(&self, mut v: Vec<u8>) { v.clear(); let mut g = self.idle.lock().unwrap(); if g.len() < 64 { g.push(v) } }
    }
    pub struct Buf { data: Option<Vec<u8>>, len: usize }
    impl Buf {
        pub fn as_mut_slice(&mut self) -> &mut [u8] {
            let v = self.data.as_mut().unwrap();
            if v.len() < self.len {
                v.resize(self.len, 0);
            }
            &mut v[..self.len]
        }
        pub fn set_len(&mut self, n: usize) { self.len = n; }
        pub fn into_bytes(mut self) -> Bytes {
            let v = self.data.take().unwrap();
            let len = self.len;
            Bytes::from_owner(Owner { pool: None, data: Some(v) }).slice(0..len)
        }
    }
    impl Drop for Buf {
        fn drop(&mut self) { if let Some(v) = self.data.take() {} }
    }
    pub struct Owner { pool: Option<Arc<Pool>>, data: Option<Vec<u8>> }
    impl AsRef<[u8]> for Owner { fn as_ref(&self) -> &[u8] { self.data.as_deref().unwrap_or(&[]) } }
    impl Drop for Owner { fn drop(&mut self) {} }
}
