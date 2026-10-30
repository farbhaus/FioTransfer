
//! A minimal Frame.io V4 REST client.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::auth::AuthManager;
use crate::netx::{self, ApiError};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_BASE_URL: &str = "https://api.frame.io";

// ---------- Errors ----------

#[derive(Debug)]
pub enum ApiErr {
    Status(ApiError),
    Auth(String),
    Cancelled,
    Other(String),
}

impl std::fmt::Display for ApiErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiErr::Status(e) => write!(f, "{}", e),
            ApiErr::Auth(s) => write!(f, "{}", s),
            ApiErr::Cancelled => write!(f, "context canceled"),
            ApiErr::Other(s) => write!(f, "{}", s),
        }
    }
}
impl std::error::Error for ApiErr {}

impl ApiErr {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiErr::Status(e) => Some(e.status),
            _ => None,
        }
    }
    /// Frame.io may still have carried the call out: it was sent, and the reply
    /// was a server error or never came.
    pub fn ambiguous(&self) -> bool {
        match self {
            ApiErr::Status(e) => e.status >= 500 && e.status != 503,
            ApiErr::Other(_) => true,
            _ => false,
        }
    }
}

// ---------- Rate limits ----------

#[derive(Clone, PartialEq)]
struct LimitSpec {
    calls: u32,
    window: Duration,
}

fn documented_limits() -> &'static HashMap<&'static str, LimitSpec> {
    static M: OnceLock<HashMap<&'static str, LimitSpec>> = OnceLock::new();
    M.get_or_init(|| {
        let mut m = HashMap::new();
        let mut add = |k: &'static str, calls: u32, window: Duration| {
            m.insert(k, LimitSpec { calls, window });
        };
        add("POST /v4/accounts/:id/folders/:id/files/local_upload", 5, Duration::from_secs(1));
        add("GET /v4/accounts/:id/files/:id/status", 5, Duration::from_secs(1));
        add("GET /v4/accounts/:id/files/:id", 10, Duration::from_secs(1));
        add("DELETE /v4/accounts/:id/files/:id", 100, Duration::from_secs(60));
        add("POST /v4/accounts/:id/folders/:id/folders", 3, Duration::from_secs(1));
        add("POST /v4/accounts/:id/projects/:id/shares", 10, Duration::from_secs(60));
        m
    })
}

fn default_limit() -> LimitSpec {
    LimitSpec { calls: 100, window: Duration::from_secs(60) }
}

fn is_id_segment(s: &str) -> bool {
    s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Turns "GET /v4/accounts/<uuid>/files/<uuid>?x=1" into "GET /v4/accounts/:id/files/:id".
pub fn route_key(method: &str, raw_url: &str) -> String {
    let mut p = raw_url;
    // Only a real scheme is stripped, and only from the front: a "://" inside a
    // query (a `next=https://…` parameter) is data, not a scheme.
    if let Some(i) = p.find("://") {
        let scheme = &p[..i];
        if !scheme.is_empty() && !scheme.contains(['/', '?', '#']) {
            let rest = &p[i + 3..];
            p = match rest.find(['/', '?', '#']) {
                Some(j) if rest.as_bytes()[j] == b'/' => &rest[j..],
                _ => "/", // no path at all: the key is the root, never the host
            };
        }
    }
    if let Some(i) = p.find(['?', '#']) {
        p = &p[..i];
    }
    let segs: Vec<String> = p.split('/').map(|s| if is_id_segment(s) { ":id".to_string() } else { s.to_string() }).collect();
    format!("{} {}", method, segs.join("/"))
}

/// A token bucket, as golang.org/x/time/rate gives the Go app.
struct Bucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(spec: &LimitSpec) -> Self {
        let per_sec = spec.calls as f64 / spec.window.as_secs_f64() * 0.9;
        let burst = if spec.window > Duration::from_secs(1) {
            (spec.calls / 4).clamp(1, 20) as f64
        } else {
            spec.calls as f64
        };
        Bucket { rate: per_sec, burst, tokens: burst, last: Instant::now() }
    }

    fn wait_time(&mut self) -> Duration {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.rate)
        }
    }
}

struct Limiters(Mutex<HashMap<String, Bucket>>);

impl Limiters {
    fn new() -> Self {
        Limiters(Mutex::new(HashMap::new()))
    }

    fn reserve(&self, key: &str) -> Duration {
        let mut g = self.0.lock().unwrap();
        let spec = documented_limits().get(key).cloned().unwrap_or_else(default_limit);
        g.entry(key.to_string()).or_insert_with(|| Bucket::new(&spec)).wait_time()
    }

    /// Adopts the limit Frame.io reports for this route, if it changed.
    fn observe(&self, key: &str, h: &reqwest::header::HeaderMap) {
        let calls = h.get("x-ratelimit-limit").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u32>().ok());
        let window_ms = h.get("x-ratelimit-window").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        let (Some(calls), Some(window_ms)) = (calls, window_ms) else { return };
        if calls == 0 || window_ms == 0 {
            return;
        }
        let spec = LimitSpec { calls, window: Duration::from_millis(window_ms) };
        let mut g = self.0.lock().unwrap();
        let b = g.entry(key.to_string()).or_insert_with(|| Bucket::new(&spec));
        if b.rate != Bucket::new(&spec).rate {
            *b = Bucket::new(&spec);
        }
    }
}

// ---------- Client ----------

pub struct Client {
    pub auth: Arc<AuthManager>,
    pub base_url: String,
    limits: Limiters,
}

impl Client {
    pub fn new(auth: Arc<AuthManager>) -> Self {
        Client { auth, base_url: DEFAULT_BASE_URL.to_string(), limits: Limiters::new() }
    }

    fn url(&self, path: &str, query: &[(String, String)]) -> String {
        let mut u = if path.starts_with("http") { path.to_string() } else { format!("{}{}", self.base_url, path) };
        if !query.is_empty() {
            let sep = if u.contains('?') { "&" } else { "?" };
            let qs: Vec<String> = query
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}={}",
                        percent_encoding::utf8_percent_encode(k, percent_encoding::NON_ALPHANUMERIC),
                        percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)
                    )
                })
                .collect();
            u.push_str(sep);
            u.push_str(&qs.join("&"));
        }
        u
    }

    /// One API call: the body of a successful reply, or why it failed.
    pub async fn send(
        &self,
        cx: &CancellationToken,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<Vec<u8>, ApiErr> {
        let u = self.url(path, query);
        let body_bytes = match &body {
            Some(b) => Some(serde_json::to_vec(b).map_err(|e| ApiErr::Other(e.to_string()))?),
            None => None,
        };
        let key = route_key(method, &u);
        // A POST that was sent but got no clear answer may still have been
        // carried out; repeating it could create a duplicate folder or file.
        let repeatable = method != "POST";
        let mut refreshed = false;
        let mut attempt: u32 = 0;
        loop {
            let wait = self.limits.reserve(&key);
            if !wait.is_zero() {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = cx.cancelled() => return Err(ApiErr::Cancelled),
                }
            }
            let tok = self.auth.token(cx).await.map_err(ApiErr::Auth)?;
            let verb = reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|e| ApiErr::Other(format!("bad HTTP method {:?}: {}", method, e)))?;
            let mut rb = netx::api_client()
                .request(verb, &u)
                .header("Authorization", format!("Bearer {}", tok))
                .header("Accept", "application/json");
            if let Some(b) = &body_bytes {
                rb = rb.header("Content-Type", "application/json").body(b.clone());
            }
            let resp = tokio::select! {
                r = rb.send() => r,
                _ = cx.cancelled() => return Err(ApiErr::Cancelled),
            };
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    let not_sent = e.is_connect();
                    if cx.is_cancelled() || attempt >= 5 || (!repeatable && !not_sent) {
                        return Err(ApiErr::Other(e.to_string()));
                    }
                    let d = netx::backoff(attempt);
                    attempt += 1;
                    netx::sleep(cx, d).await.map_err(|_| ApiErr::Cancelled)?;
                    continue;
                }
            };
            let status = resp.status().as_u16();
            self.limits.observe(&key, resp.headers());
            let retry_after = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let data = tokio::select! {
                d = resp.bytes() => d.map(|b| b.to_vec()).unwrap_or_default(),
                _ = cx.cancelled() => return Err(ApiErr::Cancelled),
            };
            if status == 401 && !refreshed {
                refreshed = true;
                self.auth.force_refresh(cx).await.map_err(ApiErr::Auth)?;
                continue;
            }
            if status == 429 || status >= 500 {
                // 429 and 503 mean the request was turned away, so even a POST can be repeated.
                if attempt >= 6 || (!repeatable && status != 429 && status != 503) {
                    return Err(ApiErr::Status(ApiError { status, body: netx::trim_body(&data) }));
                }
                let mut d = netx::backoff(attempt);
                if let Some(ra) = retry_after {
                    if ra > 0 {
                        d = Duration::from_secs(ra);
                    }
                }
                attempt += 1;
                netx::sleep(cx, d).await.map_err(|_| ApiErr::Cancelled)?;
                continue;
            }
            if status >= 300 {
                return Err(ApiErr::Status(ApiError { status, body: netx::trim_body(&data) }));
            }
            return Ok(data);
        }
    }

    pub async fn get<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        path: &str,
        query: &[(String, String)],
    ) -> Result<T, ApiErr> {
        let data = self.send(cx, "GET", path, query, None).await?;
        serde_json::from_slice(&data).map_err(|e| ApiErr::Other(e.to_string()))
    }

    /// A call whose reply is wrapped as `{"data": …}`.
    pub async fn data<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<T, ApiErr> {
        #[derive(Deserialize)]
        struct Envelope<T> {
            data: T,
        }
        let raw = self.send(cx, method, path, query, body).await?;
        let e: Envelope<T> = serde_json::from_slice(&raw).map_err(|err| ApiErr::Other(err.to_string()))?;
        Ok(e.data)
    }

    /// A call with no reply body.
    pub async fn call(
        &self,
        cx: &CancellationToken,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<(), ApiErr> {
        self.send(cx, method, path, query, body).await.map(|_| ())
    }

    /// Follows cursor pagination and collects all items.
    pub async fn list<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        path: &str,
        mut query: Vec<(String, String)>,
    ) -> Result<Vec<T>, ApiErr> {
        #[derive(Deserialize)]
        struct Page<T> {
            #[serde(default = "Vec::new")]
            data: Vec<T>,
            #[serde(default)]
            links: Links,
        }
        #[derive(Deserialize, Default)]
        struct Links {
            #[serde(default)]
            next: Option<String>,
        }
        let mut all: Vec<T> = Vec::new();
        let mut next = path.to_string();
        while !next.is_empty() {
            let page: Page<T> = self.get(cx, &next, &query).await?;
            all.extend(page.data);
            next = String::new();
            if let Some(n) = page.links.next {
                if !n.is_empty() {
                    next = n;
                    // The next link only carries the cursor; re-apply our other params.
                    if let Ok(mut pu) = url::Url::parse(&next) {
                        let mut npairs: Vec<(String, String)> = pu.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
                        for (k, v) in query.iter() {
                            if !npairs.iter().any(|(nk, _)| nk == k) {
                                npairs.push((k.clone(), v.clone()));
                            }
                        }
                        pu.set_query(None);
                        pu.query_pairs_mut().extend_pairs(npairs);
                        next = pu.to_string();
                        query = Vec::new();
                    }
                }
            }
        }
        Ok(all)
    }

    // ---------- Endpoints ----------

    pub async fn me(&self, cx: &CancellationToken) -> Result<User, ApiErr> {
        self.data(cx, "GET", "/v4/me", &[], None).await
    }

    pub async fn accounts(&self, cx: &CancellationToken) -> Result<Vec<Account>, ApiErr> {
        self.list(cx, "/v4/accounts", vec![("page_size".into(), "100".into())]).await
    }

    pub async fn workspaces(&self, cx: &CancellationToken, acct: &str) -> Result<Vec<Workspace>, ApiErr> {
        self.list(cx, &format!("/v4/accounts/{}/workspaces", acct), vec![("page_size".into(), "100".into())]).await
    }

    pub async fn projects(&self, cx: &CancellationToken, acct: &str, ws: &str) -> Result<Vec<Project>, ApiErr> {
        self.list(
            cx,
            &format!("/v4/accounts/{}/workspaces/{}/projects", acct, ws),
            vec![("page_size".into(), "100".into()), ("sort".into(), "name_asc".into())],
        )
        .await
    }

    pub async fn children(&self, cx: &CancellationToken, acct: &str, folder: &str) -> Result<Vec<Asset>, ApiErr> {
        self.list(
            cx,
            &format!("/v4/accounts/{}/folders/{}/children", acct, folder),
            vec![("page_size".into(), "500".into()), ("sort".into(), "name_asc".into())],
        )
        .await
    }

    pub async fn create_folder(&self, cx: &CancellationToken, acct: &str, parent: &str, name: &str) -> Result<Asset, ApiErr> {
        self.data(
            cx,
            "POST",
            &format!("/v4/accounts/{}/folders/{}/folders", acct, parent),
            &[],
            Some(json!({"data": {"name": name}})),
        )
        .await
    }

    pub async fn create_local_upload(
        &self,
        cx: &CancellationToken,
        acct: &str,
        folder: &str,
        name: &str,
        size: i64,
    ) -> Result<LocalUpload, ApiErr> {
        let up: LocalUpload = self
            .data(
                cx,
                "POST",
                &format!("/v4/accounts/{}/folders/{}/files/local_upload", acct, folder),
                &[],
                Some(json!({"data": {"name": name, "file_size": size}})),
            )
            .await?;
        if up.upload_urls.is_empty() {
            return Err(ApiErr::Other("Frame.io returned no upload URLs".into()));
        }
        Ok(up)
    }

    /// Deletes a project on Frame.io. Nothing local is touched: this is the
    /// one call the Space view makes, and it never runs `rm`.
    pub async fn delete_project(&self, cx: &CancellationToken, acct: &str, project: &str) -> Result<(), ApiErr> {
        self.call(cx, "DELETE", &format!("/v4/accounts/{}/projects/{}", acct, project), &[], None).await
    }

    pub async fn delete_file(&self, cx: &CancellationToken, acct: &str, file: &str) -> Result<(), ApiErr> {
        self.call(cx, "DELETE", &format!("/v4/accounts/{}/files/{}", acct, file), &[], None).await
    }

    pub async fn file_status(&self, cx: &CancellationToken, acct: &str, file: &str) -> Result<FileStatus, ApiErr> {
        self.data(cx, "GET", &format!("/v4/accounts/{}/files/{}/status", acct, file), &[], None).await
    }

    pub async fn project(&self, cx: &CancellationToken, acct: &str, id: &str) -> Result<Project, ApiErr> {
        self.data(cx, "GET", &format!("/v4/accounts/{}/projects/{}", acct, id), &[], None).await
    }

    pub async fn folder(&self, cx: &CancellationToken, acct: &str, id: &str) -> Result<Asset, ApiErr> {
        self.data(cx, "GET", &format!("/v4/accounts/{}/folders/{}", acct, id), &[], None).await
    }

    pub async fn search(&self, cx: &CancellationToken, acct: &str, query: &str, n: usize) -> Result<Vec<SearchHit>, ApiErr> {
        let data: Vec<SearchHit> = self
            .data(
                cx,
                "POST",
                &format!("/v4/accounts/{}/search", acct),
                &[("page_size".to_string(), n.to_string())],
                Some(json!({
                    "query": query,
                    "engine": "lexical",
                    "filters": {"projects": true, "folders": true, "files_and_version_stacks": false}
                })),
            )
            .await?;
        Ok(data)
    }

    /// Makes a public share link (downloads allowed) to up to 100 assets.
    pub async fn create_share(
        &self,
        cx: &CancellationToken,
        acct: &str,
        project: &str,
        name: &str,
        assets: &[String],
    ) -> Result<Share, ApiErr> {
        let s: Share = self
            .data(
                cx,
                "POST",
                &format!("/v4/accounts/{}/projects/{}/shares", acct, project),
                &[],
                Some(json!({"data": {
                    "type": "asset", "name": name, "access": "public",
                    "asset_ids": assets, "downloading_enabled": true
                }})),
            )
            .await?;
        if s.short_url.is_empty() {
            return Err(ApiErr::Other("Frame.io returned no share link".into()));
        }
        Ok(s)
    }

    pub async fn version_stack_children(&self, cx: &CancellationToken, acct: &str, stack: &str) -> Result<Vec<Asset>, ApiErr> {
        self.list(
            cx,
            &format!("/v4/accounts/{}/version_stacks/{}/children", acct, stack),
            vec![("page_size".into(), "100".into())],
        )
        .await
    }

    /// A signed URL for a file's original. An error with status 403 means the
    /// user may not download it, or Frame.io hasn't finished processing the
    /// upload yet.
    pub async fn download_url(&self, cx: &CancellationToken, acct: &str, file: &str) -> Result<String, ApiErr> {
        #[derive(Deserialize)]
        struct R {
            data: D,
        }
        #[derive(Deserialize)]
        struct D {
            #[serde(default)]
            media_links: ML,
        }
        #[derive(Deserialize, Default)]
        struct ML {
            #[serde(default)]
            original: Option<Orig>,
        }
        #[derive(Deserialize)]
        struct Orig {
            #[serde(default)]
            download_url: Option<String>,
        }
        let r: R = self
            .data(
                cx,
                "GET",
                &format!("/v4/accounts/{}/files/{}", acct, file),
                &[("include".to_string(), "media_links.original".to_string())],
                None,
            )
            .await?;
        match r.data.media_links.original.and_then(|o| o.download_url) {
            Some(u) if !u.is_empty() => Ok(u),
            _ => Err(ApiErr::Other(ERR_NO_ORIGINAL.into())),
        }
    }
}

pub const ERR_NO_ORIGINAL: &str = "Frame.io offers no download of this file's original";

// ---------- Types ----------

/// Decodes an absent field, or a `null`, or the value itself.
///
/// `#[serde(default)]` covers a key that is ABSENT; it does not cover a key
/// that is present and `null` — serde then answers "invalid type: null,
/// expected a string" and the whole document is lost. Frame.io does send nulls
/// (`"display_name": null` on an account that has not been named yet), and the
/// Go original accepted them, because `encoding/json` unmarshals `null` into a
/// `string` or an `int64` as a no-op. This is that behaviour, per field: a null
/// is read as if the key were absent, and the field keeps its default.
///
/// One null used to fail the whole page, so one unnamed account emptied the
/// Space screen for every account beside it.
fn null_as_absent<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct User {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Account {
    #[serde(default, deserialize_with = "null_as_absent")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub display_name: String,
    /// `storage_limit`: "The number of bytes of non-archived storage in the
    /// account. Value is nil when there is no [limit]" — so `None` is a real
    /// answer ("no limit") and not a missing field, and it must not become a 0.
    #[serde(default)]
    pub storage_limit: Option<i64>,
    /// `storage_usage`: the bytes of non-archived storage the account is using.
    /// `None` when Frame.io does not report it, which the Space headline says
    /// out loud instead of printing 0 B.
    #[serde(default)]
    pub storage_usage: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Workspace {
    #[serde(default, deserialize_with = "null_as_absent")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    #[serde(default, deserialize_with = "null_as_absent")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub name: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub root_folder_id: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub workspace_id: String,
    /// `storage`: "Storage Usage" — the bytes the project holds, which Frame.io
    /// sends on the project itself. `None` only when the field is absent, and
    /// then the Space view falls back to walking the project's folders.
    #[serde(default)]
    pub storage: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Asset {
    #[serde(default, deserialize_with = "null_as_absent")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub name: String,
    #[serde(rename = "type", default, deserialize_with = "null_as_absent")]
    pub type_: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub file_size: i64,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub parent_id: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub project_id: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub view_url: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub status: String,
    #[serde(default, deserialize_with = "null_as_absent")]
    pub created_at: String,
    #[serde(default)]
    pub head_version: Option<Box<Asset>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UploadPart {
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalUpload {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub file_size: i64,
    #[serde(default)]
    pub view_url: String,
    #[serde(default)]
    pub upload_urls: Vec<UploadPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileStatus {
    #[serde(default)]
    pub upload_complete: bool,
    #[serde(default)]
    pub upload_failed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Share {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub short_url: String,
}

/// A project or folder found by `search`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchHit {
    #[serde(rename = "type", default)]
    pub type_: String,
    #[serde(default)]
    pub result: SearchResult,
    #[serde(default)]
    pub matches: Vec<SearchMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub parent_id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub root_folder_id: String,
    #[serde(default)]
    pub workspace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchMatch {
    #[serde(rename = "type", default)]
    pub type_: String,
    #[serde(default)]
    pub project: Option<SearchProject>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchProject {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

impl SearchHit {
    /// Whether the hit was found by its own name (not only its project's).
    pub fn name_match(&self) -> bool {
        self.matches.iter().any(|m| m.type_ == "name_match")
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Frame.io's own example for `storage_limit` is `null` when the account
    /// has no limit ("Value is nil when there is no [limit]"), so a null must
    /// arrive as `None` and never as a limit of 0 bytes.
    #[test]
    fn an_account_with_a_null_limit_parses_as_no_limit() {
        let json = r#"{
            "id": "acct-1",
            "display_name": "Farbhaus",
            "storage_limit": null,
            "storage_usage": 12400000000
        }"#;
        let acct: Account = serde_json::from_str(json).expect("account parses");
        assert_eq!(acct.id, "acct-1");
        assert_eq!(acct.display_name, "Farbhaus");
        assert_eq!(acct.storage_limit, None, "null is \"no limit\", not zero");
        assert_eq!(acct.storage_usage, Some(12_400_000_000));
    }

    #[test]
    fn an_account_with_a_limit_parses_the_bytes() {
        let json = r#"{
            "id": "acct-2",
            "display_name": "Archive",
            "storage_limit": 2000000000000,
            "storage_usage": 12400000000
        }"#;
        let acct: Account = serde_json::from_str(json).expect("account parses");
        assert_eq!(acct.storage_limit, Some(2_000_000_000_000));
        assert_eq!(acct.storage_usage, Some(12_400_000_000));
    }

    /// The old account shape, and any account that omits the fields: both are
    /// still an account, with nothing known about its storage.
    #[test]
    fn an_account_without_the_storage_fields_parses() {
        let acct: Account = serde_json::from_str(r#"{"id": "acct-3", "display_name": "Old"}"#)
            .expect("account without storage parses");
        assert_eq!(acct.id, "acct-3");
        assert_eq!(acct.storage_limit, None);
        assert_eq!(acct.storage_usage, None);
    }

    /// A usage of null is "not reported", which the headline has a case for.
    #[test]
    fn an_account_with_a_null_usage_parses_as_not_reported() {
        let json = r#"{"id": "acct-4", "display_name": "Quiet", "storage_limit": 5, "storage_usage": null}"#;
        let acct: Account = serde_json::from_str(json).expect("account parses");
        assert_eq!(acct.storage_limit, Some(5));
        assert_eq!(acct.storage_usage, None);
    }

    /// `Project.storage` is "Storage Usage" on the project itself, so a listed
    /// project carries the number the Space view prints.
    #[test]
    fn a_project_carries_its_storage() {
        let json = r#"{
            "id": "proj-1",
            "name": "Nightfall",
            "root_folder_id": "root-1",
            "workspace_id": "ws-1",
            "storage": 6200000000
        }"#;
        let p: Project = serde_json::from_str(json).expect("project parses");
        assert_eq!(p.name, "Nightfall");
        assert_eq!(p.storage, Some(6_200_000_000));
    }

    /// An older answer, or a project that omits the field: `None`, which is what
    /// sends the Space view down the walk.
    #[test]
    fn a_project_without_storage_parses() {
        let p: Project = serde_json::from_str(r#"{"id": "proj-2", "name": "Bare"}"#)
            .expect("project without storage parses");
        assert_eq!(p.storage, None);
        assert_eq!(p.root_folder_id, "");
    }

    /// A reported zero is a figure, not a missing one: Frame.io says the project
    /// holds nothing, so there is nothing to walk.
    #[test]
    fn a_project_that_reports_zero_storage_is_still_reported() {
        let p: Project = serde_json::from_str(r#"{"id": "proj-3", "name": "Empty", "storage": 0}"#)
            .expect("project parses");
        assert_eq!(p.storage, Some(0));
    }

    /// `storage` is sent as an integer, but the field is `Option<i64>` and the
    /// payloads are re-serialised for the front ends, so the round trip has to
    /// keep both the number and the null.
    #[test]
    fn storage_survives_a_round_trip() {
        let acct = Account {
            id: "acct-5".into(),
            display_name: "Round".into(),
            storage_limit: None,
            storage_usage: Some(7),
        };
        let json = serde_json::to_string(&acct).expect("account serialises");
        assert!(json.contains("\"storage_limit\":null"), "{json}");
        let back: Account = serde_json::from_str(&json).expect("account re-parses");
        assert_eq!(back.storage_limit, None);
        assert_eq!(back.storage_usage, Some(7));

        let p = Project { id: "p".into(), storage: Some(9), ..Default::default() };
        let json = serde_json::to_string(&p).expect("project serialises");
        let back: Project = serde_json::from_str(&json).expect("project re-parses");
        assert_eq!(back.storage, Some(9));
    }

    // ---------- F1a: a null is read as if the key were absent ----------

    /// The finding, as a regression test: a present `null` used to reject the
    /// whole document, because `#[serde(default)]` only covers an absent key.
    #[test]
    fn a_null_field_parses_as_if_it_were_absent() {
        let acct: Account = serde_json::from_str(r#"{"id":"a","display_name":null}"#)
            .expect("a null display_name parses");
        assert_eq!(acct.id, "a");
        assert_eq!(acct.display_name, "");
        assert_eq!(acct.storage_limit, None);
    }

    /// Every field of the four types a walk reads: a null in any of them is the
    /// type's default, not a parse error.
    #[test]
    fn a_null_in_any_walked_field_parses() {
        let ws: Workspace = serde_json::from_str(r#"{"id":null,"name":null}"#).expect("workspace");
        assert_eq!((ws.id.as_str(), ws.name.as_str()), ("", ""));
        let p: Project = serde_json::from_str(
            r#"{"id":null,"name":null,"root_folder_id":null,"workspace_id":null,"storage":null}"#,
        )
        .expect("project");
        assert_eq!((p.id.as_str(), p.name.as_str()), ("", ""));
        assert_eq!(p.storage, None);
        let a: Asset = serde_json::from_str(
            r#"{"id":null,"name":null,"type":null,"file_size":null,"parent_id":null,
                "project_id":null,"view_url":null,"status":null,"created_at":null,
                "head_version":null}"#,
        )
        .expect("asset");
        assert_eq!(a.file_size, 0);
        assert_eq!(a.type_, "");
        assert!(a.head_version.is_none());
    }

    /// One null in a page used to lose the whole page. It loses nothing now:
    /// the row with the null keeps its place and reads as unnamed.
    #[test]
    fn one_null_no_longer_costs_the_whole_page() {
        let page = r#"{"data":[
            {"id":"acct-1","display_name":"Farbhaus","storage_usage":10},
            {"id":"acct-2","display_name":null},
            {"id":null,"display_name":"Third"}]}"#;
        let v: serde_json::Value = serde_json::from_str(page).expect("page");
        let rows: Vec<Account> =
            serde_json::from_value(v["data"].clone()).expect("a null in one row keeps the page");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].display_name, "Farbhaus");
        assert_eq!(rows[0].storage_usage, Some(10));
        assert_eq!(rows[1].id, "acct-2");
        assert_eq!(rows[1].display_name, "");
        assert_eq!(rows[2].id, "");
        assert_eq!(rows[2].display_name, "Third");
    }

    /// A *wrong type* is still an error: tolerance for `null` is not tolerance
    /// for a list where a string belongs, which is a schema change worth seeing.
    #[test]
    fn a_wrong_type_is_still_an_error() {
        let bad: Result<Account, _> = serde_json::from_str(r#"{"id": ["a"], "display_name": "x"}"#);
        assert!(bad.is_err(), "a list where a string belongs still fails");
        let bad: Result<Asset, _> = serde_json::from_str(r#"{"file_size": "12"}"#);
        assert!(bad.is_err(), "a string where a number belongs still fails");
    }
}
