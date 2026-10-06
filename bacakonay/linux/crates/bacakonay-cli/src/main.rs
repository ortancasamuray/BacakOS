// SPDX-License-Identifier: GPL-3.0-or-later
//! `bacakonay` — manage TOTP enrollments for BacakOS logins.
//!
//! ```text
//! sudo bacakonay kur [KULLANICI] [--algoritma SHA1|SHA256|SHA512] [--hane 6|8] [--zorla]
//! sudo bacakonay durum [KULLANICI]
//! sudo bacakonay dogrula [KULLANICI] [KOD]
//! sudo bacakonay kaldir [KULLANICI]
//! ```
//!
//! Every command needs root: enrollments live in the root-only
//! `/var/lib/bacakonay` so a user can neither read their secret back nor
//! replace it with one an attacker knows. `KULLANICI` defaults to the user
//! who ran `sudo` (`$SUDO_USER`).

use std::ffi::{CStr, CString};
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use bacakonay_core::store::{valid_username, Enrollment, Store};
use bacakonay_core::{otp, uri, Algorithm, OtpParams};
use qrcode::{render::unicode, QrCode};
use zeroize::Zeroizing;

const USAGE: &str = "\
Kullanım:
  bacakonay kur [KULLANICI] [--algoritma SHA1|SHA256|SHA512] [--hane 6|8] [--zorla]
      Kullanıcı için yeni gizli anahtar üretir, eşleme QR kodunu gösterir.
  bacakonay durum [KULLANICI]     Kayıt durumunu gösterir.
  bacakonay dogrula [KULLANICI] [KOD]
      Telefondaki kodu dener (kodu tüketmez; giriş yine çalışır).
  bacakonay kaldir [KULLANICI]    İki adımlı doğrulamayı kapatır.

Tüm komutlar root ister (sudo). KULLANICI verilmezse sudo'yu çalıştıran kullanıcı.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if matches!(cmd, "-h" | "--help" | "yardim") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("bacakonay: bu komut root yetkisi ister — `sudo bacakonay {cmd} …` deneyin.");
        return ExitCode::from(1);
    }
    let rest = &args[1..];
    let result = match cmd {
        "kur" => cmd_enroll(rest),
        "durum" => cmd_status(rest),
        "dogrula" | "doğrula" => cmd_verify(rest),
        "kaldir" | "kaldır" => cmd_remove(rest),
        _ => Err(format!("bilinmeyen komut: {cmd}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bacakonay: {e}");
            ExitCode::from(1)
        }
    }
}

/// Positional user (first non-flag arg) or `$SUDO_USER`; must exist in passwd.
fn target_user(positional: Option<&String>) -> Result<String, String> {
    let user = positional
        .cloned()
        .or_else(|| std::env::var("SUDO_USER").ok())
        .ok_or("kullanıcı belirtin (ör. `sudo bacakonay kur ayse`)")?;
    if !valid_username(&user) {
        return Err(format!("geçersiz kullanıcı adı: {user:?}"));
    }
    let c = CString::new(user.clone()).map_err(|_| "geçersiz kullanıcı adı")?;
    if unsafe { libc::getpwnam(c.as_ptr()) }.is_null() {
        return Err(format!("sistemde böyle bir kullanıcı yok: {user}"));
    }
    Ok(user)
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        if let Ok(s) = CStr::from_bytes_until_nul(&buf) {
            return s.to_string_lossy().into_owned();
        }
    }
    "bacakos".into()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn positionals(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if matches!(a.as_str(), "--algoritma" | "--hane") {
            skip = true;
        } else if !a.starts_with("--") {
            out.push(a);
        }
    }
    out
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn cmd_enroll(args: &[String]) -> Result<(), String> {
    let user = target_user(positionals(args).first().copied())?;
    let mut params = OtpParams::default();
    if let Some(a) = flag_value(args, "--algoritma") {
        params.algorithm = Algorithm::parse(a).ok_or(format!("bilinmeyen algoritma: {a}"))?;
    }
    if let Some(d) = flag_value(args, "--hane") {
        params.digits = d.parse().map_err(|_| format!("geçersiz hane sayısı: {d}"))?;
    }
    if !params.is_valid() {
        return Err("hane sayısı 6 ya da 8 olmalı".into());
    }
    let store = Store::system();
    if store.is_enrolled(&user).map_err(|e| e.to_string())? && !args.iter().any(|a| a == "--zorla") {
        return Err(format!(
            "{user} zaten kayıtlı. Yeni anahtar eski telefon kaydını geçersiz kılar; \
             emin iseniz `--zorla` ekleyin."
        ));
    }

    let secret = Zeroizing::new(
        otp::generate_secret(params.algorithm.recommended_secret_len()).map_err(|e| e.to_string())?,
    );
    let host = hostname();
    let link = Zeroizing::new(uri::totp_uri(&user, &host, &secret, &params));

    println!("\n  Bacak Onay — {user}@{host} için iki adımlı doğrulama\n");
    let code = QrCode::new(link.as_bytes()).map_err(|e| e.to_string())?;
    let qr = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    println!("{qr}");
    println!("  1. Telefonda Bacak Onay'ı açın → + → QR kodunu tarayın.");
    println!("     (Kamera yoksa elle girin: {})", group4(&bacakonay_core::base32::encode(&secret)));
    println!(
        "     Algoritma {}, {} hane, {} sn.\n",
        params.algorithm.as_str(),
        params.digits,
        params.period
    );

    // Confirm the phone actually has it before turning 2FA on — otherwise a
    // mis-scan would lock the user out at the next login.
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err("kod onayı için etkileşimli terminal gerekli".into());
    }
    for attempt in 1..=3 {
        print!("  2. Uygulamada görünen kodu yazın (iptal: boş Enter): ");
        io::stdout().flush().ok();
        let mut line = Zeroizing::new(String::new());
        stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            return Err("iptal edildi; hiçbir şey değiştirilmedi".into());
        }
        if otp::verify_totp(&secret, &params, &line, now(), 1, None).is_some() {
            let e = Enrollment { secret: secret.clone(), params, last_step: None };
            store.save(&user, &e).map_err(|e| e.to_string())?;
            println!(
                "\n  ✓ Etkin. {user} artık giriş ekranında paroladan sonra Bacak Onay kodu girecek."
            );
            return Ok(());
        }
        println!("  ✗ Kod tutmadı ({attempt}/3). Telefonun saatinin otomatik olduğundan emin olun.");
    }
    Err("3 hatalı deneme; hiçbir şey değiştirilmedi".into())
}

fn group4(s: &str) -> String {
    s.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

fn cmd_status(args: &[String]) -> Result<(), String> {
    let user = target_user(positionals(args).first().copied())?;
    match Store::system().load(&user).map_err(|e| e.to_string())? {
        None => println!("{user}: iki adımlı doğrulama KAPALI"),
        Some(e) => println!(
            "{user}: iki adımlı doğrulama AÇIK ({}, {} hane, {} sn)",
            e.params.algorithm.as_str(),
            e.params.digits,
            e.params.period
        ),
    }
    Ok(())
}

fn cmd_verify(args: &[String]) -> Result<(), String> {
    let pos = positionals(args);
    let user = target_user(pos.first().copied())?;
    let e = Store::system()
        .load(&user)
        .map_err(|e| e.to_string())?
        .ok_or(format!("{user} kayıtlı değil"))?;
    let code = match pos.get(1) {
        Some(c) => Zeroizing::new(c.to_string()),
        None => {
            print!("Kod: ");
            io::stdout().flush().ok();
            let mut line = Zeroizing::new(String::new());
            io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
            line
        }
    };
    // Replay state is ignored here on purpose: this is a dry run that must
    // not consume a code the user may want to log in with.
    if otp::verify_totp(&e.secret, &e.params, &code, now(), 1, None).is_some() {
        println!("✓ Kod geçerli.");
        Ok(())
    } else {
        Err("kod geçersiz (saat kayması ya da yanlış kayıt?)".into())
    }
}

fn cmd_remove(args: &[String]) -> Result<(), String> {
    let user = target_user(positionals(args).first().copied())?;
    if Store::system().remove(&user).map_err(|e| e.to_string())? {
        println!("{user}: iki adımlı doğrulama kapatıldı. Telefondaki kaydı da silebilirsiniz.");
    } else {
        println!("{user} zaten kayıtlı değildi.");
    }
    Ok(())
}
