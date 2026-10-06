// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS client side. Enrollment trusts exactly one CA — the one whose SHA-256
//! fingerprint came in the join code — so a man in the middle can't pose as
//! the server even on first contact. Afterwards only the saved CA is trusted,
//! and the agent authenticates with its own client certificate (mTLS).

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
pub struct PinnedCa {
    fp: String,
    provider: Arc<CryptoProvider>,
    pub found: Mutex<Option<CertificateDer<'static>>>,
}

impl PinnedCa {
    pub fn new(fp: &str) -> Self {
        PinnedCa { fp: fp.to_ascii_lowercase(), provider: provider(), found: Mutex::new(None) }
    }
}

impl ServerCertVerifier for PinnedCa {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let ca = intermediates
            .iter()
            .find(|c| fingerprint(c) == self.fp)
            .ok_or_else(|| rustls::Error::General("sunucunun CA parmak izi katılım koduyla eşleşmiyor".into()))?;
        let mut roots = RootCertStore::empty();
        roots.add(ca.clone().into_owned())?;
        // Full WebPKI check (signature, validity, serverAuth EKU, name)
        // against that single pinned root.
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), self.provider.clone())
            .build()
            .map_err(|e| rustls::Error::General(e.to_string()))?
            .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)?;
        *self.found.lock().unwrap() = Some(ca.clone().into_owned());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn host_of(addr: &str) -> Result<ServerName<'static>, String> {
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr).trim_matches(['[', ']']);
    ServerName::try_from(host.to_string()).map_err(|_| format!("geçersiz sunucu adı: {host}"))
}

pub async fn connect_enroll(addr: &str, pinned: Arc<PinnedCa>) -> Result<TlsStream<TcpStream>, String> {
    let cfg = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(pinned)
        .with_no_client_auth();
    dial(addr, cfg).await
}

pub async fn connect_mtls(
    addr: &str,
    ca_pem: &[u8],
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<TlsStream<TcpStream>, String> {
    let mut roots = RootCertStore::empty();
    for c in rustls_pemfile::certs(&mut &ca_pem[..]) {
        roots.add(c.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut &cert_pem[..]).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut &key_pem[..])
        .map_err(|e| e.to_string())?
        .ok_or("özel anahtar okunamadı")?;
    let cfg = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| e.to_string())?;
    dial(addr, cfg).await
}

async fn dial(addr: &str, cfg: ClientConfig) -> Result<TlsStream<TcpStream>, String> {
    let name = host_of(addr)?;
    let tcp = tokio::time::timeout(std::time::Duration::from_secs(15), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("{addr}: bağlantı zaman aşımı"))?
        .map_err(|e| format!("{addr}: {e}"))?;
    tcp.set_nodelay(true).ok();
    TlsConnector::from(Arc::new(cfg)).connect(name, tcp).await.map_err(|e| format!("TLS: {e}"))
}
