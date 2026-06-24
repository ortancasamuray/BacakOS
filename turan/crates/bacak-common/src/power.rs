//! Power-action model.
//!
//! The greeter never executes power actions itself. It asks the daemon, which
//! calls `org.freedesktop.login1` over D-Bus. logind in turn checks polkit, so
//! availability depends on the active polkit policy *and* the admin's config
//! ([`crate::config::Power`]). Each action carries its logind method name and
//! the polkit action id an admin would grant.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerAction {
    Shutdown,
    Restart,
    Suspend,
    Hibernate,
}

impl PowerAction {
    /// The `org.freedesktop.login1.Manager` method to invoke.
    pub fn logind_method(self) -> &'static str {
        match self {
            PowerAction::Shutdown => "PowerOff",
            PowerAction::Restart => "Reboot",
            PowerAction::Suspend => "Suspend",
            PowerAction::Hibernate => "Hibernate",
        }
    }

    /// The polkit action id logind consults for this verb (from the greeter
    /// seat, the `*-multiple-sessions` variants apply when others are logged
    /// in).
    pub fn polkit_action(self) -> &'static str {
        match self {
            PowerAction::Shutdown => "org.freedesktop.login1.power-off",
            PowerAction::Restart => "org.freedesktop.login1.reboot",
            PowerAction::Suspend => "org.freedesktop.login1.suspend",
            PowerAction::Hibernate => "org.freedesktop.login1.hibernate",
        }
    }

    /// Whether this action is permitted by the admin configuration.
    pub fn allowed_by(self, cfg: &crate::config::Power) -> bool {
        match self {
            PowerAction::Shutdown => cfg.allow_shutdown,
            PowerAction::Restart => cfg.allow_restart,
            PowerAction::Suspend => cfg.allow_suspend,
            PowerAction::Hibernate => cfg.allow_hibernate,
        }
    }

    pub const ALL: [PowerAction; 4] = [
        PowerAction::Shutdown,
        PowerAction::Restart,
        PowerAction::Suspend,
        PowerAction::Hibernate,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Power;

    #[test]
    fn config_gates_actions() {
        let cfg = Power {
            allow_shutdown: true,
            allow_restart: false,
            allow_suspend: true,
            allow_hibernate: false,
        };
        assert!(PowerAction::Shutdown.allowed_by(&cfg));
        assert!(!PowerAction::Restart.allowed_by(&cfg));
        assert!(!PowerAction::Hibernate.allowed_by(&cfg));
    }

    #[test]
    fn method_names_match_logind() {
        assert_eq!(PowerAction::Restart.logind_method(), "Reboot");
        assert_eq!(PowerAction::Shutdown.logind_method(), "PowerOff");
    }
}
