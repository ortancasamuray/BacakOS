//! Enumeration of *login-capable* local users for the greeter.
//!
//! The greeter must show humans, not daemons. The policy mirrors what
//! AccountsService / GDM use:
//!
//! * UID must fall within `[UID_MIN, UID_MAX]` from `/etc/login.defs`
//!   (defaulting to 1000..=60000 when the file is absent), **or** be the
//!   special-cased `root` only when explicitly allowed (never by default).
//! * The login shell must not be a "nologin"/"false" shell.
//! * Accounts explicitly hidden via `/etc/bacak-display-manager/hidden-users`
//!   are dropped.
//!
//! Parsing is done from plain `/etc/passwd` text so this stays free of libc /
//! NSS bindings and remains unit-testable. A production build can swap in an
//! `getpwent`-based provider behind the same [`UserProvider`] trait without
//! changing callers.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A login-capable account surfaced to the greeter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    /// GECOS full name (first comma-separated field), if any.
    pub full_name: Option<String>,
    pub home: PathBuf,
    pub shell: PathBuf,
}

impl User {
    /// Display string preferred by the UI: full name, else username.
    pub fn display_name(&self) -> &str {
        self.full_name.as_deref().unwrap_or(&self.name)
    }

    /// Conventional avatar path AccountsService/GDM also honour.
    pub fn avatar_path(&self) -> PathBuf {
        // ~/.face takes precedence in the greeter; this is the system fallback.
        PathBuf::from(format!("/var/lib/AccountsService/icons/{}", self.name))
    }
}

/// UID bounds, normally read from `/etc/login.defs`.
#[derive(Debug, Clone, Copy)]
pub struct UidRange {
    pub min: u32,
    pub max: u32,
}

impl Default for UidRange {
    fn default() -> Self {
        Self {
            min: 1000,
            max: 60000,
        }
    }
}

impl UidRange {
    /// Parse `UID_MIN` / `UID_MAX` out of login.defs text.
    pub fn from_login_defs(text: &str) -> Self {
        let mut range = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            match (it.next(), it.next()) {
                (Some("UID_MIN"), Some(v)) => {
                    if let Ok(v) = v.parse() {
                        range.min = v;
                    }
                }
                (Some("UID_MAX"), Some(v)) => {
                    if let Ok(v) = v.parse() {
                        range.max = v;
                    }
                }
                _ => {}
            }
        }
        range
    }

    pub fn load() -> Self {
        std::fs::read_to_string("/etc/login.defs")
            .map(|t| Self::from_login_defs(&t))
            .unwrap_or_default()
    }

    pub fn contains(&self, uid: u32) -> bool {
        uid >= self.min && uid <= self.max
    }
}

/// Login shells that mean "this account cannot start an interactive session".
fn is_nologin_shell(shell: &Path) -> bool {
    match shell.file_name().and_then(|s| s.to_str()) {
        None => true,
        Some(name) => name == "nologin" || name == "false" || name == "sync" || name == "shutdown",
    }
}

/// Abstraction over the source of accounts, so callers can be tested and the
/// backing store (passwd file vs. NSS) can change independently.
pub trait UserProvider {
    fn login_capable_users(&self) -> crate::Result<Vec<User>>;
}

/// Default provider that reads `/etc/passwd` plus filtering inputs.
pub struct PasswdProvider {
    pub passwd_path: PathBuf,
    pub uid_range: UidRange,
    pub hidden: HashSet<String>,
    pub allow_root: bool,
}

impl Default for PasswdProvider {
    fn default() -> Self {
        Self {
            passwd_path: PathBuf::from("/etc/passwd"),
            uid_range: UidRange::load(),
            hidden: load_hidden_users(),
            allow_root: false,
        }
    }
}

/// Read the optional hidden-users list, one username per line.
fn load_hidden_users() -> HashSet<String> {
    const PATH: &str = "/etc/bacak-display-manager/hidden-users";
    std::fs::read_to_string(PATH)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

impl PasswdProvider {
    /// Parse a single `/etc/passwd` line into a candidate [`User`].
    /// Format: `name:passwd:uid:gid:gecos:home:shell`.
    fn parse_line(line: &str) -> Option<User> {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let f: Vec<&str> = line.split(':').collect();
        if f.len() != 7 {
            return None;
        }
        let uid = f[2].parse().ok()?;
        let gid = f[3].parse().ok()?;
        let full_name = f[4]
            .split(',')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        Some(User {
            uid,
            gid,
            name: f[0].to_string(),
            full_name,
            home: PathBuf::from(f[5]),
            shell: PathBuf::from(f[6]),
        })
    }

    /// Apply the visibility policy to a candidate.
    fn is_visible(&self, u: &User) -> bool {
        if self.hidden.contains(&u.name) {
            return false;
        }
        if is_nologin_shell(&u.shell) {
            return false;
        }
        if u.name == "root" {
            return self.allow_root;
        }
        self.uid_range.contains(u.uid)
    }

    pub fn from_passwd_text(&self, text: &str) -> Vec<User> {
        let mut users: Vec<User> = text
            .lines()
            .filter_map(Self::parse_line)
            .filter(|u| self.is_visible(u))
            .collect();
        users.sort_by(|a, b| a.name.cmp(&b.name));
        users
    }
}

impl UserProvider for PasswdProvider {
    fn login_capable_users(&self) -> crate::Result<Vec<User>> {
        let text = std::fs::read_to_string(&self.passwd_path)
            .map_err(|e| crate::Error::io(&self.passwd_path, e))?;
        Ok(self.from_passwd_text(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin
bin:x:2:2:bin:/bin:/usr/sbin/nologin
bacak-greeter:x:991:991:Bacak Greeter:/var/lib/bacak-greeter:/usr/sbin/nologin
ayse:x:1000:1000:Ayşe Yılmaz,,,:/home/ayse:/bin/bash
mehmet:x:1001:1001::/home/mehmet:/bin/zsh
locked:x:1002:1002:Locked User:/home/locked:/usr/sbin/nologin
nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin";

    fn provider() -> PasswdProvider {
        PasswdProvider {
            passwd_path: PathBuf::from("/dev/null"),
            uid_range: UidRange {
                min: 1000,
                max: 60000,
            },
            hidden: HashSet::new(),
            allow_root: false,
        }
    }

    #[test]
    fn only_real_humans_are_listed() {
        let users = provider().from_passwd_text(PASSWD);
        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, vec!["ayse", "mehmet"]);
    }

    #[test]
    fn gecos_full_name_is_extracted() {
        let users = provider().from_passwd_text(PASSWD);
        let ayse = users.iter().find(|u| u.name == "ayse").unwrap();
        assert_eq!(ayse.display_name(), "Ayşe Yılmaz");
        let mehmet = users.iter().find(|u| u.name == "mehmet").unwrap();
        // no GECOS -> falls back to username
        assert_eq!(mehmet.display_name(), "mehmet");
    }

    #[test]
    fn root_hidden_unless_allowed() {
        let mut p = provider();
        assert!(!p.from_passwd_text(PASSWD).iter().any(|u| u.name == "root"));
        p.allow_root = true;
        assert!(p.from_passwd_text(PASSWD).iter().any(|u| u.name == "root"));
    }

    #[test]
    fn hidden_list_drops_users() {
        let mut p = provider();
        p.hidden.insert("mehmet".into());
        let names: Vec<String> = p
            .from_passwd_text(PASSWD)
            .iter()
            .map(|u| u.name.clone())
            .collect();
        assert_eq!(names, vec!["ayse"]);
    }

    #[test]
    fn login_defs_parsing() {
        let r = UidRange::from_login_defs("UID_MIN\t500\nUID_MAX 65000\n# comment\n");
        assert_eq!(r.min, 500);
        assert_eq!(r.max, 65000);
    }
}
