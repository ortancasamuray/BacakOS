//! PIN-less session resumption (ARCHITECTURE.md §2.3.3).
//!
//! The first pairing (PIN + X25519, `crypto.rs`) also derives a long-lived
//! `resume_key` and a public `client_id`, which both sides persist. To
//! (re)connect later — after a daemon restart, a Wi-Fi handoff that changed
//! the phone's address, or the PC getting a new DHCP lease — the client
//! sends a fresh random nonce, the daemon answers with its own, and both
//! derive **new** session keys from `resume_key` and the two nonces:
//!
//! ```text
//! RESUME_REQUEST  client_id[16] | client_nonce[32] | HMAC(rk, "uzakel resume req"  | client_id | client_nonce)
//! RESUME_RESPONSE accepted[1]   | daemon_nonce[32] | HMAC(rk, "uzakel resume resp" | client_nonce | daemon_nonce)
//! keys = HKDF-SHA256(salt "uzakel-resume-v1", ikm rk,
//!                    info "uzakel c2s"/"uzakel s2c" | client_nonce | daemon_nonce)
//! ```
//!
//! Fresh keys every time mean the counter nonces in `crypto::Cipher` can
//! safely restart at zero (reusing old session keys across a restart would
//! reuse (key, nonce) pairs — fatal for ChaCha20-Poly1305). Each MAC proves
//! possession of `resume_key` to the other side; a replayed request only
//! yields keys the replayer can't compute.

use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::protocol::{frame, Opcode, ProtocolError};

pub const CLIENT_ID_LEN: usize = 16;
pub const NONCE_LEN: usize = 32;
pub const MAC_LEN: usize = 32;

pub struct ResumeRequest {
    pub client_id: [u8; CLIENT_ID_LEN],
    pub client_nonce: [u8; NONCE_LEN],
    pub mac: [u8; MAC_LEN],
}

impl ResumeRequest {
    pub fn decode_payload(p: &[u8]) -> Result<Self, ProtocolError> {
        if p.len() < CLIENT_ID_LEN + NONCE_LEN + MAC_LEN {
            return Err(ProtocolError::BadPayload { opcode: Opcode::ResumeRequest, reason: "truncated ResumeRequest" });
        }
        let mut r = ResumeRequest { client_id: [0; CLIENT_ID_LEN], client_nonce: [0; NONCE_LEN], mac: [0; MAC_LEN] };
        r.client_id.copy_from_slice(&p[..16]);
        r.client_nonce.copy_from_slice(&p[16..48]);
        r.mac.copy_from_slice(&p[48..80]);
        Ok(r)
    }

    #[cfg(test)]
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(80);
        p.extend_from_slice(&self.client_id);
        p.extend_from_slice(&self.client_nonce);
        p.extend_from_slice(&self.mac);
        frame(Opcode::ResumeRequest, &p)
    }
}

pub struct ResumeResponse {
    pub accepted: bool,
    pub daemon_nonce: [u8; NONCE_LEN],
    pub mac: [u8; MAC_LEN],
}

impl ResumeResponse {
    pub fn rejected() -> Self {
        ResumeResponse { accepted: false, daemon_nonce: [0; NONCE_LEN], mac: [0; MAC_LEN] }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(1 + NONCE_LEN + MAC_LEN);
        p.push(self.accepted as u8);
        p.extend_from_slice(&self.daemon_nonce);
        p.extend_from_slice(&self.mac);
        frame(Opcode::ResumeResponse, &p)
    }
}

fn hmac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; MAC_LEN] {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC-SHA256 accepts any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

pub fn request_mac(rk: &[u8; 32], client_id: &[u8; CLIENT_ID_LEN], client_nonce: &[u8; NONCE_LEN]) -> [u8; MAC_LEN] {
    hmac(rk, &[b"uzakel resume req", client_id, client_nonce])
}

pub fn response_mac(rk: &[u8; 32], client_nonce: &[u8; NONCE_LEN], daemon_nonce: &[u8; NONCE_LEN]) -> [u8; MAC_LEN] {
    hmac(rk, &[b"uzakel resume resp", client_nonce, daemon_nonce])
}

/// `(c2s_key, s2c_key)` for a resumed session.
pub fn session_keys(rk: &[u8; 32], client_nonce: &[u8; NONCE_LEN], daemon_nonce: &[u8; NONCE_LEN]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(b"uzakel-resume-v1"), rk);
    let mut c2s = [0u8; 32];
    hk.expand_multi_info(&[b"uzakel c2s", client_nonce, daemon_nonce], &mut c2s).expect("valid length");
    let mut s2c = [0u8; 32];
    hk.expand_multi_info(&[b"uzakel s2c", client_nonce, daemon_nonce], &mut s2c).expect("valid length");
    (c2s, s2c)
}

/// Constant-time equality for MACs.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Fixed-input vector, mirrored byte-for-byte by the Kotlin
    /// `ResumeCryptoTest` — the two implementations never share code.
    /// Values were cross-checked against an independent Python
    /// (hmac/hashlib, hand-written HKDF) computation.
    #[test]
    fn known_answer_vector() {
        let rk = [7u8; 32];
        let cid = [1u8; CLIENT_ID_LEN];
        let cn = [2u8; NONCE_LEN];
        let dn = [3u8; NONCE_LEN];
        let (c2s, s2c) = session_keys(&rk, &cn, &dn);
        let req = request_mac(&rk, &cid, &cn);
        let resp = response_mac(&rk, &cn, &dn);
        println!("req={}\nresp={}\nc2s={}\ns2c={}", hex(&req), hex(&resp), hex(&c2s), hex(&s2c));
        assert_eq!(hex(&req), KAT_REQ);
        assert_eq!(hex(&resp), KAT_RESP);
        assert_eq!(hex(&c2s), KAT_C2S);
        assert_eq!(hex(&s2c), KAT_S2C);
    }

    const KAT_REQ: &str = "323314182b3dc9d2b4a6ba85d2189ce8242a0f862c8f485457927f5b8fb1b69f";
    const KAT_RESP: &str = "12ec08c387dc31ea00a45de2d91e159b6345b2ff21e5c381463cebff60ba22a8";
    const KAT_C2S: &str = "95782a01c12187161a6900e9c73c819d5f537cae01c5b1d383970a0daf9b3d3a";
    const KAT_S2C: &str = "25fac1a4c0086dd74b86039bd7b86e5a350a80f6c04651af52f12905983d5604";

    #[test]
    fn request_roundtrip_and_mac_check() {
        let rk = [9u8; 32];
        let cid = [4u8; CLIENT_ID_LEN];
        let cn = [5u8; NONCE_LEN];
        let req = ResumeRequest { client_id: cid, client_nonce: cn, mac: request_mac(&rk, &cid, &cn) };
        let bytes = req.encode();
        let back = ResumeRequest::decode_payload(&bytes[crate::protocol::HEADER_LEN..]).unwrap();
        assert!(ct_eq(&back.mac, &request_mac(&rk, &back.client_id, &back.client_nonce)));
        assert!(!ct_eq(&back.mac, &request_mac(&[8u8; 32], &back.client_id, &back.client_nonce)));
    }
}
