
//! Adobe requires an https redirect for public (PKCE) clients, so the sign-in
//! callback is served on https://localhost. Like mkcert, FioTransfer creates a
//! small local certificate authority that macOS trusts once; browsers then
//! accept the localhost certificate it signs. The CA is name-constrained to
//! localhost, so even its private key can't mint certificates for other sites.

use std::path::PathBuf;

use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType, ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, NameConstraints, SanType,
};

use crate::config;

fn pem_file(name: &str) -> PathBuf {
    config::dir().join(name)
}

const OLD_CA_FILE: &str = "local-ca-old.pem";
/// Written next to the CA once it carries the name constraints; its absence
/// means the CA came from an earlier build and must be replaced.
const CA_MARKER: &str = "local-ca-v2";

fn ca_params() -> Result<CertificateParams, String> {
    let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    p.distinguished_name = DistinguishedName::new();
    p.distinguished_name.push(DnType::CommonName, format!("FioTransfer Local CA ({})", fiotransfer_sys::platform::hostname()));
    p.distinguished_name.push(DnType::OrganizationName, "FioTransfer");
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.name_constraints = Some(NameConstraints {
        // As the Go app's CA: localhost and the loopback addresses only, so
        // even its key can't sign for another site or address.
        permitted_subtrees: vec![
            GeneralSubtree::DnsName("localhost".to_string()),
            GeneralSubtree::IpAddress(CidrSubnet::V4([127, 0, 0, 0], [255, 0, 0, 0])),
            GeneralSubtree::IpAddress(CidrSubnet::V6(std::net::Ipv6Addr::LOCALHOST.octets(), [0xff; 16])),
        ],
        excluded_subtrees: vec![],
    });
    Ok(p)
}

/// Loads or creates the local certificate authority: its PEM and its key.
fn local_ca() -> Result<(String, KeyPair), String> {
    let cert_path = pem_file("local-ca.pem");
    let key_path = pem_file("local-ca-key.pem");
    let marker = config::dir().join(CA_MARKER);
    if marker.exists() {
        if let (Ok(cert_text), Ok(key_text)) = (std::fs::read_to_string(&cert_path), std::fs::read_to_string(&key_path)) {
            if let Ok(key) = KeyPair::from_pem(&key_text) {
                return Ok((cert_text, key));
            }
        }
    } else if cert_path.exists() {
        // A CA from an earlier build (the Go app's too, which wrote no marker):
        // replace it, and leave the file for trust_local_cert to remove from
        // the keychain.
        let _ = std::fs::rename(&cert_path, pem_file(OLD_CA_FILE));
    }
    // The localhost certificate is signed by the CA being replaced. It still
    // verifies until trust_local_cert untrusts that CA, and then the browser
    // would refuse it.
    let _ = std::fs::remove_file(pem_file("localhost.pem"));
    let _ = std::fs::remove_file(pem_file("localhost-key.pem"));

    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = ca_params()?.self_signed(&key).map_err(|e| e.to_string())?;
    std::fs::write(&key_path, key.serialize_pem()).map_err(|e| e.to_string())?;
    std::fs::write(&cert_path, cert.pem()).map_err(|e| e.to_string())?;
    let _ = std::fs::write(&marker, b"1");
    Ok((cert.pem(), key))
}

pub struct LocalCert {
    pub config: std::sync::Arc<tokio_rustls::rustls::ServerConfig>,
    pub ca_path: PathBuf,
}

/// The localhost certificate (signed by the local CA) and the CA's PEM path.
pub fn local_cert() -> Result<LocalCert, String> {
    let (ca_pem, ca_key) = local_ca()?;
    let ca_path = pem_file("local-ca.pem");
    let cert_path = pem_file("localhost.pem");
    let key_path = pem_file("localhost-key.pem");

    let leaf_ok = || -> Option<(String, String)> {
        let c = std::fs::read_to_string(&cert_path).ok()?;
        let k = std::fs::read_to_string(&key_path).ok()?;
        if verify_cert(&cert_path.to_string_lossy(), "ssl", Some("localhost")) {
            Some((c, k))
        } else {
            None
        }
    };

    let (cert_pem, key_pem) = match leaf_ok() {
        Some(v) => v,
        None => {
            let (cp, kp) = mint_leaf(&ca_pem, &ca_key)?;
            std::fs::write(&cert_path, &cp).map_err(|e| e.to_string())?;
            std::fs::write(&key_path, &kp).map_err(|e| e.to_string())?;
            (cp, kp)
        }
    };
    Ok(LocalCert { config: server_config(&cert_pem, &key_pem)?, ca_path })
}

/// A localhost certificate and its key (PEM), signed by the CA. The issuer's
/// name is read from the CA as stored: `ca_params()` names it after today's
/// hostname, and a leaf naming another issuer doesn't chain to it.
fn mint_leaf(ca_pem: &str, ca_key: &KeyPair) -> Result<(String, String), String> {
    let issuer = Issuer::from_ca_cert_pem(ca_pem, ca_key).map_err(|e| e.to_string())?;
    let leaf_key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, "localhost");
    params.distinguished_name.push(DnType::OrganizationName, "FioTransfer");
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().map_err(|e: rcgen::Error| e.to_string())?),
        SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))),
        SanType::IpAddress(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
    ];
    // Within Apple's 398-day limit for TLS certificates.
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(397);
    let cert = params.signed_by(&leaf_key, &issuer).map_err(|e| e.to_string())?;
    Ok((cert.pem(), leaf_key.serialize_pem()))
}

/// The callback server's TLS config. The crypto provider is named here:
/// reqwest 0.13 compiles in aws-lc-rs and the updater's reqwest 0.12 ring, and
/// with both rustls has no process default, so `ServerConfig::builder()`
/// panics (2.0.0 and 2.0.1 quit on every Sign in).
fn server_config(cert_pem: &str, key_pem: &str) -> Result<std::sync::Arc<tokio_rustls::rustls::ServerConfig>, String> {
    use tokio_rustls::rustls;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let key: PrivateKeyDer<'static> = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| e.to_string())?;
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())?;
    Ok(std::sync::Arc::new(config))
}

/// Makes macOS trust the local CA (one password prompt, first time only).
pub fn trust_local_cert(ca_path: &std::path::Path) {
    untrust(&pem_file("localhost-cert.pem"));
    untrust(&pem_file(OLD_CA_FILE));
    let ca = ca_path.to_string_lossy().into_owned();
    if verify_cert(&ca, "basic", None) {
        return;
    }
    let keychain = config::home_dir().join("Library").join("Keychains").join("login.keychain-db");
    let _ = std::process::Command::new("/usr/bin/security")
        .args(["add-trusted-cert", "-r", "trustRoot", "-k", &keychain.to_string_lossy(), &ca])
        .status();
}

fn verify_cert(path: &str, policy: &str, host: Option<&str>) -> bool {
    let mut c = std::process::Command::new("/usr/bin/security");
    c.args(["verify-cert", "-c", path, "-p", policy]);
    if let Some(h) = host {
        c.args(["-s", h]);
    }
    c.arg("-q");
    matches!(c.status(), Ok(st) if st.success())
}

/// Removes a certificate an earlier build installed (its trust setting and its
/// keychain entry, as Go's `untrust` did), then the file itself.
fn untrust(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let _ = std::process::Command::new("/usr/bin/security").args(["remove-trusted-cert", &path.to_string_lossy()]).status();
    // `delete-certificate -Z` takes the SHA-1 of the DER: the name alone would
    // also match today's CA, which carries the same one.
    if let Some(hash) = pem_sha1(&text) {
        let _ = std::process::Command::new("/usr/bin/security").args(["delete-certificate", "-Z", &hash]).status();
    }
    let _ = std::fs::remove_file(path);
}

/// The SHA-1 of a PEM certificate's DER, in hex, from `/usr/bin/shasum`.
fn pem_sha1(pem: &str) -> Option<String> {
    use base64::Engine;
    use std::io::Write;
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(body.trim()).ok()?;
    let mut child = std::process::Command::new("/usr/bin/shasum")
        .args(["-a", "1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(&der).ok()?;
    let out = child.wait_with_output().ok()?;
    let hex = String::from_utf8_lossy(&out.stdout).split_whitespace().next()?.to_string();
    (hex.len() == 40).then_some(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    // What Sign in builds, for a CA made under another hostname (and with its
    // name in the Go app's order): the leaf names that CA as its issuer, and
    // the config builds without a process-level crypto provider.
    #[test]
    fn a_leaf_from_a_stored_ca_serves() {
        let ca_key = KeyPair::generate().unwrap();
        let mut p = ca_params().unwrap();
        p.distinguished_name = DistinguishedName::new();
        p.distinguished_name.push(DnType::OrganizationName, "FioTransfer");
        p.distinguished_name.push(DnType::CommonName, "FioTransfer Local CA (old-name.local)");
        let ca_pem = p.self_signed(&ca_key).unwrap().pem();
        let (cert_pem, key_pem) = mint_leaf(&ca_pem, &ca_key).unwrap();

        let (_, ca) = x509_parser::pem::parse_x509_pem(ca_pem.as_bytes()).unwrap();
        let ca = ca.parse_x509().unwrap();
        let (_, leaf) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).unwrap();
        let leaf = leaf.parse_x509().unwrap();
        assert_eq!(leaf.issuer().as_raw(), ca.subject().as_raw());

        assert!(tokio_rustls::rustls::crypto::CryptoProvider::get_default().is_none());
        server_config(&cert_pem, &key_pem).unwrap();
    }
}
