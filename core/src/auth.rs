
//! Adobe IMS OAuth 2.0 (PKCE, public "Single Page App" client) with an
//! https://localhost redirect, storing tokens in the macOS Keychain.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio_util::sync::CancellationToken;

use crate::localcert;
use crate::netx;
use crate::secret;

pub const SCOPES: &str = "openid email profile offline_access additional_info.roles";
const KEYRING_USER: &str = "adobe-ims";
const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

pub const ERR_NOT_LOGGED_IN: &str = "not signed in to Frame.io";
pub const ERR_LOGIN_CANCELED: &str = "sign-in canceled";

/// Where Adobe IMS lives (overridable for tests).
pub fn ims_base() -> String {
    std::env::var("FIO_IMS_BASE").unwrap_or_else(|_| "https://ims-na1.adobelogin.com".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Unix seconds.
    #[serde(default)]
    pub expiry: i64,
}

impl Token {
    fn expires_in(&self) -> Duration {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        Duration::from_secs((self.expiry - now).max(0) as u64)
    }
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

/// Hands out valid access tokens, refreshing as needed.
pub struct AuthManager {
    inner: Mutex<Inner>,
    client_id: Box<dyn Fn() -> String + Send + Sync>,
    static_token: bool,
}

struct Inner {
    tok: Option<Token>,
}

impl AuthManager {
    pub fn new<F: Fn() -> String + Send + Sync + 'static>(client_id: F) -> Arc<Self> {
        let tok = load_token();
        Arc::new(AuthManager { inner: Mutex::new(Inner { tok }), client_id: Box::new(client_id), static_token: false })
    }

    /// A Manager with a fixed token that never refreshes (tests, debugging).
    pub fn new_static(token: &str) -> Arc<Self> {
        Arc::new(AuthManager {
            inner: Mutex::new(Inner {
                tok: Some(Token { access_token: token.to_string(), refresh_token: "static".into(), expiry: now_unix() + 100 * 365 * 24 * 3600 }),
            }),
            client_id: Box::new(String::new),
            static_token: true,
        })
    }

    pub fn logged_in(&self) -> bool {
        match &self.inner.lock().unwrap().tok {
            Some(t) => !t.refresh_token.is_empty(),
            None => false,
        }
    }

    /// A valid access token, refreshed if it is about to expire.
    pub async fn token(&self, cx: &CancellationToken) -> Result<String, String> {
        {
            let g = self.inner.lock().unwrap();
            match &g.tok {
                None => return Err(ERR_NOT_LOGGED_IN.to_string()),
                Some(t) if t.expires_in() > REFRESH_MARGIN => return Ok(t.access_token.clone()),
                Some(_) => {}
            }
        }
        self.refresh(cx).await?;
        let g = self.inner.lock().unwrap();
        match &g.tok {
            Some(t) => Ok(t.access_token.clone()),
            None => Err(ERR_NOT_LOGGED_IN.to_string()),
        }
    }

    pub async fn force_refresh(&self, cx: &CancellationToken) -> Result<(), String> {
        if self.static_token {
            return Ok(());
        }
        self.refresh(cx).await
    }

    async fn refresh(&self, cx: &CancellationToken) -> Result<(), String> {
        if self.static_token {
            return Ok(());
        }
        let refresh_token = {
            let g = self.inner.lock().unwrap();
            match &g.tok {
                Some(t) if !t.refresh_token.is_empty() => t.refresh_token.clone(),
                _ => return Err(ERR_NOT_LOGGED_IN.to_string()),
            }
        };
        let client_id = (self.client_id)();
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", &client_id),
            ("refresh_token", &refresh_token),
        ];
        match self.token_request(cx, &form).await {
            Ok(mut t) => {
                if t.refresh_token.is_empty() {
                    t.refresh_token = refresh_token;
                }
                self.inner.lock().unwrap().tok = Some(t.clone());
                save_token(&t);
                Ok(())
            }
            Err(err) => {
                if err == REJECTED {
                    // The session is gone (revoked or expired): drop it so the app asks to sign in.
                    self.inner.lock().unwrap().tok = None;
                    clear_token();
                    return Err(ERR_NOT_LOGGED_IN.to_string());
                }
                Err(format!("refreshing Adobe session: {}", err))
            }
        }
    }

    pub fn logout(&self) {
        self.inner.lock().unwrap().tok = None;
        clear_token();
    }

    async fn token_request(&self, cx: &CancellationToken, form: &[(&str, &str)]) -> Result<Token, String> {
        let body = form
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    percent_encoding::utf8_percent_encode(k, percent_encoding::NON_ALPHANUMERIC),
                    percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let url = format!("{}/ims/token/v3", ims_base());
        let resp = tokio::select! {
            r = netx::api_client()
                .post(&url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body)
                .send() => r.map_err(|e| e.to_string())?,
            _ = cx.cancelled() => return Err(ERR_LOGIN_CANCELED.to_string()),
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        if status != 200 {
            let msg = format!("Adobe returned {}: {}", status, truncate(&body, 300));
            if status == 400 || status == 401 {
                return Err(REJECTED.to_string());
            }
            return Err(msg);
        }
        #[derive(Deserialize)]
        struct R {
            #[serde(default)]
            access_token: String,
            #[serde(default)]
            refresh_token: String,
            #[serde(default)]
            expires_in: f64,
        }
        let r: R = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let mut exp = Duration::from_secs(r.expires_in.max(0.0) as u64);
        if r.expires_in > 1e6 {
            exp = Duration::from_millis(r.expires_in as u64);
        }
        if exp.is_zero() {
            exp = Duration::from_secs(3600);
        }
        Ok(Token { access_token: r.access_token, refresh_token: r.refresh_token, expiry: now_unix() + exp.as_secs() as i64 })
    }

    /// The browser-based PKCE flow. `open_url` opens the system browser.
    pub async fn login<F>(&self, cx: &CancellationToken, port: u16, open_url: F) -> Result<(), String>
    where
        F: Fn(&str),
    {
        let client_id = (self.client_id)();
        if client_id.is_empty() {
            return Err(crate::config::ERR_NO_CLIENT_ID.to_string());
        }
        let verifier = rand_string(64);
        let challenge = {
            use base64::Engine;
            use sha2::Digest;
            let sum = sha2::Sha256::digest(verifier.as_bytes());
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sum)
        };
        let state = rand_string(24);
        let redirect = redirect_uri(port);

        let lc = localcert::local_cert()?;
        localcert::trust_local_cert(&lc.ca_path);
        let tls = lc.config;

        // localhost may resolve to IPv4 or IPv6; listen on both.
        let mut listeners = Vec::new();
        for addr in [format!("127.0.0.1:{}", port), format!("[::1]:{}", port)] {
            if let Ok(l) = tokio::net::TcpListener::bind(&addr).await {
                listeners.push(l);
            }
        }
        if listeners.is_empty() {
            return Err(format!("port {} is in use", port));
        }
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);

        let (tx, rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
        let tx = Arc::new(Mutex::new(Some(tx)));
        let state_c = state.clone();
        let shutdown = CancellationToken::new();
        let mut handles = Vec::new();
        for l in listeners {
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            let state_c = state_c.clone();
            let sd = shutdown.clone();
            handles.push(tokio::spawn(async move {
                loop {
                    let (sock, _) = tokio::select! {
                        r = l.accept() => match r { Ok(v) => v, Err(_) => break },
                        _ = sd.cancelled() => break,
                    };
                    let acceptor = acceptor.clone();
                    let tx = tx.clone();
                    let state_c = state_c.clone();
                    tokio::spawn(async move {
                        let Ok(mut stream) = acceptor.accept(sock).await else { return };
                        let req = read_request(&mut stream).await;
                        let Some(req) = req else { return };
                        let res = handle_callback(&req, &state_c);
                        let (body_title, body_text, out) = match &res {
                            Ok(_) => ("Signed in", "You can close this tab and return to FioTransfer.", res.clone()),
                            Err(e) => ("Sign-in failed", e.as_str(), res.clone()),
                        };
                        let page = page_html(body_title, &html_escape(body_text));
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            page.len(),
                            page
                        );
                        use tokio::io::AsyncWriteExt;
                        let _ = stream.write_all(resp.as_bytes()).await;
                        let _ = stream.flush().await;
                        if let Some(tx) = tx.lock().unwrap().take() {
                            let _ = tx.send(out);
                        }
                    });
                }
            }));
        }

        let mut q = vec![
            ("client_id", client_id.as_str()),
            ("scope", SCOPES),
            ("response_type", "code"),
            ("redirect_uri", redirect.as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]
        .iter()
        .map(|(k, v)| format!("{}={}", k, percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)))
        .collect::<Vec<_>>()
        .join("&");
        q.insert_str(0, "");
        open_url(&format!("{}/ims/authorize/v2?{}", ims_base(), q));

        let deadline = tokio::time::sleep(Duration::from_secs(5 * 60));
        tokio::pin!(deadline);
        let res = tokio::select! {
            r = rx => match r { Ok(v) => v, Err(_) => Err(ERR_LOGIN_CANCELED.to_string()) },
            _ = cx.cancelled() => Err(ERR_LOGIN_CANCELED.to_string()),
            _ = &mut deadline => Err("timed out waiting for Adobe sign-in".to_string()),
        };
        shutdown.cancel();
        for h in handles {
            h.abort();
        }
        let code = res?;
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("redirect_uri", redirect.as_str()),
        ];
        let t = self.token_request(cx, &form).await?;
        self.inner.lock().unwrap().tok = Some(t.clone());
        save_token(&t);
        Ok(())
    }
}

const REJECTED: &str = "rejected";

fn handle_callback(req: &str, state: &str) -> Result<String, String> {
    let line = req.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let _method = parts.next();
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    if path != "/callback" {
        return Err("not found".to_string());
    }
    let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let err = get("error");
    if !err.is_empty() {
        return Err(format!("Adobe sign-in failed: {} {}", err, get("error_description")));
    }
    if get("state") != state {
        return Err("sign-in state mismatch; please try again".to_string());
    }
    let code = get("code");
    if code.is_empty() {
        return Err("sign-in returned no authorization code".to_string());
    }
    Ok(code)
}

async fn read_request<S>(stream: &mut S) -> Option<String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub fn redirect_uri(port: u16) -> String {
    format!("https://localhost:{}/callback", port)
}

fn rand_string(n: usize) -> String {
    use base64::Engine;
    use rand::RngExt;
    let mut b = vec![0u8; n];
    rand::rng().fill(&mut b[..]);
    let s = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&b);
    s.chars().take(n).collect()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let mut o: String = s.chars().take(n).collect();
        o.push('…');
        o
    } else {
        s.to_string()
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn page_html(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>FioTransfer</title>\n<body style=\"font:16px -apple-system,system-ui;background:#111;color:#eee;display:grid;place-items:center;height:100vh;margin:0\">\n<div style=\"text-align:center\"><h2>{}</h2><p>{}</p></div></body>",
        title, text
    )
}

// ---- Tokens live in the Keychain; a 0600 file is the fallback ----

fn fallback_path() -> std::path::PathBuf {
    crate::config::dir().join("token.json")
}

fn save_token(t: &Token) {
    let b = serde_json::to_vec(t).unwrap_or_default();
    if secret::set(KEYRING_USER, &String::from_utf8_lossy(&b)).is_ok() {
        let _ = std::fs::remove_file(fallback_path());
        return;
    }
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    // 0600 on unix; on Windows the folder's ACL is what keeps this token to its
    // owner (see `fileio::mode`).
    crate::fileio::mode(&mut opts, 0o600);
    if let Ok(mut f) = opts.open(fallback_path()) {
        let _ = f.write_all(&b);
    }
}

fn load_token() -> Option<Token> {
    let raw = match secret::get(KEYRING_USER) {
        Some(s) => Some(s.into_bytes()),
        None => std::fs::read(fallback_path()).ok(),
    }?;
    let t: Token = serde_json::from_slice(&raw).ok()?;
    if t.refresh_token.is_empty() {
        return None;
    }
    Some(t)
}

fn clear_token() {
    secret::delete(KEYRING_USER);
    let _ = std::fs::remove_file(fallback_path());
}
