//! Bacak Onay "QR ile onay": while PAM asks for the Bacak Onay code, the
//! greeter also shows a QR. The phone app scans it, the user taps "Onayla",
//! and the app sends the current code back over the local network — the
//! greeter then answers the prompt with it exactly as if it had been typed.
//!
//! Nothing about verification changes: `pam_bacakonay` still checks the code
//! (and consumes its time step), so a code that arrives this way is worth no
//! more than one typed on the keyboard. What the QR adds is a one-time
//! AES-256-GCM key that exists only on this screen: only a phone that saw
//! the QR can get a code accepted here, and nobody on the network can read
//! the code in transit. The phone needs no fingerprint for this — the app is
//! already open (unlocked with the phone's screen lock) when it scans.
//!
//! The QR: `bacakonay://onay?v=1&h=<ip>&p=<port>&u=<user>&s=<host>&k=<key>`
//! (key: base64url, no padding). The phone connects to `h:p`, sends
//! `"BO1" ‖ iv(12) ‖ AES-GCM(key, iv, code, aad = "bacakonay-onay-v1:" ‖ user)`,
//! shuts down its write side and reads one line back: `ALINDI` or `RED`.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `PAM_TEXT_INFO` that `pam_bacakonay` sends right before its code prompt:
/// "the next prompt may be answered from the phone". Never shown.
pub const ISARET: &str = "bacakonay:qr";

const SURUM: &[u8] = b"BO1";
const AAD_ON: &[u8] = b"bacakonay-onay-v1:";
/// A login screen left alone stops listening after this long.
const SURE: Duration = Duration::from_secs(180);
const OKUMA_SURESI: Duration = Duration::from_secs(3);

/// One QR-approval window, alive while the code prompt is on screen. Dropping
/// it stops the listener.
pub struct Oturum {
    pub uri: String,
    pub kod: Receiver<String>,
    dur: Arc<AtomicBool>,
}

impl Drop for Oturum {
    fn drop(&mut self) {
        self.dur.store(true, Ordering::Relaxed);
    }
}

impl Oturum {
    /// `None` when there is no network to be reached on (the code can still
    /// be typed).
    pub fn baslat(kullanici: &str) -> Option<Oturum> {
        let ip = yerel_ip()?;
        let dinleyici = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        dinleyici.set_nonblocking(true).ok()?;
        let port = dinleyici.local_addr().ok()?.port();
        let mut anahtar = [0u8; 32];
        getrandom::fill(&mut anahtar).ok()?;
        let uri = format!(
            "bacakonay://onay?v=1&h={ip}&p={port}&u={}&s={}&k={}",
            kodla(kullanici),
            kodla(&makine_adi()),
            URL_SAFE_NO_PAD.encode(anahtar)
        );
        let (tx, kod) = channel();
        let dur = Arc::new(AtomicBool::new(false));
        let d = dur.clone();
        let mut aad = AAD_ON.to_vec();
        aad.extend_from_slice(kullanici.as_bytes());
        std::thread::spawn(move || dinle(dinleyici, anahtar, aad, d, tx));
        Some(Oturum { uri, kod, dur })
    }
}

fn dinle(dinleyici: TcpListener, mut anahtar: [u8; 32], aad: Vec<u8>, dur: Arc<AtomicBool>, tx: Sender<String>) {
    let son = Instant::now() + SURE;
    while !dur.load(Ordering::Relaxed) && Instant::now() < son {
        match dinleyici.accept() {
            Ok((mut s, _)) => {
                // Anyone on the network can connect; only a frame sealed with
                // the QR's key gets past `coz`, so a stranger just gets RED.
                let kod = s
                    .set_nonblocking(false)
                    .and_then(|_| s.set_read_timeout(Some(OKUMA_SURESI)))
                    .ok()
                    .and_then(|_| oku(&mut s))
                    .and_then(|cerceve| coz(&anahtar, &aad, &cerceve));
                match kod {
                    Some(kod) => {
                        let _ = s.write_all(b"ALINDI\n");
                        let _ = tx.send(kod);
                        break;
                    }
                    None => {
                        let _ = s.write_all(b"RED\n");
                    }
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    anahtar.fill(0);
}

/// The phone's frame: a few dozen bytes, ended by its write shutdown.
fn oku(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf = [0u8; 64];
    let mut n = 0;
    while n < buf.len() {
        match s.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return None,
        }
    }
    Some(buf[..n].to_vec())
}

/// Open a frame; `Some(code)` only for a genuine 6–8 digit code.
fn coz(anahtar: &[u8; 32], aad: &[u8], cerceve: &[u8]) -> Option<String> {
    let govde = cerceve.strip_prefix(SURUM)?;
    if govde.len() < 12 + 16 {
        return None;
    }
    let (iv, sifreli) = govde.split_at(12);
    let sifre = Aes256Gcm::new_from_slice(anahtar).ok()?;
    let acik = sifre.decrypt(Nonce::from_slice(iv), Payload { msg: sifreli, aad }).ok()?;
    let kod = String::from_utf8(acik).ok()?;
    (matches!(kod.len(), 6..=8) && kod.bytes().all(|b| b.is_ascii_digit())).then_some(kod)
}

/// The address the phone should dial: the one the default route leaves from.
/// (A UDP "connect" sends nothing; it only picks the route.)
fn yerel_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// The name `bacakonay kur` put in the enrollment label (`user@host`), so the
/// phone can pick the right account.
fn makine_adi() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Percent-encode a query value (RFC 3986 unreserved kept).
fn kodla(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The QR as an egui image: dark modules on white with the 4-module quiet
/// zone the spec asks for, `olcek` pixels per module.
pub fn qr_resmi(uri: &str, olcek: usize) -> Option<eframe::egui::ColorImage> {
    let qr = qrcode::QrCode::with_error_correction_level(uri.as_bytes(), qrcode::EcLevel::M).ok()?;
    let n = qr.width();
    let renkler = qr.to_colors();
    let kenar = (n + 8) * olcek;
    let mut piksel = vec![eframe::egui::Color32::WHITE; kenar * kenar];
    for (i, r) in renkler.iter().enumerate() {
        if *r != qrcode::Color::Dark {
            continue;
        }
        let (x0, y0) = ((i % n + 4) * olcek, (i / n + 4) * olcek);
        for y in y0..y0 + olcek {
            for x in x0..x0 + olcek {
                piksel[y * kenar + x] = eframe::egui::Color32::BLACK;
            }
        }
    }
    Some(eframe::egui::ColorImage { size: [kenar, kenar], pixels: piksel })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn muhurle(anahtar: &[u8; 32], aad: &[u8], kod: &str) -> Vec<u8> {
        let iv = [7u8; 12];
        let mut c = SURUM.to_vec();
        c.extend_from_slice(&iv);
        let sifre = Aes256Gcm::new_from_slice(anahtar).unwrap();
        c.extend(sifre.encrypt(Nonce::from_slice(&iv), Payload { msg: kod.as_bytes(), aad }).unwrap());
        c
    }

    #[test]
    fn only_a_frame_sealed_with_the_qr_key_for_this_user_opens() {
        let anahtar = [9u8; 32];
        let aad = b"bacakonay-onay-v1:ayse".to_vec();
        assert_eq!(coz(&anahtar, &aad, &muhurle(&anahtar, &aad, "123456")).as_deref(), Some("123456"));
        // Another key, another user, a tampered byte, junk, a non-code.
        assert!(coz(&[1u8; 32], &aad, &muhurle(&anahtar, &aad, "123456")).is_none());
        assert!(coz(&anahtar, b"bacakonay-onay-v1:ali", &muhurle(&anahtar, &aad, "123456")).is_none());
        let mut bozuk = muhurle(&anahtar, &aad, "123456");
        *bozuk.last_mut().unwrap() ^= 1;
        assert!(coz(&anahtar, &aad, &bozuk).is_none());
        assert!(coz(&anahtar, &aad, b"BO1 kisa").is_none());
        assert!(coz(&anahtar, &aad, &muhurle(&anahtar, &aad, "12ab56")).is_none());
    }

    /// The phone app's test (`LoginApprovalTest.kt`) seals to these bytes.
    #[test]
    fn opens_the_frame_the_phone_app_builds() {
        let cerceve = [
            &b"BO1"[..],
            &[7u8; 12],
            &[
                0x68, 0x89, 0xbe, 0xab, 0x5d, 0x47, 0x29, 0x40, 0x54, 0xeb, 0xcb, 0xbb, 0xfb, 0x12, 0x00, 0xe4,
                0xac, 0xd0, 0xcd, 0x46, 0xdb, 0xdb,
            ],
        ]
        .concat();
        assert_eq!(coz(&[9u8; 32], b"bacakonay-onay-v1:ayse", &cerceve).as_deref(), Some("123456"));
    }

    #[test]
    fn end_to_end_over_a_socket() {
        let o = Oturum::baslat("ayse");
        let Some(o) = o else { return }; // no network in this sandbox
        let q = |k: &str| o.uri.split(&format!("{k}=")).nth(1).unwrap().split('&').next().unwrap().to_string();
        let anahtar: [u8; 32] = URL_SAFE_NO_PAD.decode(q("k")).unwrap().try_into().unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", q("p").parse::<u16>().unwrap())).unwrap();
        s.write_all(&muhurle(&anahtar, b"bacakonay-onay-v1:ayse", "654321")).unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        let mut yanit = String::new();
        s.read_to_string(&mut yanit).unwrap();
        assert_eq!(yanit, "ALINDI\n");
        assert_eq!(o.kod.recv_timeout(Duration::from_secs(2)).unwrap(), "654321");
    }
}
