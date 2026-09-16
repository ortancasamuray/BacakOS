// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Security sandbox — the core of Altay's access model.
//!
//! A non-root user must only ever reach:
//!   * their own home directory,
//!   * mounted removable / external devices (`/media/$USER`, `/run/media/$USER`, `/mnt`),
//!   * accessible network mounts (`/run/user/$UID/gvfs`, `$XDG_RUNTIME_DIR/gvfs`).
//!
//! System directories (`/root /etc /usr /var /boot /proc /sys /dev`) are never
//! exposed by default. Every path that crosses the UI boundary is funnelled
//! through [`Sandbox::resolve`], which **canonicalises symlinks first** so a
//! symlink living inside `~` that points at `/etc` cannot be used to escape.

use std::path::{Component, Path, PathBuf};

mod roots;

pub use roots::{AllowedRoot, RootKind};

/// Reasons a path may be rejected by the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccessDenied {
    #[error("path lies outside the permitted areas (home, removable devices, network mounts)")]
    OutsideSandbox,
    #[error("path resolves into a protected system directory: {0}")]
    ProtectedSystemDir(String),
    #[error("path could not be resolved: {0}")]
    Unresolvable(String),
    #[error("a parent component does not exist")]
    MissingParent,
}

/// A path that has been validated against the sandbox. The inner `PathBuf` is
/// always absolute and canonical (symlinks resolved). Constructed only by the
/// [`Sandbox`]; this makes "validated" un-forgeable elsewhere in the codebase.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SafePath(PathBuf);

impl SafePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for SafePath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// System prefixes that are denied even if some mount or symlink would otherwise
/// make them reachable.
const PROTECTED_PREFIXES: &[&str] = &[
    "/root", "/etc", "/usr", "/var", "/boot", "/proc", "/sys", "/dev", "/bin", "/sbin", "/lib",
    "/lib64", "/opt", "/srv", "/run/lock", "/run/systemd",
];

/// The access policy. Built once at startup from the real environment and then
/// queried for every navigation / file operation.
#[derive(Debug, Clone)]
pub struct Sandbox {
    roots: Vec<AllowedRoot>,
}

impl Sandbox {
    /// Build a sandbox from the current user's environment.
    pub fn from_env() -> Self {
        Self {
            roots: roots::discover(),
        }
    }

    /// Construct an explicit sandbox (used by tests).
    pub fn with_roots(roots: Vec<AllowedRoot>) -> Self {
        Self { roots }
    }

    /// The roots that should be shown in the sidebar.
    pub fn roots(&self) -> &[AllowedRoot] {
        &self.roots
    }

    /// Validate an existing path. Resolves symlinks, then checks the *canonical*
    /// path against the allow/deny lists.
    pub fn resolve(&self, path: impl AsRef<Path>) -> Result<SafePath, AccessDenied> {
        let canonical = std::fs::canonicalize(path.as_ref())
            .map_err(|e| AccessDenied::Unresolvable(e.to_string()))?;
        self.check_canonical(canonical)
    }

    /// Validate a path that does **not** yet exist (creating a file/folder).
    /// The deepest existing ancestor is canonicalised and re-joined with the
    /// remaining components, blocking `..` escapes via the not-yet-real tail.
    pub fn resolve_for_create(&self, path: impl AsRef<Path>) -> Result<SafePath, AccessDenied> {
        let path = path.as_ref();
        // Walk up to the first ancestor that exists.
        let mut existing = path;
        let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
        loop {
            if existing.exists() {
                break;
            }
            match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name);
                    existing = parent;
                }
                _ => return Err(AccessDenied::MissingParent),
            }
        }
        let mut resolved = std::fs::canonicalize(existing)
            .map_err(|e| AccessDenied::Unresolvable(e.to_string()))?;
        for name in tail.into_iter().rev() {
            // Reject `.`/`..` in the synthetic tail to prevent escape.
            if name == ".." || name == "." {
                return Err(AccessDenied::OutsideSandbox);
            }
            resolved.push(name);
        }
        self.check_canonical(resolved)
    }

    /// Is the given (already canonical) path permitted? Centralised policy.
    fn check_canonical(&self, canonical: PathBuf) -> Result<SafePath, AccessDenied> {
        // Defence in depth: even a canonical path is rejected if it falls under
        // a protected system prefix (e.g. a future bind-mount of /etc into /mnt).
        if let Some(p) = protected_prefix(&canonical) {
            return Err(AccessDenied::ProtectedSystemDir(p.to_string()));
        }
        if self.roots.iter().any(|r| canonical.starts_with(r.path())) {
            Ok(SafePath(canonical))
        } else {
            Err(AccessDenied::OutsideSandbox)
        }
    }

    /// Convenience: the default landing location (home directory).
    pub fn home(&self) -> SafePath {
        self.roots
            .iter()
            .find(|r| r.is_home())
            .map(|r| SafePath(r.path().to_path_buf()))
            .unwrap_or_else(|| SafePath(PathBuf::from("/")))
    }
}

/// Returns the protected prefix a canonical path falls under, if any.
fn protected_prefix(canonical: &Path) -> Option<&'static str> {
    // Compare component-wise so `/usrlocal` is not matched by `/usr`.
    PROTECTED_PREFIXES.iter().copied().find(|prefix| {
        let prefix_path = Path::new(prefix);
        canonical_starts_with_components(canonical, prefix_path)
    })
}

/// `Path::starts_with` but guaranteed component-wise (it already is, but this
/// makes the intent explicit and ignores trailing separators).
fn canonical_starts_with_components(path: &Path, prefix: &Path) -> bool {
    let mut p = path.components();
    for c in prefix.components() {
        match (c, p.next()) {
            (Component::RootDir, Some(Component::RootDir)) => continue,
            (Component::Normal(a), Some(Component::Normal(b))) if a == b => continue,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> PathBuf {
        let base = std::env::temp_dir().join(format!("altay-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&base);
        base
    }

    #[test]
    fn allows_paths_inside_home() {
        let home = tmp().join("home");
        fs::create_dir_all(home.join("Documents")).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(home.clone())]);
        assert!(sb.resolve(home.join("Documents")).is_ok());
    }

    #[test]
    fn rejects_paths_outside_home() {
        let home = tmp().join("home2");
        fs::create_dir_all(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(home)]);
        // /etc exists on every Linux box used for CI.
        assert_eq!(sb.resolve("/etc"), Err(AccessDenied::ProtectedSystemDir("/etc".into())));
    }

    #[test]
    fn symlink_escape_is_blocked() {
        let home = tmp().join("home3");
        fs::create_dir_all(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(home.clone())]);
        let link = home.join("escape");
        let _ = fs::remove_file(&link);
        // A symlink inside home pointing at /etc must NOT grant access to /etc.
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        let res = sb.resolve(&link);
        assert!(matches!(res, Err(AccessDenied::ProtectedSystemDir(_))), "got {res:?}");
    }

    #[test]
    fn dotdot_escape_is_blocked() {
        let home = tmp().join("home4").join("sub");
        fs::create_dir_all(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(home.clone())]);
        // ~/sub/../../ would climb above home; canonicalize collapses it.
        assert!(sb.resolve(home.join("..").join("..")).is_err());
    }

    #[test]
    fn create_under_home_is_allowed() {
        let home = tmp().join("home5");
        fs::create_dir_all(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(home.clone())]);
        let new = home.join("a").join("b.txt"); // neither exists yet
        assert!(sb.resolve_for_create(new).is_ok());
    }

    #[test]
    fn create_with_dotdot_tail_is_blocked() {
        let home = tmp().join("home6");
        fs::create_dir_all(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(home.clone())]);
        let new = home.join("..").join("evil.txt");
        assert!(sb.resolve_for_create(new).is_err());
    }
}
