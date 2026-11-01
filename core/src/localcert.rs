
//! Adobe requires an https redirect for public (PKCE) clients, so the sign-in
//! callback is served on https://localhost. Like mkcert, FioTransfer creates a
//! small local certificate authority that macOS trusts once; browsers then
//! accept the localhost certificate it signs. The CA is name-constrained to
//! localhost, so even its private key can't mint certificates for other sites.

use std::path::PathBuf;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer,
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
        permitted_subtrees: vec![GeneralSubtree::DnsName("localhost".to_string())],
        excluded_subtrees: vec![],
    });
    Ok(p)
}

/// Loads or creates the local certificate authority.
fn local_ca() -> Result<(CertificateParams, KeyPair), String> {
    let cert_path = pem_file("local-ca.pem");
    let key_path = pem_file("local-ca-key.pem");
    let marker = config::dir().join(CA_MARKER);
    if marker.exists() {
        if let Ok(key_text) = std::fs::read_to_string(&key_path) {
            if let Ok(key) = KeyPair::from_pem(&key_text) {
                if cert_path.exists() {
                    return Ok((ca_params()?, key));
                }
            }
        }
    } else if cert_path.exists() {
        // A CA from an earlier build without name constraints: replace it, and
        // leave the file for trust_local_cert to remove from the keychain.
        let _ = std::fs::rename(&cert_path, pem_file(OLD_CA_FILE));
    }

    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let params = ca_params()?;
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    std::fs::write(&key_path, key.serialize_pem()).map_err(|e| e.to_string())?;
    std::fs::write(&cert_path, cert.pem()).map_err(|e| e.to_string())?;
    let _ = std::fs::write(&marker, b"1");
    Ok((params, key))
}

pub struct LocalCert {
    pub config: std::sync::Arc<tokio_rustls::rustls::ServerConfig>,
    pub ca_path: PathBuf,
}

/// The localhost certificate (signed by the local CA) and the CA's PEM path.
pub fn local_cert() -> Result<LocalCert, String> {
    let (ca_params, ca_key) = local_ca()?;
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
            let issuer = Issuer::from_params(&ca_params, &ca_key);
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
            let cp = cert.pem();
            let kp = leaf_key.serialize_pem();
            std::fs::write(&cert_path, &cp).map_err(|e| e.to_string())?;
            std::fs::write(&key_path, &kp).map_err(|e| e.to_string())?;
            (cp, kp)
        }
    };

    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let key: PrivateKeyDer<'static> = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|e| e.to_string())?;
    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())?;
    Ok(LocalCert { config: std::sync::Arc::new(config), ca_path })
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

/// Removes a certificate an earlier build installed, then the file itself.
fn untrust(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    let _ = std::process::Command::new("/usr/bin/security").args(["remove-trusted-cert", &path.to_string_lossy()]).status();
    let _ = std::fs::remove_file(path);
}
