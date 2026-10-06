// SPDX-License-Identifier: GPL-3.0-or-later
//! The server's private certificate authority.
//!
//! * One CA (ECDSA P-256), created by `kurulum`, key 0600 in the data dir.
//! * The gateway/panel certificate is a CA-signed leaf with the `serverAuth`
//!   EKU and the addresses agents use as SANs.
//! * Agent certificates are built **only from the CSR's public key** — every
//!   other field (CN = machine id, `clientAuth` EKU, not a CA, 5-year
//!   validity) is set here, so a hostile CSR can't ask for more.

use std::path::Path;

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, CertificateSigningRequestParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use sha2::{Digest, Sha256};

pub struct Ca {
    pub cert: Certificate,
    pub key: KeyPair,
    pub cert_pem: String,
}

pub fn fingerprint_der(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn pem_to_der(pem: &str) -> Result<Vec<u8>, String> {
    rustls_pemfile::certs(&mut pem.as_bytes())
        .next()
        .ok_or("PEM'de sertifika yok")?
        .map(|c| c.to_vec())
        .map_err(|e| e.to_string())
}

impl Ca {
    pub fn create(dir: &Path) -> Result<Ca, String> {
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
        p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "Uzak Yönetim CA");
        dn.push(DnType::OrganizationName, "BacakOS");
        p.distinguished_name = dn;
        p.not_before = rcgen::date_time_ymd(2026, 1, 1);
        p.not_after = rcgen::date_time_ymd(2046, 1, 1);
        let cert = p.self_signed(&key).map_err(|e| e.to_string())?;
        crate::files::write_private(&dir.join("ca.key"), key.serialize_pem().as_bytes())?;
        crate::files::write_public(&dir.join("ca.crt"), cert.pem().as_bytes())?;
        let cert_pem = cert.pem();
        Ok(Ca { cert, key, cert_pem })
    }

    pub fn load(dir: &Path) -> Result<Ca, String> {
        let key_pem = std::fs::read_to_string(dir.join("ca.key")).map_err(|e| format!("ca.key: {e} — önce `kurulum`"))?;
        let cert_pem = std::fs::read_to_string(dir.join("ca.crt")).map_err(|e| format!("ca.crt: {e}"))?;
        let key = KeyPair::from_pem(&key_pem).map_err(|e| e.to_string())?;
        // rcgen signs with a `Certificate`; rebuilding it from the stored
        // params + same key yields the same subject and key identifier, which
        // is all issued certificates refer to. The stored PEM stays the one
        // agents pin and verify against.
        let params = CertificateParams::from_ca_cert_pem(&cert_pem).map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        Ok(Ca { cert, key, cert_pem })
    }

    pub fn fingerprint(&self) -> String {
        fingerprint_der(&pem_to_der(&self.cert_pem).unwrap_or_default())
    }

    /// Leaf for the gateway (and panel unless an external cert is configured).
    pub fn issue_server(&self, names: &[String]) -> Result<(String, String), String> {
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
        for n in names {
            p.subject_alt_names.push(match n.parse::<std::net::IpAddr>() {
                Ok(ip) => SanType::IpAddress(ip),
                Err(_) => SanType::DnsName(n.clone().try_into().map_err(|_| format!("geçersiz ad: {n}"))?),
            });
        }
        p.distinguished_name.push(DnType::CommonName, names.first().cloned().unwrap_or_default());
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.not_before = rcgen::date_time_ymd(2026, 1, 1);
        p.not_after = rcgen::date_time_ymd(2036, 1, 1);
        let cert = p.signed_by(&key, &self.cert, &self.key).map_err(|e| e.to_string())?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    /// Sign an agent CSR. Returns `(cert_pem, sha256 fingerprint of DER)`.
    pub fn sign_agent(&self, csr_pem: &str, machine_id: &str) -> Result<(String, String), String> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem).map_err(|e| format!("CSR: {e}"))?;
        let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
        p.distinguished_name.push(DnType::CommonName, machine_id);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.is_ca = IsCa::ExplicitNoCa;
        p.not_before = rcgen::date_time_ymd(2026, 1, 1);
        p.not_after = rcgen::date_time_ymd(2031, 1, 1);
        let signed = CertificateSigningRequestParams { params: p, public_key: csr.public_key }
            .signed_by(&self.cert, &self.key)
            .map_err(|e| e.to_string())?;
        Ok((signed.pem(), fingerprint_der(signed.der())))
    }
}
