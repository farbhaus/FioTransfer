//! Property tests for `src/share`: the six providers' link parsers and the
//! public `resolve_link` / `looks_like_link` pair.
//!
//! The network clients cannot run here, so every case stays on the pure path: an
//! input either matches a provider's URL grammar (and is therefore recognised
//! before any request is made) or cannot be a URL with a dotted host at all.
//! `resolve_offline` fails loudly if a case ever reaches the network instead of
//! hanging for the client's 30 s timeout.
//!
//! The `pub(crate)` machinery (rel_path, dedupe_paths, the private JSON
//! decoders, the per-provider `pub(crate)` parsers) is property tested where it
//! lives: `mod props` in `src/share/mod.rs`, plus decoder cases in `dropbox.rs`,
//! `gdrive.rs` and `legacy.rs`.

use std::time::Duration;

use proptest::prelude::*;
use proptest::sample::select;

use fiotransfer::share::{
    looks_like_link, resolve_link, safe_name, Link, KIND_DRIVE, KIND_DROPBOX, KIND_PORTAL,
    KIND_REVIEW, KIND_SHARE, KIND_SMASH, KIND_WETRANSFER,
};
use tokio_util::sync::CancellationToken;

const HEX: &[u8] = b"0123456789abcdefABCDEF";
const ALNUM: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const TOKEN: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_-";
const NOISY: &[&str] = &[
    "a", "z", "A", "0", "9", "_", "-", ":", "?", "&", "=", "+", "~", "%", "#", " ", "\u{e9}",
    "\u{4e2d}", "\u{0}",
];

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

fn chars_of(alphabet: &'static [u8], n: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(select(alphabet), n).prop_map(|v| String::from_utf8(v).unwrap())
}

fn hexs(n: usize) -> impl Strategy<Value = String> {
    chars_of(HEX, n)
}

fn alnums(min: usize, max: usize) -> impl Strategy<Value = String> {
    (min..=max).prop_flat_map(|n| chars_of(ALNUM, n))
}

fn tokens(min: usize, max: usize) -> impl Strategy<Value = String> {
    (min..=max).prop_flat_map(|n| chars_of(TOKEN, n))
}

/// A 8-4-4-4-12 UUID, the shape the Frame.io parsers accept.
fn uuid() -> impl Strategy<Value = String> {
    (hexs(8), hexs(4), hexs(4), hexs(4), hexs(12))
        .prop_map(|(a, b, c, d, e)| format!("{}-{}-{}-{}-{}", a, b, c, d, e))
}

/// A Drive id: at least ten of [A-Za-z0-9_-].
fn drive_id() -> impl Strategy<Value = String> {
    tokens(10, 30)
}

/// True when the input is a URL whose host has a dot: `resolve_link` would open
/// it unless a provider parser recognises it first.
fn parses_with_dotted_host(raw: &str) -> bool {
    let s = raw.trim();
    let full = if s.contains("://") { s.to_string() } else { format!("https://{}", s) };
    match url::Url::parse(&full) {
        Ok(u) => u.host_str().map(|h| h.contains('.')).unwrap_or(false),
        Err(_) => false,
    }
}

/// Runs `resolve_link` on an input that must be decided without a request.
fn resolve_offline(raw: &str) -> Result<Link, String> {
    let rt = rt();
    let cx = CancellationToken::new();
    match rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), resolve_link(&cx, raw)).await }) {
        Ok(r) => r,
        Err(_) => panic!("resolve_link({:?}) reached the network; the case is not offline", raw),
    }
}

/// Puts a link in the surroundings a pasted link really has: no scheme, a
/// trailing slash, a tracking query, a fragment, or stray whitespace.
fn dress(base: String, how: usize, pad: bool) -> String {
    let mut s = match how {
        0 => base,
        1 => base.replacen("https://", "", 1),
        2 => format!("{}/", base),
        3 => format!("{}?utm_source=copy", base),
        4 => format!("{}/view?usp=sharing", base),
        _ => format!("{}#fragment", base),
    };
    if pad {
        s = format!("  {}\n", s);
    }
    s
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        // A fixed seed keeps `caught / survived` reproducible when the suite is
        // used to calibrate itself against planted defects.
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5EED_5EED),
        ..ProptestConfig::default()
    })]

    /// `safe_name` returns one harmless path component: never empty, never a
    /// directory reference, never a separator, trimmed, and stable when applied
    /// twice. Arbitrary unicode, control characters and dot-only names are the
    /// interesting cases.
    #[test]
    fn safe_name_is_always_one_harmless_component(s in any::<String>()) {
        let n = safe_name(&s);
        prop_assert!(!n.is_empty(), "safe_name({:?}) is empty", s);
        prop_assert_ne!(n.as_str(), ".", "safe_name({:?}) = {:?}", s, n);
        prop_assert_ne!(n.as_str(), "..", "safe_name({:?}) = {:?}", s, n);
        prop_assert!(!n.contains('/'), "safe_name({:?}) = {:?} is a path", s, n);
        prop_assert!(!n.contains('\0'), "safe_name({:?}) = {:?} keeps a NUL", s, n);
        prop_assert!(!n.contains(':'), "safe_name({:?}) = {:?} keeps the legacy separator", s, n);
        prop_assert_eq!(n.trim(), n.as_str(), "safe_name({:?}) = {:?} is not trimmed", s, n);
        prop_assert_eq!(safe_name(&n), n.clone(), "safe_name is not idempotent on {:?}", s);
    }

    /// Names that are only dots, or dots dressed with the separators and blanks a
    /// sender can put around them, must still come back as one usable component.
    /// Every case is one of these templates, so the boundary is always reached.
    #[test]
    fn dot_names_become_untitled(
        template in select(vec![".", "..", "...", "....", " . ", "..\u{0}", ".:", ":/", "\u{0}..", ". ."]),
        prefix in select(vec!["", " ", ".", "/", ":", "\u{0}"]),
        suffix in select(vec!["", " ", ".", "/", "\u{0}", ":"]),
    ) {
        let s = format!("{}{}{}", prefix, template, suffix);
        let n = safe_name(&s);
        prop_assert!(!n.is_empty(), "safe_name({:?}) is empty", s);
        prop_assert!(n != "." && n != "..", "safe_name({:?}) = {:?}", s, n);
        prop_assert!(!n.contains('/'), "safe_name({:?}) = {:?}", s, n);
        prop_assert!(!n.contains(':'), "safe_name({:?}) = {:?} keeps the legacy separator", s, n);
        prop_assert!(!n.contains('\u{0}'), "safe_name({:?}) = {:?} keeps a NUL", s, n);
        prop_assert_eq!(n.trim(), n.as_str(), "safe_name({:?}) = {:?} is not trimmed", s, n);
    }

    /// A label is what the download panel shows: never empty, and it mentions at
    /// most eight characters of the id, counted in characters not bytes.
    #[test]
    fn labels_are_short_and_never_empty(
        kind in select(vec![
            KIND_SHARE, KIND_REVIEW, KIND_SMASH, KIND_WETRANSFER, KIND_DROPBOX, KIND_DRIVE, KIND_PORTAL,
        ]),
        id in any::<String>(),
    ) {
        let l = Link { kind: kind.to_string(), id: id.clone(), url: String::new() };
        let text = l.label();
        prop_assert!(!text.is_empty(), "empty label for {:?}", l);
        prop_assert!(!text.contains('\n'), "label {:?} has a newline", text);
        let short: String = id.chars().take(8).collect();
        prop_assert!(text.contains(&short), "label {:?} should mention {:?}", text, short);
    }

    /// A Frame.io share or review link, however it is pasted, resolves to that
    /// kind and to the id inside it, lowercased, with nothing else attached.
    #[test]
    fn frame_io_links_resolve_to_kind_and_id(
        id in uuid(),
        review in any::<bool>(),
        how in 0usize..6,
        upper in any::<bool>(),
        pad in any::<bool>(),
    ) {
        let shown = if upper { id.to_uppercase() } else { id.clone() };
        let base = if review {
            format!("https://app.frame.io/reviews/{}", shown)
        } else {
            format!("https://next.frame.io/share/{}", shown)
        };
        let raw = dress(base, how, pad);
        let got = resolve_offline(&raw).expect("a Frame.io link resolves");
        prop_assert_eq!(got.kind.as_str(), if review { KIND_REVIEW } else { KIND_SHARE });
        let want = id.to_lowercase();
        prop_assert_eq!(got.id.as_str(), want.as_str());
    }

    /// A bare UUID is a share link; the id comes back lowercased whatever case
    /// it was pasted in.
    #[test]
    fn bare_uuids_resolve_lowercased(id in uuid(), pad in any::<bool>()) {
        let raw = if pad { format!("  {}  ", id) } else { id.clone() };
        let got = resolve_offline(&raw).expect("a bare share id resolves");
        prop_assert_eq!(got.kind.as_str(), KIND_SHARE);
        let want = id.to_lowercase();
        prop_assert_eq!(got.id.as_str(), want.as_str());
    }

    /// WeTransfer links: /downloads/<id>/<hash> and the recipient form, on
    /// wetransfer.com or a branded subdomain.
    #[test]
    fn wetransfer_links_resolve_to_kind_and_id(
        id in alnums(8, 20),
        hash in alnums(8, 20),
        recipient in proptest::option::of(alnums(4, 12)),
        host in select(vec!["wetransfer.com", "www.wetransfer.com", "studio.wetransfer.com"]),
    ) {
        let path = match &recipient {
            Some(r) => format!("downloads/{}/{}/{}", id, r, hash),
            None => format!("downloads/{}/{}", id, hash),
        };
        let got = resolve_offline(&format!("https://{}/{}", host, path)).expect("resolves");
        prop_assert_eq!(got.kind.as_str(), KIND_WETRANSFER);
        prop_assert_eq!(got.id.as_str(), id.as_str());
        prop_assert!(got.url.starts_with("https://wetransfer.com/downloads/"), "url {:?}", got.url);
        prop_assert!(!got.url.contains('?'), "url {:?} keeps a query", got.url);
    }

    /// Dropbox links: /scl/fo|fi/<key>/<x> and /sh|s/<key>/<x>. The id is the
    /// link key, never a path or query fragment.
    #[test]
    fn dropbox_links_resolve_to_the_link_key(
        seg in select(vec!["scl/fo", "scl/fi", "sh", "s"]),
        key in tokens(3, 20),
        last in alnums(1, 12),
        rlkey in proptest::option::of(tokens(1, 12)),
        dl in any::<bool>(),
    ) {
        let mut raw = format!("https://www.dropbox.com/{}/{}/{}", seg, key, last);
        let mut q: Vec<String> = Vec::new();
        if let Some(r) = &rlkey {
            q.push(format!("rlkey={}", r));
        }
        if dl {
            q.push("dl=0".to_string());
        }
        if !q.is_empty() {
            raw.push('?');
            raw.push_str(&q.join("&"));
        }
        let got = resolve_offline(&raw).expect("resolves");
        prop_assert_eq!(got.kind.as_str(), KIND_DROPBOX);
        prop_assert_eq!(got.id.as_str(), key.as_str());
        let mut want_url = format!("https://www.dropbox.com/{}/{}/{}", seg, key, last);
        if let Some(r) = &rlkey {
            want_url.push_str(&format!("?rlkey={}", r));
        }
        prop_assert_eq!(got.url.as_str(), want_url.as_str(), "the rebuilt link lost a segment");
        prop_assert!(!got.url.contains("dl=0"), "url {:?} keeps dl", got.url);
        let keeps_rlkey = got.url.contains(&format!("rlkey={}", rlkey.as_deref().unwrap_or("")));
        prop_assert_eq!(keeps_rlkey, rlkey.is_some(), "url {:?} rlkey {:?}", got.url, rlkey);
    }

    /// Drive links: /file/d/<id>, /drive/[u/N/]folders/<id> and ?id=<id>. The id
    /// is the file or folder id; a resource key rides along.
    #[test]
    fn drive_links_resolve_to_the_file_or_folder_id(
        id in drive_id(),
        shape in 0usize..5,
        user in proptest::option::of(0usize..9),
        resource_key in proptest::option::of(tokens(4, 16)),
    ) {
        let mut raw = match shape {
            0 => format!("https://drive.google.com/file/d/{}/view", id),
            1 => format!("https://drive.google.com/drive/folders/{}", id),
            2 => match &user {
                Some(n) => format!("https://drive.google.com/drive/u/{}/folders/{}", n, id),
                None => format!("https://drive.google.com/drive/folders/{}", id),
            },
            3 => format!("https://drive.google.com/open?id={}", id),
            _ => format!("https://drive.google.com/uc?export=download&id={}", id),
        };
        if let Some(rk) = &resource_key {
            raw.push(if raw.contains('?') { '&' } else { '?' });
            raw.push_str(&format!("resourcekey={}", rk));
        }
        let got = resolve_offline(&raw).expect("resolves");
        prop_assert_eq!(got.kind.as_str(), KIND_DRIVE);
        prop_assert_eq!(got.id.as_str(), id.as_str());
        prop_assert!(got.url.starts_with("https://drive.google.com/open?id="), "url {:?}", got.url);
        prop_assert!(!got.url.contains("/view"), "url {:?} keeps the page path", got.url);
    }

    /// Portal links (<host>/d/<token>) and the two short-link hosts look like
    /// links without being opened: that is what keeps a copied link from
    /// spending a one-time download.
    #[test]
    fn portal_and_short_links_look_like_links(
        token in tokens(8, 24),
        host in select(vec!["portal.example.com", "files.example.org", "example.net"]),
        code in alnums(4, 12),
        which in 0usize..3,
    ) {
        let raw = match which {
            0 => format!("https://{}/d/{}", host, token),
            1 => format!("https://f.io/{}", code),
            _ => format!("https://we.tl/{}", code),
        };
        prop_assert!(looks_like_link(&raw), "{:?} should look like a link", raw);
    }

    /// A Smash link is recognised from its host alone, with or without a scheme,
    /// on the service's own domain or a sender's subdomain.
    #[test]
    fn smash_links_look_like_links(
        name in tokens(1, 20),
        sub in proptest::option::of(alnums(2, 8)),
        scheme in any::<bool>(),
    ) {
        let host = match &sub {
            Some(s) => format!("{}.fromsmash.com", s.to_lowercase()),
            None => "fromsmash.com".to_string(),
        };
        let raw = if scheme { format!("https://{}/{}", host, name) } else { format!("{}/{}", host, name) };
        prop_assert!(looks_like_link(&raw), "{:?} should look like a link", raw);
    }

    /// Near-misses: one element of each provider's grammar broken. None of them
    /// is a link, so none of them is opened.
    #[test]
    fn near_miss_links_are_not_links(id in uuid(), which in 0usize..12) {
        let broken_id: String = id
            .chars()
            .enumerate()
            .map(|(i, c)| if i == 4 { 'g' } else { c })
            .collect();
        let short: String = id.chars().take(7).collect();
        let raw = match which {
            0 => format!("https://next.frame.io/share/{}", &id[..35]),
            1 => format!("https://app.frame.io/reviews/{}", broken_id),
            2 => "https://wetransfer.com/downloads".to_string(),
            3 => "https://www.dropbox.com/home".to_string(),
            4 => format!("https://drive.google.com/file/d/{}", &id[..9]),
            5 => format!("https://example.com/d/{}", short),
            6 => "https://fromsmash.com/".to_string(),
            7 => "https://example.com/reset?token=abc".to_string(),
            8 => "https://wetransfer.com/downloads/abc-def/0123abcd".to_string(),
            9 => "https://wetransfer.com/downloads/a/b/c/d".to_string(),
            10 => "https://www.dropbox.com/scl/fo/onlyakey".to_string(),
            _ => "https://example.com/d/AbCd12.4fg".to_string(),
        };
        prop_assert!(!looks_like_link(&raw), "{:?} should not look like a link", raw);
    }

    /// Arbitrary noise never looks like a link, so a copied chunk of text is
    /// never opened.
    #[test]
    fn junk_is_never_a_link(s in prop::collection::vec(select(NOISY), 0..20)) {
        let raw = s.join("");
        prop_assert!(!looks_like_link(&raw), "junk {:?} looks like a link", raw);
    }

    /// Noise that cannot be a host name at all never resolves either: it is
    /// refused in words, without opening it. (A run of digits is left out here:
    /// the URL parser reads one as an IP address, and `resolve_link` would try
    /// to open it.)
    #[test]
    fn junk_that_cannot_be_a_host_never_resolves(s in prop::collection::vec(select(NOISY), 0..20)) {
        let raw = s.join("");
        prop_assume!(!raw.trim().chars().all(|c| c.is_ascii_digit()));
        prop_assume!(parses_with_dotted_host(&raw) == false);
        match resolve_offline(&raw) {
            Ok(l) => prop_assert!(false, "junk {:?} resolved to {:?}", raw, l),
            Err(e) => prop_assert!(e.contains("not a Frame.io"), "junk {:?} failed with {:?}", raw, e),
        }
    }

    /// Surrounding whitespace never changes the answer: the parsers trim.
    #[test]
    fn whitespace_does_not_change_what_a_link_is(
        s in prop::collection::vec("[a-zA-Z0-9_:/?#&=.-]", 0..24),
        which in 0usize..3,
    ) {
        let core: String = s.join("");
        let padded = match which {
            0 => format!("  {}  ", core),
            1 => format!("\t{}\n", core),
            _ => format!("\r\n{}\r\n", core),
        };
        prop_assert_eq!(looks_like_link(&padded), looks_like_link(&core), "{:?} vs {:?}", padded, core);
    }
}
