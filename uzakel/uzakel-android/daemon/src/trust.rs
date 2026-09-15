//! Shared record of which LAN addresses have completed the ECDH-plus-PIN
//! pairing handshake (`discovery.rs`, ARCHITECTURE.md §2.3/§2.3.1) — the
//! input and file-transfer channels check this before acting on a
//! packet/connection, and use the stored session keys to decrypt/encrypt
//! everything on it.
//!
//! **What pairing now provides:** real confidentiality. A successful
//! pairing derives an ECDH shared secret (see `crypto.rs`) that a passive
//! LAN eavesdropper cannot recover from the exchanged public keys alone,
//! and a PIN-bound confirmation tag that lets the client detect an active
//! man-in-the-middle substituting its own keys during the exchange. Once
//! paired, every input packet and file-transfer frame is authenticated
//! and encrypted with ChaCha20-Poly1305 under keys derived from that
//! exchange — successfully decrypting a frame *is* the real proof the
//! sender is the paired client, not just "same IP as before."
//!
//! **What this still is not:** the PIN itself is only ~20 bits of entropy,
//! checked once server-side at `PairRequest` time — this scheme doesn't
//! give that check resistance to a full man-in-the-middle who *does* know
//! the PIN (nothing meaningfully could, at that entropy). A real PAKE
//! would remove that residual reliance on the PIN's own strength; see
//! ARCHITECTURE.md §5.
//!
//! **In-memory only:** sessions don't survive a daemon restart. A
//! previously-paired client (Android's "saved host" list) needs to
//! re-pair once after the daemon restarts before its input/file traffic
//! is accepted again.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use crate::crypto::{Cipher, Opener};

/// One paired peer's live encryption state. `opener` decrypts frames this
/// peer sends *to* the daemon; `cipher` encrypts frames the daemon sends
/// *to* this peer. Each wraps its own nonce counter/replay-window state
/// (see `crypto.rs`), so they need their own lock — a `Mutex` per field
/// rather than one lock over both, since encrypting a reply while
/// decrypting the next incoming packet shouldn't have to serialize on
/// each other.
pub struct Session {
    pub opener: Mutex<Opener>,
    pub cipher: Mutex<Cipher>,
}

#[derive(Clone, Default)]
pub struct TrustStore {
    sessions: Arc<Mutex<HashMap<IpAddr, Arc<Session>>>>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a completed pairing. `rx_key` decrypts what `addr` sends
    /// *to* the daemon; `tx_key` encrypts what the daemon sends *to*
    /// `addr`. Replaces any existing session for this address (a fresh
    /// pairing attempt from the same IP supersedes the old one, e.g. after
    /// the app reconnects).
    pub fn trust(&self, addr: IpAddr, rx_key: [u8; 32], tx_key: [u8; 32]) {
        let session = Arc::new(Session {
            opener: Mutex::new(Opener::new(rx_key)),
            cipher: Mutex::new(Cipher::new(tx_key)),
        });
        self.sessions.lock().unwrap().insert(addr, session);
    }

    /// The live session for `addr`, if paired — clone is cheap (an `Arc`
    /// bump); callers lock `.opener`/`.cipher` themselves for as short a
    /// window as the actual encrypt/decrypt call needs.
    pub fn session(&self, addr: IpAddr) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(&addr).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> IpAddr {
        "192.168.1.100".parse().unwrap()
    }

    #[test]
    fn unpaired_address_has_no_session() {
        let store = TrustStore::new();
        assert!(store.session(addr()).is_none());
    }

    #[test]
    fn trusting_an_address_creates_a_session() {
        let store = TrustStore::new();
        store.trust(addr(), [1u8; 32], [2u8; 32]);
        assert!(store.session(addr()).is_some());
    }

    #[test]
    fn trust_is_shared_across_clones() {
        let store = TrustStore::new();
        let clone = store.clone();
        clone.trust(addr(), [1u8; 32], [2u8; 32]);
        assert!(store.session(addr()).is_some());
    }

    #[test]
    fn session_keys_actually_work_for_encrypt_decrypt() {
        // End-to-end sanity check that TrustStore wires the right key into
        // the right role: what the daemon's `cipher` (tx_key) encrypts,
        // a peer holding the matching rx_key (== our tx_key, by
        // construction in this test) can decrypt with an `Opener`.
        let store = TrustStore::new();
        store.trust(addr(), [3u8; 32], [4u8; 32]);
        let session = store.session(addr()).unwrap();

        let (nonce, ciphertext) = session.cipher.lock().unwrap().seal(b"payload");
        let mut peer_side_opener = Opener::new([4u8; 32]); // peer's rx_key == our tx_key
        let plaintext = peer_side_opener.open(nonce, &ciphertext).unwrap();
        assert_eq!(plaintext, b"payload");
    }
}
