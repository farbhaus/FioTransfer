//! Legacy Frame.io review links (app.frame.io/reviews/<id>).

use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{
    dedupe_paths, encode_query, netx_sleep, path_join, query_escape, read_limited, safe_name, File, Info,
    Lister, Source, ERR_BAD_PASSPHRASE, ERR_INVALID_SHARE, ERR_PASSPHRASE,
};
use crate::netx;

/// The Frame.io legacy (v2) API used by app.frame.io review links
/// (overridable for tests).
pub fn legacy_api() -> String {
    std::env::var("FIO_LEGACY_API").unwrap_or_else(|_| "https://api.frame.io/v2".to_string())
}

/// LegacyClient reads legacy review links (app.frame.io/reviews/<id>) the way
/// the legacy review page does: anonymous requests scoped by x-review-link-id.
#[derive(Debug, Clone, Default)]
pub struct LegacyClient {
    pub review_id: String,
    pub passphrase: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LegacyAsset {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub type_: String, // file | folder | version_stack
    #[serde(default)]
    pub original: Option<String>,
    #[serde(default)]
    pub filesize: i64,
    #[serde(default)]
    pub index: f64,
    #[serde(rename = "cover_asset", default)]
    pub cover_asset: Option<Box<LegacyAsset>>,
}

/// A response the API refused with a status, kept so callers can tell an
/// authentication problem from a missing link.
#[derive(Debug, Clone)]
pub(crate) struct HttpStatusError {
    pub status: u16,
    pub body: String,
}

impl std::fmt::Display for HttpStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame.io returned {}: {}", self.status, self.body)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum LErr {
    Status(HttpStatusError),
    Msg(String),
}

impl LErr {
    pub fn msg(s: impl Into<String>) -> LErr {
        LErr::Msg(s.into())
    }
}

impl std::fmt::Display for LErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LErr::Status(e) => write!(f, "{}", e),
            LErr::Msg(s) => write!(f, "{}", s),
        }
    }
}

type LResult<T> = Result<T, LErr>;

impl LegacyClient {
    async fn get_raw(
        &self,
        cx: &CancellationToken,
        p: &str,
        q: &[(String, String)],
    ) -> LResult<(reqwest::header::HeaderMap, Vec<u8>)> {
        let mut u = format!("{}{}", legacy_api(), p);
        if !q.is_empty() {
            u.push('?');
            u.push_str(&encode_query(q));
        }
        let mut last_err = String::new();
        for attempt in 0..6 {
            if attempt > 0 {
                netx_sleep(cx, netx::backoff(attempt)).await.map_err(LErr::Msg)?;
            }
            let mut rb = netx::api_client()
                .get(&u)
                .header("Accept", "application/json")
                .header("x-review-link-id", &self.review_id);
            if !self.passphrase.is_empty() {
                rb = rb.header("x-password", query_escape(&self.passphrase));
            }
            let req = match rb.build() {
                Ok(r) => r,
                Err(e) => return Err(LErr::msg(e.to_string())),
            };
            let resp = match netx::api_client().execute(req).await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    continue;
                }
            };
            let status = resp.status().as_u16();
            let headers = resp.headers().clone();
            let data = read_limited(resp, 64 << 20).await;
            if status == 429 || status >= 500 {
                last_err = format!("Frame.io returned {}", status);
                continue;
            }
            if status >= 300 {
                let body = String::from_utf8_lossy(&data).to_string();
                let body: String = body.chars().take(200).collect();
                return Err(LErr::Status(HttpStatusError { status, body }));
            }
            return Ok((headers, data));
        }
        Err(LErr::Msg(last_err))
    }

    async fn get<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        p: &str,
        q: &[(String, String)],
    ) -> LResult<(reqwest::header::HeaderMap, T)> {
        let (h, data) = self.get_raw(cx, p, q).await?;
        let out = serde_json::from_slice(&data).map_err(|e| LErr::msg(e.to_string()))?;
        Ok((h, out))
    }

    fn map_auth_err(&self, err: LErr) -> LErr {
        if let LErr::Status(he) = &err {
            match he.status {
                401 | 403 => {
                    if self.passphrase.is_empty() {
                        return LErr::msg(ERR_PASSPHRASE);
                    }
                    return LErr::msg(ERR_BAD_PASSPHRASE);
                }
                404 | 410 => return LErr::msg(ERR_INVALID_SHARE),
                _ => {}
            }
        }
        err
    }

    /// pages fetches a paginated legacy collection (page/total-pages headers).
    async fn pages<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        p: &str,
        page_size: usize,
    ) -> LResult<Vec<T>> {
        let mut all: Vec<T> = Vec::new();
        let mut page = 1usize;
        loop {
            let q = vec![
                ("page".to_string(), page.to_string()),
                ("page_size".to_string(), page_size.to_string()),
            ];
            let (h, batch): (reqwest::header::HeaderMap, Vec<T>) =
                match self.get(cx, p, &q).await {
                    Ok(v) => v,
                    Err(e) => return Err(self.map_auth_err(e)),
                };
            let n = batch.len();
            all.extend(batch);
            let total = h
                .get("total-pages")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok());
            if n == 0 || page > 10000 {
                return Ok(all);
            }
            if let Some(t) = total {
                if page as i64 >= t {
                    return Ok(all);
                }
            } else if n < page_size {
                // no page count: a short page is the last
                return Ok(all);
            }
            page += 1;
        }
    }
}

#[async_trait::async_trait]
impl Source for LegacyClient {
    async fn info(&self, cx: &CancellationToken) -> Result<Info, String> {
        #[derive(Debug, Default, Deserialize)]
        struct ReviewLink {
            #[serde(default)]
            id: String,
            #[serde(default)]
            name: String,
            #[serde(rename = "is_active", default)]
            is_active: Option<bool>,
            #[serde(rename = "enable_downloading", default)]
            enable_downloading: Option<bool>,
            #[serde(default)]
            has_password: bool,
            #[serde(rename = "expires_at", default)]
            expires_at: Option<String>,
        }
        let p = format!("/review_links/{}", self.review_id);
        let r: ReviewLink = match self.get(cx, &p, &[]).await {
            Ok((_, r)) => r,
            Err(e @ LErr::Status(_)) => {
                if let LErr::Status(he) = &e {
                    if he.status == 404 || he.status == 401 || he.status == 403 {
                        return Err(ERR_INVALID_SHARE.to_string());
                    }
                }
                return Err(e.to_string());
            }
            Err(e) => return Err(e.to_string()),
        };
        if r.is_active == Some(false) {
            return Err(ERR_INVALID_SHARE.to_string());
        }
        if let Some(exp) = &r.expires_at {
            if !exp.is_empty() {
                if let Some(t) = super::parse_rfc3339(exp) {
                    if super::now_unix() > t {
                        return Err(ERR_INVALID_SHARE.to_string());
                    }
                }
            }
        }
        if r.has_password {
            // Check the passphrase by touching the item list.
            let q = vec![("page".to_string(), "1".to_string()), ("page_size".to_string(), "1".to_string())];
            let _touched: Vec<serde_json::Value> = match self.get(cx, &format!("{}/items/shared", p), &q).await {
                Ok((_, v)) => v,
                Err(e) => return Err(self.map_auth_err(e).to_string()),
            };
        }
        Ok(Info {
            share_id: r.id,
            name: r.name,
            downloading_enabled: r.enable_downloading.unwrap_or(true),
        })
    }

    async fn list(
        &self,
        cx: &CancellationToken,
        progress: &(dyn Fn(usize) + Send + Sync),
    ) -> Result<Vec<File>, String> {
        #[derive(Debug, Default, Deserialize)]
        struct Item {
            #[serde(default)]
            asset: Option<LegacyAsset>,
        }
        let l = Lister::new(cx, progress);
        let p = format!("/review_links/{}/items/shared", self.review_id);
        let items: Vec<Item> = l
            .call(async {
                self.pages::<Item>(l.token(), &p, 100)
                    .await
                    .map_err(|e| self.map_auth_err(e).to_string())
            })
            .await?;
        let top: Vec<LegacyAsset> = items.into_iter().filter_map(|it| it.asset).collect();
        let mut files = self.walk(&l, &top, String::new(), 0).await?;
        dedupe_paths(&mut files);
        Ok(files)
    }

    async fn fresh_url(&self, cx: &CancellationToken, asset: &str) -> Result<String, String> {
        let a: LegacyAsset = match self.get(cx, &format!("/assets/{}", asset), &[]).await {
            Ok((_, a)) => a,
            Err(e) => return Err(self.map_auth_err(e).to_string()),
        };
        match a.original {
            Some(u) if !u.is_empty() => Ok(u),
            _ => Err("could not refresh the download link for this file".to_string()),
        }
    }

    /// MarkDownloaded is a no-op for legacy links.
    async fn mark_downloaded(&self, cx: &CancellationToken, _ids: &[String]) -> Result<(), String> {
        let _ = cx;
        Ok(())
    }
}

impl LegacyClient {
    /// walk lists assets in order, descending into folders and picking each
    /// version stack's newest version. Folders (and stacks without a usable
    /// cover) need a request each; those run in parallel.
    fn walk<'a>(
        &'a self,
        l: &'a Lister<'_>,
        assets: &'a [LegacyAsset],
        prefix: String,
        depth: usize,
    ) -> futures::future::BoxFuture<'a, Result<Vec<File>, String>> {
        Box::pin(async move {
            if depth > 64 {
                let e = "folder nesting too deep".to_string();
                l.fail(&e);
                return Err(e);
            }
            let mut parts: Vec<Vec<File>> = vec![Vec::new(); assets.len()];
            let mut fetch: Vec<usize> = Vec::new();
            for (i, a) in assets.iter().enumerate() {
                let needs_cover = a.type_ == "version_stack" && a.cover_asset.as_ref().map(|c| c.original.is_none()).unwrap_or(true);
                if a.type_ == "folder" || needs_cover {
                    fetch.push(i);
                } else if a.type_ == "version_stack" {
                    parts[i] = vec![legacy_file(a.cover_asset.as_deref().unwrap_or(a), &prefix)];
                } else {
                    parts[i] = vec![legacy_file(a, &prefix)];
                }
            }
            l.found(assets.len() - fetch.len());
            let mut futs = Vec::with_capacity(fetch.len());
            for &i in &fetch {
                let a = &assets[i];
                let prefix = prefix.clone();
                futs.push(async move {
                    let p = format!("/assets/{}/children", a.id);
                    let kids: Vec<LegacyAsset> = l
                        .call(async {
                            self.pages::<LegacyAsset>(l.token(), &p, 200)
                                .await
                                .map_err(|e| self.map_auth_err(e).to_string())
                        })
                        .await?;
                    if a.type_ == "folder" {
                        let sub = path_join(&prefix, &safe_name(&a.name));
                        return Ok::<(usize, Vec<File>), String>((i, self.walk(l, &kids, sub, depth + 1).await?));
                    }
                    let mut head: Option<&LegacyAsset> = a.cover_asset.as_deref();
                    for k in &kids {
                        if head.map(|h| h.original.is_none() || k.index > h.index).unwrap_or(true) {
                            head = Some(k);
                        }
                    }
                    match head {
                        Some(h) => {
                            l.found(1);
                            Ok((i, vec![legacy_file(h, &prefix)]))
                        }
                        None => Ok::<(usize, Vec<File>), String>((i, Vec::new())),
                    }
                });
            }
            let results = futures::future::join_all(futs).await;
            if let Some(c) = l.cause() {
                return Err(c);
            }
            for r in results {
                let (i, files) = r?;
                parts[i] = files;
            }
            Ok(parts.into_iter().flatten().collect())
        })
    }
}

fn legacy_file(a: &LegacyAsset, prefix: &str) -> File {
    let mut f = File {
        asset_id: a.id.clone(),
        rel_path: path_join(prefix, &safe_name(&a.name)),
        size: a.filesize,
        url: String::new(),
        problem: String::new(),
    };
    match &a.original {
        Some(u) => f.url = u.clone(),
        None => f.problem = "original not available".to_string(),
    }
    f
}
