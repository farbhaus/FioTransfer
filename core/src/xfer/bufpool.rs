
//! A small pool of read buffers, so an upload doesn't allocate (and page-fault)
//! a fresh megabyte for every chunk it sends.
//!
//! A `PooledBuf` hands its `Vec` back to the pool when the last `Bytes` that
//! owns it is dropped — which is after the socket has written it — so the same
//! memory is reused for tens of megabytes and never faults twice.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

pub struct BufPool {
    size: usize,
    idle: Mutex<Vec<Vec<u8>>>,
    made: AtomicUsize,
    reused: AtomicUsize,
}

impl BufPool {
    pub fn new(size: usize) -> Arc<BufPool> {
        Arc::new(BufPool { size, idle: Mutex::new(Vec::new()), made: AtomicUsize::new(0), reused: AtomicUsize::new(0) })
    }

    pub fn buf_size(&self) -> usize {
        self.size
    }

    /// A buffer of `buf_size` bytes, reused when one is free.
    pub fn take(self: &Arc<Self>) -> PooledBuf {
        let got = self.idle.lock().unwrap().pop();
        let data = match got {
            Some(v) => {
                self.reused.fetch_add(1, Ordering::Relaxed);
                v
            }
            None => {
                self.made.fetch_add(1, Ordering::Relaxed);
                vec![0u8; self.size]
            }
        };
        PooledBuf { pool: Arc::clone(self), data: Some(data), len: 0 }
    }

    fn give(&self, v: Vec<u8>) {
        let mut g = self.idle.lock().unwrap();
        if g.len() < 64 {
            g.push(v);
        }
    }

    /// How many buffers were allocated, and how many takes came from the pool.
    pub fn stats(&self) -> (usize, usize) {
        (self.made.load(Ordering::Relaxed), self.reused.load(Ordering::Relaxed))
    }
}

pub struct PooledBuf {
    pool: Arc<BufPool>,
    data: Option<Vec<u8>>,
    len: usize,
}

impl PooledBuf {
    pub fn set_len(&mut self, n: usize) {
        debug_assert!(n <= self.data.as_ref().map(|d| d.len()).unwrap_or(0));
        self.len = n;
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let n = self.len;
        &mut self.data.as_mut().expect("buffer")[..n]
    }

    /// The bytes read so far, as a `Bytes` that keeps this buffer until it is
    /// written and then returns it to the pool.
    pub fn into_bytes(self) -> Bytes {
        Bytes::from_owner(self)
    }
}

impl AsRef<[u8]> for PooledBuf {
    fn as_ref(&self) -> &[u8] {
        &self.data.as_ref().expect("buffer")[..self.len]
    }
}

impl Drop for PooledBuf {
    fn drop(&mut self) {
        if let Some(v) = self.data.take() {
            self.pool.give(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_comes_back_after_the_bytes_are_dropped() {
        let pool = BufPool::new(64);
        {
            let mut b = pool.take();
            b.set_len(10);
            b.as_mut_slice().copy_from_slice(&[1u8; 10]);
            let bytes = b.into_bytes();
            assert_eq!(bytes.len(), 10);
            assert_eq!(pool.stats(), (1, 0));
        }
        // The Bytes is gone, so the buffer is back in the pool.
        let mut b2 = pool.take();
        b2.set_len(4);
        assert_eq!(pool.stats(), (1, 1), "the second take reuses the first buffer");
    }

    #[test]
    fn buffers_are_not_shared_while_in_flight() {
        let pool = BufPool::new(8);
        let mut a = pool.take();
        a.set_len(8);
        let mut b = pool.take();
        b.set_len(8);
        assert_eq!(pool.stats(), (2, 0), "two live buffers are two allocations");
        a.as_mut_slice()[0] = 7;
        assert_eq!(b.as_mut_slice()[0], 0);
    }
}
