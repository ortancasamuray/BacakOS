//! Shared record of which LAN addresses have completed PIN pairing
//! (`discovery.rs`'s handshake, ARCHITECTURE.md §2.3) — the input and
//! file-transfer channels check this before acting on a packet/connection.
//!
//! This closes the gap ARCHITECTURE.md's §5/§6 flagged repeatedly: before
//! this module, `input_manager.rs` and `file_server.rs` accepted from any
//! sender on the LAN regardless of pairing state.
//!
//! **What this is not:** cryptographic authentication. Trust is keyed by IP
//! address, which is trivially spoofable by a deliberately hostile device
//! already on the LAN — this stops accidental cross-talk (another phone
//! running a stray copy of the app, a leftover session from a previous
//! pairing target) and enforces that *some* human approved a PIN before
//! traffic is acted on, not a guarantee against an adversarial LAN peer. A
//! TLS/PSK session tied to the pairing handshake is still the real fix and
//! is tracked in ARCHITECTURE.md's open questions.
//!
//! **In-memory only:** trust doesn't survive a daemon restart. A
//! previously-paired client (Android's "saved host" list, which skips
//! straight to the input/file channels on reconnect) will have its traffic
//! silently rejected until it re-pairs — expected, not a bug, but worth
//! knowing if "Bağlan" on a saved host stops working after the daemon
//! restarts.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct TrustStore {
    trusted: Arc<Mutex<HashSet<IpAddr>>>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `addr` as trusted after it completes the PIN handshake.
    pub fn trust(&self, addr: IpAddr) {
        self.trusted.lock().unwrap().insert(addr);
    }

    pub fn is_trusted(&self, addr: IpAddr) -> bool {
        self.trusted.lock().unwrap().contains(&addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpaired_address_is_not_trusted() {
        let store = TrustStore::new();
        let addr: IpAddr = "192.168.1.100".parse().unwrap();
        assert!(!store.is_trusted(addr));
    }

    #[test]
    fn trusting_an_address_makes_is_trusted_true() {
        let store = TrustStore::new();
        let addr: IpAddr = "192.168.1.100".parse().unwrap();
        store.trust(addr);
        assert!(store.is_trusted(addr));
    }

    #[test]
    fn trust_is_shared_across_clones() {
        let store = TrustStore::new();
        let addr: IpAddr = "192.168.1.100".parse().unwrap();
        let clone = store.clone();
        clone.trust(addr);
        assert!(store.is_trusted(addr));
    }
}
