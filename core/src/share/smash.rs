//! Smash (fromsmash.com) transfers.

use once_cell::sync::Lazy;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::time::{Duration, SystemTime};
use tokio_util::sync::CancellationToken;

use super::{
    dedupe_paths, encode_query, netx_sleep, path_escape, query_escape, read_limited, rel_path, File, Info,
    Link, Source, ERR_BAD_PASSPHRASE, ERR_INVALID_SHARE, ERR_NOT_A_SHARE_LINK, ERR_PASSPHRASE,
    KIND_SMASH,
};
use crate::netx;

// Smash (fromsmash.com) endpoints, overridable for tests. {region} is the
// transfer's region. These are the services the Smash web page calls.
pub fn smash_iam_url() -> String {
    std::env::var("FIO_SMASH_IAM_URL").unwrap_or_else(|_| "https://iam.eu-west-1.fromsmash.co".to_string())
}
pub fn smash_link_url() -> String {
    std::env::var("FIO_SMASH_LINK_URL").unwrap_or_else(|_| "https://link.fromsmash.co".to_string())
}
pub fn smash_transfer_url() -> String {
    std::env::var("FIO_SMASH_TRANSFER_URL")
        .unwrap_or_else(|_| "https://transfer.{region}.fromsmash.co".to_string())
}

/// smashRegions maps the second-to-last character of a Smash ID to its region,
/// as the Smash web page does.
pub(crate) fn smash_regions(c: u8) -> &'static str {
    match c {
        b'a' => "eu-west-1",
        b'b' => "eu-west-2",
        b'c' => "eu-west-3",
        b'd' => "eu-central-1",
        b'e' => "us-east-1",
        b'f' => "us-east-2",
        b'g' => "us-west-1",
        b'h' => "us-west-2",
        b'i' => "ca-central-1",
        b'j' => "sa-east-1",
        b'k' => "ap-northeast-1",
        b'l' => "ap-northeast-2",
        b'm' => "ap-south-1",
        b'n' => "ap-southeast-1",
        b'o' => "ap-southeast-2",
        b'p' => "eu-north-1",
        _ => "",
    }
}

/// smashPageMarker is in the config of every page the Smash web app serves,
/// custom domains included.
pub(crate) const SMASH_PAGE_MARKER: &[u8] = b"https://domain.fromsmash.co";

/// smashTarget reports whether s is on fromsmash.com and returns its host+path,
/// the form the Smash link service looks up (empty for a page that isn't a
/// transfer).
pub(crate) fn smash_target(s: &str) -> Option<String> {
    let full = if s.contains("://") { s.to_string() } else { format!("https://{}", s) };
    let u = url::Url::parse(&full).ok()?;
    let host = u.host_str().unwrap_or("").to_lowercase();
    if host != "fromsmash.com" && !host.ends_with(".fromsmash.com") {
        return None;
    }
    let path = u.path().strip_suffix('/').unwrap_or(u.path()).to_string();
    if path.is_empty() {
        return Some(String::new());
    }
    Some(format!("{}{}", host, path))
}

/// resolveSmash looks up a Smash link (custom names included) and returns the
/// transfer it points to.
pub(crate) async fn resolve_smash(cx: &CancellationToken, target: &str) -> Result<Link, String> {
    if target.is_empty() {
        return Err(ERR_NOT_A_SHARE_LINK.to_string());
    }
    #[derive(Debug, Default, Deserialize)]
    struct Target {
        #[serde(default)]
        target: Option<TargetInner>,
    }
    #[derive(Debug, Default, Deserialize)]
    struct TargetInner {
        #[serde(default)]
        target: String,
    }
    let c = SmashClient::default();
    let url = format!("{}/target/{}?version=10-2019", smash_link_url(), path_escape(target));
    let (_, r): (u16, Target) = c.do_with(cx, reqwest::Method::GET, &url, None).await?;
    match r.target {
        Some(t) if !smash_region(&t.target).is_empty() => Ok(Link {
            kind: KIND_SMASH.to_string(),
            id: t.target,
            url: String::new(),
        }),
        _ => Err(ERR_INVALID_SHARE.to_string()),
    }
}

pub(crate) fn smash_region(id: &str) -> &'static str {
    let b = id.as_bytes();
    if b.len() < 2 || id.contains(['/', '?', '#', '%']) {
        return "";
    }
    smash_regions(b[b.len() - 2])
}

/// smashAuth is the anonymous Smash identity, shared by every client. The web
/// page creates one per visitor; it lasts a week.
#[derive(Debug, Default)]
struct SmashAuth {
    token: String,
    exp: Option<SystemTime>,
}

static SMASH_AUTH: Lazy<tokio::sync::Mutex<SmashAuth>> = Lazy::new(|| tokio::sync::Mutex::new(SmashAuth::default()));

/// smashToken returns the current token, or creates a new identity when there
/// is none, it is about to expire, or it is the stale one the caller was
/// refused with.
async fn smash_token(cx: &CancellationToken, stale: &str) -> Result<String, String> {
    let mut auth = SMASH_AUTH.lock().await;
    let fresh = !auth.token.is_empty()
        && auth.token != stale
        && match auth.exp {
            Some(e) => e
                .duration_since(SystemTime::now())
                .map(|d| d > Duration::from_secs(3600))
                .unwrap_or(false),
            None => false,
        };
    if fresh {
        return Ok(auth.token.clone());
    }
    #[derive(Debug, Default, Deserialize)]
    struct Token {
        #[serde(default)]
        token: String,
        #[serde(default)]
        expiration: String,
    }
    #[derive(Debug, Default, Deserialize)]
    struct Account {
        #[serde(default)]
        token: Token,
    }
    #[derive(Debug, Default, Deserialize)]
    struct Reply {
        #[serde(default)]
        account: Account,
    }
    let mut last_err = String::new();
    for attempt in 0..4 {
        if attempt > 0 {
            netx_sleep(cx, netx::backoff(attempt)).await?;
        }
        let req = netx::api_client()
            .post(format!("{}/account", smash_iam_url()))
            .header("Content-Type", "application/json")
            .body("{}")
            .build();
        let req = match req {
            Ok(r) => r,
            Err(e) => return Err(e.to_string()),
        };
        let resp = match netx::api_client().execute(req).await {
            Ok(r) => r,
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        let status = resp.status().as_u16();
        let data = read_limited(resp, 1 << 20).await;
        if status != 201 && status != 200 {
            last_err = format!("Smash sign-in returned {}", status);
            if status < 500 && status != 429 {
                break;
            }
            continue;
        }
        let r: Reply = match serde_json::from_slice(&data) {
            Ok(r) => r,
            Err(_) => return Err("unexpected response from Smash".to_string()),
        };
        if r.account.token.token.is_empty() {
            return Err("unexpected response from Smash".to_string());
        }
        let exp = super::parse_rfc3339(&r.account.token.expiration)
            .ok_or_else(|| "unexpected response from Smash".to_string())?;
        auth.token = r.account.token.token;
        auth.exp = SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(exp.max(0) as u64));
        return Ok(auth.token.clone());
    }
    Err(last_err)
}

/// SmashClient reads a Smash transfer the way its download page does.
#[derive(Debug, Clone, Default)]
pub struct SmashClient {
    pub transfer_id: String,
    pub passphrase: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SmashError {
    #[serde(default)]
    name: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

impl SmashClient {
    async fn do_with<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        method: reqwest::Method,
        url: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, T), String> {
        let payload = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
        let mut token = smash_token(cx, "").await?;
        let mut last_err = String::new();
        let mut renewed = false;
        for attempt in 0..6 {
            let mut rb = netx::api_client()
                .request(method.clone(), url)
                .header("Accept", "application/json")
                .header("Authorization", format!("Bearer {}", token))
                // Sent on every call, empty without a password, as the web page does.
                .header("smash-authorization", query_escape(&self.passphrase).replace('+', "%20"));
            if let Some(p) = &payload {
                rb = rb.header("Content-Type", "application/json").body(p.clone());
            }
            let req = match rb.build() {
                Ok(r) => r,
                Err(e) => return Err(e.to_string()),
            };
            let resp = match netx::api_client().execute(req).await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    netx_sleep(cx, netx::backoff(attempt + 1)).await?;
                    continue;
                }
            };
            let status = resp.status().as_u16();
            let data = read_limited(resp, 64 << 20).await;
            let se: SmashError = if status >= 300 {
                serde_json::from_slice(&data).unwrap_or_default()
            } else {
                SmashError::default()
            };
            if status == 429 || status >= 500 {
                last_err = format!("Smash returned {}", status);
                netx_sleep(cx, netx::backoff(attempt + 1)).await?;
                continue;
            }
            if status == 401 && se.name == "UnauthorizedError" && !renewed {
                token = smash_token(cx, &token).await?;
                renewed = true;
                continue;
            }
            if status >= 300 {
                return Err(self.map_error(status, &se));
            }
            let out = serde_json::from_slice(&data)
                .map_err(|_| format!("unexpected response from Smash ({})", status))?;
            return Ok((status, out));
        }
        Err(last_err)
    }

    fn map_error(&self, status: u16, e: &SmashError) -> String {
        if e.name.contains("Password") {
            if self.passphrase.is_empty() {
                return ERR_PASSPHRASE.to_string();
            }
            return ERR_BAD_PASSPHRASE.to_string();
        }
        if e.name.starts_with("NotFound") || status == 404 || status == 410 {
            return ERR_INVALID_SHARE.to_string();
        }
        if e.name == "InvalidEmailError" {
            return "this transfer is limited to invited recipients".to_string();
        }
        if e.name.ends_with("UsageExceededError") {
            return "Smash download limit reached for this transfer".to_string();
        }
        let msg = if e.error.is_empty() { e.message.clone() } else { e.error.clone() };
        format!("Smash returned {}: {}", status, msg)
    }

    /// api returns the URL of a call on this transfer.
    fn api(&self, p: &str, q: &[(String, String)]) -> String {
        let mut q = q.to_vec();
        q.push(("version".to_string(), "01-2024".to_string()));
        let host = smash_transfer_url().replace("{region}", smash_region(&self.transfer_id));
        format!(
            "{}/transfer/{}{}?{}",
            host,
            path_escape(&self.transfer_id),
            p,
            encode_query(&q)
        )
    }
}

#[async_trait::async_trait]
impl Source for SmashClient {
    async fn info(&self, cx: &CancellationToken) -> Result<Info, String> {
        if smash_region(&self.transfer_id).is_empty() {
            return Err(ERR_INVALID_SHARE.to_string());
        }
        #[derive(Debug, Default, Deserialize)]
        struct Transfer {
            #[serde(default)]
            id: String,
            #[serde(default)]
            title: String,
            #[serde(default)]
            status: String,
            #[serde(rename = "availabilityEndDate", default)]
            availability_end_date: Option<String>,
        }
        #[derive(Debug, Default, Deserialize)]
        struct Reply {
            #[serde(default)]
            transfer: Option<Transfer>,
        }
        let (_, r): (u16, Reply) = self
            .do_with(cx, reqwest::Method::GET, &self.api("/preview", &[]), None)
            .await?;
        let t = match r.transfer {
            Some(t) => t,
            None => return Err(ERR_INVALID_SHARE.to_string()),
        };
        if t.status == "Uploading" || t.status == "Processing" {
            return Err("this transfer is still uploading".to_string());
        }
        let expired = match t.availability_end_date.as_ref().filter(|s| !s.is_empty()) {
            Some(end) => {
                let end = super::parse_rfc3339(end)
                    .ok_or_else(|| "unexpected response from Smash (200)".to_string())?;
                super::now_unix() > end
            }
            None => false,
        };
        if t.status != "Uploaded" || expired {
            return Err(ERR_INVALID_SHARE.to_string());
        }
        Ok(Info { share_id: t.id, name: t.title, downloading_enabled: true })
    }

    /// List returns the transfer's files. Their URLs are left empty: like the
    /// web page, a file's signed URL is requested (fresh_url) when it starts
    /// downloading.
    async fn list(
        &self,
        cx: &CancellationToken,
        progress: &(dyn Fn(usize) + Send + Sync),
    ) -> Result<Vec<File>, String> {
        #[derive(Debug, Default, Deserialize)]
        struct Items {
            #[serde(default)]
            files: Vec<ItemsFile>,
            #[serde(default)]
            next: String,
        }
        #[derive(Debug, Default, Deserialize)]
        struct ItemsFile {
            #[serde(default)]
            id: String,
            #[serde(default)]
            name: String,
            #[serde(default)]
            size: i64,
        }
        let mut files: Vec<File> = Vec::new();
        let mut start = String::new();
        for _ in 0..10000 {
            let mut q = vec![("limit".to_string(), SMASH_PAGE_SIZE.to_string())];
            if !start.is_empty() {
                q.push(("start".to_string(), start.clone()));
            }
            let (_, r): (u16, Items) = self
                .do_with(cx, reqwest::Method::GET, &self.api("/files/preview", &q), None)
                .await?;
            for f in &r.files {
                files.push(File {
                    asset_id: f.id.clone(),
                    rel_path: rel_path(&f.name),
                    size: f.size,
                    url: String::new(),
                    problem: String::new(),
                });
            }
            if !r.files.is_empty() {
                (progress)(files.len());
            }
            if r.next.is_empty() || r.next == start || r.files.is_empty() {
                break;
            }
            start = r.next;
        }
        dedupe_paths(&mut files);
        Ok(files)
    }

    /// FreshURL asks for a signed download URL for one file.
    async fn fresh_url(&self, cx: &CancellationToken, file_id: &str) -> Result<String, String> {
        #[derive(Debug, Default, Deserialize)]
        struct Reply {
            #[serde(default)]
            transfer: Option<TransferFiles>,
        }
        #[derive(Debug, Default, Deserialize)]
        struct TransferFiles {
            #[serde(default)]
            files: Vec<FileUrl>,
        }
        #[derive(Debug, Default, Deserialize)]
        struct FileUrl {
            #[serde(default)]
            id: String,
            #[serde(default)]
            url: String,
        }
        let body = serde_json::json!({ "files": [{ "id": file_id }] });
        let (_, r): (u16, Reply) = self
            .do_with(cx, reqwest::Method::PUT, &self.api("/urls", &[]), Some(&body))
            .await?;
        if let Some(t) = r.transfer {
            for f in &t.files {
                if f.id == file_id && !f.url.is_empty() {
                    return Ok(f.url.clone());
                }
            }
        }
        Err("could not get a download link for this file".to_string())
    }

    /// MarkDownloaded is a no-op: Smash counts a download when its URL is
    /// requested.
    async fn mark_downloaded(&self, cx: &CancellationToken, _ids: &[String]) -> Result<(), String> {
        let _ = cx;
        Ok(())
    }
}

/// smashPageSize is the most files the listing returns per call.
const SMASH_PAGE_SIZE: usize = 500;
