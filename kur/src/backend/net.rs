// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Connectivity check for the Debian mirror.
//!
//! `debootstrap` downloads the whole base system, so a missing network turns
//! into a failure eight minutes into the install, with the target disk already
//! wiped. Checking first costs a second and turns that into a refusal.
//!
//! A TCP connect is deliberately all we do: no HTTP client, no TLS, no extra
//! dependency. It proves DNS resolves and the route works, which is exactly the
//! set of failures we can warn about.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Host and port taken from the mirror URL used by [`super::install`].
const MIRROR_HOST: &str = "deb.debian.org";
const MIRROR_PORT: u16 = 80;

/// Long enough for a slow DNS server, short enough that the user does not think
/// the installer hung.
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Checking,
    Online,
    /// Resolution succeeded but no route, or the connect timed out.
    Offline,
    /// DNS failed: usually no DHCP lease at all.
    NoDns,
}

impl Status {
    /// Whether the install may start. Only a confirmed `Online` is enough —
    /// `Checking` means we simply do not know yet.
    pub fn allows_install(self) -> bool {
        self == Status::Online
    }

    pub fn message(self) -> &'static str {
        match self {
            Status::Checking => "Ağ bağlantısı denetleniyor…",
            Status::Online => "Ağ bağlantısı hazır.",
            Status::Offline => "Debian deposuna ulaşılamıyor. Kurulum için internet gerekli.",
            Status::NoDns => "Alan adı çözümlenemedi. Ağ bağlantınızı denetleyin.",
        }
    }
}

/// Resolve and connect. Blocking; call from [`spawn_check`], not the UI thread.
fn probe() -> Status {
    let addresses = match (MIRROR_HOST, MIRROR_PORT).to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(error) => {
            log::warn!("{MIRROR_HOST} çözümlenemedi: {error}");
            return Status::NoDns;
        }
    };

    // A dual-stack host resolves to both A and AAAA records; an IPv6-only route
    // failing does not mean the machine is offline, so try each in turn.
    for address in addresses {
        match TcpStream::connect_timeout(&address, TIMEOUT) {
            Ok(_) => return Status::Online,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
            Err(error) => log::debug!("{address} bağlanılamadı: {error}"),
        }
    }
    Status::Offline
}

/// Run the probe on a worker thread and hand the result to `report`.
///
/// `report` runs on that thread; the caller marshals it back to the UI exactly
/// as [`super::install::spawn`] does.
pub fn spawn_check<F>(report: F)
where
    F: FnOnce(Status) + Send + 'static,
{
    std::thread::Builder::new()
        .name("kur-netcheck".into())
        .spawn(move || report(probe()))
        // A failed thread spawn means the system is out of resources; the
        // install would not survive anyway, so leaving the UI in `Checking`
        // (which blocks the start button) is the safe outcome.
        .map(|_| ())
        .unwrap_or_else(|error| log::error!("ağ denetimi başlatılamadı: {error}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_online_permits_install() {
        assert!(Status::Online.allows_install());
        assert!(!Status::Checking.allows_install());
        assert!(!Status::Offline.allows_install());
        assert!(!Status::NoDns.allows_install());
    }

    #[test]
    fn every_status_has_a_message() {
        for status in [Status::Checking, Status::Online, Status::Offline, Status::NoDns] {
            assert!(!status.message().is_empty());
        }
    }
}
