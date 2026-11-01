//! Property tests for the per-route rate-limit key, `api::route_key`, and for
//! the path-component sanitiser, `share::safe_name`.
//!
//! The model for `route_key` is built from the *segments the generator chose*,
//! not from re-parsing the URL, so it cannot inherit the implementation's
//! parsing mistakes: the contract is "method, one space, then the path with
//! every id-shaped segment replaced by `:id`", and the scheme, host, port,
//! userinfo, query and fragment contribute nothing.
//!
//! Run: CARGO_TARGET_DIR=/Users/chaos/work/target-prop-parse-2 \
//!      cargo test --test prop_parse --offline

use proptest::collection::vec as pvec;
use proptest::prelude::*;

use fiotransfer::api::route_key;
use fiotransfer::share::safe_name;

// ---------- the model ----------

/// The documented masking rule, written out on its own: a segment is id-shaped
/// when it is 16 bytes or longer and every byte is a hex digit or a dash.
fn id_shaped(s: &str) -> bool {
    s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn mask_one(s: &str) -> &str {
    if id_shaped(s) {
        ":id"
    } else {
        s
    }
}

/// The path the key must carry: a leading slash, then the generated segments
/// with the id-shaped ones masked.
fn want_path(segs: &[String]) -> String {
    let masked: Vec<&str> = segs.iter().map(|s| mask_one(s)).collect();
    format!("/{}", masked.join("/"))
}

fn want_key(method: &str, segs: &[String]) -> String {
    format!("{} {}", method, want_path(segs))
}

/// The path as it goes into the URL, unmasked.
fn raw_path(segs: &[String]) -> String {
    format!("/{}", segs.join("/"))
}

fn substitute(segs: &[String], at: usize, with: String) -> Vec<String> {
    let mut v = segs.to_vec();
    let i = if v.is_empty() { 0 } else { at % (v.len() + 1) };
    v.insert(i, with);
    v
}

// ---------- generators ----------

/// Segments that are ids: uuids, the 16-byte boundary, longer and upper-case
/// runs, dash-grouped runs, and one absurdly long one.
fn id_seg() -> impl Strategy<Value = String> {
    prop_oneof![
        "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
        "[0-9a-f]{16}",
        "[0-9A-F]{17,40}",
        "([0-9a-f]{4}-){4,6}[0-9a-f]{4}",
        Just("a".repeat(1000)),
    ]
}

/// Segments that only look like ids: one byte short, one stray letter, long but
/// not hex, or hex split by something that is neither a digit nor a dash.
fn near_id_seg() -> impl Strategy<Value = String> {
    prop_oneof![
        "[0-9a-f]{1,15}",
        "[0-9a-f]{14}g[0-9a-f]{2}",
        "[0-9a-f]{15}g",
        "[g-z]{16,24}",
        "[0-9a-f]{6}[._][0-9a-f]{9}",
        Just("z".repeat(1000)),
    ]
}

/// Ordinary segments, including the empty one (adjacent or trailing slashes).
fn plain_seg() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{1,10}",
        "[A-Za-z0-9_.~-]{0,12}",
        Just(String::new()),
        Just("accounts".to_string()),
        ":[a-z]{1,5}",
    ]
}

fn seg() -> impl Strategy<Value = String> {
    prop_oneof![id_seg(), near_id_seg(), plain_seg()]
}

fn scheme() -> impl Strategy<Value = String> {
    prop_oneof![Just("http".to_string()), Just("https".to_string()), Just("HTTPS".to_string())]
}

fn host() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9.]{2,15}"
}

/// Query text that contains none of `?`, `#`, `/` or `:`: it must be dropped
/// whole, so it must not be able to reach the key.
fn plain_query() -> impl Strategy<Value = String> {
    "[a-z]{1,6}=[A-Za-z0-9_]{0,12}"
}

fn plain_frag() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_.-]{0,12}"
}

/// Wrappers around one path: bare, with a scheme, with userinfo, with a port,
/// with a query, with a fragment, with both, and a fully loaded URL.
fn build_url(kind: u8, path: &str, scheme: &str, host: &str, port: u16, q: &str, frag: &str) -> String {
    match kind % 8 {
        0 => path.to_string(),
        1 => format!("{scheme}://{host}{path}"),
        2 => format!("{scheme}://user:pw@{host}{path}"),
        3 => format!("{scheme}://{host}:{port}{path}"),
        4 => format!("{path}?{q}"),
        5 => format!("{path}#{frag}"),
        6 => format!("{path}?{q}#{frag}"),
        _ => format!("{scheme}://{host}:{port}{path}?{q}"),
    }
}

fn nasty_method() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("GET".to_string()),
        Just("get".to_string()),
        Just("Get".to_string()),
        Just(String::new()),
        Just("G ET".to_string()),
        Just("G\u{c9}T".to_string()),
        Just("GET\n".to_string()),
        Just("X".repeat(200)),
        "[A-Z]{1,6}",
    ]
}

// ---------- properties ----------

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// Every id-shaped segment becomes `:id`, and no other segment changes at
    /// all: not the 15-byte one, not the one with a stray `g`, not the empty
    /// one, not the 1000-byte non-hex one.
    #[test]
    fn masks_id_segments_and_leaves_every_other_segment_alone(segs in pvec(seg(), 0..7)) {
        let url = raw_path(&segs);
        let got = route_key("GET", &url);
        let want = want_key("GET", &segs);
        prop_assert_eq!(&got, &want, "url {:?} segs {:?}", url, segs);
        // And stated the other way round: an id-shaped segment never survives.
        for s in &segs {
            if id_shaped(s) {
                prop_assert!(!got.contains(s.as_str()), "{:?} survived in {:?}", s, got);
            }
        }
    }

    /// The scheme, host, port, userinfo, query and fragment never reach the key,
    /// whatever the wrapper: the key depends on the path alone.
    #[test]
    fn the_wrapper_never_reaches_the_key(
        segs in pvec(seg(), 0..6),
        kind in 0u8..8,
        sch in scheme(),
        h in host(),
        port in 1u16..65535,
        q in plain_query(),
        frag in plain_frag(),
    ) {
        let path = raw_path(&segs);
        let url = build_url(kind, &path, &sch, &h, port, &q, &frag);
        let got = route_key("GET", &url);
        let want = want_key("GET", &segs);
        prop_assert_eq!(&got, &want, "wrapper kind {} url {:?}", kind % 8, url);
        prop_assert!(!got.contains(&h), "host {:?} leaked into {:?}", h, got);
        prop_assert!(!got.contains(&format!("{}://", sch)), "scheme leaked into {:?}", got);
        prop_assert!(!got.contains(&q) || q.is_empty(), "query {:?} leaked into {:?}", q, got);
    }

    /// Two URLs that differ only in an id are one route: same key.
    #[test]
    fn two_urls_that_differ_only_in_an_id_have_the_same_key(
        segs in pvec(seg(), 0..5),
        at in 0usize..8,
        id1 in id_seg(),
        id2 in id_seg(),
        kind in 0u8..8,
        q in plain_query(),
        frag in plain_frag(),
    ) {
        let s1 = substitute(&segs, at, id1.clone());
        let s2 = substitute(&segs, at, id2.clone());
        let u1 = build_url(kind, &raw_path(&s1), "https", "api.frame.io", 8443, &q, &frag);
        let u2 = build_url(kind, &raw_path(&s2), "https", "api.frame.io", 8443, &q, &frag);
        let k1 = route_key("GET", &u1);
        let k2 = route_key("GET", &u2);
        prop_assert_eq!(&k1, &k2, "ids {:?} and {:?} into {:?}", id1, id2, s1);
        prop_assert_eq!(&k1, &want_key("GET", &s1));
    }

    /// Two URLs that differ in a segment that is not an id are two routes:
    /// different keys, so they get their own rate-limit buckets.
    #[test]
    fn two_urls_that_differ_in_a_non_id_segment_have_different_keys(
        segs in pvec(seg(), 0..5),
        at in 0usize..8,
        a in "[a-z]{1,8}",
        b in "[a-z]{1,8}",
        kind in 0u8..8,
    ) {
        prop_assume!(a != b);
        let s1 = substitute(&segs, at, a.clone());
        let s2 = substitute(&segs, at, b.clone());
        let u1 = build_url(kind, &raw_path(&s1), "https", "api.frame.io", 443, "x=1", "f");
        let u2 = build_url(kind, &raw_path(&s2), "https", "api.frame.io", 443, "x=1", "f");
        let k1 = route_key("GET", &u1);
        let k2 = route_key("GET", &u2);
        prop_assert_ne!(&k1, &k2, "{:?} vs {:?} differ in {:?} at {}", a, b, segs, at);
        prop_assert_eq!(&k1, &want_key("GET", &s1));
        prop_assert_eq!(&k2, &want_key("GET", &s2));
    }

    /// The method is carried through exactly: case, spaces, non-ASCII and
    /// emptiness all survive, and the path part does not depend on it.
    #[test]
    fn the_method_is_preserved_verbatim(
        method in nasty_method(),
        segs in pvec(seg(), 0..6),
        kind in 0u8..8,
    ) {
        let path = raw_path(&segs);
        let url = build_url(kind, &path, "https", "api.frame.io", 8443, "page=2", "top");
        let key = route_key(&method, &url);
        prop_assert_eq!(&key, &want_key(&method, &segs), "method {:?} url {:?}", method, url);
        let head = format!("{} ", method);
        prop_assert_eq!(key.strip_prefix(&head).map(|s| s.to_string()), Some(want_path(&segs)));
    }
}

// ---------- safe_name ----------

/// Names from a share: separators, NULs, blanks, the dot names, control bytes,
/// other scripts, and one long one.
fn nasty_name() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-z0-9 ._-]{0,20}",
        "[a-z]{0,10}/[a-z]{0,10}",
        "[a-z]{0,6}:[a-z]{0,6}",
        "[\\x00-\\x7f]{0,12}",
        "[\\p{L} .]{0,12}",
        Just(String::new()),
        Just(".".to_string()),
        Just("..".to_string()),
        Just("...".to_string()),
        Just("\0".to_string()),
        Just(" / : ".to_string()),
        Just("\u{a0}".to_string()),
        Just("a".repeat(300)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// The result is one usable path component: no separator, no NUL, not
    /// empty, not `.` or `..`, and no surrounding whitespace to lose.
    #[test]
    fn a_safe_name_is_exactly_one_path_component(s in nasty_name()) {
        let out = safe_name(&s);
        prop_assert!(!out.is_empty(), "empty name for {:?}", s);
        prop_assert!(!out.contains('/'), "{:?} kept a slash", out);
        prop_assert!(!out.contains('\0'), "{:?} kept a NUL", out);
        prop_assert!(out != "." && out != "..", "{:?} is a dot name", out);
        prop_assert_eq!(out.trim(), out.as_str(), "untrimmed name for {:?}", s);
    }

    /// Sanitising twice is the same as sanitising once, so a sanitised name can
    /// be fed back through the same code paths (dedupe, rel_path) unchanged.
    #[test]
    fn safe_name_is_idempotent(s in nasty_name()) {
        let once = safe_name(&s);
        let twice = safe_name(&once);
        prop_assert_eq!(&once, &twice, "input {:?}", s);
    }
}

// ---------- known defects ----------
//
// Both are #[ignore]d because the fix belongs in `src/api.rs`, which another
// worker owns in this worktree. Run them with:
//   cargo test --test prop_parse --offline -- --ignored

/// KNOWN DEFECT 1. `route_key("GET", "https://api.frame.io")` returns
/// `"GET https://api.frame.io"`. When the URL has a scheme and no path at all,
/// the `find("://")` branch finds no slash after the host and leaves the whole
/// URL in place, so the scheme and host become part of the rate-limit key. The
/// key is meant to be the method and the path; the host can never be in it.
#[test]
#[allow(dead_code, unused)] // "known defect: a URL with a scheme and an empty path leaks scheme and host into the key"]
fn a_url_without_a_path_does_not_put_its_host_in_the_key() {
    for url in ["https://api.frame.io", "https://api.frame.io?x=1", "HTTP://host.example"] {
        let k = route_key("GET", url);
        assert!(
            !k.contains("://") && !k.contains("api.frame.io") && !k.contains("host.example"),
            "scheme/host reached the key: route_key(\"GET\", {:?}) == {:?}",
            url,
            k
        );
    }
}

/// KNOWN DEFECT 2. A query or fragment that itself contains `://` truncates the
/// key to whatever follows that slash pair:
///   route_key("GET", "/v4/accounts/a/files/b?next=https://example.com/x") == "GET /x"
/// The path is lost and the query decides the key, so two unrelated routes
/// collapse into one rate-limit bucket. The query and fragment must never
/// contribute to the key.
#[test]
#[allow(dead_code, unused)] // "known defect: a query or fragment containing \"://\" truncates the key"]
fn a_query_that_looks_like_a_url_does_not_change_the_key() {
    let plain = route_key("GET", "/v4/accounts/a/files/b");
    assert_eq!(plain, "GET /v4/accounts/a/files/b");
    for tail in ["?next=https://example.com/x", "#see/https://example.com/x", "?u=a://b/c"] {
        let with_tail = route_key("GET", &format!("/v4/accounts/a/files/b{}", tail));
        assert_eq!(with_tail, plain, "a query or fragment changed the key ({:?})", tail);
    }
}
