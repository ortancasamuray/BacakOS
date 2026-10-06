//! Persistent pairings for PIN-less resumption (`resume.rs`).
//!
//! `$XDG_STATE_HOME/uzakel/eslesmeler` (default `~/.local/state/uzakel/`),
//! directory 0700, file 0600, one line per paired phone:
//!
//! ```text
//! <client_id hex> <resume_key hex> <paired_at unix> <last_used unix>
//! ```
//!
//! Pairings unused for [`MAX_IDLE_SECS`] (30 days) are pruned — a phone
//! that was lost, reset or simply stopped being used drops off on its own.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::resume::{CLIENT_ID_LEN, NONCE_LEN};

pub const MAX_IDLE_SECS: u64 = 30 * 24 * 60 * 60;
/// Remember this many recent client nonces per pairing to refuse replays.
const RECENT_NONCES: usize = 64;

type ClientId = [u8; CLIENT_ID_LEN];

struct Pairing {
    resume_key: [u8; 32],
    paired_at: u64,
    last_used: u64,
}

struct Inner {
    path: Option<PathBuf>,
    map: HashMap<ClientId, Pairing>,
    recent: HashMap<ClientId, VecDeque<[u8; NONCE_LEN]>>,
}

#[derive(Clone)]
pub struct Pairings(Arc<Mutex<Inner>>);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into())).join(".local/state")
    });
    base.join("uzakel/eslesmeler")
}

impl Pairings {
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut map = HashMap::new();
        if let Some(p) = &path {
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    let f: Vec<&str> = line.split_whitespace().collect();
                    if f.len() != 4 {
                        continue;
                    }
                    let (Some(cid), Some(rk), Ok(pa), Ok(lu)) =
                        (unhex::<CLIENT_ID_LEN>(f[0]), unhex::<32>(f[1]), f[2].parse(), f[3].parse())
                    else {
                        continue;
                    };
                    map.insert(cid, Pairing { resume_key: rk, paired_at: pa, last_used: lu });
                }
            }
        }
        let me = Pairings(Arc::new(Mutex::new(Inner { path, map, recent: HashMap::new() })));
        let pruned = me.prune();
        info!(count = me.0.lock().unwrap().map.len(), pruned, "eşleşmeler yüklendi");
        me
    }

    pub fn add(&self, cid: ClientId, resume_key: [u8; 32]) {
        let mut g = self.0.lock().unwrap();
        let t = now();
        g.map.insert(cid, Pairing { resume_key, paired_at: t, last_used: t });
        save(&g);
    }

    pub fn resume_key(&self, cid: &ClientId) -> Option<[u8; 32]> {
        self.0.lock().unwrap().map.get(cid).map(|p| p.resume_key)
    }

    /// `false` if this nonce was already used for this pairing (replay).
    pub fn fresh_nonce(&self, cid: &ClientId, nonce: &[u8; NONCE_LEN]) -> bool {
        let mut g = self.0.lock().unwrap();
        let q = g.recent.entry(*cid).or_default();
        if q.contains(nonce) {
            return false;
        }
        q.push_back(*nonce);
        if q.len() > RECENT_NONCES {
            q.pop_front();
        }
        true
    }

    pub fn touch(&self, cid: &ClientId) {
        let mut g = self.0.lock().unwrap();
        let t = now();
        let changed = match g.map.get_mut(cid) {
            // Only persist once an hour of drift; resumes can be frequent.
            Some(p) if t.saturating_sub(p.last_used) > 3600 => {
                p.last_used = t;
                true
            }
            Some(p) => {
                p.last_used = t;
                false
            }
            None => false,
        };
        if changed {
            save(&g);
        }
    }

    /// Drop pairings idle longer than [`MAX_IDLE_SECS`]. Returns how many.
    pub fn prune(&self) -> usize {
        let mut g = self.0.lock().unwrap();
        let cutoff = now().saturating_sub(MAX_IDLE_SECS);
        let before = g.map.len();
        g.map.retain(|_, p| p.last_used >= cutoff);
        let n = before - g.map.len();
        if n > 0 {
            let dead: Vec<ClientId> = g.recent.keys().filter(|k| !g.map.contains_key(*k)).copied().collect();
            for k in dead {
                g.recent.remove(&k);
            }
            save(&g);
        }
        n
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap().map.len()
    }

    #[cfg(test)]
    fn backdate(&self, cid: &ClientId, secs: u64) {
        let mut g = self.0.lock().unwrap();
        if let Some(p) = g.map.get_mut(cid) {
            p.last_used = p.last_used.saturating_sub(secs);
        }
    }
}

fn save(g: &Inner) {
    let Some(path) = &g.path else { return };
    if let Err(err) = write_file(path, g) {
        warn!(?err, path = %path.display(), "eşleşmeler kaydedilemedi");
    }
}

fn write_file(path: &Path, g: &Inner) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    for (cid, p) in &g.map {
        writeln!(f, "{} {} {} {}", hex(cid), hex(&p.resume_key), p.paired_at, p.last_used)?;
    }
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        // Tests run in parallel: a per-call counter keeps their dirs apart.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("uzakel-pairings-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("eslesmeler")
    }

    #[test]
    fn persists_and_reloads_with_private_permissions() {
        let p = tmp();
        let a = Pairings::load(Some(p.clone()));
        a.add([1; 16], [2; 32]);
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let b = Pairings::load(Some(p.clone()));
        assert_eq!(b.resume_key(&[1; 16]), Some([2; 32]));
        assert_eq!(b.resume_key(&[9; 16]), None);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn idle_pairings_are_pruned() {
        let p = tmp();
        let a = Pairings::load(Some(p.clone()));
        a.add([1; 16], [2; 32]);
        a.add([3; 16], [4; 32]);
        a.backdate(&[1; 16], MAX_IDLE_SECS + 10);
        assert_eq!(a.prune(), 1);
        assert_eq!(a.len(), 1);
        assert_eq!(Pairings::load(Some(p.clone())).len(), 1, "budama diske de yazılmalı");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn replayed_nonce_is_refused() {
        let a = Pairings::load(None);
        assert!(a.fresh_nonce(&[1; 16], &[5; 32]));
        assert!(!a.fresh_nonce(&[1; 16], &[5; 32]));
        assert!(a.fresh_nonce(&[1; 16], &[6; 32]));
    }
}
