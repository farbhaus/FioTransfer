//! Portal download links (<host>/d/<token>, github.com/farbhaus/portal).

use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{
    dedupe_paths, netx_sleep, origin, path_escape, path_join, path_parts, read_limited, rel_path, Link,
    Source,
    ERR_BAD_PASSPHRASE, ERR_INVALID_SHARE, ERR_PASSPHRASE, KIND_PORTAL, VIEWER,
};
use crate::netx;

/// portalTokenRe is ^[A-Za-z0-9_-]{8,}$.
pub(crate) fn portal_token_ok(s: &str) -> bool {
    s.len() >= 8 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// portalLink checks whether u is a download link of a Portal
/// (github.com/farbhaus/portal), which can run on any host: <host>/d/<token>.
pub(crate) async fn portal_link(cx: &CancellationToken, u: &url::Url) -> Option<Link> {
    let p = path_parts(u);
    if p.len() != 2 || p[0] != "d" || !portal_token_ok(&p[1]) {
        return None;
    }
    let l = Link {
        kind: KIND_PORTAL.to_string(),
        id: p[1].clone(),
        url: format!("{}/d/{}", origin(u), p[1]),
    };
    let c = PortalClient::new(l.url.clone(), "");
    let (status, res) = c.call::<PortalResolve>(cx, reqwest::Method::GET, "", None).await;
    if status == 410 || (res.is_ok() && res.map(|r| !r.state.is_empty()).unwrap_or(false)) {
        // 410: an expired or revoked link
        return Some(l);
    }
    None
}

/// PortalClient reads a Portal download link the way its download page does.
pub struct PortalClient {
    /// <host>/d/<token>
    pub url: String,
    pub passphrase: String,

    /// Held while a session starts.
    mu: tokio::sync::Mutex<()>,
    /// Download session, started by list or the first fresh_url.
    session: std::sync::Mutex<String>,
}

/// The link's own answer. `fields` (viewer_fields_required) is decoded but not
/// acted on: Portal answers 422 when it needs the name and email.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct PortalResolve {
    #[serde(default)]
    pub state: String,
    #[serde(rename = "display_name", default)]
    pub display_name: String,
    #[serde(rename = "password_required", default)]
    pub password_required: bool,
    #[serde(rename = "verify_email", default)]
    pub verify_email: bool,
    #[serde(rename = "viewer_fields_required", default)]
    pub fields: ViewerFields,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub(crate) struct ViewerFields {
    #[serde(default)]
    pub name: bool,
    #[serde(default)]
    pub email: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct PortalFile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub ready: Option<bool>,
}

impl PortalClient {
    pub(crate) fn new(url: String, passphrase: &str) -> Self {
        PortalClient {
            url,
            passphrase: passphrase.to_string(),
            mu: tokio::sync::Mutex::new(()),
            session: std::sync::Mutex::new(String::new()),
        }
    }

    /// api returns the link's API URL plus p.
    fn api(&self, p: &str) -> String {
        let u = match url::Url::parse(&self.url) {
            Ok(u) => u,
            Err(_) => return String::new(),
        };
        let token = u.path().trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
        format!("{}/api/public/downloads/{}{}", origin(&u), path_escape(&token), p)
    }

    /// call sends one request to the link's API and decodes the answer into T.
    /// 429 isn't retried: Portal sends it for a reached download limit or too
    /// many wrong passwords.
    async fn call<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        method: reqwest::Method,
        p: &str,
        body: Option<&serde_json::Value>,
    ) -> (u16, Result<T, String>) {
        let payload = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
        let mut last_err = String::new();
        for attempt in 0..6 {
            if attempt > 0 {
                if let Err(e) = netx_sleep(cx, netx::backoff(attempt)).await {
                    return (0, Err(e));
                }
            }
            let mut rb = netx::api_client()
                .request(method.clone(), self.api(p))
                .header("Accept", "application/json");
            if let Some(b) = &payload {
                rb = rb.header("Content-Type", "application/json").body(b.clone());
            }
            let req = match rb.build() {
                Ok(r) => r,
                Err(e) => return (0, Err(e.to_string())),
            };
            let resp = match netx::api_client().execute(req).await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    continue;
                }
            };
            let status = resp.status().as_u16();
            let data = read_limited(resp, 64 << 20).await;
            if status >= 500 && status != 503 {
                last_err = format!("Portal returned {}", status);
                continue;
            }
            if status >= 300 {
                #[derive(Debug, Default, Deserialize)]
                struct E {
                    #[serde(default)]
                    detail: String,
                }
                let e: E = serde_json::from_slice(&data).unwrap_or_default();
                if status == 404 || status == 410 {
                    return (status, Err(ERR_INVALID_SHARE.to_string()));
                }
                if status == 403 && e.detail.contains("password") {
                    if self.passphrase.is_empty() {
                        return (status, Err(ERR_PASSPHRASE.to_string()));
                    }
                    return (status, Err(ERR_BAD_PASSPHRASE.to_string()));
                }
                if !e.detail.is_empty() {
                    return (status, Err(e.detail));
                }
                return (status, Err(format!("Portal returned {}", status)));
            }
            return match serde_json::from_slice(&data) {
                Ok(v) => (status, Ok(v)),
                Err(_) => (status, Err("unexpected response from Portal".to_string())),
            };
        }
        (0, Err(last_err))
    }

    /// start opens a download session, which lists the files. Portal logs each
    /// session as a visit, with the viewer's name and email when the link asks
    /// for them.
    async fn start(&self, cx: &CancellationToken) -> Result<(String, Vec<PortalFile>), String> {
        let mut body = serde_json::Map::new();
        if !self.passphrase.is_empty() {
            body.insert("password".to_string(), serde_json::Value::String(self.passphrase.clone()));
        }
        let mut asked_for_email = false;
        if let Some((name, email)) = viewer() {
            if !email.is_empty() {
                asked_for_email = true;
                body.insert("name".to_string(), serde_json::Value::String(name));
                body.insert("email".to_string(), serde_json::Value::String(email));
            }
        }
        #[derive(Debug, Default, Deserialize)]
        struct R {
            #[serde(rename = "session_id", default)]
            session_id: String,
            #[serde(default)]
            files: Vec<PortalFile>,
        }
        let body = serde_json::Value::Object(body);
        let (status, res) = self.call::<R>(cx, reqwest::Method::POST, "/sessions", Some(&body)).await;
        if status == 422 && !asked_for_email {
            return Err(
                "this link asks for your name and email: sign in to Frame.io and FioTransfer sends those"
                    .to_string(),
            );
        }
        if status == 401 {
            return Err("this link emails you a code first; open it in a browser".to_string());
        }
        let r = res?;
        Ok((r.session_id, r.files))
    }

    /// sessionID returns the client's session, starting one if it has none.
    async fn session_id(&self, cx: &CancellationToken) -> Result<String, String> {
        let _guard = self.mu.lock().await;
        let existing = self.session.lock().map(|g| g.clone()).unwrap_or_default();
        if !existing.is_empty() {
            return Ok(existing);
        }
        let (sid, _) = self.start(cx).await?;
        if let Ok(mut g) = self.session.lock() {
            *g = sid.clone();
        }
        Ok(sid)
    }
}

/// viewer is who is downloading, for links that ask.
fn viewer() -> Option<(String, String)> {
    let f = VIEWER.lock().ok().and_then(|g| g.clone())?;
    f()
}

#[async_trait::async_trait]
impl Source for PortalClient {
    async fn info(&self, cx: &CancellationToken) -> Result<super::Info, String> {
        let (_, res) = self.call::<PortalResolve>(cx, reqwest::Method::GET, "", None).await;
        let r = res?;
        if r.state != "ok" {
            return Err(ERR_INVALID_SHARE.to_string());
        }
        if r.password_required && self.passphrase.is_empty() {
            return Err(ERR_PASSPHRASE.to_string());
        }
        if r.verify_email {
            return Err("this link emails you a code first; open it in a browser".to_string());
        }
        let token = self
            .url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        Ok(super::Info { share_id: token, name: r.display_name, downloading_enabled: true })
    }

    async fn list(
        &self,
        cx: &CancellationToken,
        progress: &(dyn Fn(usize) + Send + Sync),
    ) -> Result<Vec<super::File>, String> {
        let (sid, pf) = self.start(cx).await?;
        if let Ok(mut g) = self.session.lock() {
            *g = sid;
        }
        let mut files: Vec<super::File> = Vec::with_capacity(pf.len());
        for f in &pf {
            let mut file = super::File {
                asset_id: f.id.clone(),
                rel_path: rel_path(&path_join(&f.path, &f.name)),
                size: f.size,
                url: String::new(),
                problem: String::new(),
            };
            if f.ready == Some(false) {
                file.problem = "still uploading to Frame.io".to_string();
            }
            files.push(file);
        }
        (progress)(files.len());
        dedupe_paths(&mut files);
        Ok(files)
    }

    /// FreshURL asks Portal for a file's Frame.io download URL. Each one counts
    /// against the link's download limit, as in the browser.
    async fn fresh_url(&self, cx: &CancellationToken, file_id: &str) -> Result<String, String> {
        let sid = self.session_id(cx).await?;
        #[derive(Debug, Default, Deserialize)]
        struct R {
            #[serde(default)]
            url: String,
        }
        let p = format!("/sessions/{}/files/{}/url", path_escape(&sid), path_escape(file_id));
        let (_, res) = self.call::<R>(cx, reqwest::Method::POST, &p, None).await;
        let r = res?;
        if r.url.is_empty() {
            return Err("could not get a download link for this file".to_string());
        }
        Ok(r.url)
    }

    /// MarkDownloaded is a no-op: Portal records each download when its URL is
    /// requested.
    async fn mark_downloaded(&self, cx: &CancellationToken, _ids: &[String]) -> Result<(), String> {
        let _ = cx;
        Ok(())
    }
}
