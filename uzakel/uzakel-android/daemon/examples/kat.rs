//! One-off known-answer-test vector generator — NOT part of the daemon,
//! not called from anywhere else. Prints fixed-input crypto.rs derivation
//! output as hex so it can be cross-checked against an independent Kotlin/
//! BouncyCastle computation of the same values, to catch any HKDF/AEAD
//! byte-layout mismatch between the two implementations before trusting
//! them to interoperate. Delete once cross-language interop is confirmed
//! on real hardware (or keep as a standing interop regression check —
//! either is fine, it's cheap to keep).

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use x25519_dalek::x25519;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let client_priv = [1u8; 32];
    let daemon_priv = [2u8; 32];
    const BASEPOINT: [u8; 32] = [9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    let client_pub = x25519(client_priv, BASEPOINT);
    let daemon_pub = x25519(daemon_priv, BASEPOINT);
    let shared = x25519(client_priv, daemon_pub);
    let shared_from_daemon_side = x25519(daemon_priv, client_pub);
    assert_eq!(shared, shared_from_daemon_side, "ECDH must agree from both sides");

    let pin: u32 = 123456;

    let (_, hk) = Hkdf::<Sha256>::extract(Some(b"uzakel-pairing-v1"), &shared);
    let mut transcript = Vec::with_capacity(64);
    transcript.extend_from_slice(&client_pub);
    transcript.extend_from_slice(&daemon_pub);

    let mut c2s_key = [0u8; 32];
    hk.expand_multi_info(&[b"uzakel c2s", &transcript], &mut c2s_key).unwrap();
    let mut s2c_key = [0u8; 32];
    hk.expand_multi_info(&[b"uzakel s2c", &transcript], &mut s2c_key).unwrap();
    let mut confirm_key = [0u8; 32];
    hk.expand_multi_info(&[b"uzakel confirm", &transcript, &pin.to_le_bytes()], &mut confirm_key).unwrap();

    let mut mac = Hmac::<Sha256>::new_from_slice(&confirm_key).unwrap();
    mac.update(&transcript);
    let confirm_tag: [u8; 32] = mac.finalize().into_bytes().into();

    println!("client_pub    = {}", hex(&client_pub));
    println!("daemon_pub    = {}", hex(&daemon_pub));
    println!("shared_secret = {}", hex(&shared));
    println!("pin           = {pin}");
    println!("c2s_key       = {}", hex(&c2s_key));
    println!("s2c_key       = {}", hex(&s2c_key));
    println!("confirm_key   = {}", hex(&confirm_key));
    println!("confirm_tag   = {}", hex(&confirm_tag));

    // Also print one ChaCha20-Poly1305 seal() output for a fixed
    // plaintext under c2s_key with nonce counter 0, to cross-check AEAD
    // framing (nonce layout, tag placement) independently of HKDF.
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    let cipher = ChaCha20Poly1305::new(&Key::from(c2s_key));
    let nonce = [0u8; 12];
    let ciphertext = cipher.encrypt(&Nonce::from(nonce), &b"hello uzakel"[..]).unwrap();
    println!("aead_nonce0_ct = {}", hex(&ciphertext));
}
