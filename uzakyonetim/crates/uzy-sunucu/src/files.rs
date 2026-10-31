// SPDX-License-Identifier: GPL-3.0-or-later
//! Atomic file writes with explicit modes.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

fn write(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    // A unique temp name: two writers of the same file (a periodic and a
    // manual screenshot both updating `son.jpg`) must not trip over one.
    let suffix: String = random_bytes::<6>().iter().map(|b| format!("{b:02x}")).collect();
    let tmp = path.with_extension(format!("tmp.{suffix}"));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Err(e) = f.write_all(data).and_then(|_| f.sync_all()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

pub fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    write(path, data, 0o600)
}

pub fn write_public(path: &Path, data: &[u8]) -> Result<(), String> {
    write(path, data, 0o644)
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("OS CSPRNG");
    b
}

pub fn random_token() -> String {
    random_bytes::<32>().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}
