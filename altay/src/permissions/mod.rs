// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Read and (when authorized) change POSIX permissions. Altay never runs as
//! root; changing permissions on a file the user does not own would require an
//! elevation portal (org.freedesktop.policykit) — wired in a later phase.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::security::{AccessDenied, Sandbox};

/// A human + machine readable view of a path's mode bits.
#[derive(Debug, Clone)]
pub struct Permissions {
    pub mode: u32,
    pub owner_read: bool,
    pub owner_write: bool,
    pub owner_exec: bool,
    pub group_read: bool,
    pub group_write: bool,
    pub group_exec: bool,
    pub other_read: bool,
    pub other_write: bool,
    pub other_exec: bool,
}

impl Permissions {
    fn from_mode(mode: u32) -> Self {
        let b = |shift: u32| mode & (1 << shift) != 0;
        Permissions {
            mode: mode & 0o777,
            owner_read: b(8),
            owner_write: b(7),
            owner_exec: b(6),
            group_read: b(5),
            group_write: b(4),
            group_exec: b(3),
            other_read: b(2),
            other_write: b(1),
            other_exec: b(0),
        }
    }

    /// Render as `rwxr-xr-x`.
    pub fn symbolic(&self) -> String {
        let bit = |v: bool, c: char| if v { c } else { '-' };
        format!(
            "{}{}{}{}{}{}{}{}{}",
            bit(self.owner_read, 'r'), bit(self.owner_write, 'w'), bit(self.owner_exec, 'x'),
            bit(self.group_read, 'r'), bit(self.group_write, 'w'), bit(self.group_exec, 'x'),
            bit(self.other_read, 'r'), bit(self.other_write, 'w'), bit(self.other_exec, 'x'),
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PermError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("authorization failed or was cancelled")]
    Elevation(String),
}

impl PermError {
    /// True if the error is "operation not permitted" (i.e. the user does not
    /// own the file) — the case where PolicyKit elevation can help.
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, PermError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied)
    }
}

/// Read the permission bits of a sandbox-permitted path.
pub fn read(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<Permissions, PermError> {
    let safe = sandbox.resolve(path)?;
    let meta = std::fs::metadata(safe.as_path())?;
    Ok(Permissions::from_mode(meta.permissions().mode()))
}

/// Change permission bits. Succeeds only for files the user can already chmod
/// (typically files they own); otherwise the OS returns EPERM, which we surface
/// rather than attempting any privilege escalation.
pub fn set_mode(sandbox: &Sandbox, path: impl AsRef<Path>, mode: u32) -> Result<(), PermError> {
    let safe = sandbox.resolve(path)?;
    let perms = std::fs::Permissions::from_mode(mode & 0o777);
    std::fs::set_permissions(safe.as_path(), perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    #[test]
    fn symbolic_rendering() {
        assert_eq!(Permissions::from_mode(0o755).symbolic(), "rwxr-xr-x");
        assert_eq!(Permissions::from_mode(0o644).symbolic(), "rw-r--r--");
    }

    #[test]
    fn elevation_still_respects_the_sandbox() {
        // A home-only sandbox must refuse even an *elevated* chmod of /etc —
        // elevation grants privilege, never path access beyond the sandbox.
        let home = std::env::temp_dir().join(format!("altay-elev-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(std::fs::canonicalize(&home).unwrap())]);
        assert!(matches!(
            set_mode_elevated(&sb, "/etc/shadow", 0o600),
            Err(PermError::Denied(_))
        ));
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Change permission bits with **PolicyKit elevation** (`pkexec chmod`).
///
/// Used only when the plain [`set_mode`] is refused because the user does not
/// own the file. The path is still sandbox-validated first, so elevation grants
/// ownership-level privilege but **cannot escape the sandbox** — system
/// directories remain unreachable. `pkexec` shows the desktop's polkit
/// authentication prompt; the whole file manager keeps running unprivileged.
pub fn set_mode_elevated(sandbox: &Sandbox, path: impl AsRef<Path>, mode: u32) -> Result<(), PermError> {
    let safe = sandbox.resolve(path)?;
    let status = std::process::Command::new("pkexec")
        .arg("chmod")
        .arg(format!("{:o}", mode & 0o777))
        .arg(safe.as_path())
        .status()
        .map_err(|e| PermError::Elevation(format!("pkexec unavailable: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        // pkexec exits 126 (not authorized) / 127 (dismissed) / chmod's code.
        Err(PermError::Elevation(format!("pkexec exited with {status}")))
    }
}
