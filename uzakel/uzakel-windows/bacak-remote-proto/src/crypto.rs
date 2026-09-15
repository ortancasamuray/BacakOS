//! X25519 ECDH + HKDF-SHA256 key derivation + ChaCha20-Poly1305 AEAD for
//! the encrypted pairing session, mirroring `uzakel/uzakel-android/daemon/src/crypto.rs`
//! byte-for-byte in structure (different domain-separation strings only) —
//! that scheme is already verified on real hardware (uzakel's ARCHITECTURE.md
//! §2.3.1/§6), so reusing it here is a deliberate low-risk choice over
//! designing a new one.
//!
//! # Why this shape
//!
//! A 6-digit PIN is far too weak to use as an encryption key directly. The
//! actual confidentiality comes from an **ephemeral X25519 key exchange**:
//! both sides generate a fresh keypair per pairing attempt and derive a
//! session key from the ECDH shared secret — a passive eavesdropper on the
//! LAN learns nothing usable from watching it. The PIN's job shrinks to one
//! thing: letting the **client** confirm it's talking to the real server,
//! not a man-in-the-middle who intercepted the exchange and substituted
//! their own keys — via a `confirm_tag` = HMAC-SHA256 over the exchanged
//! public keys, keyed by a value that also depends on the PIN.
//!
//! Not a general-purpose PAKE (SPAKE2, OPAQUE, …): those give the PIN check
//! itself resistance to offline brute force even when an attacker observes
//! the whole exchange. An attacker who already knows the PIN (or intercepts
//! it) can still complete a valid-looking handshake — this scheme's job is
//! confidentiality against passive eavesdropping plus MITM detection, not
//! defending a weak PIN against a capable active attacker. See
//! `uzakel/ARCHITECTURE.md` §2.3.1 for the same caveat spelled out in full.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key as AeadKey, Nonce as AeadNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey};

pub const PUBKEY_LEN: usize = 32;
pub const TAG_LEN: usize = 32;
pub const NONCE_LEN: usize = 12;

/// A fresh X25519 keypair for one pairing attempt. Deliberately not
/// `Clone` — each pairing attempt needs its own fresh randomness.
pub struct EphemeralKeypair {
    secret: EphemeralSecret,
    pub public_bytes: [u8; PUBKEY_LEN],
}

impl EphemeralKeypair {
    pub fn generate() -> Self {
        let secret = EphemeralSecret::random();
        let public = PublicKey::from(&secret);
        Self { secret, public_bytes: public.to_bytes() }
    }

    /// Completes the ECDH exchange and derives the PIN-bound confirmation
    /// tag, keeping the underlying PRK around so [`SessionMaterial::channel_keys`]
    /// can derive as many independent per-channel key pairs as needed.
    /// `client_pubkey` and `server_pubkey` must be given in that fixed order
    /// on both sides so the transcript — and therefore every derived key —
    /// matches.
    pub fn derive(
        self,
        their_public_bytes: [u8; PUBKEY_LEN],
        pin: u32,
        client_pubkey: [u8; PUBKEY_LEN],
        server_pubkey: [u8; PUBKEY_LEN],
    ) -> SessionMaterial {
        let their_public = PublicKey::from(their_public_bytes);
        let shared = self.secret.diffie_hellman(&their_public);

        let (_, hk) = Hkdf::<Sha256>::extract(Some(b"bacak-remote-pairing-v1"), shared.as_bytes());

        let mut transcript = Vec::with_capacity(PUBKEY_LEN * 2);
        transcript.extend_from_slice(&client_pubkey);
        transcript.extend_from_slice(&server_pubkey);

        let mut confirm_key = [0u8; 32];
        hk.expand_multi_info(&[b"bacak-remote confirm", &transcript, &pin.to_le_bytes()], &mut confirm_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");

        let mut mac = Hmac::<Sha256>::new_from_slice(&confirm_key).expect("HMAC-SHA256 accepts any key length");
        mac.update(&transcript);
        let confirm_tag: [u8; TAG_LEN] = mac.finalize().into_bytes().into();

        SessionMaterial { hk, transcript, confirm_tag }
    }
}

/// Everything derived from one completed pairing's ECDH exchange: the
/// PIN-bound confirmation tag, plus the raw HKDF pseudorandom key needed to
/// derive as many independent channel key pairs as the transport needs
/// (see [`Self::channel_keys`]).
pub struct SessionMaterial {
    hk: Hkdf<Sha256>,
    transcript: Vec<u8>,
    pub confirm_tag: [u8; TAG_LEN],
}

/// One channel's directional key pair. `c2s_key`/`s2c_key` name the
/// direction, not "mine"/"theirs" — the server uses `c2s_key` to *decrypt*
/// (client encrypted with it) and `s2c_key` to *encrypt*; the client does
/// the reverse.
pub struct ChannelKeys {
    pub c2s_key: [u8; 32],
    pub s2c_key: [u8; 32],
}

impl SessionMaterial {
    /// Derives a fresh, independent key pair for one named channel (e.g.
    /// `"video"`, `"input"`). **Critical:** every channel that gets its own
    /// [`Cipher`] (its own nonce counter starting at 0) must use a distinct
    /// `channel` label — reusing one label's keys for two independently-
    /// counted `Cipher`s would reuse a (key, nonce) pair, breaking
    /// ChaCha20-Poly1305's security entirely. This is why `bacak-remote`
    /// derives separate keys per UDP socket instead of sharing one pair
    /// across the video and input channels.
    pub fn channel_keys(&self, channel: &str) -> ChannelKeys {
        let mut c2s_key = [0u8; 32];
        self.hk
            .expand_multi_info(&[format!("bacak-remote c2s {channel}").as_bytes(), &self.transcript], &mut c2s_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let mut s2c_key = [0u8; 32];
        self.hk
            .expand_multi_info(&[format!("bacak-remote s2c {channel}").as_bytes(), &self.transcript], &mut s2c_key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        ChannelKeys { c2s_key, s2c_key }
    }
}

/// Encrypts outgoing messages for one direction of one paired session.
/// Nonces are a plain little-endian counter zero-extended to 12 bytes —
/// safe because each `Cipher` is built from a key used for exactly one
/// pairing session's lifetime (a fresh ECDH exchange every time), so the
/// (key, nonce) pair is never reused across sessions.
pub struct Cipher {
    aead: ChaCha20Poly1305,
    next_counter: u64,
}

impl Cipher {
    pub fn new(key: [u8; 32]) -> Self {
        Self { aead: ChaCha20Poly1305::new(&AeadKey::from(key)), next_counter: 0 }
    }

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

/// Decrypts incoming messages for one direction of one paired session, with
/// replay protection: a nonce counter at or below the highest one already
/// accepted is rejected. UDP packets can arrive out of order, so an
/// occasional out-of-order-but-legitimate packet gets dropped rather than
/// accepted — the right tradeoff for a fire-and-forget input/video stream.
pub struct Opener {
    aead: ChaCha20Poly1305,
    highest_seen: Option<u64>,
}

impl Opener {
    pub fn new(key: [u8; 32]) -> Self {
        Self { aead: ChaCha20Poly1305::new(&AeadKey::from(key)), highest_seen: None }
    }

    /// Returns `None` on any failure: bad tag (wrong key, corrupted/forged
    /// ciphertext) or a replayed/stale nonce. Deliberately not a `Result` —
    /// callers treat every failure identically (drop the packet), avoiding
    /// an oracle that would tell an attacker *why* decryption failed.
    pub fn open(&mut self, nonce_bytes: [u8; NONCE_LEN], ciphertext: &[u8]) -> Option<Vec<u8>> {
        let counter = u64::from_le_bytes(nonce_bytes[..8].try_into().unwrap());
        if let Some(highest) = self.highest_seen
            && counter <= highest
        {
            return None;
        }
        let plaintext = self.aead.decrypt(&AeadNonce::from(nonce_bytes), ciphertext).ok()?;
        self.highest_seen = Some(counter);
        Some(plaintext)
    }
}

fn counter_nonce(counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[..8].copy_from_slice(&counter.to_le_bytes());
    nonce
}

/// Generates a fresh 6-digit pairing PIN (100000..=999999), shown to the
/// person at the server so they can type it into the client — mirrors
/// `uzakel/uzakel-android/daemon/src/discovery.rs`'s PIN generation.
pub fn generate_pin() -> u32 {
    use rand::Rng;
    rand::thread_rng().gen_range(100_000..=999_999)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecdh_roundtrip_agrees_on_keys() {
        let client = EphemeralKeypair::generate();
        let server = EphemeralKeypair::generate();
        let client_pub = client.public_bytes;
        let server_pub = server.public_bytes;

        let client_material = client.derive(server_pub, 123456, client_pub, server_pub);
        let server_material = server.derive(client_pub, 123456, client_pub, server_pub);

        let client_video = client_material.channel_keys("video");
        let server_video = server_material.channel_keys("video");
        assert_eq!(client_video.c2s_key, server_video.c2s_key);
        assert_eq!(client_video.s2c_key, server_video.s2c_key);
        assert_eq!(client_material.confirm_tag, server_material.confirm_tag);
    }

    #[test]
    fn different_channels_yield_independent_keys() {
        let client = EphemeralKeypair::generate();
        let server_pub = EphemeralKeypair::generate().public_bytes;
        let client_pub = client.public_bytes;
        let material = client.derive(server_pub, 123456, client_pub, server_pub);

        let video = material.channel_keys("video");
        let input = material.channel_keys("input");
        assert_ne!(video.c2s_key, input.c2s_key, "reusing a channel's key elsewhere would reuse (key, nonce) pairs");
        assert_ne!(video.s2c_key, input.s2c_key);
    }

    #[test]
    fn wrong_pin_yields_a_confirm_tag_the_other_side_wont_match() {
        let client = EphemeralKeypair::generate();
        let server = EphemeralKeypair::generate();
        let client_pub = client.public_bytes;
        let server_pub = server.public_bytes;

        let client_material = client.derive(server_pub, 111111, client_pub, server_pub);
        let server_material = server.derive(client_pub, 999999, client_pub, server_pub);

        assert_eq!(client_material.channel_keys("video").c2s_key, server_material.channel_keys("video").c2s_key);
        assert_ne!(client_material.confirm_tag, server_material.confirm_tag);
    }

    #[test]
    fn cipher_opener_roundtrip() {
        let mut cipher = Cipher::new([7u8; 32]);
        let mut opener = Opener::new([7u8; 32]);

        let (nonce, ct) = cipher.seal(b"hello bacak-remote");
        let pt = opener.open(nonce, &ct).expect("should decrypt");
        assert_eq!(pt, b"hello bacak-remote");
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
        assert!(opener.open(nonce, &ct).is_none());
    }

    #[test]
    fn generated_pin_is_six_digits() {
        for _ in 0..100 {
            let pin = generate_pin();
            assert!((100_000..=999_999).contains(&pin));
        }
    }
}
