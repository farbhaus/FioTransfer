//! Portal: the service that publishes a project's upload and download links,
//! and the `.turbosort` marker that names its `Live_Work` folder.
//!
//! FioTransfer talks to Portal for one reason: to learn which project folders
//! exist and which Frame.io folder each one publishes into, so that
//! [`agent::reconcile`] can keep a mirrored watch folder for every one of them.
//! No file passes through Portal. Transfers still go to and from Frame.io; the
//! local side is ours (`Settings::portal_root`), and Portal's own destination
//! paths are never adopted.
//!
//! Only two endpoints are used, both documented here and neither invented:
//! `GET /api/upload-links` (the links, each with its `target_folder_id`,
//! `target_folder_name`, `folder` and `turbosort`) and `GET /api/sync-rules`
//! (read-only reference: what Portal's own worker would do).
//!
//! A link's `folder` may also carry a `sync` object — see [`Sync`] — which says
//! the project is not one mirror but two one-way folders: one the client sends
//! into, one we make things in. It rides on the same row, so no endpoint is
//! added for it.
//!
//! Two credentials open them, and the caller never has to choose:
//!
//! * `Authorization: Bearer <PORTAL_API_TOKEN>`, Portal's machine-client token.
//!   It wins when one is stored, because that is what a headless install has.
//!   Portal's `PORTAL_API_TOKEN` is optional and `deploy.sh` never writes one,
//!   so on many deployments there is no token to send at all.
//! * Portal's own admin session: `POST /api/auth/login` with the admin's email
//!   and password, which answers with a `tower-sessions` cookie (Portal's
//!   `portal_session`, `Max-Age` fourteen days). The cookie is kept — the
//!   password never is — and every later call sends it back. This is the way in
//!   when Portal holds no token: its `/api` endpoints accept a session and
//!   nothing else.
//!
//! Both credentials are stored in the Keychain, never in settings.json: the
//! token under [`TOKEN_KEY`], the cookie jar as JSON under [`SESSION_KEY`].
//!
//! Tolerance: Portal is a live service and its rows may grow fields or send
//! `null` where a string is expected. `#[serde(default)]` alone does not cover
//! a present `null` (the Space property sweep found the same thing for
//! Frame.io), so every string field also goes through [`nullable`].

pub mod agent;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::secret;

/// The Keychain item the bearer token lives under.
pub const TOKEN_KEY: &str = "portal_token";

/// The Keychain item the sign-in cookie jar lives under. A different key from
/// the token's, so forgetting one leaves the other alone.
pub const SESSION_KEY: &str = "portal_session";

/// The 0600 file that stands in where there is no Keychain, exactly as `auth`
/// keeps the Adobe session's `token.json`. It is never written to settings.json.
const TOKEN_FILE: &str = "portal-token";

/// The same fallback for the session cookie jar.
const SESSION_FILE: &str = "portal-session";

/// Portal answers within this or is treated as unreachable.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// How many times one GET is tried before the error is reported.
pub const MAX_ATTEMPTS: u32 = 6;

// ---------- errors ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalErr {
    /// `Settings::portal_url` is empty: nothing to talk to.
    NoUrl,
    /// Neither a bearer token nor a session is stored.
    NoToken,
    /// A session is stored, but it was issued by a different Portal than the
    /// address in the settings — after a typo was fixed, say.
    SessionFor { stored: String, wanted: String },
    /// Portal refused the stored session's cookie: it expired, or somebody
    /// signed out. A different failure from a refused token, because the fix is
    /// different: sign in again.
    SessionExpired { status: u16 },
    /// `POST /api/auth/login` would not sign this email and password in. The
    /// detail is Portal's own sentence; the password is never part of it.
    Login { status: u16, detail: String },
    /// Portal accepted the password and wants a one-time code
    /// (`totp_required`): the sign-in is not finished, and the same session is
    /// waiting for [`PortalClient::login_totp`]. Nothing secret is carried: the
    /// code has not been typed yet.
    TotpRequired,
    /// `POST /api/auth/login/totp` would not take this code. The detail is
    /// Portal's own sentence ("Invalid code", "Your login session expired —
    /// start again", "Too many incorrect codes — sign in again"), which says
    /// which of the two it was; the code itself is never part of it.
    TotpRefused { status: u16, detail: String },
    /// A code was offered and there is no half-finished sign-in to give it to:
    /// the password step never ran, or another one replaced it.
    NoPendingSignIn,
    /// The session could not be written to the Keychain or its 0600 stand-in.
    /// Nothing about the credential is in the sentence.
    Store(String),
    /// Portal accepted the password but set no session cookie, so there was
    /// nothing to keep and nothing to send later.
    NoSessionCookie,
    /// Portal answered, and the answer was not a success.
    Status { status: u16, body: String },
    /// The connection failed, or failed and stayed failed.
    Http(String),
    /// The reply was not the JSON document this module knows.
    Json(String),
}

/// The human part of a Portal error body: `{"detail": "…"}` when it is that shape,
/// otherwise the body trimmed. Never the whole JSON document.
fn detail_of(body: &str) -> String {
    let t = body.trim();
    if t.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        if let Some(d) = v.get("detail").and_then(|d| d.as_str()) {
            return d.trim().to_string();
        }
    }
    // A web page is never Portal's answer: every error Portal sends is JSON. This is
    // what a mistyped address looks like — `example.com` answers `405` with its home
    // page — so say that, rather than pasting a page of HTML into a status line.
    if t.starts_with('<') || t.to_ascii_lowercase().contains("<html") {
        return String::new();
    }
    crate::netx::trim_body(t.as_bytes())
}


/// JSON from a body that should be JSON.
///
/// `serde_json`'s own message for a web page is "expected value at line 1 column 1",
/// which tells a person nothing. A Portal behind a proxy that answers with a login
/// page — an expired session, a WAF, a wrong address — lands here, so name that.
fn json_body(data: &[u8]) -> Result<Value, PortalErr> {
    if let Ok(v) = serde_json::from_slice::<Value>(data) {
        return Ok(v);
    }
    let text = String::from_utf8_lossy(data);
    let head = text.trim_start();
    if head.starts_with('<') || head.to_ascii_lowercase().contains("<html") {
        return Err(PortalErr::Json(
            "the address answered with a web page instead of JSON — the address is not Portal, or a proxy in \
             front of it wants a sign-in"
                .to_string(),
        ));
    }
    Err(PortalErr::Json(match serde_json::from_slice::<Value>(data) {
        Err(e) => e.to_string(),
        Ok(_) => "the reply is not JSON".to_string(),
    }))
}

impl std::fmt::Display for PortalErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortalErr::NoUrl => write!(f, "no Portal URL is set"),
            PortalErr::NoToken => {
                write!(f, "no Portal token or signed-in session is stored")
            }
            PortalErr::SessionFor { stored, wanted } => write!(
                f,
                "the stored session belongs to {}, not {}: sign in again for this address",
                stored, wanted
            ),
            PortalErr::SessionExpired { status } => write!(
                f,
                "the Portal session expired ({}): sign in again in Settings, or store a bearer token",
                status
            ),
            PortalErr::Login { status, detail } => {
                let detail = detail.trim();
                if detail.is_empty() {
                    write!(f, "Portal refused the sign-in ({})", status)
                } else {
                    write!(f, "Portal refused the sign-in ({}): {}", status, detail)
                }
            }
            PortalErr::TotpRequired => write!(
                f,
                "Portal wants a one-time code to finish this sign-in: type the six digits from your authenticator app"
            ),
            PortalErr::TotpRefused { status, detail } => {
                let detail = detail.trim();
                if detail.is_empty() {
                    write!(f, "Portal refused the one-time code ({})", status)
                } else {
                    write!(f, "Portal refused the one-time code ({}): {}", status, detail)
                }
            }
            PortalErr::NoPendingSignIn => write!(
                f,
                "there is no sign-in waiting for a code: sign in with the password first"
            ),
            PortalErr::Store(e) => write!(f, "{}", e),
            PortalErr::NoSessionCookie => write!(
                f,
                "Portal accepted the password but set no session cookie, so there is nothing to keep"
            ),
            PortalErr::Status { status, body } => {
                // The user reads this in the settings status line, so it says what
                // to do rather than repeating a JSON error document.
                match *status {
                    401 | 403 => write!(
                        f,
                        "Portal refused the token ({}). It may be wrong, or Portal's .env may set none at all — see docs/PORTAL.md",
                        status
                    ),
                    404 => write!(f, "Portal has no /api/upload-links ({}): is that the Portal address?", status),
                    // A page of HTML where JSON belongs means the address answered,
                    // but it is not Portal: the mistyped-URL case, and the one a
                    // person is most likely to hit first.
                    _ if body.trim_start().starts_with('<') => write!(
                        f,
                        "that address answered {} with a web page, not Portal's API: is that the Portal address?",
                        status
                    ),
                    _ => {
                        let body = detail_of(body);
                        if body.is_empty() {
                            write!(f, "Portal answered {} (no detail)", status)
                        } else {
                            write!(f, "Portal answered {}: {}", status, body)
                        }
                    }
                }
            }
            PortalErr::Http(e) => write!(f, "can't reach Portal: {}", e),
            PortalErr::Json(e) => write!(f, "Portal sent something else: {}", e),
        }
    }
}
impl std::error::Error for PortalErr {}

// ---------- the token ----------

fn token_path() -> PathBuf {
    crate::config::dir().join(TOKEN_FILE)
}

/// The stored bearer token, if there is one.
pub fn token() -> Option<String> {
    let raw = match secret::get(TOKEN_KEY) {
        Some(s) => s,
        None => std::fs::read_to_string(token_path()).ok()?,
    };
    let t = raw.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Whether a credential is stored, a bearer token or a signed-in session: what
/// `portal_status`'s `signed_in` answers.
pub fn signed_in() -> bool {
    token().is_some() || session().is_some()
}

/// Stores the bearer token. The Keychain is asked first; where it is absent (or
/// disabled for a headless run) a 0600 file under the app's own data folder
/// stands in, as it does for the Adobe session.
pub fn set_token(t: &str) -> Result<(), String> {
    let t = t.trim();
    if t.is_empty() {
        return Err("no token".to_string());
    }
    if secret::set(TOKEN_KEY, t).is_ok() {
        let _ = std::fs::remove_file(token_path());
        return Ok(());
    }
    use std::io::Write;
    let p = token_path();
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    // 0600 on unix; on Windows the folder's ACL is what keeps this token to its
    // owner (see `fileio::mode`).
    crate::fileio::mode(&mut opts, 0o600);
    let mut f = opts.open(&p).map_err(|e| format!("could not store the Portal token: {}", e))?;
    f.write_all(t.as_bytes()).map_err(|e| format!("could not store the Portal token: {}", e))
}

/// Forgets the bearer token.
pub fn clear_token() {
    secret::delete(TOKEN_KEY);
    let _ = std::fs::remove_file(token_path());
}

// ---------- the session ----------

/// A signed-in session: the cookies Portal set when it accepted the email and
/// password, kept as the `Set-Cookie` header values it sent.
///
/// This is everything the sign-in leaves behind. The password is not in this
/// struct, not in the Keychain and not in settings.json: it is a parameter of
/// [`PortalClient::login`], and it is dropped when that call returns.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// The Portal this jar came from, without a trailing slash. A jar is only
    /// ever replayed to the Portal that issued it.
    #[serde(default)]
    pub base: String,
    /// The admin Portal named at sign-in. Not a secret: it is what the Settings
    /// screen shows as "Signed in as …".
    #[serde(default)]
    pub email: String,
    /// The `Set-Cookie` values, verbatim. `reqwest`'s cookie store parses them,
    /// so `path`, `Max-Age` and `Secure` are honoured as Portal wrote them.
    #[serde(default)]
    pub cookies: Vec<String>,
}

impl Session {
    /// The same jar, named by the account Portal reported.
    pub fn named(&self, email: &str) -> Session {
        Session { email: email.trim().to_string(), ..self.clone() }
    }

    /// Whether there is a cookie to send. An empty jar is a session whose
    /// cookies are gone: its calls answer 401, which reads as
    /// [`PortalErr::SessionExpired`].
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }
}

/// Never the cookie values: a `Debug` of a jar is what a log line would print.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("base", &self.base)
            .field("email", &self.email)
            .field("cookies", &format!("{} cookie(s)", self.cookies.len()))
            .finish()
    }
}

/// A base URL as this module uses it: trimmed, without a trailing slash.
fn base_of(base: &str) -> String {
    base.trim().trim_end_matches('/').to_string()
}

/// Whether two base URLs name the same Portal. Host names are case-insensitive,
/// so the whole URL is compared that way; the path belongs to one Portal.
fn same_portal(a: &str, b: &str) -> bool {
    base_of(a).eq_ignore_ascii_case(&base_of(b))
}

fn session_path() -> PathBuf {
    crate::config::dir().join(SESSION_FILE)
}

/// The stored session jar, if there is one.
pub fn session() -> Option<Session> {
    let raw = match secret::get(SESSION_KEY) {
        Some(s) => s,
        None => std::fs::read_to_string(session_path()).ok()?,
    };
    parse_session(&raw)
}

/// A stored record as a jar. A record that names no Portal is not a session.
fn parse_session(raw: &str) -> Option<Session> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let s: Session = serde_json::from_str(t).ok()?;
    let base = base_of(&s.base);
    if base.is_empty() {
        return None;
    }
    Some(Session {
        base,
        email: s.email.trim().to_string(),
        cookies: s.cookies.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
    })
}

/// Stores the session jar. The Keychain is asked first; where it is absent (or
/// disabled for a headless run) a 0600 file under the app's own data folder
/// stands in, exactly as it does for the token.
///
/// There is no password parameter here, and nowhere in this struct to put one.
pub fn save_session(s: &Session) -> Result<(), String> {
    let s = parse_session(&serde_json::to_string(s).unwrap_or_default())
        .ok_or_else(|| "the session names no Portal".to_string())?;
    let text = serde_json::to_string(&s).map_err(|e| format!("could not write the Portal session: {}", e))?;
    if secret::set(SESSION_KEY, &text).is_ok() {
        let _ = std::fs::remove_file(session_path());
        return Ok(());
    }
    use std::io::Write;
    let p = session_path();
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    crate::fileio::mode(&mut opts, 0o600);
    let mut f = opts.open(&p).map_err(|e| format!("could not store the Portal session: {}", e))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("could not store the Portal session: {}", e))
}

/// Forgets the session jar. The bearer token, if any, is left alone: it is a
/// different credential under a different key.
pub fn clear_session() {
    secret::delete(SESSION_KEY);
    let _ = std::fs::remove_file(session_path());
}

/// The admin the stored session belongs to, when it named one.
pub fn signed_in_email() -> Option<String> {
    session().map(|s| s.email).filter(|e| !e.is_empty())
}

// ---------- which credential ----------

/// How one call authenticates.
#[derive(Clone)]
pub enum Credential {
    /// `Authorization: Bearer <token>`: Portal's `PORTAL_API_TOKEN`.
    Bearer(String),
    /// The cookie Portal set at sign-in, sent back as `Cookie:`.
    Session(Arc<Session>),
}

impl Credential {
    /// The word a status line uses for it.
    pub fn kind(&self) -> &'static str {
        match self {
            Credential::Bearer(_) => "token",
            Credential::Session(_) => "session",
        }
    }
}

/// Neither the token nor a cookie value: this is what a log line may print.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Bearer(_) => write!(f, "Bearer(…)"),
            Credential::Session(s) => {
                write!(f, "Session {{ email: {:?}, {} cookie(s) }}", s.email, s.cookies.len())
            }
        }
    }
}

/// Which credential a call uses: a stored bearer token wins (a headless install
/// has one and nothing else), then a stored session for this same Portal, then
/// nothing at all.
pub fn pick(base: &str, token: Option<String>, session: Option<Session>) -> Option<Credential> {
    if let Some(t) = token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        return Some(Credential::Bearer(t));
    }
    session
        .filter(|s| same_portal(&s.base, base))
        .map(|s| Credential::Session(Arc::new(s)))
}

// ---------- tolerant reading ----------

/// `null` where a value is expected reads as the value's default. This is what
/// `#[serde(default)]` does not do: it covers an absent key, not a present one
/// that is `null`.
pub fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

fn text_at(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// A Frame.io folder as Portal names it: `{"id": …, "name": …, "account_id":
/// …}`, or just its name as a string, or nothing at all.
///
/// A live deployment sends the folder as `{"type":"frameio","account_id":…,
/// "folder_id":…,"path":[{"id":…,"name":…}],"project_id":…,"turbosort":…}`: the
/// id is under `folder_id`, and the name is the last element of `path` rather
/// than a `name` key. Both spellings are read, because getting this wrong is not
/// a parse error — it is a link that is silently skipped for having no folder.
///
/// It may also carry `sync` (see [`Sync`]), which turns the link's one mirror
/// into two one-way folders. Absent, the link is exactly what it was.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub account_id: String,
    /// The link's two one-way folders, when Portal says the link is not one
    /// mirror. `None` — the normal case, and the only one an older Portal sends
    /// — is the single mirrored watch this module has always made.
    pub sync: Option<Sync>,
}

/// One side of a link's `sync` object: the local folder name and the Frame.io
/// folder it belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncFolder {
    /// One path segment, under `<portal root>/<turbosort>`. The same rule the
    /// marker follows: no `/`, `\`, `%`, control characters, `.` or `..`.
    pub local: String,
    /// The Frame.io folder. Empty when Portal named none: for the drive side
    /// that means the link's own folder, and for the exports side there is
    /// nothing to default to.
    pub folder_id: String,
}

/// The `sync` object Portal may add **inside a link's existing `folder`**, which
/// `GET /api/upload-links` already returns verbatim:
///
/// ```json
/// "folder": {
///   "type": "frameio", "account_id": "…", "folder_id": "…", "path": […],
///   "turbosort": "20261028_YML-XMAS-Luc",
///   "sync": {
///     "mode": "add_only",
///     "drive":   {"local": "1_DRIVE",   "folder_id": "…"},
///     "exports": {"local": "0_EXPORTS", "folder_id": "…"}
///   }
/// }
/// ```
///
/// `add_only` is the mode this build implements, and the only one: two one-way
/// folders, and nothing is ever deleted at either end. `drive` is the folder the
/// client sends into (Frame.io → local); `exports` is the one we make things in
/// (local → Frame.io) and may be absent. An unstated or unknown mode is read as
/// `add_only`, and the reconciler notes it instead of refusing the link: a link
/// that watches nothing is worse than one that watches what the contract says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Sync {
    pub mode: String,
    pub drive: Option<SyncFolder>,
    pub exports: Option<SyncFolder>,
}

/// The one mode this build knows: two one-way folders, no deletions propagated.
pub const SYNC_ADD_ONLY: &str = "add_only";

impl Sync {
    /// The mode, trimmed and lowercased. Empty when Portal said nothing.
    pub fn mode(&self) -> String {
        self.mode.trim().to_ascii_lowercase()
    }

    /// Whether this build can act on the mode. An unstated mode is `add_only`;
    /// anything else is not this build's to guess at, and is noted as read as
    /// `add_only` rather than refused.
    pub fn is_known_mode(&self) -> bool {
        let m = self.mode();
        m.is_empty() || m == SYNC_ADD_ONLY
    }
}

/// The `sync` object of a `folder`, when there is a usable one.
///
/// A `sync` that is present but not an object is read as absent, and the link
/// then keeps the single mirrored watch it has today. Guessing here would be the
/// one wrong guess that turns a one-way folder into a mirror; leaving it alone
/// means a malformed row changes nothing.
fn sync_at(v: &Value) -> Option<Sync> {
    let s = v.get("sync")?;
    if !s.is_object() {
        return None;
    }
    Some(Sync { mode: text_at(s, "mode"), drive: sync_folder_at(s.get("drive")), exports: sync_folder_at(s.get("exports")) })
}

/// One side of a `sync` object. A side that is absent, `null` or not an object
/// is `None`: Portal named no such folder.
fn sync_folder_at(v: Option<&Value>) -> Option<SyncFolder> {
    let v = v?;
    if !v.is_object() {
        return None;
    }
    let folder_id = {
        let id = text_at(v, "folder_id");
        if id.is_empty() {
            text_at(v, "id")
        } else {
            id
        }
    };
    Some(SyncFolder { local: text_at(v, "local"), folder_id })
}

impl<'de> Deserialize<'de> for Folder {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Folder, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(match &v {
            Value::String(s) => Folder {
                id: String::new(),
                name: s.trim().to_string(),
                account_id: String::new(),
                sync: None,
            },
            Value::Object(_) => Folder {
                id: {
                    let id = text_at(&v, "id");
                    if id.is_empty() { text_at(&v, "folder_id") } else { id }
                },
                name: {
                    let name = text_at(&v, "name");
                    if !name.is_empty() {
                        name
                    } else {
                        // `path` is the breadcrumb, root first: the folder itself is
                        // its last element.
                        v.get("path")
                            .and_then(|p| p.as_array())
                            .and_then(|a| a.last())
                            .map(|last| text_at(last, "name"))
                            .unwrap_or_default()
                    }
                },
                account_id: {
                    let a = text_at(&v, "account_id");
                    if a.is_empty() {
                        text_at(&v, "target_account_id")
                    } else {
                        a
                    }
                },
                sync: sync_at(&v),
            },
            _ => Folder::default(),
        })
    }
}

/// One row of `GET /api/upload-links`.
///
/// Only the fields the brief and the service name are modelled; every one is
/// optional on the wire, and unknown fields are ignored (serde's own default),
/// so a row that grows a field keeps working.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UploadLink {
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    /// A display name, when Portal has one.
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    /// The same, under the other word Portal uses for it.
    #[serde(default, deserialize_with = "nullable")]
    pub title: String,
    #[serde(default, deserialize_with = "nullable")]
    pub target_folder_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub target_folder_name: String,
    /// The Frame.io account the target folder is in. Not named in the brief; a
    /// link that does not carry it (here, or on `folder`) cannot be watched, and
    /// the reconciler says so instead of inventing one.
    #[serde(default, deserialize_with = "nullable")]
    pub target_account_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub account_id: String,
    /// The `.turbosort` marker: the `Live_Work` folder this project belongs to.
    #[serde(default, deserialize_with = "nullable")]
    pub turbosort: String,
    /// The Frame.io folder, as an object or as a bare name.
    #[serde(default)]
    pub folder: Folder,
    /// Portal's word for a link that is not live.
    #[serde(default, deserialize_with = "nullable")]
    pub status: String,
    #[serde(default, deserialize_with = "nullable")]
    pub disabled: bool,
    /// The same flag the other way round; absent means "not said".
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// The statuses that mean a link is not live. Portal's exact vocabulary is not
/// documented in anything this crate can see, so the words a row may carry are
/// listed here rather than guessed at one call site.
const OFF_STATUSES: [&str; 6] = ["disabled", "inactive", "archived", "deleted", "expired", "paused"];

impl UploadLink {
    /// The marker, trimmed. Empty means Portal is not asking for a watch.
    pub fn marker(&self) -> String {
        self.turbosort.trim().to_string()
    }

    /// The folder name to show on the watch row: the first name Portal gave it.
    pub fn label(&self) -> String {
        for s in [&self.name, &self.title, &self.target_folder_name, &self.folder.name, &self.target_folder_id, &self.id] {
            if !s.is_empty() {
                return s.clone();
            }
        }
        String::new()
    }

    /// The Frame.io account. Empty when the row does not say.
    pub fn acct(&self) -> String {
        for s in [&self.target_account_id, &self.account_id, &self.folder.account_id] {
            if !s.is_empty() {
                return s.clone();
            }
        }
        String::new()
    }

    /// The Frame.io folder the link publishes into. Empty when the row does not
    /// say.
    pub fn dest_folder(&self) -> String {
        for s in [&self.target_folder_id, &self.folder.id] {
            if !s.is_empty() {
                return s.clone();
            }
        }
        String::new()
    }

    /// Whether Portal says this link is not live: its watch goes.
    pub fn is_disabled(&self) -> bool {
        if self.disabled || self.enabled == Some(false) {
            return true;
        }
        let st = self.status.trim().to_ascii_lowercase();
        !st.is_empty() && OFF_STATUSES.contains(&st.as_str())
    }
}

/// One row of `GET /api/sync-rules`.
///
/// Read-only reference: it says what Portal's own worker would do with a
/// marker, including a `destination_path` and `path_template` under *its* disk.
/// None of it is adopted — the local root is a FioTransfer setting — so these
/// rows are for showing, not for acting on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncRule {
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    #[serde(default, deserialize_with = "nullable")]
    pub turbosort: String,
    #[serde(default, deserialize_with = "nullable")]
    pub destination_path: String,
    #[serde(default, deserialize_with = "nullable")]
    pub path_template: String,
    #[serde(default, deserialize_with = "nullable")]
    pub target_folder_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub target_folder_name: String,
}

// ---------- the client ----------

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().timeout(TIMEOUT).build().expect("portal client"))
}

/// The `name=value` at the front of a `Set-Cookie` value, when it has one: what
/// makes a reply a session worth keeping. A deletion (`name=; Max-Age=0`) has
/// no value and is not kept.
fn cookie_pair(set_cookie: &str) -> Option<(&str, &str)> {
    let kv = set_cookie.split(';').next()?.trim();
    let (name, value) = kv.split_once('=')?;
    let (name, value) = (name.trim(), value.trim());
    if name.is_empty() || value.is_empty() {
        None
    } else {
        Some((name, value))
    }
}

/// A stored cookie as it is sent to this Portal.
///
/// Portal always writes `Secure` — `SESSION_COOKIE_SECURE` defaults true, even
/// behind a plain-http Caddy — and a cookie store sends a `Secure` cookie only
/// over https or to loopback (Firefox's rule, which `reqwest` follows). So on a
/// plain-http Portal that is not loopback the attribute is dropped before the jar
/// parses it: the password just crossed that same plain http, so the cookie is no
/// more exposed, and refusing it would make sign-in impossible on exactly the
/// small deployments that need it. Over https, and for a cookie that is not
/// `Secure`, the string is kept verbatim.
fn cookie_for_plain_http(plain_http: bool, set_cookie: &str) -> String {
    if !plain_http {
        return set_cookie.to_string();
    }
    set_cookie
        .split(';')
        .filter(|part| !part.trim().eq_ignore_ascii_case("secure"))
        .collect::<Vec<_>>()
        .join(";")
}

/// What `POST /api/auth/login` answered: a finished sign-in, or the password
/// step of one that Portal wants a one-time code for.
///
/// Both carry a client, because the difference is a second request on the same
/// session — not a different session. Neither holds the password, and neither
/// holds a code.
pub enum SignIn {
    /// Portal accepted the email and the password: the client holds the session
    /// cookie, and `store_session` keeps it.
    Done(PortalClient),
    /// Portal accepted the email and the password *and* wants a one-time code.
    /// The client holds Portal's pending session — a uid and a timestamp, not a
    /// signed-in admin — so it can finish the sign-in and can do nothing else.
    Totp(PortalClient),
}

/// A cookie-enabled client, seeded from a stored jar: `reqwest` adds `Cookie:`
/// to every request and keeps what a reply sets.
///
/// The jar itself is not read back — it is write-only — which is why the login
/// reads the `Set-Cookie` headers off its own reply and stores those.
fn session_http(base: &str, cookies: &[String]) -> Result<reqwest::Client, PortalErr> {
    let url = reqwest::Url::parse(&format!("{}/", base))
        .map_err(|e| PortalErr::Http(format!("the Portal address {:?} is not a URL: {}", base, e)))?;
    let plain = url.scheme() == "http";
    let jar = reqwest::cookie::Jar::default();
    for c in cookies {
        jar.add_cookie_str(&cookie_for_plain_http(plain, c), &url);
    }
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .cookie_provider(Arc::new(jar))
        .build()
        .map_err(|e| PortalErr::Http(e.to_string()))
}

/// Portal's read endpoints, with whichever credential is stored: a bearer token
/// or the cookie Portal's own sign-in issued.
///
/// `Debug` prints the base URL and which credential it uses, never the token and
/// never a cookie: a client is exactly the kind of value a log line prints.
pub struct PortalClient {
    base: String,
    cred: Credential,
    /// The HTTP client for this credential: the shared one for a bearer token,
    /// one of its own — with the cookie jar — for a session.
    http: reqwest::Client,
    /// Set while Portal has taken the password and is waiting for a one-time
    /// code: the email that was typed, so a sign-in can still be named when
    /// `GET /api/auth/me` cannot be read. The jar in `cred` is Portal's
    /// *pending* session — it is not a signed-in session and is never stored.
    pending: Option<Pending>,
}

/// A sign-in Portal asked a second factor for. The email only: the password is
/// gone the moment the login reply arrived, and a code has not been typed yet.
#[derive(Clone)]
struct Pending {
    email: String,
}

impl Clone for PortalClient {
    fn clone(&self) -> PortalClient {
        PortalClient {
            base: self.base.clone(),
            cred: self.cred.clone(),
            http: self.http.clone(),
            pending: self.pending.clone(),
        }
    }
}

impl std::fmt::Debug for PortalClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortalClient")
            .field("base", &self.base)
            .field("credential", &self.cred)
            .field("waiting_for_a_code", &self.pending.is_some())
            .finish()
    }
}

impl PortalClient {
    /// `base_url` is used as given, less a trailing slash, so `base + "/api/…"`
    /// has exactly one separator.
    pub fn new(base_url: &str, token: &str) -> PortalClient {
        PortalClient {
            base: base_of(base_url),
            cred: Credential::Bearer(token.trim().to_string()),
            http: http().clone(),
            pending: None,
        }
    }

    /// A client that sends a stored jar instead of a bearer token.
    pub fn with_session(base_url: &str, session: Session) -> Result<PortalClient, PortalErr> {
        let base = base_of(base_url);
        if base.is_empty() {
            return Err(PortalErr::NoUrl);
        }
        let http = session_http(&base, &session.cookies)?;
        Ok(PortalClient { base, cred: Credential::Session(Arc::new(session)), http, pending: None })
    }

    /// The client the app builds: the base URL from the settings, then the first
    /// credential that is stored — the bearer token, or a session this Portal
    /// issued. `NoUrl` / `NoToken` when there is neither, so the caller never
    /// calls Portal half-configured.
    pub fn from_settings(s: &crate::config::Settings) -> Result<PortalClient, PortalErr> {
        if s.portal_url.trim().is_empty() {
            return Err(PortalErr::NoUrl);
        }
        let base = base_of(&s.portal_url);
        let stored = session();
        let elsewhere = stored.as_ref().map(|st| st.base.clone());
        match pick(&base, token(), stored) {
            Some(Credential::Bearer(t)) => Ok(PortalClient::new(&base, &t)),
            Some(Credential::Session(sess)) => PortalClient::with_session(&base, (*sess).clone()),
            // A jar for another Portal is not this Portal's; saying which is
            // more use than saying nothing is stored.
            None => match elsewhere {
                Some(stored) => Err(PortalErr::SessionFor { stored, wanted: base }),
                None => Err(PortalErr::NoToken),
            },
        }
    }

    /// The jar this client holds, when it signed in with one. While Portal is
    /// waiting for a one-time code this is its *pending* session: see
    /// [`PortalClient::waiting_for_code`].
    pub fn session(&self) -> Option<&Session> {
        match &self.cred {
            Credential::Session(s) => Some(s),
            Credential::Bearer(_) => None,
        }
    }

    /// Whether Portal has taken the password and is waiting for a one-time
    /// code: the sign-in is not finished and there is nothing to keep yet.
    pub fn waiting_for_code(&self) -> bool {
        self.pending.is_some()
    }

    /// Which credential the calls use: `token` or `session`.
    pub fn credential(&self) -> &'static str {
        self.cred.kind()
    }

    /// Keeps this client's jar in the Keychain, under its own key. The password
    /// is not here: the client never held it. A half-finished sign-in is
    /// refused: its cookie says "a uid is waiting for a code", which is not a
    /// session any call would accept.
    pub fn store_session(&self) -> Result<(), String> {
        if self.pending.is_some() {
            return Err(
                "Portal is waiting for a one-time code: finish the sign-in before keeping the session"
                    .to_string(),
            );
        }
        match self.session() {
            Some(s) => save_session(s),
            None => Err("this client is not signed in with a session".to_string()),
        }
    }

    /// Portal's own admin sign-in: `POST /api/auth/login` with the email and the
    /// password, answered with a `tower-sessions` cookie.
    ///
    /// Portal's request body is `{"email", "password"}` and its reply is
    /// `{"totp_required", "id", "email"}`; on a wrong password it answers 401
    /// `{"detail": "Invalid email or password"}`. The cookie arrives on that
    /// reply's `Set-Cookie`, so it is read here — once — and kept as the
    /// client's [`Session`]. The password is read from the argument, used for
    /// this one request, and dropped.
    ///
    /// Two answers are a success: [`SignIn::Done`], the client a signed-in
    /// session makes, and [`SignIn::Totp`], the same client holding Portal's
    /// *pending* session because this account has a second factor. A password
    /// sign-in that stops there stores nothing; [`PortalClient::login_totp`]
    /// is what finishes it.
    pub async fn signin(base_url: &str, email: &str, password: &str) -> Result<SignIn, PortalErr> {
        let base = base_of(base_url);
        if base.is_empty() {
            return Err(PortalErr::NoUrl);
        }
        let url = reqwest::Url::parse(&format!("{}/api/auth/login", base))
            .map_err(|e| PortalErr::Http(format!("the Portal address {:?} is not a URL: {}", base, e)))?;
        let jar = reqwest::cookie::Jar::default();
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .cookie_provider(Arc::new(jar))
            .build()
            .map_err(|e| PortalErr::Http(e.to_string()))?;
        let body = serde_json::json!({ "email": email.trim(), "password": password });
        // One attempt: a sign-in is not a safe request, and Portal rate-limits
        // this route. A failure is the answer.
        let res = http
            .post(url)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| PortalErr::Http(e.to_string()))?;
        let status = res.status().as_u16();
        let cookies: Vec<String> = res
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| cookie_pair(v).is_some())
            .collect();
        let data = res.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
        if status == 401 || status == 403 {
            return Err(PortalErr::Login { status, detail: detail_of(&crate::netx::trim_body(&data)) });
        }
        if status >= 300 {
            return Err(PortalErr::Status { status, body: crate::netx::trim_body(&data) });
        }
        let v: Value = json_body(&data)?;
        if v.get("totp_required").and_then(|b| b.as_bool()).unwrap_or(false) {
            // The password was right and half a sign-in exists: Portal keeps the
            // pending uid in the same session, so the cookie is kept — as a
            // *pending* jar, which no call of this client uses and which is
            // never stored as a session.
            if cookies.is_empty() {
                return Err(PortalErr::NoSessionCookie);
            }
            // Built the way every session client is, so a plain-http Portal
            // gets the same treatment of its `Secure` cookie as it does on the
            // next run — the pending uid is in that cookie.
            let at = base.clone();
            let jar = Session { base, email: email.trim().to_string(), cookies };
            let mut client = PortalClient::with_session(&at, jar)?;
            client.pending = Some(Pending { email: email.trim().to_string() });
            return Ok(SignIn::Totp(client));
        }
        if cookies.is_empty() {
            return Err(PortalErr::NoSessionCookie);
        }
        let named = text_at(&v, "email");
        let session = Session {
            base: base.clone(),
            email: if named.is_empty() { email.trim().to_string() } else { named },
            cookies,
        };
        Ok(SignIn::Done(PortalClient::with_session(&base, session)?))
    }

    /// The same sign-in, for a caller that cannot supply a one-time code: a
    /// second factor is the end of the road here, reported as
    /// [`PortalErr::TotpRequired`]. The pending jar is dropped, because nothing
    /// is left that could finish the sign-in — a caller that can finish it uses
    /// [`PortalClient::signin`] and [`PortalClient::login_totp`].
    pub async fn login(base_url: &str, email: &str, password: &str) -> Result<PortalClient, PortalErr> {
        match PortalClient::signin(base_url, email, password).await? {
            SignIn::Done(c) => Ok(c),
            SignIn::Totp(_) => Err(PortalErr::TotpRequired),
        }
    }

    /// `POST /api/auth/login/totp` with the six digits, on this same client and
    /// so on the same session cookie the password step left behind — which is
    /// what Portal requires: the code finishes *that* session, and a jar of its
    /// own would have no pending uid in it.
    ///
    /// On Portal's yes the client becomes a signed-in one: the session cookie
    /// Portal re-set is kept, `GET /api/auth/me` names the admin, and the jar is
    /// stored exactly as [`PortalClient::login`]'s is (Keychain, or the 0600
    /// stand-in). On Portal's no nothing is stored and nothing changes: the
    /// refusal is [`PortalErr::TotpRefused`] with Portal's own sentence, which
    /// says whether the code was wrong or the pending step is over.
    ///
    /// The code is this call's argument and nothing else. It is not stored, not
    /// logged, and not part of any error: only Portal's reply is.
    pub async fn login_totp(&mut self, code: &str) -> Result<(), PortalErr> {
        let Some(pending) = self.pending.clone() else {
            return Err(PortalErr::NoPendingSignIn);
        };
        // Portal normalises spaces and dashes itself; trimming is all this needs
        // to do, and an empty code is not worth one of the five attempts Portal
        // gives the pending step.
        let code = code.trim();
        if code.is_empty() {
            return Err(PortalErr::TotpRequired);
        }
        let url = reqwest::Url::parse(&format!("{}/api/auth/login/totp", self.base))
            .map_err(|e| PortalErr::Http(format!("the Portal address {:?} is not a URL: {}", self.base, e)))?;
        let body = serde_json::json!({ "code": code });
        // One attempt, as `login` makes one: Portal rate-limits this route too.
        let res = self
            .http
            .post(url)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| PortalErr::Http(e.to_string()))?;
        let status = res.status().as_u16();
        let cookies: Vec<String> = res
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| cookie_pair(v).is_some())
            .collect();
        let data = res.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
        if status == 401 || status == 403 {
            return Err(PortalErr::TotpRefused { status, detail: detail_of(&crate::netx::trim_body(&data)) });
        }
        if status >= 300 {
            return Err(PortalErr::Status { status, body: crate::netx::trim_body(&data) });
        }
        // Portal re-sets the session cookie with the signed-in uid in it. If it
        // sent none, the jar already held is the one it means: the same cookie,
        // and `/api/auth/me` below is what decides whether it is signed in.
        let held = self.session().map(|s| s.cookies.clone()).unwrap_or_default();
        let jar = if cookies.is_empty() { held } else { cookies };
        self.cred = Credential::Session(Arc::new(Session {
            base: self.base.clone(),
            email: pending.email.clone(),
            cookies: jar,
        }));
        // The same confirmation a password sign-in makes. A session Portal will
        // not accept is not stored; a read that merely failed is not proof, so
        // the typed address names the session, as `App::portal_sign_in` has it.
        let email = match self.me().await {
            Ok(who) => who.email,
            Err(e @ PortalErr::SessionExpired { .. }) => return Err(e),
            Err(_) => pending.email.clone(),
        };
        let session = match self.session() {
            Some(s) => s.named(&email),
            None => return Err(PortalErr::NoSessionCookie),
        };
        save_session(&session).map_err(PortalErr::Store)?;
        self.cred = Credential::Session(Arc::new(session));
        self.pending = None;
        Ok(())
    }

    /// The URL one call goes to.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Who the credential belongs to: `GET /api/auth/me`, `{"id", "email"}`.
    /// For a session this is the name the Settings screen shows.
    pub async fn me(&self) -> Result<Who, PortalErr> {
        let raw = self.get("/api/auth/me").await?;
        let v: Value = json_body(&raw)?;
        let who = Who { id: text_at(&v, "id"), email: text_at(&v, "email") };
        if who.email.is_empty() {
            return Err(PortalErr::Json("no email in the reply".to_string()));
        }
        Ok(who)
    }

    /// The links, each with its `target_folder_id`, `target_folder_name`,
    /// `folder` and `turbosort`.
    pub async fn upload_links(&self) -> Result<Vec<UploadLink>, PortalErr> {
        let raw = self.get("/api/upload-links").await?;
        let rows = rows(&raw, "upload_links")?;
        let mut out: Vec<UploadLink> = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(serde_json::from_value(r).map_err(|e| PortalErr::Json(e.to_string()))?);
        }
        Ok(out)
    }

    /// Portal's own sync rules. Read-only: nothing here acts on them.
    pub async fn sync_rules(&self) -> Result<Vec<SyncRule>, PortalErr> {
        let raw = self.get("/api/sync-rules").await?;
        let rows = rows(&raw, "sync_rules")?;
        let mut out: Vec<SyncRule> = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(serde_json::from_value(r).map_err(|e| PortalErr::Json(e.to_string()))?);
        }
        Ok(out)
    }

    /// One GET, retried the way `api::Client` retries: exponential backoff from
    /// `netx::backoff`, a `Retry-After` honoured when Portal sends one, a
    /// connect error or timeout repeated (a GET is safe to repeat), and 429 or a
    /// server error repeated. 4xx is the answer: it is not tried again.
    async fn get(&self, path: &str) -> Result<Vec<u8>, PortalErr> {
        if self.base.is_empty() {
            return Err(PortalErr::NoUrl);
        }
        if let Credential::Bearer(t) = &self.cred {
            if t.is_empty() {
                return Err(PortalErr::NoToken);
            }
        }
        let url = self.url(path);
        let mut attempt: u32 = 0;
        loop {
            // A bearer token goes in the header; a session's cookie is added to
            // every request by the client's cookie store, from the stored jar.
            let mut req = self.http.get(&url).header("Accept", "application/json");
            if let Credential::Bearer(t) = &self.cred {
                req = req.header("Authorization", format!("Bearer {}", t));
            }
            let res = req.send().await;
            let resp = match res {
                Ok(r) => r,
                Err(e) => {
                    // A GET that never reached Portal can always be repeated; a
                    // 30 s timeout is the one case where it may have been read.
                    if attempt >= MAX_ATTEMPTS || !(e.is_connect() || e.is_timeout()) {
                        return Err(PortalErr::Http(e.to_string()));
                    }
                    let d = crate::netx::backoff(attempt);
                    attempt += 1;
                    tokio::time::sleep(d).await;
                    continue;
                }
            };
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let data = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
            if status == 429 || status >= 500 {
                if attempt >= MAX_ATTEMPTS {
                    return Err(PortalErr::Status { status, body: crate::netx::trim_body(&data) });
                }
                let d = match retry_after {
                    Some(s) if s > 0 => Duration::from_secs(s),
                    _ => crate::netx::backoff(attempt),
                };
                attempt += 1;
                tokio::time::sleep(d).await;
                continue;
            }
            if status >= 300 {
                // A session Portal no longer accepts is not a refused token: the
                // cookie expired or was signed out, and the fix is a fresh
                // sign-in, so it gets its own message.
                return Err(match (&self.cred, status) {
                    (Credential::Session(_), 401) | (Credential::Session(_), 403) => {
                        PortalErr::SessionExpired { status }
                    }
                    _ => PortalErr::Status { status, body: crate::netx::trim_body(&data) },
                });
            }
            return Ok(data);
        }
    }
}

/// Who a credential belongs to: `GET /api/auth/me`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Who {
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub email: String,
}

/// The list inside a Portal reply.
///
/// A bare array, or an object that carries it under the endpoint's own name
/// (`upload_links`, `sync_rules`), or under `data` / `items` / `results` /
/// `rows`, one level down at most. `links` is deliberately not searched: it is
/// how Frame.io's envelopes carry pagination, and reading a cursor as a list
/// would be worse than saying the reply is not understood.
pub fn rows(raw: &[u8], key: &str) -> Result<Vec<Value>, PortalErr> {
    let v: Value = json_body(raw)?;
    if let Value::Array(a) = &v {
        return Ok(a.clone());
    }
    let obj = match v.as_object() {
        Some(o) => o,
        None => return Err(PortalErr::Json("expected a list of rows".to_string())),
    };
    for k in [key, "data", "items", "results", "rows"] {
        match obj.get(k) {
            Some(Value::Array(a)) => return Ok(a.clone()),
            Some(Value::Object(inner)) => {
                if let Some(Value::Array(a)) = inner.get(key) {
                    return Ok(a.clone());
                }
                if k == "data" {
                    if let Some(Value::Array(a)) = inner.get("data") {
                        return Ok(a.clone());
                    }
                }
            }
            _ => {}
        }
    }
    Err(PortalErr::Json(format!("no {} list in the reply", key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_reads_as_the_default_and_unknown_fields_are_ignored() {
        let raw = r#"{"id":"L1","name":null,"target_folder_id":"F1","target_folder_name":"Reel",
                      "folder":{"id":"F1","name":null,"account_id":"A1"},"turbosort":"Rakete",
                      "something_new":{"a":1}}"#;
        let l: UploadLink = serde_json::from_str(raw).expect("tolerant read");
        assert_eq!(l.id, "L1");
        assert_eq!(l.name, "", "a null name is an empty name");
        assert_eq!(l.marker(), "Rakete");
        assert_eq!(l.acct(), "A1");
        assert_eq!(l.dest_folder(), "F1");
        assert_eq!(l.label(), "Reel", "the folder name when no link name survives");
    }

    #[test]
    fn a_folder_may_be_just_a_name() {
        let l: UploadLink = serde_json::from_str(r#"{"id":"L2","folder":"Projekt X","turbosort":"X"}"#).unwrap();
        assert_eq!(l.folder.name, "Projekt X");
        assert_eq!(l.label(), "Projekt X");
        assert_eq!(l.dest_folder(), "", "a bare name is not an id");
    }

    #[test]
    fn a_link_says_when_it_is_off() {
        let on: UploadLink = serde_json::from_str(r#"{"id":"L3"}"#).unwrap();
        assert!(!on.is_disabled());
        for raw in [r#"{"disabled":true}"#, r#"{"enabled":false}"#, r#"{"status":"Archived"}"#] {
            let l: UploadLink = serde_json::from_str(raw).unwrap();
            assert!(l.is_disabled(), "{} says the link is off", raw);
        }
        let l: UploadLink = serde_json::from_str(r#"{"enabled":true,"status":"live"}"#).unwrap();
        assert!(!l.is_disabled());
    }

    #[test]
    fn rows_are_found_in_every_shape_portal_is_known_to_send() {
        let one = r#"[{"id":"a"}]"#;
        assert_eq!(rows(one.as_bytes(), "upload_links").unwrap().len(), 1);
        for raw in [
            r#"{"upload_links":[{"id":"a"}]}"#,
            r#"{"data":[{"id":"a"}]}"#,
            r#"{"items":[{"id":"a"}]}"#,
            r#"{"data":{"upload_links":[{"id":"a"}]}}"#,
        ] {
            assert_eq!(rows(raw.as_bytes(), "upload_links").unwrap().len(), 1, "{}", raw);
        }
        // An empty list is a list, not an error.
        assert_eq!(rows(br#"{"upload_links":[]}"#, "upload_links").unwrap().len(), 0);
        // A cursor is not a list, and a row that is not an object is not a row.
        assert!(rows(br#"{"links":{"next":"/x"}}"#, "upload_links").is_err());
        assert!(rows(br#"{"data":{}}"#, "upload_links").is_err());
        assert!(rows(b"nope", "upload_links").is_err());
    }

    #[test]
    fn a_client_refuses_a_missing_url_before_it_touches_anything() {
        let s = crate::config::Settings::default();
        assert!(matches!(PortalClient::from_settings(&s), Err(PortalErr::NoUrl)));
    }

    #[test]
    fn a_client_joins_one_separator() {
        let c = PortalClient::new("https://portal.example.com/", " t ");
        assert_eq!(c.url("/api/upload-links"), "https://portal.example.com/api/upload-links");
        match &c.cred {
            Credential::Bearer(t) => assert_eq!(t, "t", "the token is trimmed"),
            Credential::Session(_) => panic!("a bearer token makes a bearer client"),
        }
        assert_eq!(c.credential(), "token");
    }

    fn session_at(base: &str) -> Session {
        Session {
            base: base.to_string(),
            email: "chaos@localhost".to_string(),
            cookies: vec!["portal_session=eyJ1c2VyX2lkIjoiYWJjIn0.auQbjA.sig; path=/; Max-Age=1209600; httponly; samesite=lax; secure".to_string()],
        }
    }

    #[test]
    fn a_stored_token_wins_then_a_session_for_this_portal_then_nothing() {
        let s = session_at("https://portal.example.com");
        match pick("https://portal.example.com/", Some("tok".to_string()), Some(s.clone())) {
            Some(Credential::Bearer(t)) => assert_eq!(t, "tok", "the token wins"),
            other => panic!("a stored token must win: {:?}", other.map(|c| c.kind())),
        }
        match pick("https://portal.example.com", None, Some(s.clone())) {
            Some(Credential::Session(got)) => assert_eq!(got.email, "chaos@localhost"),
            other => panic!("the session is used when no token is stored: {:?}", other.map(|c| c.kind())),
        }
        // A jar is only replayed to the Portal that issued it.
        assert!(pick("https://elsewhere.example.com", None, Some(s.clone())).is_none());
        // A blank token is no token.
        match pick("https://portal.example.com", Some("  ".to_string()), Some(s.clone())) {
            Some(Credential::Session(_)) => {}
            other => panic!("an empty token is not a credential: {:?}", other.map(|c| c.kind())),
        }
        assert!(pick("https://portal.example.com", None, None).is_none());
    }

    #[test]
    fn the_session_record_is_the_jar_as_json_and_holds_no_password() {
        let s = session_at("https://portal.example.com");
        let text = serde_json::to_string(&s).expect("json");
        assert!(text.contains("portal_session="), "the cookie is the jar: {text}");
        assert!(!text.to_lowercase().contains("password"), "{text}");
        assert_eq!(serde_json::from_str::<Session>(&text).expect("back"), s);

        // A record that names no Portal is not a session.
        assert!(parse_session(r#"{"email":"x@y","cookies":["portal_session=1"]}"#).is_none());
        assert!(parse_session("   ").is_none());
        assert!(parse_session("not json").is_none());
        // A jar whose cookies are gone is still a session: it reads as expired.
        let empty = parse_session(r#"{"base":"https://p.example","email":"x@y","cookies":["  ",""]}"#).expect("a session");
        assert!(empty.is_empty(), "an emptied jar has no cookie to send");
        // A jar with no base is not replayed anywhere, so it is not stored. (A jar
        // whose cookies are gone is stored: see `empty` above.)
        assert!(save_session(&Session { base: String::new(), email: "x@y".into(), cookies: vec!["a=b".into()] }).is_err());
    }

    #[test]
    fn what_a_client_prints_never_carries_the_token_or_a_cookie() {
        let c = PortalClient::new("https://portal.example.com", "tok-sekret");
        let text = format!("{:?}", c);
        assert!(!text.contains("tok-sekret"), "{text}");
        assert!(text.contains("https://portal.example.com"), "the address is useful in a log: {text}");

        let s = session_at("https://portal.example.com");
        let jar = format!("{:?}", s);
        assert!(!jar.contains("eyJ1c2VyX2lk"), "{jar}");
        assert!(!jar.contains("auQbjA"), "{jar}");
        let wrapped = format!("{:?}", Credential::Session(Arc::new(s)));
        assert!(!wrapped.contains("eyJ1c2VyX2lk"), "{wrapped}");
        assert!(wrapped.contains("1 cookie(s)"), "{wrapped}");
    }

    #[test]
    fn an_expired_session_reads_as_its_own_message_not_as_a_refused_token() {
        let expired = PortalErr::SessionExpired { status: 401 }.to_string();
        assert!(expired.contains("session expired"), "{expired}");
        assert!(expired.contains("sign in again"), "{expired}");
        assert!(!expired.contains("refused the token"), "{expired}");

        let refused = PortalErr::Status { status: 401, body: r#"{"detail":"Authentication required"}"#.into() }.to_string();
        assert!(refused.contains("refused the token"), "{refused}");
        assert_ne!(refused, expired);

        // A jar for another Portal says so, and says which.
        let elsewhere = PortalErr::SessionFor {
            stored: "https://portal.example.com".into(),
            wanted: "https://portal.example.org".into(),
        }
        .to_string();
        assert!(elsewhere.contains("belongs to https://portal.example.com"), "{elsewhere}");
        assert!(elsewhere.contains("not https://portal.example.org"), "{elsewhere}");

        let bad = PortalErr::Login { status: 401, detail: "Invalid email or password".into() }.to_string();
        assert_eq!(bad, "Portal refused the sign-in (401): Invalid email or password");
        assert!(PortalErr::TotpRequired.to_string().contains("one-time code"));
        assert!(PortalErr::NoSessionCookie.to_string().contains("no session cookie"));
        assert!(PortalErr::NoToken.to_string().contains("token"));
    }

    #[test]
    fn a_second_factor_has_three_sentences_and_none_of_them_carries_a_code() {
        // Wanted: the state, not a refusal. It asks for the six digits.
        let wanted = PortalErr::TotpRequired.to_string();
        assert!(wanted.contains("wants a one-time code"), "{wanted}");
        assert!(wanted.contains("six digits"), "{wanted}");
        assert!(!wanted.contains("bearer token"), "a token is no longer the only way in: {wanted}");

        // Refused: Portal's own sentence, and never the code that was typed.
        let refused = PortalErr::TotpRefused { status: 401, detail: "Invalid code".into() }.to_string();
        assert_eq!(refused, "Portal refused the one-time code (401): Invalid code");
        let expired = PortalErr::TotpRefused {
            status: 401,
            detail: "Your login session expired — start again".into(),
        }
        .to_string();
        assert!(expired.contains("start again"), "{expired}");
        let bare = PortalErr::TotpRefused { status: 401, detail: "  ".into() }.to_string();
        assert_eq!(bare, "Portal refused the one-time code (401)");

        // No half-finished sign-in to give it to.
        let nothing = PortalErr::NoPendingSignIn.to_string();
        assert!(nothing.contains("sign in with the password first"), "{nothing}");
        assert_eq!(PortalErr::Store("could not store the Portal session: no room".into()).to_string(), "could not store the Portal session: no room");
    }

    #[test]
    fn a_pending_sign_in_is_never_stored_as_a_session() {
        // The shape `signin` builds when Portal answers `totp_required`: the
        // pending jar in `cred`, and `pending` naming the address.
        let client = PortalClient {
            base: "https://portal.example.com".to_string(),
            cred: Credential::Session(Arc::new(session_at("https://portal.example.com"))),
            http: http().clone(),
            pending: Some(Pending { email: "chaos@localhost".to_string() }),
        };
        assert!(client.waiting_for_code());
        let err = client.store_session().expect_err("a pending jar is not a session");
        assert!(err.contains("one-time code"), "{err}");
        // Its `{:?}` says a code is awaited, and prints no cookie.
        let text = format!("{:?}", client);
        assert!(text.contains("waiting_for_a_code: true"), "{text}");
        assert!(!text.contains("eyJ1c2VyX2lk"), "{text}");

        // A signed-in client is not waiting, and is storable.
        let done = PortalClient::with_session("https://portal.example.com", session_at("https://portal.example.com")).unwrap();
        assert!(!done.waiting_for_code());
        assert_eq!(done.credential(), "session");
    }

    #[test]
    fn a_secure_cookie_is_sent_over_plain_http_because_the_password_already_was() {
        let c = "portal_session=abc; path=/; Max-Age=1209600; httponly; samesite=lax; secure";
        assert_eq!(cookie_for_plain_http(false, c), c, "over https, verbatim");
        let plain = cookie_for_plain_http(true, c);
        assert!(!plain.to_ascii_lowercase().contains("secure"), "{plain}");
        assert!(plain.contains("portal_session=abc"), "{plain}");
        assert!(plain.contains("httponly"), "{plain}");
        // A cookie that was never `Secure` is untouched either way.
        let open = "portal_session=abc; path=/";
        assert_eq!(cookie_for_plain_http(true, open), open);
    }

    #[test]
    fn a_set_cookie_is_kept_only_when_it_names_a_value() {
        assert_eq!(cookie_pair("portal_session=abc; path=/; secure"), Some(("portal_session", "abc")));
        assert_eq!(cookie_pair("portal_session=eyJ.abc.sig; httponly"), Some(("portal_session", "eyJ.abc.sig")));
        assert_eq!(cookie_pair("portal_session=; Max-Age=0"), None, "a deletion is not a session");
        assert_eq!(cookie_pair("nonsense"), None);
        assert_eq!(cookie_pair("=value"), None);
    }
    #[test]
    fn an_error_reads_as_advice_not_as_a_json_document() {
        let refused = PortalErr::Status { status: 401, body: r#"{"detail":"Authentication required"}"#.into() };
        let text = refused.to_string();
        assert!(text.contains("refused the token"), "{text}");
        assert!(text.contains("401"), "{text}");
        assert!(!text.contains("detail"), "the JSON wrapper must not reach the user: {text}");

        let wrong_host = PortalErr::Status { status: 404, body: r#"{"detail":"Not Found"}"#.into() };
        assert!(wrong_host.to_string().contains("is that the Portal address"), "{}", wrong_host);

        let broken = PortalErr::Status { status: 502, body: r#"{"detail":"bad gateway upstream"}"#.into() };
        assert_eq!(broken.to_string(), "Portal answered 502: bad gateway upstream");

        let bare = PortalErr::Status { status: 503, body: "  ".into() };
        assert_eq!(bare.to_string(), "Portal answered 503 (no detail)");

        let html = PortalErr::Status { status: 500, body: "<html>oops</html>".into() };
        let text = html.to_string();
        assert!(text.contains("web page"), "{text}");
        assert!(!text.contains("oops"), "a bank of HTML is not a message: {text}");
    }

    #[test]
    fn a_web_page_is_not_reported_as_portals_answer() {
        // What example.com actually answered when the shipped CLI was pointed at it.
        let page = PortalErr::Status {
            status: 405,
            body: "<!doctype html><html lang=\"en\"><head><title>Example Domain</title></head></html>".into(),
        };
        let text = page.to_string();
        assert!(text.contains("web page"), "{text}");
        assert!(text.contains("is that the Portal address"), "{text}");
        assert!(!text.contains("<html"), "HTML reached the user: {text}");
        assert!(text.len() < 160, "the sentence is a status line, not a page: {text}");

        // Portal's own JSON still reads normally.
        let json = PortalErr::Status { status: 502, body: r#"{"detail":"bad gateway upstream"}"#.into() };
        assert_eq!(json.to_string(), "Portal answered 502: bad gateway upstream");
    }

    #[test]
    fn a_login_page_where_json_belongs_says_so() {
        // What a proxy or an auth gate in front of Portal can answer with.
        let page = json_body(b"<!doctype html><html><body>Sign in</body></html>");
        let text = page.unwrap_err().to_string();
        assert!(text.contains("web page"), "{text}");
        assert!(text.contains("proxy"), "{text}");
        assert!(!text.contains("<html"), "HTML reached the user: {text}");

        // A body that is simply not JSON keeps serde's own message, which is
        // accurate for a truncated or empty reply.
        let junk = json_body(b"not json at all").unwrap_err().to_string();
        assert!(junk.contains("expected") || junk.contains("JSON"), "{junk}");

        // And real JSON still parses.
        assert_eq!(json_body(br#"{"a":1}"#).unwrap()["a"], 1);
    }

    #[test]
    fn the_base_url_and_the_path_join_with_exactly_one_separator() {
        // A Portal behind a proxy on a subpath is a normal deployment.
        for base in ["https://host/portal", "https://host/portal/", "https://host/portal///"] {
            let c = PortalClient::new(base, "t");
            assert_eq!(c.url("/api/upload-links"), "https://host/portal/api/upload-links", "base {base:?}");
        }
        // Surrounding whitespace is not part of an address.
        let c = PortalClient::new("  https://host  ", "t");
        assert_eq!(c.url("/api/health"), "https://host/api/health");
        // And an empty base still produces a relative path rather than panicking:
        // `get` refuses before it ever uses it.
        assert_eq!(PortalClient::new("", "t").url("/api/health"), "/api/health");
    }

    #[test]
    fn the_folder_shape_a_live_portal_sends_is_read() {
        // Copied from GET /api/upload-links on a real deployment (abridged).
        let raw = r#"{"id":"1d66131e-b3fc-4dd2-96d3-8970190f9a70","name":"LUC","kind":"upload",
          "turbosort":"20261028_YML-XMAS-Luc",
          "folder":{"path":[{"id":"8008cf70-f28e-474d-a209-d67afb9fddb6","name":"LUC"}],
                    "type":"frameio","folder_id":"8008cf70-f28e-474d-a209-d67afb9fddb6",
                    "turbosort":"20261028_YML-XMAS-Luc",
                    "account_id":"6f4121b3-628d-4b7a-91ea-8b1ac4a3f0a3",
                    "project_id":"7a4609ad-f5ef-4542-9b3b-d2f6c37086d7"}}"#;
        let l: UploadLink = serde_json::from_str(raw).expect("a live row parses");
        assert_eq!(l.marker(), "20261028_YML-XMAS-Luc");
        assert_eq!(l.acct(), "6f4121b3-628d-4b7a-91ea-8b1ac4a3f0a3", "the account is on the folder");
        assert_eq!(l.dest_folder(), "8008cf70-f28e-474d-a209-d67afb9fddb6", "the id is under `folder_id`");
        assert_eq!(l.folder.name, "LUC", "the name is the last breadcrumb, not a `name` key");
        assert_eq!(l.label(), "LUC", "the link's own name wins for the label");
    }

}
