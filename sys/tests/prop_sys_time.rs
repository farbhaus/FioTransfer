//! `platform::local_time`: the epoch seconds the app holds, as wall-clock
//! fields.
//!
//! Two oracles, neither of which asks the C library for a field mapping:
//!
//! * **A pinned table.** Every expected row was computed by CPython's
//!   `zoneinfo` (the `datetime` module) for eight zones — `UTC`, a European DST
//!   zone, `America/New_York`, `Australia/Lord_Howe` (a *half-hour* DST shift),
//!   `Asia/Kathmandu` (+05:45), `America/St_Johns` (-03:30), `Pacific/Kiritimati`
//!   (+14:00) and `Asia/Tehran` — at 0, at ±1, at the 2038 `time_t` boundary, in
//!   1900, in year 9999, and either side of four 2021 DST transitions.
//! * **A round trip.** The fields come back through Howard Hinnant's
//!   `days_from_civil` (an independent algorithm, in `common`) to a point on the
//!   epoch's scale; that point minus the input is the zone's offset, which must
//!   look like an offset (bounded, whole minutes for modern dates) and must step
//!   by a whole DST quantum per hour.
//!
//! `TZ` is process-global and `localtime_r` caches the zone, so every test here
//! takes one lock and restores `TZ` after itself.
//!
//! unix only: `local_time` is `localtime_r`, and on Windows it answers `None`,
//! so every case here has nothing to compare against.

#![cfg(unix)]

mod common;

use std::sync::Mutex;

use fiotransfer_sys::platform::{local_time, LocalParts};
use proptest::prelude::*;

use common::arb_secs;

static TZ_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

extern "C" {
    /// `<time.h>`: drops the cached zone so the next `localtime_r` re-reads
    /// `TZ`. The crate does not expose it (it has no `unix` reason to), so the
    /// test declares it.
    fn tzset();
}

/// Runs `f` with `TZ` set to `zone`, then puts the machine's own zone back.
fn with_zone<R>(zone: &str, f: impl FnOnce() -> R) -> R {
    let saved = std::env::var("TZ").ok();
    std::env::set_var("TZ", zone);
    unsafe { tzset() };
    let out = f();
    match saved {
        Some(v) => std::env::set_var("TZ", v),
        None => std::env::remove_var("TZ"),
    }
    unsafe { tzset() };
    out
}

fn parts(p: Option<LocalParts>) -> Option<(i64, u32, u32, u32, u32, u32)> {
    p.map(|p| (p.year as i64, p.month, p.day, p.hour, p.minute, p.second))
}

// ---------- the pinned table (from CPython's zoneinfo) ----------

#[rustfmt::skip]
const PINNED: &[(&str, i64, Option<(i64, u32, u32, u32, u32, u32)>)] = &[
    // UTC
    ("UTC", 0, Some((1970, 1, 1, 0, 0, 0))),
    ("UTC", 1, Some((1970, 1, 1, 0, 0, 1))),
    ("UTC", -1, Some((1969, 12, 31, 23, 59, 59))),
    ("UTC", -86400, Some((1969, 12, 31, 0, 0, 0))),
    ("UTC", 1234567890, Some((2009, 2, 13, 23, 31, 30))),
    ("UTC", 1616857199, Some((2021, 3, 27, 14, 59, 59))),
    ("UTC", 1616857200, Some((2021, 3, 27, 15, 0, 0))),
    ("UTC", 1616893199, Some((2021, 3, 28, 0, 59, 59))),
    ("UTC", 1616893200, Some((2021, 3, 28, 1, 0, 0))),
    ("UTC", 1635645599, Some((2021, 10, 31, 1, 59, 59))),
    ("UTC", 1635645600, Some((2021, 10, 31, 2, 0, 0))),
    ("UTC", 1615705199, Some((2021, 3, 14, 6, 59, 59))),
    ("UTC", 1615705200, Some((2021, 3, 14, 7, 0, 0))),
    ("UTC", 1636261199, Some((2021, 11, 7, 4, 59, 59))),
    ("UTC", 1636261200, Some((2021, 11, 7, 5, 0, 0))),
    ("UTC", 2147483647, Some((2038, 1, 19, 3, 14, 7))),
    ("UTC", 4102444800, Some((2100, 1, 1, 0, 0, 0))),
    ("UTC", 253402300799, Some((9999, 12, 31, 23, 59, 59))),
    ("UTC", -2208988800, Some((1900, 1, 1, 0, 0, 0))),
    // Europe/London
    ("Europe/London", 0, Some((1970, 1, 1, 1, 0, 0))),
    ("Europe/London", 1, Some((1970, 1, 1, 1, 0, 1))),
    ("Europe/London", -1, Some((1970, 1, 1, 0, 59, 59))),
    ("Europe/London", -86400, Some((1969, 12, 31, 1, 0, 0))),
    ("Europe/London", 1234567890, Some((2009, 2, 13, 23, 31, 30))),
    ("Europe/London", 1616857199, Some((2021, 3, 27, 14, 59, 59))),
    ("Europe/London", 1616857200, Some((2021, 3, 27, 15, 0, 0))),
    ("Europe/London", 1616893199, Some((2021, 3, 28, 0, 59, 59))),
    ("Europe/London", 1616893200, Some((2021, 3, 28, 2, 0, 0))),
    ("Europe/London", 1635645599, Some((2021, 10, 31, 1, 59, 59))),
    ("Europe/London", 1635645600, Some((2021, 10, 31, 2, 0, 0))),
    ("Europe/London", 1615705199, Some((2021, 3, 14, 6, 59, 59))),
    ("Europe/London", 1615705200, Some((2021, 3, 14, 7, 0, 0))),
    ("Europe/London", 1636261199, Some((2021, 11, 7, 4, 59, 59))),
    ("Europe/London", 1636261200, Some((2021, 11, 7, 5, 0, 0))),
    ("Europe/London", 2147483647, Some((2038, 1, 19, 3, 14, 7))),
    ("Europe/London", 4102444800, Some((2100, 1, 1, 0, 0, 0))),
    ("Europe/London", 253402300799, Some((9999, 12, 31, 23, 59, 59))),
    ("Europe/London", -2208988800, Some((1900, 1, 1, 0, 0, 0))),
    // America/New_York
    ("America/New_York", 0, Some((1969, 12, 31, 19, 0, 0))),
    ("America/New_York", 1, Some((1969, 12, 31, 19, 0, 1))),
    ("America/New_York", -1, Some((1969, 12, 31, 18, 59, 59))),
    ("America/New_York", -86400, Some((1969, 12, 30, 19, 0, 0))),
    ("America/New_York", 1234567890, Some((2009, 2, 13, 18, 31, 30))),
    ("America/New_York", 1616857199, Some((2021, 3, 27, 10, 59, 59))),
    ("America/New_York", 1616857200, Some((2021, 3, 27, 11, 0, 0))),
    ("America/New_York", 1616893199, Some((2021, 3, 27, 20, 59, 59))),
    ("America/New_York", 1616893200, Some((2021, 3, 27, 21, 0, 0))),
    ("America/New_York", 1635645599, Some((2021, 10, 30, 21, 59, 59))),
    ("America/New_York", 1635645600, Some((2021, 10, 30, 22, 0, 0))),
    ("America/New_York", 1615705199, Some((2021, 3, 14, 1, 59, 59))),
    ("America/New_York", 1615705200, Some((2021, 3, 14, 3, 0, 0))),
    ("America/New_York", 1636261199, Some((2021, 11, 7, 0, 59, 59))),
    ("America/New_York", 1636261200, Some((2021, 11, 7, 1, 0, 0))),
    ("America/New_York", 2147483647, Some((2038, 1, 18, 22, 14, 7))),
    ("America/New_York", 4102444800, Some((2099, 12, 31, 19, 0, 0))),
    ("America/New_York", 253402300799, Some((9999, 12, 31, 18, 59, 59))),
    ("America/New_York", -2208988800, Some((1899, 12, 31, 19, 0, 0))),
    // Australia/Lord_Howe
    ("Australia/Lord_Howe", 0, Some((1970, 1, 1, 10, 0, 0))),
    ("Australia/Lord_Howe", 1, Some((1970, 1, 1, 10, 0, 1))),
    ("Australia/Lord_Howe", -1, Some((1970, 1, 1, 9, 59, 59))),
    ("Australia/Lord_Howe", -86400, Some((1969, 12, 31, 10, 0, 0))),
    ("Australia/Lord_Howe", 1234567890, Some((2009, 2, 14, 10, 31, 30))),
    ("Australia/Lord_Howe", 1616857199, Some((2021, 3, 28, 1, 59, 59))),
    ("Australia/Lord_Howe", 1616857200, Some((2021, 3, 28, 2, 0, 0))),
    ("Australia/Lord_Howe", 1616893199, Some((2021, 3, 28, 11, 59, 59))),
    ("Australia/Lord_Howe", 1616893200, Some((2021, 3, 28, 12, 0, 0))),
    ("Australia/Lord_Howe", 1635645599, Some((2021, 10, 31, 12, 59, 59))),
    ("Australia/Lord_Howe", 1635645600, Some((2021, 10, 31, 13, 0, 0))),
    ("Australia/Lord_Howe", 1615705199, Some((2021, 3, 14, 17, 59, 59))),
    ("Australia/Lord_Howe", 1615705200, Some((2021, 3, 14, 18, 0, 0))),
    ("Australia/Lord_Howe", 1636261199, Some((2021, 11, 7, 15, 59, 59))),
    ("Australia/Lord_Howe", 1636261200, Some((2021, 11, 7, 16, 0, 0))),
    ("Australia/Lord_Howe", 2147483647, Some((2038, 1, 19, 14, 14, 7))),
    ("Australia/Lord_Howe", 4102444800, Some((2100, 1, 1, 11, 0, 0))),
    ("Australia/Lord_Howe", 253402300799, None),
    ("Australia/Lord_Howe", -2208988800, Some((1900, 1, 1, 10, 0, 0))),
    // Asia/Kathmandu
    ("Asia/Kathmandu", 0, Some((1970, 1, 1, 5, 30, 0))),
    ("Asia/Kathmandu", 1, Some((1970, 1, 1, 5, 30, 1))),
    ("Asia/Kathmandu", -1, Some((1970, 1, 1, 5, 29, 59))),
    ("Asia/Kathmandu", -86400, Some((1969, 12, 31, 5, 30, 0))),
    ("Asia/Kathmandu", 1234567890, Some((2009, 2, 14, 5, 16, 30))),
    ("Asia/Kathmandu", 1616857199, Some((2021, 3, 27, 20, 44, 59))),
    ("Asia/Kathmandu", 1616857200, Some((2021, 3, 27, 20, 45, 0))),
    ("Asia/Kathmandu", 1616893199, Some((2021, 3, 28, 6, 44, 59))),
    ("Asia/Kathmandu", 1616893200, Some((2021, 3, 28, 6, 45, 0))),
    ("Asia/Kathmandu", 1635645599, Some((2021, 10, 31, 7, 44, 59))),
    ("Asia/Kathmandu", 1635645600, Some((2021, 10, 31, 7, 45, 0))),
    ("Asia/Kathmandu", 1615705199, Some((2021, 3, 14, 12, 44, 59))),
    ("Asia/Kathmandu", 1615705200, Some((2021, 3, 14, 12, 45, 0))),
    ("Asia/Kathmandu", 1636261199, Some((2021, 11, 7, 10, 44, 59))),
    ("Asia/Kathmandu", 1636261200, Some((2021, 11, 7, 10, 45, 0))),
    ("Asia/Kathmandu", 2147483647, Some((2038, 1, 19, 8, 59, 7))),
    ("Asia/Kathmandu", 4102444800, Some((2100, 1, 1, 5, 45, 0))),
    ("Asia/Kathmandu", 253402300799, None),
    ("Asia/Kathmandu", -2208988800, Some((1900, 1, 1, 5, 41, 16))),
    // America/St_Johns
    ("America/St_Johns", 0, Some((1969, 12, 31, 20, 30, 0))),
    ("America/St_Johns", 1, Some((1969, 12, 31, 20, 30, 1))),
    ("America/St_Johns", -1, Some((1969, 12, 31, 20, 29, 59))),
    ("America/St_Johns", -86400, Some((1969, 12, 30, 20, 30, 0))),
    ("America/St_Johns", 1234567890, Some((2009, 2, 13, 20, 1, 30))),
    ("America/St_Johns", 1616857199, Some((2021, 3, 27, 12, 29, 59))),
    ("America/St_Johns", 1616857200, Some((2021, 3, 27, 12, 30, 0))),
    ("America/St_Johns", 1616893199, Some((2021, 3, 27, 22, 29, 59))),
    ("America/St_Johns", 1616893200, Some((2021, 3, 27, 22, 30, 0))),
    ("America/St_Johns", 1635645599, Some((2021, 10, 30, 23, 29, 59))),
    ("America/St_Johns", 1635645600, Some((2021, 10, 30, 23, 30, 0))),
    ("America/St_Johns", 1615705199, Some((2021, 3, 14, 4, 29, 59))),
    ("America/St_Johns", 1615705200, Some((2021, 3, 14, 4, 30, 0))),
    ("America/St_Johns", 1636261199, Some((2021, 11, 7, 1, 29, 59))),
    ("America/St_Johns", 1636261200, Some((2021, 11, 7, 1, 30, 0))),
    ("America/St_Johns", 2147483647, Some((2038, 1, 18, 23, 44, 7))),
    ("America/St_Johns", 4102444800, Some((2099, 12, 31, 20, 30, 0))),
    ("America/St_Johns", 253402300799, Some((9999, 12, 31, 20, 29, 59))),
    ("America/St_Johns", -2208988800, Some((1899, 12, 31, 20, 29, 8))),
    // Pacific/Kiritimati
    ("Pacific/Kiritimati", 0, Some((1969, 12, 31, 13, 20, 0))),
    ("Pacific/Kiritimati", 1, Some((1969, 12, 31, 13, 20, 1))),
    ("Pacific/Kiritimati", -1, Some((1969, 12, 31, 13, 19, 59))),
    ("Pacific/Kiritimati", -86400, Some((1969, 12, 30, 13, 20, 0))),
    ("Pacific/Kiritimati", 1234567890, Some((2009, 2, 14, 13, 31, 30))),
    ("Pacific/Kiritimati", 1616857199, Some((2021, 3, 28, 4, 59, 59))),
    ("Pacific/Kiritimati", 1616857200, Some((2021, 3, 28, 5, 0, 0))),
    ("Pacific/Kiritimati", 1616893199, Some((2021, 3, 28, 14, 59, 59))),
    ("Pacific/Kiritimati", 1616893200, Some((2021, 3, 28, 15, 0, 0))),
    ("Pacific/Kiritimati", 1635645599, Some((2021, 10, 31, 15, 59, 59))),
    ("Pacific/Kiritimati", 1635645600, Some((2021, 10, 31, 16, 0, 0))),
    ("Pacific/Kiritimati", 1615705199, Some((2021, 3, 14, 20, 59, 59))),
    ("Pacific/Kiritimati", 1615705200, Some((2021, 3, 14, 21, 0, 0))),
    ("Pacific/Kiritimati", 1636261199, Some((2021, 11, 7, 18, 59, 59))),
    ("Pacific/Kiritimati", 1636261200, Some((2021, 11, 7, 19, 0, 0))),
    ("Pacific/Kiritimati", 2147483647, Some((2038, 1, 19, 17, 14, 7))),
    ("Pacific/Kiritimati", 4102444800, Some((2100, 1, 1, 14, 0, 0))),
    ("Pacific/Kiritimati", 253402300799, None),
    ("Pacific/Kiritimati", -2208988800, Some((1899, 12, 31, 13, 30, 40))),
    // Asia/Tehran
    ("Asia/Tehran", 0, Some((1970, 1, 1, 3, 30, 0))),
    ("Asia/Tehran", 1, Some((1970, 1, 1, 3, 30, 1))),
    ("Asia/Tehran", -1, Some((1970, 1, 1, 3, 29, 59))),
    ("Asia/Tehran", -86400, Some((1969, 12, 31, 3, 30, 0))),
    ("Asia/Tehran", 1234567890, Some((2009, 2, 14, 3, 1, 30))),
    ("Asia/Tehran", 1616857199, Some((2021, 3, 27, 19, 29, 59))),
    ("Asia/Tehran", 1616857200, Some((2021, 3, 27, 19, 30, 0))),
    ("Asia/Tehran", 1616893199, Some((2021, 3, 28, 5, 29, 59))),
    ("Asia/Tehran", 1616893200, Some((2021, 3, 28, 5, 30, 0))),
    ("Asia/Tehran", 1635645599, Some((2021, 10, 31, 5, 29, 59))),
    ("Asia/Tehran", 1635645600, Some((2021, 10, 31, 5, 30, 0))),
    ("Asia/Tehran", 1615705199, Some((2021, 3, 14, 10, 29, 59))),
    ("Asia/Tehran", 1615705200, Some((2021, 3, 14, 10, 30, 0))),
    ("Asia/Tehran", 1636261199, Some((2021, 11, 7, 8, 29, 59))),
    ("Asia/Tehran", 1636261200, Some((2021, 11, 7, 8, 30, 0))),
    ("Asia/Tehran", 2147483647, Some((2038, 1, 19, 6, 44, 7))),
    ("Asia/Tehran", 4102444800, Some((2100, 1, 1, 3, 30, 0))),
    ("Asia/Tehran", 253402300799, None),
    ("Asia/Tehran", -2208988800, Some((1900, 1, 1, 3, 25, 44))),
];

#[test]
fn the_pinned_zone_table_matches() {
    let _g = lock();
    let mut zones: Vec<&str> = PINNED.iter().map(|r| r.0).collect();
    zones.sort_unstable();
    zones.dedup();
    let mut checked = 0;
    for zone in zones {
        with_zone(zone, || {
            for (z, secs, want) in PINNED.iter().filter(|r| r.0 == zone) {
                assert_eq!(z, &zone);
                // `None` rows are the ones CPython's `datetime` cannot represent
                // (year 9999 plus a positive offset overflows its year); there is
                // no expectation to check, so they only have to not panic.
                if let Some(want) = want {
                    let got = parts(local_time(*secs));
                    assert_eq!(got, Some(*want), "TZ={zone} local_time({secs})");
                    checked += 1;
                } else {
                    let _ = local_time(*secs);
                }
            }
        });
    }
    assert!(checked >= 140, "only {checked} rows checked");
}

// ---------- the properties ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// Totality and the round trip, on the machine's own zone.
    #[test]
    fn local_time_never_panics_and_its_fields_round_trip(secs in arb_secs()) {
        let _g = lock();
        let got = local_time(secs); // panics here are the bug this hunts
        let Some(p) = got else { return Ok(()) };

        // Ranges a civil clock can actually read.
        prop_assert!((1..=12).contains(&p.month), "{p:?} from {secs}");
        prop_assert!((1..=31).contains(&p.day), "{p:?} from {secs}");
        prop_assert!(p.hour <= 23, "{p:?} from {secs}");
        prop_assert!(p.minute <= 59, "{p:?} from {secs}");
        prop_assert!(p.second <= 60, "{p:?} from {secs}"); // 60 is a leap second

        // The three date fields name a real day: the inverse algorithm gives
        // the same year, month and day back. A day of 0 or 32, or a month
        // shifted by one, does not survive that.
        let days = common::days_from_civil(p.year as i64, p.month as i64, p.day as i64);
        prop_assert_eq!(
            common::civil_from_days(days),
            (p.year as i64, p.month as i64, p.day as i64),
            "{:?} from {}", p, secs
        );

        // The fields, read back as an instant, differ from the input by the
        // zone's offset: bounded by a couple of time-zone extremes, never a
        // whole day (which is what a wrong date field would produce).
        let off = common::local_as_epoch(&p) - secs;
        prop_assert!(off.abs() < 20 * 3600, "offset {off} from {} ({{{:?}}})", secs, p);
        if (0..=4102444800).contains(&secs) {
            prop_assert_eq!(off % 60, 0, "offset {} is not whole minutes, from {}", off, secs);
            prop_assert!(off.abs() <= 14 * 3600, "offset {off} out of range, from {}", secs);
        }
    }

    /// Metamorphic: one hour later, the wall clock is one hour later, two hours
    /// later (DST starts), or the same (DST ends) — and a half-hour zone moves
    /// by half an hour. Nothing else can happen, at any instant in 2000..2100.
    #[test]
    fn local_time_advances_by_a_dst_quantum_per_hour(secs in 946_684_800i64..4_102_444_800i64) {
        let _g = lock();
        let (Some(a), Some(b)) = (local_time(secs), local_time(secs + 3600)) else { return Ok(()) };
        let step = common::local_as_epoch(&b) - common::local_as_epoch(&a);
        prop_assert!(
            matches!(step, 0 | 1800 | 3600 | 5400 | 7200),
            "one hour moved the wall clock by {step}s at {} ({} -> {})", secs, a.year, b.year
        );
    }
}

#[test]
fn the_extremes_are_answerable_and_bounded() {
    let _g = lock();
    // Every one of these either answers with a civil reading or says None; none
    // panics, and a civil reading is a whole minute-fine offset from the input
    // when the input is in range.
    let values = [
        i64::MIN,
        i64::MIN + 1,
        i64::MIN / 2,
        -63_107_232_000, // year 0001, if the zone database goes back that far
        -2_208_988_800,  // 1900-01-01
        -1,
        0,
        1,
        2_147_483_647, // 2038-01-19T03:14:07Z
        2_147_483_648,
        253_402_300_799, // 9999-12-31T23:59:59Z
        i64::MAX / 2,
        i64::MAX - 1,
        i64::MAX,
    ];
    for secs in values {
        match local_time(secs) {
            None => {}
            Some(p) => {
                assert!((1..=12).contains(&p.month), "{p:?} from {secs}");
                assert!((1..=31).contains(&p.day), "{p:?} from {secs}");
                let days = common::days_from_civil(p.year as i64, p.month as i64, p.day as i64);
                assert_eq!(
                    common::civil_from_days(days),
                    (p.year as i64, p.month as i64, p.day as i64),
                    "{p:?} from {secs}"
                );
            }
        }
    }
}

#[test]
fn every_hour_of_two_years_and_every_day_of_eight_centuries() {
    let _g = lock();
    // Exhaustive over the hours around four DST changes in the machine's own
    // zone, and over the days of 1600..2400: no panic, and the day/month pair
    // always names a real day.
    let mut checked = 0usize;
    let mut hour = 1_609_459_200i64; // 2021-01-01T00:00:00Z
    while hour < 1_703_980_800 {
        if let Some(p) = local_time(hour) {
            let days = common::days_from_civil(p.year as i64, p.month as i64, p.day as i64);
            assert_eq!(common::civil_from_days(days), (p.year as i64, p.month as i64, p.day as i64));
            checked += 1;
        }
        hour += 3600;
    }
    let mut day = common::days_from_civil(1600, 1, 1) * 86_400;
    let end = common::days_from_civil(2400, 1, 1) * 86_400;
    while day < end {
        if let Some(p) = local_time(day) {
            let days = common::days_from_civil(p.year as i64, p.month as i64, p.day as i64);
            assert_eq!(common::civil_from_days(days), (p.year as i64, p.month as i64, p.day as i64));
            checked += 1;
        }
        day += 86_400;
    }
    assert!(checked > 300_000, "only {checked} readings checked");
}

#[test]
fn the_civil_model_agrees_with_known_days() {
    // Guards the oracle itself: if these drift, every round trip above is
    // meaningless.
    assert_eq!(common::days_from_civil(1970, 1, 1), 0);
    assert_eq!(common::days_from_civil(1969, 12, 31), -1);
    assert_eq!(common::days_from_civil(2000, 3, 1), 11017);
    assert_eq!(common::days_from_civil(9999, 12, 31), 2_932_896);
    assert_eq!(common::days_from_civil(1900, 1, 1), -25_567);
    for d in [-719_162i64, -25_567, -1, 0, 1, 11_016, 18_714, 2_932_896] {
        let (y, m, dd) = common::civil_from_days(d);
        assert_eq!(common::days_from_civil(y, m, dd), d);
    }
}
