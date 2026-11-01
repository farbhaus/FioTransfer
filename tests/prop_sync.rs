//! Property tests for `Limiter` (src/limiter.rs): the counting limiter that caps
//! concurrent HTTP streams and lets the limit change while transfers run.
//!
//! The model is a counter — at most `limit()` permits outstanding, each one
//! released exactly once, `active()` back to 0 once the dust settles. Every
//! `acquire` is wrapped in a timeout so a lost wake-up (the classic failure of a
//! `Notify`-based limiter) fails a case instead of hanging the run.
//!
//! Discovery suite. Regression tests for anything it finds belong next to the code.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proptest::collection::vec as pvec;
use proptest::prelude::*;
use tokio_util::sync::CancellationToken;

use fiotransfer::limiter::Limiter;

/// Long enough that no correct wait takes it; short enough to fail a hang.
const HANG: Duration = Duration::from_secs(2);
/// How long a queued waiter is watched before we believe it is queued.
const QUEUED: Duration = Duration::from_millis(50);

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

// ---------------------------------------------------------------- properties

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Never more permits held than `limit()` allows, for random limits and task
    /// counts. The model is a counter incremented under the grant.
    #[test]
    fn never_more_permits_held_than_the_limit(
        limit in 1usize..=8,
        holds in pvec(0usize..=3, 0..=20),
    ) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Arc::new(Limiter::new(limit));
            prop_assert_eq!(limiter.limit(), limit);
            let (held, worst, grants) = (
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicUsize::new(0)),
            );
            let mut tasks = Vec::new();
            for n in holds.iter().copied() {
                let (l, held, worst, grants) =
                    (limiter.clone(), held.clone(), worst.clone(), grants.clone());
                tasks.push(tokio::spawn(async move {
                    let cx = CancellationToken::new();
                    let p = tokio::time::timeout(HANG, l.acquire(&cx))
                        .await
                        .expect("acquire must not hang")
                        .expect("a permit");
                    let now = held.fetch_add(1, Ordering::SeqCst) + 1;
                    worst.fetch_max(now, Ordering::SeqCst);
                    grants.fetch_add(1, Ordering::SeqCst);
                    for _ in 0..n {
                        tokio::task::yield_now().await;
                    }
                    held.fetch_sub(1, Ordering::SeqCst);
                    drop(p);
                }));
            }
            for t in tasks {
                t.await.expect("task");
            }
            prop_assert!(
                worst.load(Ordering::SeqCst) <= limit,
                "{} permits held at once with limit {}",
                worst.load(Ordering::SeqCst),
                limit
            );
            prop_assert_eq!(grants.load(Ordering::SeqCst), holds.len(), "every task got one");
            prop_assert_eq!(limiter.active(), 0, "nobody left holding");
            Ok(())
        });
        checked?;
    }

    /// Every permit is returned: after a storm of grants, drops and limit
    /// changes, `active()` is exactly 0 and the limiter still grants.
    #[test]
    fn every_permit_is_returned(
        start in 0usize..=6,
        changes in pvec(0usize..=6, 0..=4),
        rounds in 1usize..=10,
    ) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Limiter::new(start);
            prop_assert_eq!(limiter.limit(), start.max(1), "a limit of 0 clamps to 1");
            for c in changes.iter().copied() {
                limiter.set_limit(c);
                prop_assert_eq!(limiter.limit(), c.max(1));
            }
            for _ in 0..rounds {
                let cap = limiter.limit();
                let mut held = Vec::new();
                for _ in 0..cap {
                    let cx = CancellationToken::new();
                    let p = tokio::time::timeout(HANG, limiter.acquire(&cx))
                        .await
                        .expect("acquire must not hang")
                        .expect("a permit");
                    held.push(p);
                    prop_assert!(limiter.active() <= cap, "{} > {}", limiter.active(), cap);
                }
                prop_assert_eq!(limiter.active(), cap, "a full house");
                drop(held);
                prop_assert_eq!(limiter.active(), 0, "every permit returned");
            }
            prop_assert_eq!(limiter.active(), 0);
            Ok(())
        });
        checked?;
    }

    /// A waiter cancelled while queued consumes nothing, and the limiter still
    /// works afterwards. The survivors must be woken by the releases.
    #[test]
    fn a_cancelled_waiter_consumes_nothing(
        limit in 1usize..=4,
        waiters in 1usize..=8,
        cancel in pvec(any::<bool>(), 8),
        settle in 0usize..=3,
    ) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Arc::new(Limiter::new(limit));
            let mut held = Vec::new();
            for _ in 0..limit {
                let cx = CancellationToken::new();
                held.push(
                    tokio::time::timeout(HANG, limiter.acquire(&cx))
                        .await
                        .expect("acquire must not hang")
                        .expect("a permit"),
                );
            }
            prop_assert_eq!(limiter.active(), limit, "start full");

            let mut waiting = Vec::new();
            for i in 0..waiters {
                let l = limiter.clone();
                let cancel = cancel[i % cancel.len()];
                let cx = CancellationToken::new();
                let c = cx.clone();
                let h = tokio::spawn(async move {
                    tokio::time::timeout(HANG, l.acquire(&cx))
                        .await
                        .expect("acquire must not hang")
                        .is_ok()
                });
                waiting.push((h, c, cancel));
            }
            for _ in 0..=settle {
                tokio::task::yield_now().await;
            }
            // A full limiter cannot grant, even to someone who just arrived.
            let probe = CancellationToken::new();
            prop_assert!(
                tokio::time::timeout(QUEUED, limiter.acquire(&probe)).await.is_err(),
                "a full limiter must not grant"
            );
            for (_, c, cancel) in &waiting {
                if *cancel {
                    c.cancel();
                }
            }
            let mut survivors = Vec::new();
            for (h, _, cancel) in waiting {
                if cancel {
                    prop_assert!(!h.await.expect("task"), "a cancelled acquire must fail");
                } else {
                    survivors.push(h);
                }
            }
            prop_assert_eq!(limiter.active(), limit, "cancelled waiters took no slot");

            drop(held);
            for h in survivors {
                prop_assert!(h.await.expect("task"), "a live waiter must be woken and granted");
            }
            prop_assert_eq!(limiter.active(), 0, "everything returned");
            let cx = CancellationToken::new();
            let p = tokio::time::timeout(HANG, limiter.acquire(&cx))
                .await
                .expect("the limiter still works afterwards")
                .expect("a permit");
            prop_assert_eq!(limiter.active(), 1);
            drop(p);
            prop_assert_eq!(limiter.active(), 0);
            Ok(())
        });
        checked?;
    }

    /// Lowering `set_limit` below the number in flight never over-grants, never
    /// kills a permit and never underflows the count.
    #[test]
    fn lowering_the_limit_never_over_grants(
        start in 1usize..=8,
        in_flight in 0usize..=8,
        lower_to in 0usize..=8,
        keys in pvec(any::<u32>(), 8),
    ) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Limiter::new(start);
            let held_n = in_flight.min(start);
            let mut held: Vec<Option<_>> = Vec::new();
            for _ in 0..held_n {
                let cx = CancellationToken::new();
                held.push(Some(
                    tokio::time::timeout(HANG, limiter.acquire(&cx))
                        .await
                        .expect("acquire must not hang")
                        .expect("a permit"),
                ));
            }
            prop_assert_eq!(limiter.active(), held_n);

            limiter.set_limit(lower_to);
            let new_limit = lower_to.max(1);
            prop_assert_eq!(limiter.limit(), new_limit, "the setting is what was asked for");
            prop_assert_eq!(limiter.active(), held_n, "lowering kills nobody, no underflow");

            // Nothing new may start while the number in flight meets the limit.
            let probe = CancellationToken::new();
            let immediate = tokio::time::timeout(QUEUED, limiter.acquire(&probe)).await;
            if held_n >= new_limit {
                prop_assert!(
                    immediate.is_err(),
                    "over-granted: {} in flight, limit {}",
                    held_n,
                    new_limit
                );
            } else {
                prop_assert!(
                    immediate.is_ok(),
                    "under-granted: {} in flight, room for {}",
                    held_n,
                    new_limit
                );
                drop(immediate.unwrap().expect("a permit"));
            }

            // Release in an arbitrary order: exactly one tick down each time.
            let mut order: Vec<usize> = (0..held.len()).collect();
            order.sort_by_key(|i| keys[*i % keys.len()]);
            let mut left = held.len();
            for i in order {
                let p = held[i].take().expect("each permit dropped once");
                drop(p);
                left -= 1;
                prop_assert_eq!(limiter.active(), left, "released exactly once");
                prop_assert!(limiter.active() <= left.max(new_limit), "never over-grants");
            }
            prop_assert_eq!(limiter.active(), 0);
            Ok(())
        });
        checked?;
    }

    /// Limit 1 serialises: the logical-time windows of the holders must not
    /// intersect.
    #[test]
    fn limit_one_serialises_holders(holds in pvec(0usize..=3, 1..=12)) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Arc::new(Limiter::new(1));
            // A logical clock: a holder's window is (start, end) in ticks.
            let clock = Arc::new(AtomicUsize::new(0));
            let windows = Arc::new(Mutex::new(Vec::new()));
            let mut tasks = Vec::new();
            for n in holds.iter().copied() {
                let (l, clock, windows) = (limiter.clone(), clock.clone(), windows.clone());
                tasks.push(tokio::spawn(async move {
                    let cx = CancellationToken::new();
                    let p = tokio::time::timeout(HANG, l.acquire(&cx))
                        .await
                        .expect("acquire must not hang")
                        .expect("a permit");
                    let start = clock.fetch_add(1, Ordering::SeqCst);
                    for _ in 0..n {
                        tokio::task::yield_now().await;
                    }
                    let end = clock.fetch_add(1, Ordering::SeqCst);
                    windows.lock().unwrap().push((start, end));
                    drop(p);
                }));
            }
            for t in tasks {
                t.await.expect("task");
            }
            let mut w = windows.lock().unwrap().clone();
            w.sort();
            prop_assert_eq!(w.len(), holds.len(), "every task held the only slot");
            for pair in w.windows(2) {
                let (a_start, a_end) = pair[0];
                let (b_start, b_end) = pair[1];
                prop_assert!(a_start < a_end && b_start < b_end, "non-empty windows");
                prop_assert!(
                    a_end < b_start,
                    "windows overlap at limit 1: ({}, {}) vs ({}, {})",
                    a_start, a_end, b_start, b_end
                );
            }
            prop_assert_eq!(limiter.active(), 0);
            Ok(())
        });
        checked?;
    }

    /// `Permit` dropped in any order releases exactly once: after each drop the
    /// count is exactly what is left, and the freed slot is really usable.
    #[test]
    fn permits_release_exactly_once_in_any_order(
        limit in 1usize..=6,
        keys in pvec(any::<u32>(), 0..=6),
    ) {
        let rt = rt();
        let checked: Result<(), TestCaseError> = rt.block_on(async move {
            let limiter = Limiter::new(limit);
            let k = keys.len().min(limit);
            let mut held: Vec<Option<_>> = Vec::new();
            for _ in 0..k {
                let cx = CancellationToken::new();
                let p = tokio::time::timeout(HANG, limiter.acquire(&cx))
                    .await
                    .expect("acquire must not hang")
                    .expect("a permit");
                held.push(Some(p));
            }
            prop_assert_eq!(limiter.active(), k);

            let mut order: Vec<usize> = (0..k).collect();
            order.sort_by_key(|i| keys[*i % keys.len().max(1)]);
            let mut left = k;
            for i in order {
                let p = held[i].take().expect("each permit dropped once");
                left -= 1;
                drop(p);
                prop_assert_eq!(limiter.active(), left, "released exactly once");
                // The freed slot must be grantable at once: no lost wake-up.
                let cx = CancellationToken::new();
                let probe =
                    tokio::time::timeout(QUEUED, limiter.acquire(&cx)).await;
                prop_assert!(probe.is_ok(), "a freed slot must be usable ({} left)", left);
                drop(probe.unwrap().expect("a permit"));
                prop_assert_eq!(limiter.active(), left, "the probe gave its permit back");
            }
            prop_assert_eq!(limiter.active(), 0);
            Ok(())
        });
        checked?;
    }
}
