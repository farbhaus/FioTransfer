//! WeTransfer transfers.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{
    dedupe_paths, parse_link, path_escape, path_parts, rel_path, send, File, Info, Link, Source,
    ERR_BAD_PASSPHRASE, ERR_INVALID_SHARE, ERR_PASSPHRASE, KIND_WETRANSFER, WEB_UA,
};

/// WeTransferAPI is the API the WeTransfer download page calls (overridable for
/// tests).
pub fn we_transfer_api() -> String {
    std::env::var("FIO_WETRANSFER_API").unwrap_or_else(|_| "https://wetransfer.com/api/v4".to_string())
}

fn is_alnum(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// weTransferLink recognises wetransfer.com/downloads/<transfer>/[<recipient>/]<security hash>,
/// on wetransfer.com or a branded subdomain. we.tl short links redirect there.
pub(crate) fn we_transfer_link(s: &str) -> Option<Link> {
    let u = parse_link(s)?;
    let host = u.host_str().unwrap_or("");
    if host != "wetransfer.com" && !host.ends_with(".wetransfer.com") {
        return None;
    }
    let parts = path_parts(&u);
    if parts.len() < 3 || parts.len() > 4 || parts[0] != "downloads" {
        return None;
    }
    for p in &parts[1..] {
        if !is_alnum(p) {
            return None;
        }
    }
    Some(Link {
        kind: KIND_WETRANSFER.to_string(),
        id: parts[1].clone(),
        url: format!("https://wetransfer.com/{}", parts.join("/")),
    })
}

/// WeTransferClient reads a transfer the way the WeTransfer download page does.
pub struct WeTransferClient {
    pub transfer_id: String,
    /// Set for links sent by email.
    pub recipient_id: String,
    pub security_hash: String,
    pub passphrase: String,

    /// From info, reused by list.
    prepared: Mutex<Option<WeTransfer>>,
}

/// newWeTransfer builds the client for a resolved link.
pub(crate) fn new_we_transfer(l: &Link, passphrase: &str) -> WeTransferClient {
    let mut c = WeTransferClient {
        transfer_id: l.id.clone(),
        recipient_id: String::new(),
        security_hash: String::new(),
        passphrase: passphrase.to_string(),
        prepared: Mutex::new(None),
    };
    if let Some(u) = parse_link(&l.url) {
        let p = path_parts(&u);
        match p.len() {
            3 => c.security_hash = p[2].clone(),
            4 => {
                c.recipient_id = p[2].clone();
                c.security_hash = p[3].clone();
            }
            _ => {}
        }
    }
    c
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct WeTransfer {
    #[serde(default)]
    state: String,
    #[serde(rename = "display_name", default)]
    display_name: String,
    #[serde(default)]
    encrypted: bool,
    /// anonymous, email or restricted
    #[serde(rename = "downloader_email_verification", default)]
    access: String,
    #[serde(default)]
    items: Vec<WeTransferItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct WeTransferItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: i64,
    #[serde(rename = "content_identifier", default)]
    content_identifier: String,
}

impl WeTransferClient {
    /// post calls a transfer endpoint with the link's credentials added to body.
    async fn post<T: DeserializeOwned>(
        &self,
        cx: &CancellationToken,
        endpoint: &str,
        body: serde_json::Value,
    ) -> Result<(u16, T), String> {
        let mut body = match body {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        body.insert("security_hash".to_string(), serde_json::Value::String(self.security_hash.clone()));
        if !self.recipient_id.is_empty() {
            body.insert("recipient_id".to_string(), serde_json::Value::String(self.recipient_id.clone()));
        }
        if !self.passphrase.is_empty() {
            body.insert("password".to_string(), serde_json::Value::String(self.passphrase.clone()));
        }
        let payload = serde_json::to_vec(&serde_json::Value::Object(body)).unwrap_or_default();
        let u = format!("{}/transfers/{}{}", we_transfer_api(), path_escape(&self.transfer_id), endpoint);
        let (status, data) = send(cx, crate::netx::api_client(), "WeTransfer", || {
            crate::netx::api_client()
                .post(&u)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .header("User-Agent", WEB_UA)
                .body(payload.clone())
        })
        .await?;
        if status >= 300 {
            #[derive(Debug, Default, Deserialize)]
            struct E {
                #[serde(default)]
                message: String,
                #[serde(default)]
                error: String,
            }
            let e: E = serde_json::from_slice(&data).unwrap_or_default();
            if e.error == "invalid_password" || e.error == "wrong_password" {
                if self.passphrase.is_empty() {
                    return Err(ERR_PASSPHRASE.to_string());
                }
                return Err(ERR_BAD_PASSPHRASE.to_string());
            }
            if status == 404 || status == 410 || (status == 403 && e.message.contains("No download access")) {
                return Err(ERR_INVALID_SHARE.to_string());
            }
            let msg = if e.message.is_empty() { e.error } else { e.message };
            return Err(format!("WeTransfer returned {}: {}", status, msg));
        }
        let out = serde_json::from_slice(&data)
            .map_err(|_| format!("unexpected response from WeTransfer ({})", status))?;
        Ok((status, out))
    }

    async fn prepare(&self, cx: &CancellationToken) -> Result<WeTransfer, String> {
        if let Some(t) = self.prepared.lock().ok().and_then(|g| g.clone()) {
            return Ok(t);
        }
        let (_, t): (u16, WeTransfer) = self
            .post(cx, "/prepare-download", serde_json::json!({}))
            .await?;
        match t.state.as_str() {
            "downloadable" => {}
            "password_protected" => {
                if self.passphrase.is_empty() {
                    return Err(ERR_PASSPHRASE.to_string());
                }
                return Err(ERR_BAD_PASSPHRASE.to_string());
            }
            "uploading" | "processing" | "started" => {
                return Err("this transfer is still uploading".to_string())
            }
            _ => return Err(ERR_INVALID_SHARE.to_string()),
        }
        if t.encrypted {
            return Err("this transfer is end-to-end encrypted; open it in a browser".to_string());
        }
        if t.access == "email" || t.access == "restricted" {
            return Err("this transfer emails you a code first; open it in a browser".to_string());
        }
        if let Ok(mut g) = self.prepared.lock() {
            *g = Some(t.clone());
        }
        Ok(t)
    }
}

#[async_trait::async_trait]
impl Source for WeTransferClient {
    async fn info(&self, cx: &CancellationToken) -> Result<Info, String> {
        let t = self.prepare(cx).await?;
        // Without a title, WeTransfer shows the first file's name: that's no
        // name for the transfer.
        let mut name = t.display_name.clone();
        for it in &t.items {
            if it.name == name {
                name = String::new();
                break;
            }
        }
        Ok(Info { share_id: self.transfer_id.clone(), name, downloading_enabled: true })
    }

    /// List returns the transfer's files. URLs are requested per file when it
    /// starts (fresh_url), so each file downloads on its own instead of as one
    /// zip.
    async fn list(
        &self,
        cx: &CancellationToken,
        progress: &(dyn Fn(usize) + Send + Sync),
    ) -> Result<Vec<File>, String> {
        let t = self.prepare(cx).await?;
        let mut files: Vec<File> = Vec::new();
        for it in &t.items {
            if !it.content_identifier.is_empty() && it.content_identifier != "file" {
                continue;
            }
            files.push(File {
                asset_id: it.id.clone(),
                rel_path: rel_path(&it.name),
                size: it.size,
                url: String::new(),
                problem: String::new(),
            });
        }
        (progress)(files.len());
        dedupe_paths(&mut files);
        Ok(files)
    }

    /// FreshURL asks for one file's download link.
    async fn fresh_url(&self, cx: &CancellationToken, file_id: &str) -> Result<String, String> {
        #[derive(Debug, Default, Deserialize)]
        struct R {
            #[serde(rename = "direct_link", default)]
            direct_link: String,
        }
        let body = serde_json::json!({ "file_ids": [file_id], "intent": "single_file" });
        let (_, r): (u16, R) = self.post(cx, "/download", body).await?;
        if r.direct_link.is_empty() {
            return Err("could not get a download link for this file".to_string());
        }
        Ok(r.direct_link)
    }

    /// MarkDownloaded is a no-op: WeTransfer counts a download when its link is
    /// requested.
    async fn mark_downloaded(&self, cx: &CancellationToken, _ids: &[String]) -> Result<(), String> {
        let _ = cx;
        Ok(())
    }
}
