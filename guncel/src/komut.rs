//! Alt süreç çalıştırma ve durdurma sinyalleri.
//!
//! Hizmet `KillMode=process` ile çalışır: kapatmada SIGTERM yalnız bu sürece
//! gelir. İndirme gibi kesilebilir adımlarda alt süreç (kendi süreç grubunda)
//! durdurulur; kurulum adımında ise ([`Korumali`] etkinken) sinyal yalnız
//! not edilir ve dpkg işini bitirene kadar beklenir — systemd kapatmayı
//! `TimeoutStopSec` kadar erteler.

use std::fmt;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static DURDUR: AtomicBool = AtomicBool::new(false);
static KORUMA: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub enum Hata {
    /// Durdurma isteği geldi, adım yarıda bırakıldı.
    Kesildi,
    Io(io::Error),
    Mesaj(String),
}

impl fmt::Display for Hata {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Hata::Kesildi => write!(f, "durdurma isteğiyle kesildi"),
            Hata::Io(e) => write!(f, "{e}"),
            Hata::Mesaj(m) => f.write_str(m),
        }
    }
}

impl From<io::Error> for Hata {
    fn from(e: io::Error) -> Self {
        Hata::Io(e)
    }
}

pub type Sonuc<T> = Result<T, Hata>;

pub fn hata<T>(m: impl Into<String>) -> Sonuc<T> {
    Err(Hata::Mesaj(m.into()))
}

extern "C" fn sinyal_geldi(_: libc::c_int) {
    DURDUR.store(true, Ordering::SeqCst);
}

pub fn sinyalleri_kur() {
    for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: işleyici yalnız atomik bir bayrak yazar (async-signal-safe).
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = sinyal_geldi as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(s, &sa, std::ptr::null_mut());
        }
    }
}

pub fn durduruldu() -> bool {
    DURDUR.load(Ordering::SeqCst)
}

/// Yaşadığı sürece durdurma sinyalleri çalışan alt süreci kesmez.
pub struct Korumali(());

impl Korumali {
    pub fn al() -> Self {
        KORUMA.fetch_add(1, Ordering::SeqCst);
        Korumali(())
    }
}

impl Drop for Korumali {
    fn drop(&mut self) {
        KORUMA.fetch_sub(1, Ordering::SeqCst);
    }
}

fn korumada() -> bool {
    KORUMA.load(Ordering::SeqCst) > 0
}

pub struct Cikti {
    pub kod: i32,
    pub stdout: String,
}

impl Cikti {
    pub fn basarili(&self) -> bool {
        self.kod == 0
    }
}

/// Komutu çalıştırır; `yakala` ise stdout döndürülür, değilse günlüğe akar.
/// stderr her zaman günlüğe akar.
pub fn calistir(cmd: &mut Command, yakala: bool) -> Sonuc<Cikti> {
    if durduruldu() && !korumada() {
        return Err(Hata::Kesildi);
    }
    cmd.process_group(0).stdin(Stdio::null());
    if yakala {
        cmd.stdout(Stdio::piped());
    }
    let mut cocuk = cmd
        .spawn()
        .map_err(|e| Hata::Mesaj(format!("{:?} çalıştırılamadı: {e}", cmd.get_program())))?;
    let okuyucu = cocuk.stdout.take().map(|mut o| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = o.read_to_string(&mut s);
            s
        })
    });
    let pid = cocuk.id() as libc::pid_t;
    let mut kesme_zamani: Option<Instant> = None;
    let durum = loop {
        if let Some(d) = cocuk.try_wait()? {
            break d;
        }
        if durduruldu() && !korumada() {
            match kesme_zamani {
                None => {
                    // SAFETY: kendi başlattığımız süreç grubuna sinyal.
                    unsafe { libc::kill(-pid, libc::SIGTERM) };
                    kesme_zamani = Some(Instant::now());
                }
                Some(t) if t.elapsed() > Duration::from_secs(15) => unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                },
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let stdout = okuyucu.and_then(|t| t.join().ok()).unwrap_or_default();
    if kesme_zamani.is_some() {
        return Err(Hata::Kesildi);
    }
    Ok(Cikti {
        kod: durum.code().unwrap_or(-1),
        stdout,
    })
}

/// Komut başarısızsa hata döndüren kısa yol.
pub fn calistir_basarili(cmd: &mut Command, yakala: bool) -> Sonuc<Cikti> {
    let ad = format!("{:?}", cmd.get_program());
    let c = calistir(cmd, yakala)?;
    if !c.basarili() {
        return hata(format!("{ad} {} koduyla bitti", c.kod));
    }
    Ok(c)
}

pub fn var_mi(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}
