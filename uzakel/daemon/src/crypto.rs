//! X25519 ECDH + HKDF-SHA256 key derivation + ChaCha20-Poly1305 AEAD for
//! the encrypted session pairing establishes (ARCHITECTURE.md §2.3.1).
//!
//! # Why this shape
//!
//! A 6-digit PIN (≈20 bits of entropy) is far too weak to use as an
//! encryption key directly — anyone who captures one ciphertext could
//! brute-force it offline in well under a second. So the actual
//! confidentiality here comes from an **ephemeral X25519 key exchange**:
//! the client and daemon each generate a fresh keypair per pairing
//! attempt, exchange public keys, and derive a session key from the ECDH
//! shared secret. A passive eavesdropper watching the exchange learns
//! nothing usable — recovering the shared secret from the two public keys
//! is the discrete-log problem, not a PIN-guessing problem.
//!
//! The PIN's job shrinks to one thing: letting the **client** confirm it's
//! actually talking to the real daemon, not a man-in-the-middle who
//! intercepted the key exchange and substituted their own keys. The
//! daemon derives a `confirm_tag` = HMAC-SHA256 over the exchanged public
//! keys, keyed by a value that also depends on the PIN; the client
//! recomputes the same tag and rejects the pairing if it doesn't match.
//! This doesn't defeat an attacker who already knows the PIN (nothing
//! could, with 20 bits of entropy — see the caveat in `trust.rs`), but it
//! does mean an attacker *without* the PIN can't quietly sit in the
//! middle of a legitimate pairing and still end up with working keys.
//!
//! # What this is not
//!
//! Not a general-purpose PAKE (SPAKE2, OPAQUE, …) — those give the PIN
//! check itself resistance to offline brute force even when an attacker
//! observes the whole exchange. This scheme's server-side PIN check still
//! happens the same way it always did (`discovery.rs` compares
//! `PairRequest.pin` to the current PIN before deriving/keeping anything),
//! so that property doesn't change. What's new here is confidentiality
//! for everything sent *after* pairing, plus the confirm-tag's MITM check.
//! A real PAKE is still a valid future upgrade — see ARCHITECTURE.md §5.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key as AeadKey, Nonce as AeadNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey};

use crate::protocol::NONCE_LEN;

pub const PUBKEY_LEN: usize = 32;
pub const TAG_LEN: usize = 32;

/// A fresh X25519 keypair for one pairing attempt. Consumed by
/// [`Self::derive`] — deliberately not `Clone`, so a keypair can't
/// accidentally be reused across two pairing attempts (each needs its own
/// fresh randomness).
pub struct EphemeralKeypair {
    secret: EphemeralSecret,
    pub public_bytes: [u8; PUBKEY_LEN],
}

impl EphemeralKeypair {
    pub fn generate() -> Self {
        let secret = EphemeralSecret::random();
        let public = PublicKey::from(&secret);
        Self {
            secret,
            public_bytes: public.to_bytes(),
        }
    }

    /// Completes the ECDH exchange and derives both directional session
    /// keys plus the PIN-bound confirmation tag. `client_pubkey` and
    /// `daemon_pubkey` must be given in that fixed order on both sides —
    /// whichever side calls this, the transcript (and therefore the
    /// derived keys) has to match the other side's.
    pub fn derive(
        self,
        their_public_bytes: [u8; PUBKEY_LEN],
        pin: u32,
        client_pubkey: [u8; PUBKEY_LEN],
        daemon_pubkey: [u8; PUBKEY_LEN],
    ) -> SessionMaterial {
        let their_public = PublicKey::from(their_public_bytes);
        let shared = self.secret.diffie_hellman(&their_public);

        let (_, hk) = Hkdf::<Sha256>::extract(Some(b"uzakel-pairing-v1"), shared.as_bytes());

        let mut transcript = Vec::with_capacity(PUBKEY_LEN * 2);
        transcript.extend_from_slice(&client_pubkey);
        transcript.extend_from_slice(&daemon_pubkey);

        let mut c2s_key = [0u8; 32];
        hk.expand_multi_info(&[b"uzakel c2s", &transcript], &mut c2s_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let mut s2c_key = [0u8; 32];
        hk.expand_multi_info(&[b"uzakel s2c", &transcript], &mut s2c_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let mut confirm_key = [0u8; 32];
        hk.expand_multi_info(&[b"uzakel confirm", &transcript, &pin.to_le_bytes()], &mut confirm_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");

        let mut mac = Hmac::<Sha256>::new_from_slice(&confirm_key)
            .expect("HMAC-SHA256 accepts any key length");
        mac.update(&transcript);
        let confirm_tag: [u8; TAG_LEN] = mac.finalize().into_bytes().into();

        SessionMaterial {
            c2s_key,
            s2c_key,
            confirm_tag,
        }
    }
}

/// Everything derived from one completed pairing's ECDH exchange.
/// `c2s_key`/`s2c_key` name the direction, not "mine" vs "theirs" — the
/// daemon uses `c2s_key` to *decrypt* (client encrypted with it) and
/// `s2c_key` to *encrypt* (client will decrypt with it); the client does
/// the reverse. See `trust.rs` for how the daemon wires these into
/// `Cipher`/`Opener`.
pub struct SessionMaterial {
    pub c2s_key: [u8; 32],
    pub s2c_key: [u8; 32],
    pub confirm_tag: [u8; TAG_LEN],
}

/// Encrypts outgoing frames for one direction of one paired session.
/// Nonces are a plain little-endian counter zero-extended to 12 bytes —
/// safe because each `Cipher` is built from a key that's used for exactly
/// one pairing session's lifetime (a fresh ECDH exchange every time), so
/// the (key, nonce) pair this produces is never reused across sessions,
/// and the counter never wraps in practice (2^64 messages).
pub struct Cipher {
    aead: ChaCha20Poly1305,
    next_counter: u64,
}

impl Cipher {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            aead: ChaCha20Poly1305::new(&AeadKey::from(key)),
            next_counter: 0,
        }
    }

    /// Encrypts `plaintext` (a full inner frame's bytes), returning the
    /// nonce used and the ciphertext (with the Poly1305 tag appended).
    pub fn seal(&mut self, plaintext: &[u8]) -> ([u8; NONCE_LEN], Vec<u8>) {
        let nonce_bytes = counter_nonce(self.next_counter);
        self.next_counter += 1;
        let ciphertext = self
            .aead
            .encrypt(&AeadNonce::from(nonce_bytes), plaintext)
            .expect("ChaCha20-Poly1305 encryption of an in-memory buffer cannot fail");
        (nonce_bytes, ciphertext)
    }
}

/// Decrypts incoming frames for one direction of one paired session, with
/// replay protection: a nonce counter at or below the highest one already
/// accepted is rejected. UDP packets can arrive out of order, so this
/// follows the same philosophy `input_manager.rs`'s own `seq` check
/// already uses for staleness — an occasional out-of-order-but-legitimate
/// packet gets dropped rather than accepted, which is the right tradeoff
/// for a fire-and-forget input stream (see that module's doc comment).
pub struct Opener {
    aead: ChaCha20Poly1305,
    highest_seen: Option<u64>,
}

impl Opener {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            aead: ChaCha20Poly1305::new(&AeadKey::from(key)),
            highest_seen: None,
        }
    }

    /// Returns `None` on any failure: bad tag (wrong key, corrupted/forged
    /// ciphertext) or a replayed/stale nonce. Deliberately not a `Result`
    /// with an error type — callers treat every failure mode identically
    /// (drop the packet), and an AEAD library should never hand back
    /// *why* decryption failed anyway, to avoid oracle-style attacks.
    pub fn open(&mut self, nonce_bytes: [u8; NONCE_LEN], ciphertext: &[u8]) -> Option<Vec<u8>> {
        let counter = u64::from_le_bytes(nonce_bytes[..8].try_into().unwrap());
        if let Some(highest) = self.highest_seen {
            if counter <= highest {
                return None;
            }
        }
        let plaintext = self
            .aead
            .decrypt(&AeadNonce::from(nonce_bytes), ciphertext)
            .ok()?;
        self.highest_seen = Some(counter);
        Some(plaintext)
    }
}

fn counter_nonce(counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[..8].copy_from_slice(&counter.to_le_bytes());
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecdh_roundtrip_agrees_on_keys() {
        let client = EphemeralKeypair::generate();
        let daemon = EphemeralKeypair::generate();
        let client_pub = client.public_bytes;
        let daemon_pub = daemon.public_bytes;

        let client_material = client.derive(daemon_pub, 123456, client_pub, daemon_pub);
        let daemon_material = daemon.derive(client_pub, 123456, client_pub, daemon_pub);

        assert_eq!(client_material.c2s_key, daemon_material.c2s_key);
        assert_eq!(client_material.s2c_key, daemon_material.s2c_key);
        assert_eq!(client_material.confirm_tag, daemon_material.confirm_tag);
    }

    #[test]
    fn different_pins_yield_different_confirm_tags() {
        let client = EphemeralKeypair::generate();
        let daemon_pub = EphemeralKeypair::generate().public_bytes;
        let client_pub = client.public_bytes;

        let with_pin_a = client.derive(daemon_pub, 111111, client_pub, daemon_pub);
        let client2 = EphemeralKeypair::generate();
        let client2_pub = client2.public_bytes;
        let with_pin_b = client2.derive(daemon_pub, 222222, client2_pub, daemon_pub);

        assert_ne!(with_pin_a.confirm_tag, with_pin_b.confirm_tag);
    }

    #[test]
    fn cipher_opener_roundtrip() {
        let mut cipher = Cipher::new([7u8; 32]);
        let mut opener = Opener::new([7u8; 32]);

        let (nonce, ct) = cipher.seal(b"hello uzakel");
        let pt = opener.open(nonce, &ct).expect("should decrypt");
        assert_eq!(pt, b"hello uzakel");
    }

    #[test]
    fn opener_rejects_wrong_key() {
        let mut cipher = Cipher::new([7u8; 32]);
        let mut opener = Opener::new([9u8; 32]);

        let (nonce, ct) = cipher.seal(b"hello");
        assert!(opener.open(nonce, &ct).is_none());
    }

    #[test]
    fn opener_rejects_replayed_nonce() {
        let mut cipher = Cipher::new([7u8; 32]);
        let mut opener = Opener::new([7u8; 32]);

        let (nonce, ct) = cipher.seal(b"first");
        assert!(opener.open(nonce, &ct).is_some());
        // Replaying the exact same (nonce, ciphertext) must be rejected.
        assert!(opener.open(nonce, &ct).is_none());
    }

    #[test]
    fn opener_rejects_out_of_order_nonce() {
        let mut cipher = Cipher::new([7u8; 32]);
        let mut opener = Opener::new([7u8; 32]);

        let (_n0, _c0) = cipher.seal(b"zero");
        let (n1, c1) = cipher.seal(b"one");
        let (n2, c2) = cipher.seal(b"two");

        assert!(opener.open(n2, &c2).is_some()); // accept 2 first
        assert!(opener.open(n1, &c1).is_none()); // 1 arrives late — rejected
    }

    #[test]
    fn wrong_pin_yields_a_confirm_tag_the_other_side_wont_match() {
        // Simulates the MITM-detection property: if the client and daemon
        // disagree on the PIN (e.g. an attacker relaying the ECDH exchange
        // without knowing it), their confirm_tags diverge even though the
        // ECDH shared secret — and therefore c2s_key/s2c_key — still agree.
        let client = EphemeralKeypair::generate();
        let daemon = EphemeralKeypair::generate();
        let client_pub = client.public_bytes;
        let daemon_pub = daemon.public_bytes;

        let client_material = client.derive(daemon_pub, 111111, client_pub, daemon_pub);
        let daemon_material = daemon.derive(client_pub, 999999, client_pub, daemon_pub);

        assert_eq!(client_material.c2s_key, daemon_material.c2s_key);
        assert_ne!(client_material.confirm_tag, daemon_material.confirm_tag);
    }
}
