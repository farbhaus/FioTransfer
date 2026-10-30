//! Dropbox shared files and folders.

use once_cell::sync::Lazy;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::{
    dedupe_paths, encode_query, netx_sleep, parse_link, path_join, path_parts, rel_path, safe_name, send,
    File, Info, Link, Lister, Source, ERR_INVALID_SHARE, KIND_DROPBOX, WEB_UA,
};
use crate::netx;

/// DropboxWeb is the Dropbox web site, whose shared-link pages list folders
/// this way (overridable for tests).
pub fn dropbox_web() -> String {
    std::env::var("FIO_DROPBOX_WEB").unwrap_or_else(|_| "https://www.dropbox.com".to_string())
}

/// dropboxLink recognises a Dropbox shared link: /scl/fo/<key>/<hash> and
/// /sh/<key>/<hash> (folders), /scl/fi/<key>/<name> and /s/<key>/<name> (files).
/// The link keeps only its rlkey, which is what grants access.
pub(crate) fn dropbox_link(s: &str) -> Option<Link> {
    let u = parse_link(s)?;
    let host = u.host_str().unwrap_or("");
    if host != "dropbox.com" && host != "www.dropbox.com" {
        return None;
    }
    let p = path_parts(&u);
    let keep: Vec<String> = if p.len() >= 4 && p[0] == "scl" && (p[1] == "fo" || p[1] == "fi") {
        p[..4].to_vec()
    } else if p.len() >= 3 && (p[0] == "sh" || p[0] == "s") {
        p[..3].to_vec()
    } else {
        return None;
    };
    let mut url = format!("https://www.dropbox.com/{}", keep.join("/"));
    if let Some((_, rl)) = u.query_pairs().find(|(k, _)| k == "rlkey") {
        if !rl.is_empty() {
            url.push('?');
            url.push_str(&encode_query(&[("rlkey".to_string(), rl.to_string())]));
        }
    }
    let key = if keep[0] == "scl" { keep[2].clone() } else { keep[1].clone() };
    Some(Link { kind: KIND_DROPBOX.to_string(), id: key, url })
}

/// dropboxSession holds the cookies and CSRF token ("t") the web page gets on
/// its first visit.
#[derive(Debug, Default)]
struct DropboxSession {
    client: Option<reqwest::Client>,
    cookies: String,
    token: String,
}

static DROPBOX_SESSION: Lazy<tokio::sync::Mutex<DropboxSession>> =
    Lazy::new(|| tokio::sync::Mutex::new(DropboxSession::default()));

/// dropboxClient returns the session, starting one when there is none or the
/// caller's token was refused.
async fn dropbox_client(cx: &CancellationToken, stale: &str) -> Result<(reqwest::Client, String, String), String> {
    let mut sess = DROPBOX_SESSION.lock().await;
    if let Some(c) = sess.client.clone() {
        if sess.token != stale {
            return Ok((c, sess.cookies.clone(), sess.token.clone()));
        }
    }
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let web = dropbox_web();
    let mut last_err = String::new();
    for attempt in 0..6 {
        let req = c.head(format!("{}/", web)).header("User-Agent", WEB_UA).build();
        let req = match req {
            Ok(r) => r,
            Err(e) => return Err(e.to_string()),
        };
        match c.execute(req).await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if status != 429 && status < 500 {
                    // Go keeps the cookies in a jar; the listing page also takes
                    // the token from the form, so it is all this session needs.
                    let mut cookies: Vec<String> = Vec::new();
                    let mut token = String::new();
                    for v in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                        if let Ok(s) = v.to_str() {
                            let pair = s.split(';').next().unwrap_or("").trim().to_string();
                            if let Some(v) = pair.strip_prefix("t=") {
                                token = v.to_string();
                            }
                            if !pair.is_empty() {
                                cookies.push(pair);
                            }
                        }
                    }
                    if token.is_empty() {
                        return Err(format!("Dropbox didn't start a session ({})", status));
                    }
                    sess.client = Some(c.clone());
                    sess.cookies = cookies.join("; ");
                    sess.token = token.clone();
                    return Ok((c, sess.cookies.clone(), token));
                }
                last_err = format!("Dropbox returned {}", status);
            }
            Err(e) => last_err = e.to_string(),
        }
        netx_sleep(cx, netx::backoff(attempt + 1)).await?;
    }
    Err(last_err)
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DropboxPage {
    #[serde(default)]
    folder: Option<DropboxFolder>,
    #[serde(default)]
    entries: Vec<DropboxEntry>,
    #[serde(rename = "share_tokens", default)]
    share_tokens: Vec<DropboxShareToken>,
    #[serde(rename = "has_more_entries", default)]
    has_more: bool,
    #[serde(rename = "next_request_voucher", default)]
    voucher: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DropboxFolder {
    #[serde(default)]
    filename: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DropboxEntry {
    #[serde(default)]
    filename: String,
    #[serde(rename = "is_dir", default)]
    is_dir: bool,
    #[serde(default)]
    href: String,
    #[serde(default)]
    bytes: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DropboxShareToken {
    #[serde(rename = "secureHash", default)]
    secure_hash: String,
}

/// DropboxClient reads a shared link the way the Dropbox web page does.
pub struct DropboxClient {
    pub url: String,

    /// The root listing from info, reused by list.
    first: Mutex<Option<DropboxPage>>,
    /// A shared file, from info, with its download URL.
    single: Mutex<Option<File>>,
}

impl DropboxClient {
    pub(crate) fn new(url: String) -> Self {
        DropboxClient { url, first: Mutex::new(None), single: Mutex::new(None) }
    }

    /// parts returns the link's path parts, the query, and whether it is a folder.
    fn parts(&self) -> (Vec<String>, Vec<(String, String)>, bool) {
        let u = match url::Url::parse(&self.url) {
            Ok(u) => u,
            Err(_) => return (Vec::new(), Vec::new(), false),
        };
        let p = path_parts(&u);
        let q: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let folder = !p.is_empty() && (p[0] == "sh" || (p.len() > 1 && p[1] == "fo"));
        (p, q, folder)
    }

    fn link_key(p: &[String]) -> String {
        if p.first().map(|s| s == "scl").unwrap_or(false) {
            p.get(2).cloned().unwrap_or_default()
        } else {
            p.get(1).cloned().unwrap_or_default()
        }
    }

    fn rlkey(q: &[(String, String)]) -> String {
        q.iter().find(|(k, _)| k == "rlkey").map(|(_, v)| v.clone()).unwrap_or_default()
    }

    /// folderPage fetches one page of a folder's listing (sub_path "" = the
    /// shared folder).
    async fn folder_page(
        &self,
        cx: &CancellationToken,
        secure_hash: &str,
        sub_path: &str,
        voucher: &str,
    ) -> Result<DropboxPage, String> {
        let (p, q, _) = self.parts();
        let key = Self::link_key(&p);
        let rlkey = Self::rlkey(&q);
        let (mut client, mut cookies, mut token) = dropbox_client(cx, "").await?;
        let mut refreshed = false;
        loop {
            let mut form: Vec<(String, String)> = vec![
                ("is_xhr".to_string(), "true".to_string()),
                ("t".to_string(), token.clone()),
                ("link_key".to_string(), key.clone()),
                ("secure_hash".to_string(), secure_hash.to_string()),
                ("rlkey".to_string(), rlkey.clone()),
                ("link_type".to_string(), "s".to_string()),
                ("sub_path".to_string(), sub_path.to_string()),
            ];
            if !voucher.is_empty() {
                form.push(("voucher".to_string(), voucher.to_string()));
            }
            let body = encode_query(&form);
            let web = dropbox_web();
            let (status, data) = send(cx, &client, "Dropbox", || {
                client
                    .post(format!("{}/list_shared_link_folder_entries", web))
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .header("Origin", web.clone())
                    .header("User-Agent", WEB_UA)
                    .header("Cookie", cookies.clone())
                    .body(body.clone())
            })
            .await?;
            if status == 403 && !refreshed {
                // the session's token expired
                let (c, ck, t) = dropbox_client(cx, &token).await?;
                client = c;
                cookies = ck;
                token = t;
                refreshed = true;
                continue;
            }
            if status == 404 || status == 410 {
                return Err(ERR_INVALID_SHARE.to_string());
            }
            if status >= 300 {
                return Err(format!("Dropbox returned {}", status));
            }
            let mut pg: DropboxPage = serde_json::from_slice(&data)
                .map_err(|_| "unexpected response from Dropbox".to_string())?;
            if pg.share_tokens.len() != pg.entries.len() {
                pg.share_tokens = Vec::new();
            }
            return Ok(pg);
        }
    }

    fn walk<'a>(
        &'a self,
        l: &'a Lister<'_>,
        secure_hash: String,
        sub_path: String,
        prefix: String,
        depth: usize,
    ) -> futures::future::BoxFuture<'a, Result<Vec<File>, String>> {
        Box::pin(async move {
            if depth > 64 {
                let e = "folder nesting too deep".to_string();
                l.fail(&e);
                return Err(e);
            }
            let mut pages: Vec<DropboxPage> = Vec::new();
            let mut voucher = String::new();
            loop {
                let first = if sub_path.is_empty() && voucher.is_empty() {
                    l_fetch_first(self)
                } else {
                    None
                };
                let pg = match first {
                    Some(p) => p,
                    None => {
                        let (h, s, v) = (secure_hash.clone(), sub_path.clone(), voucher.clone());
                        l.call(self.folder_page(l.token(), &h, &s, &v)).await?
                    }
                };
                pages.push(pg);
                let last = pages.last().cloned().unwrap_or_default();
                if !last.has_more || last.voucher.is_empty() || pages.len() > 10000 {
                    break;
                }
                voucher = last.voucher.clone();
            }
            // Files in listing order; each subfolder holds its place until it is
            // walked.
            struct Dir {
                hash: String,
                sub: String,
                prefix: String,
            }
            let mut parts: Vec<Vec<File>> = Vec::new();
            let mut dirs: Vec<(usize, Dir)> = Vec::new();
            for pg in &pages {
                for (i, e) in pg.entries.iter().enumerate() {
                    let name = safe_name(&e.filename);
                    if e.is_dir {
                        let mut h = secure_hash.clone();
                        if let Some(t) = pg.share_tokens.get(i) {
                            if !t.secure_hash.is_empty() {
                                h = t.secure_hash.clone();
                            }
                        }
                        dirs.push((
                            parts.len(),
                            Dir { hash: h, sub: format!("{}/{}", sub_path, e.filename), prefix: path_join(&prefix, &name) },
                        ));
                        parts.push(Vec::new());
                        continue;
                    }
                    parts.push(vec![File {
                        asset_id: e.href.clone(),
                        rel_path: path_join(&prefix, &name),
                        size: e.bytes,
                        url: String::new(),
                        problem: String::new(),
                    }]);
                }
            }
            l.found(parts.len().saturating_sub(dirs.len()));
            let futs = dirs.iter().map(|(i, d)| {
                let (i, hash, sub, pfx) = (*i, d.hash.clone(), d.sub.clone(), d.prefix.clone());
                async move { (i, self.walk(l, hash, sub, pfx, depth + 1).await) }
            });
            let results = futures::future::join_all(futs).await;
            if let Some(c) = l.cause() {
                return Err(c);
            }
            for (i, r) in results {
                parts[i] = r?;
            }
            Ok(parts.into_iter().flatten().collect())
        })
    }

    /// fileHead resolves a shared file: its name, size and download URL.
    async fn file_head(&self, cx: &CancellationToken) -> Result<File, String> {
        let resp = fetch_first_byte(cx, &self.url).await?;
        let (p, _, _) = self.parts();
        let mut name = percent_encoding::percent_decode_str(p.last().map(|s| s.as_str()).unwrap_or(""))
            .decode_utf8_lossy()
            .to_string();
        // The /cd/0/inline/ server names every file "unspecified" (only
        // /cd/0/get/ sends the real name), so the link's name stands (2026-10-29).
        if let Some(v) = resp.headers.get(reqwest::header::CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()) {
            if let Some(f) = media_filename(v) {
                if !f.is_empty() && f != "unspecified" {
                    name = f;
                }
            }
        }
        let mut size: i64 = 0;
        if let Some(cr) = resp.headers.get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
            if let Some((_, total)) = cr.rsplit_once('/') {
                size = total.trim().parse::<i64>().unwrap_or(0);
            }
        }
        Ok(File { asset_id: self.url.clone(), rel_path: rel_path(&name), size, url: resp.content_url, problem: String::new() })
    }
}

fn l_fetch_first(c: &DropboxClient) -> Option<DropboxPage> {
    c.first.lock().ok().and_then(|g| g.clone())
}

/// download returns href with dl=1, which serves the file itself.
fn dropbox_download(href: &str) -> Result<String, String> {
    let mut u = url::Url::parse(href).map_err(|e| e.to_string())?;
    let q: Vec<(String, String)> = u.query_pairs().filter(|(k, _)| k != "dl").map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let mut q = q;
    q.push(("dl".to_string(), "1".to_string()));
    u.set_query(Some(&encode_query(&q)));
    Ok(u.to_string())
}

/// dropboxFileUA is the user agent that resolves file links. Dropbox sends curl
/// and Wget to a /cd/0/inline/ URL that serves parallel ranges; browsers and
/// other clients get a /cd/0/get/ URL that refuses all but one or two (403),
/// which held a 390 MB file to 5 MB/s instead of line rate (2026-10-23).
const DROPBOX_FILE_UA: &str = "Wget/1.24.5";

/// A file request's answer: the headers that name the file, and the content
/// server URL that serves it.
struct FirstByte {
    headers: reqwest::header::HeaderMap,
    content_url: String,
}

/// fetchFirstByte asks for a file's first byte, following Dropbox's redirect to
/// its content server. No session is needed.
async fn fetch_first_byte(cx: &CancellationToken, href: &str) -> Result<FirstByte, String> {
    let u = dropbox_download(href)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let mut last_err = String::new();
    for attempt in 0.. {
        match follow(&client, &u).await {
            Ok((chain, resp)) => {
                let status = resp.status().as_u16();
                if (status != 429 && status < 500) || attempt == 5 {
                    let headers = resp.headers().clone();
                    let content_url = content_url(&chain);
                    if status == 404 || status == 410 {
                        return Err(ERR_INVALID_SHARE.to_string());
                    }
                    if status != 200 && status != 206 {
                        return Err(format!("Dropbox returned {}", status));
                    }
                    return Ok(FirstByte { headers, content_url });
                }
                last_err = format!("Dropbox returned {}", status);
            }
            Err(e) => last_err = e,
        }
        netx_sleep(cx, netx::backoff(attempt + 1)).await?;
    }
    Err(last_err)
}

/// follow performs one request, following redirects the way Go's client does
/// (at most 10), and keeps the chain of URLs so a redirect on the content
/// server can be told from the one into it.
async fn follow(client: &reqwest::Client, u: &str) -> Result<(Vec<String>, reqwest::Response), String> {
    let mut chain: Vec<String> = vec![u.to_string()];
    for _ in 0..11 {
        let current = chain.last().cloned().unwrap_or_default();
        let req = client
            .get(&current)
            .header("Range", "bytes=0-0")
            .header("User-Agent", DROPBOX_FILE_UA)
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.execute(req).await.map_err(|e| e.to_string())?;
        if resp.status().is_redirection() {
            if let Some(loc) = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()) {
                let base = url::Url::parse(&current).map_err(|e| e.to_string())?;
                let next = base.join(loc).map_err(|e| e.to_string())?;
                chain.push(next.to_string());
                continue;
            }
        }
        return Ok((chain, resp));
    }
    Err("stopped after 10 redirects".to_string())
}

/// contentURL is the first URL on the content server, before any redirect
/// there. MXF files go on from /cd/0/inline/ to /cd/0/inline2/, which serves
/// only the user agent that followed that redirect (404 to the engine's, so 28
/// MXF files of a 105-file folder failed, 2026-10-23); the engine follows it
/// itself.
fn content_url(chain: &[String]) -> String {
    let host_of = |s: &str| -> String {
        match url::Url::parse(s) {
            Ok(u) => format!("{}:{}", u.host_str().unwrap_or(""), u.port_or_known_default().unwrap_or(0)),
            Err(_) => String::new(),
        }
    };
    let mut i = chain.len().saturating_sub(1);
    while i > 0 && host_of(&chain[i - 1]) == host_of(&chain[i]) {
        i -= 1;
    }
    chain.get(i).cloned().unwrap_or_default()
}

/// media_filename reads a Content-Disposition filename parameter the way
/// Go's mime.ParseMediaType does.
fn media_filename(v: &str) -> Option<String> {
    let mut simple: Option<String> = None;
    let mut extended: Option<String> = None;
    let mut parts = v.split(';');
    parts.next();
    for p in parts {
        let p = p.trim();
        if let Some((k, val)) = p.split_once('=') {
            let k = k.trim().to_ascii_lowercase();
            let val = val.trim();
            if k == "filename" {
                simple = Some(unquote(val));
            } else if k == "filename*" {
                // RFC 2231: charset'lang'percent-encoded
                let mut it = val.splitn(3, '\'');
                let _charset = it.next();
                let _lang = it.next();
                if let Some(enc) = it.next() {
                    extended = Some(
                        percent_encoding::percent_decode_str(enc).decode_utf8_lossy().to_string(),
                    );
                }
            }
        }
    }
    extended.or(simple)
}

/// unquote removes the quotes and backslash escapes of a quoted-string.
fn unquote(v: &str) -> String {
    if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    v.to_string()
}

#[async_trait::async_trait]
impl Source for DropboxClient {
    async fn info(&self, cx: &CancellationToken) -> Result<Info, String> {
        let (p, _, folder) = self.parts();
        if !folder {
            let f = self.file_head(cx).await?;
            if let Ok(mut g) = self.single.lock() {
                *g = Some(f);
            }
            let share_id = if p.len() >= 2 { p[p.len() - 2].clone() } else { String::new() };
            return Ok(Info { share_id, name: String::new(), downloading_enabled: true });
        }
        let secure_hash = p.last().cloned().unwrap_or_default();
        let pg = self.folder_page(cx, &secure_hash, "", "").await?;
        let info = Info {
            share_id: if p.len() >= 2 { p[p.len() - 2].clone() } else { String::new() },
            name: pg.folder.as_ref().map(|f| f.filename.clone()).unwrap_or_default(),
            downloading_enabled: true,
        };
        if let Ok(mut g) = self.first.lock() {
            *g = Some(pg);
        }
        Ok(info)
    }

    /// List walks a shared folder and returns each file to download on its own
    /// (Dropbox would send the folder as one zip). A shared file is a list of
    /// one.
    async fn list(
        &self,
        cx: &CancellationToken,
        progress: &(dyn Fn(usize) + Send + Sync),
    ) -> Result<Vec<File>, String> {
        let (p, _, folder) = self.parts();
        if !folder {
            let single = self.single.lock().ok().and_then(|g| g.clone());
            let f = match single {
                Some(f) => f,
                None => {
                    self.info(cx).await?;
                    match self.single.lock().ok().and_then(|g| g.clone()) {
                        Some(f) => f,
                        None => return Err(ERR_INVALID_SHARE.to_string()),
                    }
                }
            };
            (progress)(1);
            return Ok(vec![f]);
        }
        let l = Lister::new(cx, progress);
        let secure_hash = p.last().cloned().unwrap_or_default();
        let mut files = self.walk(&l, secure_hash, String::new(), String::new(), 0).await?;
        dedupe_paths(&mut files);
        Ok(files)
    }

    /// FreshURL resolves a file's link (its href) to the content server URL that
    /// serves it.
    async fn fresh_url(&self, cx: &CancellationToken, href: &str) -> Result<String, String> {
        let resp = fetch_first_byte(cx, href).await?;
        Ok(resp.content_url)
    }

    /// MarkDownloaded is a no-op for Dropbox links.
    async fn mark_downloaded(&self, cx: &CancellationToken, _ids: &[String]) -> Result<(), String> {
        let _ = cx;
        Ok(())
    }
}

/// Property tests for the link parser, the client's reading of a link, the
/// listing page's shape and the Content-Disposition / redirect helpers. All of
/// these are pure; the client's requests live behind `Source` and cannot run
/// here.
#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;
    use proptest::sample::select;

    const TOKEN: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_-";

    fn tokens(min: usize, max: usize) -> impl Strategy<Value = String> {
        (min..=max)
            .prop_flat_map(|n| prop::collection::vec(select(TOKEN), n))
            .prop_map(|v| String::from_utf8(v).unwrap())
    }

    proptest! {
        #![proptest_config(ProptestConfig {
        cases: 64,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5EED_5EED),
        ..ProptestConfig::default()
    })]

        /// A folder link asks for the folder the user pasted: the key the parser
        /// reports is the key the listing uses, and only /fo and /sh links are
        /// folders.
        #[test]
        fn client_agrees_with_the_parser(
            seg in select(vec!["scl/fo", "scl/fi", "sh", "s"]),
            key in tokens(3, 20),
            last in tokens(1, 12),
            host in select(vec!["www.dropbox.com", "dropbox.com"]),
        ) {
            let raw = format!("https://{}/{}/{}/{}", host, seg, key, last);
            let l = dropbox_link(&raw).expect("a share link");
            let c = DropboxClient::new(l.url.clone());
            let (p, _q, folder) = c.parts();
            prop_assert_eq!(DropboxClient::link_key(&p), l.id.clone(), "{:?}", l);
            // /sh is a shared folder, /s a shared file; /scl/fo a folder,
            // /scl/fi a file.
            prop_assert_eq!(folder, seg == "sh" || seg.ends_with("fo"));
            prop_assert!(!p.iter().any(|s| s.contains('?')), "{:?}", p);
        }

        /// `dropbox_download` serves the file itself: one dl=1 and nothing else
        /// changed, stable when applied twice.
        #[test]
        fn download_urls_carry_one_dl(
            host in select(vec!["https://www.dropbox.com", "https://dl.dropboxusercontent.com"]),
            seg in tokens(1, 12),
            key in tokens(1, 12),
            rlkey in proptest::option::of(tokens(1, 10)),
            dl in proptest::option::of(select(vec!["0", "1", "yes"])),
        ) {
            let mut raw = format!("{}/{}/{}", host, seg, key);
            let mut q: Vec<String> = Vec::new();
            if let Some(r) = &rlkey {
                q.push(format!("rlkey={}", r));
            }
            if let Some(d) = &dl {
                q.push(format!("dl={}", d));
            }
            if !q.is_empty() {
                raw.push('?');
                raw.push_str(&q.join("&"));
            }
            let once = dropbox_download(&raw).expect("a URL");
            let u = url::Url::parse(&once).expect("parses");
            let pairs: Vec<(String, String)> =
                u.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            prop_assert_eq!(pairs.iter().filter(|(k, _)| k == "dl").count(), 1, "{:?}", once);
            prop_assert!(pairs.iter().any(|(k, v)| k == "dl" && v == "1"), "{:?}", once);
            prop_assert_eq!(
                pairs.iter().filter(|(k, _)| k == "rlkey").count(),
                usize::from(rlkey.is_some()),
                "{:?}",
                once
            );
            let twice = dropbox_download(&once).expect("parses");
            prop_assert_eq!(twice.as_str(), once.as_str());
            prop_assert_eq!(dropbox_download("not a url").is_err(), true);
        }

        /// The content server URL is the first hop on the content host: the
        /// redirect into it is crossed, no redirect after it is.
        #[test]
        fn content_url_is_the_first_hop_on_the_content_host(
            hops in 1usize..6,
            tail in 1usize..4,
        ) {
            let mut chain: Vec<String> =
                (0..hops).map(|i| format!("https://www.dropbox.com/scl/fi/k/n?x={}", i)).collect();
            let first_content = format!("https://content.dropboxapi.com/cd/0/inline/{}/x", 0);
            for i in 0..tail {
                chain.push(format!("https://content.dropboxapi.com/cd/0/inline/{}/x", i));
            }
            let got = content_url(&chain);
            prop_assert_eq!(got.as_str(), first_content.as_str(), "{:?}", chain);
            prop_assert!(chain.contains(&got));
            let all_content: Vec<String> =
                (0..tail).map(|i| format!("https://content.dropboxapi.com/cd/0/inline/{}", i)).collect();
            let all = content_url(&all_content);
            prop_assert_eq!(all.as_str(), all_content[0].as_str());
            let mut one = vec!["https://content.dropboxapi.com/cd/0/inline/x".to_string()];
            let first = content_url(&one);
            prop_assert_eq!(first.as_str(), one[0].as_str());
            // Both hops are on the content host, so the first of them is the
            // one the engine follows itself.
            one.push("https://content.dropboxapi.com/cd/0/inline2/x".to_string());
            let second = content_url(&one);
            prop_assert_eq!(second.as_str(), one[0].as_str());
            prop_assert_eq!(content_url(&[]), String::new());
        }

        /// The Content-Disposition reader takes the quoted filename, unescapes it,
        /// prefers an RFC 2231 filename* and gives nothing when there is no name.
        #[test]
        fn content_disposition_names_are_read(
            name in "[a-zA-Z0-9 ._-]{0,24}",
            quoted in any::<bool>(),
            extended in any::<bool>(),
            size in 0u64..100000,
        ) {
            // Two names, always different: the plain parameter and the RFC 2231
            // one. When both are there the extended form must win.
            let plain = format!("plain-{}", name);
            let shown = if quoted { format!("\"{}\"", plain) } else { plain.clone() };
            let header = if extended {
                let enc: String = name.bytes().map(|b| format!("%{:02X}", b)).collect();
                format!("attachment; size={}; filename={}", size, shown)
                    + &format!("; filename*=UTF-8''{}", enc)
            } else {
                format!("attachment; size={}; filename={}", size, shown)
            };
            let got = media_filename(&header);
            // An unquoted value is trimmed the way an HTTP parameter is; a
            // quoted one keeps its spaces.
            let want = if extended {
                name.clone()
            } else if quoted {
                plain.clone()
            } else {
                plain.trim().to_string()
            };
            prop_assert_eq!(got, Some(want), "{:?}", header);
            prop_assert_eq!(media_filename("attachment"), None);
            prop_assert_eq!(media_filename(""), None);
            // A backslash inside a quoted name is an escape, not part of the name.
            if quoted && !name.contains('"') {
                let header = format!("attachment; filename=\"a\\{}b\"", name);
                let got = media_filename(&header).unwrap_or_default();
                prop_assert_eq!(got, format!("a{}b", name), "{:?}", header);
            }
        }

        /// Every shape of the listing page tolerates junk: an empty object is a
        /// default page, a scalar in the first field is an error, and a page that
        /// says it has more entries keeps its voucher for the next request.
        #[test]
        fn listing_pages_take_junk(body in prop::collection::vec(any::<u8>(), 0..48), shape in 0usize..4) {
            let payload: Vec<u8> = match shape {
                0 => body.clone(),
                1 => b"{}".to_vec(),
                2 => b"[1,2,3]".to_vec(),
                _ => br#"{"entries":[{"filename":"a.mov","is_dir":false,"href":"h","bytes":7}],"share_tokens":[{"secureHash":"s"}],"has_more_entries":true,"next_request_voucher":"v","folder":{"filename":"f"}}"#.to_vec(),
            };
            let r = serde_json::from_slice::<DropboxPage>(&payload);
            match shape {
                1 => prop_assert!(r.is_ok(), "an empty object is a default page"),
                2 => prop_assert!(r.is_err(), "a sequence is not a page"),
                3 => {
                    let pg = r.expect("a page");
                    prop_assert_eq!(pg.entries.len(), 1);
                    prop_assert_eq!(pg.entries[0].filename.as_str(), "a.mov");
                    prop_assert_eq!(pg.entries[0].bytes, 7);
                    prop_assert_eq!(pg.entries[0].href.as_str(), "h");
                    prop_assert!(pg.has_more);
                    prop_assert_eq!(pg.voucher.as_str(), "v");
                    prop_assert_eq!(pg.share_tokens.len(), 1);
                    let folder = pg.folder.map(|f| f.filename).unwrap_or_default();
                    prop_assert_eq!(folder.as_str(), "f");
                }
                _ => prop_assert!(r.is_ok() || r.is_err(), "junk must not panic"),
            }
        }
    }
}
