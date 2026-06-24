//! Network — connectivity diagnostics and routing.
//!
//! Today this module only exposes a small synchronous reachability probe; the
//! production implementation will surface routes, DNS, captive-portal detection,
//! and a structured connectivity-change stream. The Wi-Fi radio itself lives in
//! [`crate::device`] because it shares the BlueZ/PipeWire/NM provider plumbing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reachability {
    Online,
    LocalOnly,
    Offline,
    Unknown,
}

/// Coarse network reachability — `Unknown` until the platform probe lands.
pub fn reachability() -> Reachability {
    Reachability::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_returns_unknown() {
        assert_eq!(reachability(), Reachability::Unknown);
    }
}
