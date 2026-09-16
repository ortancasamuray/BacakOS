//! Power actions, delegated to `systemd-logind`.
//!
//! The greeter cannot power off the machine directly. It asks the daemon, the
//! daemon asks logind (here via `systemctl`, which talks to
//! `org.freedesktop.login1`).
//!
//! Note on policy: this daemon runs as **root**, so `systemctl <verb>` performs
//! the action directly — polkit is *not* consulted (it gates unprivileged
//! callers). The actual gate here is therefore the admin's `[power]` config
//! (`allow_*`), enforced by [`PowerAction::allowed_by`] before we shell out.
//! [`PowerAction::polkit_action`] is retained for a future variant that talks to
//! `org.freedesktop.login1.Manager` over `zbus` from an unprivileged context,
//! where polkit *would* apply.

use bacak_common::config::Config;
use bacak_common::ipc::Response;
use bacak_common::power::PowerAction;
use std::process::Command;

/// Returns `Ok(true)` when the action terminates the daemon's reason to live
/// (poweroff / reboot), so the caller exits its loop. Suspend/hibernate return
/// `Ok(false)`: the machine will resume to the same greeter.
pub fn handle(
    config: &Config,
    action: PowerAction,
    conn: &mut crate::ipc::Conn,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !action.allowed_by(&config.power) {
        log::warn!("power action {action:?} rejected by config");
        conn.write_response(&Response::Error {
            message: "This action is not permitted.".into(),
        })?;
        return Ok(false);
    }

    let verb = match action {
        PowerAction::Shutdown => "poweroff",
        PowerAction::Restart => "reboot",
        PowerAction::Suspend => "suspend",
        PowerAction::Hibernate => "hibernate",
    };

    log::info!(
        "invoking logind: systemctl {verb} (polkit: {})",
        action.polkit_action()
    );
    let status = Command::new("systemctl").arg(verb).status();

    match status {
        Ok(s) if s.success() => {
            let terminal = matches!(action, PowerAction::Shutdown | PowerAction::Restart);
            Ok(terminal)
        }
        Ok(s) => {
            conn.write_response(&Response::Error {
                message: "The power action was denied.".into(),
            })?;
            log::warn!("systemctl {verb} exited with {s}");
            Ok(false)
        }
        Err(e) => {
            conn.write_response(&Response::Error {
                message: "Power management is unavailable.".into(),
            })?;
            log::error!("failed to run systemctl {verb}: {e}");
            Ok(false)
        }
    }
}
