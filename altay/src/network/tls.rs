// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! TLS for FTPS and WebDAV over HTTPS: certificates a known authority signed
//! are checked as usual; any other (a NAS's own, self-signed one) is trusted
//! only once the user accepted its fingerprint, which is then remembered in
//! `~/.config/altay/known_certs` — like SSH's `known_hosts`. A remembered
//! server presenting another certificate is refused.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

/// Why a handshake was refused, when the certificate was the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertProblem {
    /// Not signed by a known authority and not accepted yet: its SHA-256
    /// fingerprint, for the user to compare.
    Unknown(String),
    /// Differs from the one accepted for this server before.
    Changed,
}

/// What the verifier saw, read after a failed connect.
pub type Verdict = Arc<Mutex<Option<CertProblem>>>;

fn store_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("altay").join("known_certs"))
}

/// `AB:CD:…` of the certificate's SHA-256.
fn fingerprint(der: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    let mut out = String::with_capacity(95);
    for (i, b) in digest.as_ref().iter().enumerate() {
        if i > 0 {
            out.push(':');
        }
        let _ = write!(out, "{b:02X}");
    }
    out
}

fn key(host: &str, port: u16) -> String {
    format!("{}:{port}", host.to_lowercase())
}

/// The fingerprint accepted for `host:port`, if any.
fn known(host: &str, port: u16) -> Option<String> {
    let text = std::fs::read_to_string(store_path()?).ok()?;
    let key = key(host, port);
    text.lines().filter_map(|l| l.split_once(' ')).find(|(k, _)| *k == key).map(|(_, fp)| fp.trim().to_string())
}

/// Remember `fingerprint` as `host:port`'s certificate (the user accepted
/// it), replacing an older one.
pub fn trust(host: &str, port: u16, fingerprint: &str) -> std::io::Result<()> {
    let path = store_path().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let key = key(host, port);
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out = String::new();
    for line in old.lines().filter(|l| l.split_once(' ').is_none_or(|(k, _)| k != key)) {
        out += line;
        out.push('\n');
    }
    out += &format!("{key} {fingerprint}\n");
    let mut f = std::fs::File::create(&path)?;
    f.write_all(out.as_bytes())
}

#[derive(Debug)]
struct Verifier {
    webpki: Arc<WebPkiServerVerifier>,
    host: String,
    port: u16,
    verdict: Verdict,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let err = match self.webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Ok(ok) => return Ok(ok),
            Err(e) => e,
        };
        let seen = fingerprint(end_entity);
        match known(&self.host, self.port) {
            Some(fp) if fp == seen => Ok(ServerCertVerified::assertion()),
            Some(_) => {
                *self.verdict.lock().unwrap() = Some(CertProblem::Changed);
                Err(err)
            }
            None => {
                log::info!("{}:{}: certificate not trusted ({err}), SHA-256 {seen}", self.host, self.port);
                *self.verdict.lock().unwrap() = Some(CertProblem::Unknown(seen));
                Err(err)
            }
        }
    }

    // The handshake must still prove the server holds the certificate's key,
    // whichever way the certificate itself was accepted.
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}

/// A TLS client configuration for `host:port`, and where it reports a
/// certificate it refused.
pub fn client_config(host: &str, port: u16) -> (Arc<ClientConfig>, Verdict) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = Arc::new(RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() });
    let webpki = WebPkiServerVerifier::builder_with_provider(roots, provider.clone()).build().expect("web PKI roots");
    let verdict = Verdict::default();
    let verifier = Verifier { webpki, host: host.to_string(), port, verdict: verdict.clone() };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    (Arc::new(config), verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(b"abc");
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert!(fp.starts_with("BA:78:16:BF"));
    }
}
