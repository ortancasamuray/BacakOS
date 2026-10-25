// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Name patterns to leave out when creating an archive (build output, VM
//! images, …). Patterns match a single file or directory name, not a path:
//! `target` skips every directory called `target`, `*.img` every image file.
//! `*` matches any run of characters, `?` exactly one.

use std::ffi::OsStr;

/// Build output and caches that are rebuilt from source and rarely belong in
/// an archive.
pub const BUILD_OUTPUTS: &[&str] = &["target", "node_modules", "__pycache__", ".gradle", ".cache"];

#[derive(Debug, Clone, Default)]
pub struct Exclude {
    patterns: Vec<String>,
}

impl Exclude {
    /// Parse a comma-separated pattern list (blank entries ignored), adding
    /// [`BUILD_OUTPUTS`] when `build_outputs` is set.
    pub fn new(list: &str, build_outputs: bool) -> Exclude {
        let mut patterns: Vec<String> =
            list.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect();
        if build_outputs {
            patterns.extend(BUILD_OUTPUTS.iter().map(|p| p.to_string()));
        }
        Exclude { patterns }
    }

    /// Whether an entry with this file name should be left out.
    pub fn matches(&self, name: &OsStr) -> bool {
        let name = name.to_string_lossy();
        self.patterns.iter().any(|p| wildcard(p.as_bytes(), name.as_bytes()))
    }
}

/// `*` / `?` wildcard match over bytes (iterative, backtracks only to the last
/// `*`, so it is linear for the patterns people type).
fn wildcard(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some((p, i));
            p += 1;
        } else if let Some((sp, si)) = star {
            p = sp + 1;
            i = si + 1;
            star = Some((sp, si + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(list: &str, name: &str) -> bool {
        Exclude::new(list, false).matches(OsStr::new(name))
    }

    #[test]
    fn exact_and_wildcards() {
        assert!(m("target", "target"));
        assert!(!m("target", "targets"));
        assert!(m("*.img", "bacakos-test.img"));
        assert!(!m("*.img", "image.png"));
        assert!(m("f?o", "foo"));
        assert!(m("*test*", "bacakos-test-uefi.img"));
        assert!(m("a*b*c", "aXXbYYc"));
        assert!(!m("a*b*c", "aXXbYY"));
    }

    #[test]
    fn list_parsing() {
        let e = Exclude::new(" *.img , .git,, ", false);
        assert!(e.matches(OsStr::new("disk.img")));
        assert!(e.matches(OsStr::new(".git")));
        assert!(!e.matches(OsStr::new("src")));
        assert!(!Exclude::new(" , ", false).matches(OsStr::new("")));
    }

    #[test]
    fn build_outputs() {
        let e = Exclude::new("", true);
        assert!(e.matches(OsStr::new("target")));
        assert!(e.matches(OsStr::new("node_modules")));
        assert!(!e.matches(OsStr::new("src")));
        assert!(!Exclude::new("", false).matches(OsStr::new("target")));
    }
}
