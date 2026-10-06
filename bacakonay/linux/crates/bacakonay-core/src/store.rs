// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user enrollment store: one small text file per user under
//! [`DEFAULT_DIR`].
//!
//! ```text
//! # bacakonay v1
//! secret=<base32>
//! algorithm=SHA1
//! digits=6
//! period=30
//! last_step=56666666
//! ```
//!
//! Security properties (checked on every read, enforced on every write):
//! * the directory and files are owned by the **effective uid** of the
//!   process (root for the PAM module / CLI) and carry no group/other bits —
//!   users cannot read their own secret back, nor swap in one they know;
//! * files are opened with `O_NOFOLLOW` and must be regular files;
//! * writes go to a temp file + `fsync` + `rename`, so a crash never leaves a
//!   half-written secret;
//! * verify-and-consume runs under an exclusive `flock`, so two concurrent
//!   logins can't both spend the same code.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::{base32, Algorithm, OtpParams};

/// Where enrollments live in production.
pub const DEFAULT_DIR: &str = "/var/lib/bacakonay";

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    /// Ownership/permissions are not what we require — refuse rather than
    /// trust a file someone else could have written.
    Insecure(String),
    Corrupt(String),
    InvalidUser,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "G/Ç hatası: {e}"),
            StoreError::Insecure(m) => write!(f, "güvensiz kayıt: {m}"),
            StoreError::Corrupt(m) => write!(f, "bozuk kayıt: {m}"),
            StoreError::InvalidUser => f.write_str("geçersiz kullanıcı adı"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

/// One user's TOTP enrollment.
pub struct Enrollment {
    pub secret: Zeroizing<Vec<u8>>,
    pub params: OtpParams,
    /// Last time step successfully used to log in (replay protection).
    pub last_step: Option<u64>,
}

/// Usernames are used as file names: allow only the portable POSIX set and
/// reject anything that could walk the path (`/`, `..`, leading `.`/`-`).
pub fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['.', '-'])
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    pub fn system() -> Self {
        Store::new(DEFAULT_DIR)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self, user: &str) -> Result<PathBuf, StoreError> {
        if !valid_username(user) {
            return Err(StoreError::InvalidUser);
        }
        Ok(self.dir.join(user))
    }

    fn check_secure(meta: &fs::Metadata, what: &Path) -> Result<(), StoreError> {
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return Err(StoreError::Insecure(format!(
                "{} sahibi uid {} (beklenen {euid})",
                what.display(),
                meta.uid()
            )));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(StoreError::Insecure(format!(
                "{} izinleri {:o} (grup/diğerleri erişememeli)",
                what.display(),
                meta.mode() & 0o777
            )));
        }
        Ok(())
    }

    /// The store directory must exist and be private, else nothing in it is
    /// trusted. `Ok(false)` when it simply doesn't exist yet.
    fn check_dir(&self) -> Result<bool, StoreError> {
        match fs::symlink_metadata(&self.dir) {
            Ok(m) if m.is_dir() => Self::check_secure(&m, &self.dir).map(|_| true),
            Ok(_) => Err(StoreError::Insecure(format!("{} dizin değil", self.dir.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn ensure_dir(&self) -> Result<(), StoreError> {
        if !self.check_dir()? {
            fs::create_dir_all(&self.dir)?;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
            self.check_dir()?;
        }
        Ok(())
    }

    /// Whether `user` has an enrollment (does not validate its contents).
    pub fn is_enrolled(&self, user: &str) -> Result<bool, StoreError> {
        if !self.check_dir()? {
            return Ok(false);
        }
        Ok(fs::symlink_metadata(self.file(user)?).is_ok())
    }

    /// Load `user`'s enrollment; `Ok(None)` when not enrolled.
    pub fn load(&self, user: &str) -> Result<Option<Enrollment>, StoreError> {
        let path = self.file(user)?;
        if !self.check_dir()? {
            return Ok(None);
        }
        let f = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(StoreError::Insecure(format!("{} sembolik bağlantı", path.display())))
            }
            Err(e) => return Err(e.into()),
        };
        let meta = f.metadata()?;
        if !meta.is_file() {
            return Err(StoreError::Insecure(format!("{} normal dosya değil", path.display())));
        }
        Self::check_secure(&meta, &path)?;
        let mut text = Zeroizing::new(String::new());
        f.take(4096).read_to_string(&mut text)?;
        parse(&text).map(Some)
    }

    /// Atomically write `user`'s enrollment (0600, owned by the euid).
    pub fn save(&self, user: &str, e: &Enrollment) -> Result<(), StoreError> {
        let path = self.file(user)?;
        self.ensure_dir()?;
        let tmp = self.dir.join(format!(".{user}.tmp"));
        let _ = fs::remove_file(&tmp);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        let body = serialize(e);
        let res = f.write_all(body.as_bytes()).and_then(|_| f.sync_all());
        drop(f);
        if let Err(err) = res {
            let _ = fs::remove_file(&tmp);
            return Err(err.into());
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Delete `user`'s enrollment. `Ok(false)` if there was none.
    pub fn remove(&self, user: &str) -> Result<bool, StoreError> {
        let path = self.file(user)?;
        if !self.check_dir()? {
            return Ok(false);
        }
        let _ = fs::remove_file(self.dir.join(format!(".{user}.lock")));
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Run `f` while holding an exclusive lock for `user`, so a concurrent
    /// login can't verify the same code between our load and save.
    pub fn with_lock<T>(
        &self,
        user: &str,
        f: impl FnOnce(&Store) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.file(user)?;
        if !self.check_dir()? {
            // Nothing enrolled, nothing to race on.
            return f(self);
        }
        let lock_path = self.dir.join(format!(".{user}.lock"));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock_path)?;
        Self::check_secure(&lock.metadata()?, &lock_path)?;
        flock(&lock, libc::LOCK_EX)?;
        let out = f(self);
        let _ = flock(&lock, libc::LOCK_UN);
        out
    }
}

fn flock(f: &File, op: libc::c_int) -> io::Result<()> {
    loop {
        if unsafe { libc::flock(f.as_raw_fd(), op) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn serialize(e: &Enrollment) -> Zeroizing<String> {
    let mut s = Zeroizing::new(String::from("# bacakonay v1\n"));
    s.push_str("secret=");
    s.push_str(&base32::encode(&e.secret));
    s.push('\n');
    s.push_str(&format!(
        "algorithm={}\ndigits={}\nperiod={}\n",
        e.params.algorithm.as_str(),
        e.params.digits,
        e.params.period
    ));
    if let Some(step) = e.last_step {
        s.push_str(&format!("last_step={step}\n"));
    }
    s
}

fn parse(text: &str) -> Result<Enrollment, StoreError> {
    let bad = |m: &str| StoreError::Corrupt(m.to_string());
    let mut secret = None;
    let mut params = OtpParams::default();
    let mut last_step = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.split_once('=').ok_or_else(|| bad("anahtar=değer bekleniyordu"))?;
        match k.trim() {
            "secret" => {
                secret = Some(Zeroizing::new(
                    base32::decode(v.trim()).ok_or_else(|| bad("secret base32 değil"))?,
                ))
            }
            "algorithm" => {
                params.algorithm =
                    Algorithm::parse(v).ok_or_else(|| bad("bilinmeyen algoritma"))?
            }
            "digits" => params.digits = v.trim().parse().map_err(|_| bad("digits"))?,
            "period" => params.period = v.trim().parse().map_err(|_| bad("period"))?,
            "last_step" => last_step = Some(v.trim().parse().map_err(|_| bad("last_step"))?),
            _ => {} // forward-compatible: ignore unknown keys
        }
    }
    let secret = secret.ok_or_else(|| bad("secret yok"))?;
    if secret.len() < 10 {
        return Err(bad("secret 80 bitten kısa"));
    }
    if !params.is_valid() {
        return Err(bad("digits 6/8, period 15–300 olmalı"));
    }
    Ok(Enrollment { secret, params, last_step })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("bacakonay-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    fn sample() -> Enrollment {
        Enrollment {
            secret: Zeroizing::new(b"12345678901234567890".to_vec()),
            params: OtpParams { algorithm: Algorithm::Sha256, digits: 8, period: 30 },
            last_step: Some(42),
        }
    }

    #[test]
    fn roundtrip_and_permissions() {
        let s = tmp_store("rt");
        assert!(s.load("ayse").unwrap().is_none());
        s.save("ayse", &sample()).unwrap();
        let m = fs::metadata(s.dir().join("ayse")).unwrap();
        assert_eq!(m.mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(s.dir()).unwrap().mode() & 0o777, 0o700);
        let e = s.load("ayse").unwrap().unwrap();
        assert_eq!(&*e.secret, b"12345678901234567890");
        assert_eq!(e.params.algorithm, Algorithm::Sha256);
        assert_eq!(e.params.digits, 8);
        assert_eq!(e.last_step, Some(42));
        assert!(s.remove("ayse").unwrap());
        assert!(!s.remove("ayse").unwrap());
        let _ = fs::remove_dir_all(s.dir());
    }

    #[test]
    fn rejects_world_readable_file_and_symlink() {
        let s = tmp_store("perm");
        s.save("ali", &sample()).unwrap();
        let p = s.dir().join("ali");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(s.load("ali"), Err(StoreError::Insecure(_))));
        fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", &p).unwrap();
        assert!(matches!(s.load("ali"), Err(StoreError::Insecure(_))));
        let _ = fs::remove_dir_all(s.dir());
    }

    #[test]
    fn rejects_open_directory() {
        let s = tmp_store("dir");
        s.save("veli", &sample()).unwrap();
        fs::set_permissions(s.dir(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(s.load("veli"), Err(StoreError::Insecure(_))));
        fs::set_permissions(s.dir(), fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(s.dir());
    }

    #[test]
    fn username_validation() {
        for ok in ["os2", "ayse.yilmaz", "user_1", "a-b"] {
            assert!(valid_username(ok), "{ok}");
        }
        for bad in ["", "../root", "a/b", ".hidden", "-x", "ali veli", "ş"] {
            assert!(!valid_username(bad), "{bad}");
        }
        assert!(matches!(tmp_store("u").load("../x"), Err(StoreError::InvalidUser)));
    }

    #[test]
    fn corrupt_files_are_rejected() {
        assert!(parse("secret=!!!\n").is_err());
        assert!(parse("digits=6\n").is_err());
        assert!(parse("secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\ndigits=7\n").is_err());
        assert!(parse("secret=GEZDGNBV\n").is_err());
    }
}
