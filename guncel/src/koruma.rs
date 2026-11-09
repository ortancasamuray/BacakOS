//! Kurulumdan önceki ön koşullar ve kurulum sırasındaki korumalar.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Pil durumu yeterli mi? Prize takılı bir güç kaynağı varsa ya da hiç pil
/// yoksa evet; yalnız pille çalışıyorsa en düşük pil doluluğu >= `en_az`.
pub fn guc_yeterli(en_az: u8) -> bool {
    guc_yeterli_dizinde(Path::new("/sys/class/power_supply"), en_az)
}

fn oku(yol: &Path) -> Option<String> {
    fs::read_to_string(yol).ok().map(|s| s.trim().to_string())
}

pub fn guc_yeterli_dizinde(dizin: &Path, en_az: u8) -> bool {
    let Ok(girdiler) = fs::read_dir(dizin) else {
        return true;
    };
    let mut en_dusuk_pil: Option<u8> = None;
    for g in girdiler.flatten() {
        let p = g.path();
        match oku(&p.join("type")).as_deref() {
            Some("Mains") | Some("USB") => {
                if oku(&p.join("online")).as_deref() == Some("1") {
                    return true;
                }
            }
            Some("Battery") => {
                // Fare/klavye pilleri (scope=Device) sistemi beslemez.
                if oku(&p.join("scope")).as_deref() == Some("Device") {
                    continue;
                }
                if let Some(c) = oku(&p.join("capacity")).and_then(|s| s.parse::<u8>().ok()) {
                    en_dusuk_pil = Some(en_dusuk_pil.map_or(c, |e| e.min(c)));
                }
            }
            _ => {}
        }
    }
    en_dusuk_pil.map_or(true, |c| c >= en_az)
}

/// Yolun bulunduğu dosya sisteminde kök olmayanlara da açık boş alan (bayt).
pub fn bos_alan(yol: &str) -> io::Result<u64> {
    let c = CString::new(yol).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: statvfs, geçerli C dizgisi ve sıfırlanmış yapı ile.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(s.f_bavail as u64 * s.f_frsize as u64)
    }
}

const POLICY_RC_D: &str = "/usr/sbin/policy-rc.d";
const POLICY_IMZA: &str = "# bacak-guncel:";

/// Kurulum sürerken invoke-rc.d / deb-systemd-invoke hizmet başlatma ve
/// yeniden başlatmalarını reddeder (çıkış 101): açık oturumun giriş ekranı,
/// birleştirici vb. hizmetleri kurulum ortasında kapanmaz; yeni sürümler
/// sonraki açılışta çalışır. Yalnız kendi yazdığımız dosyayı siler; başka
/// bir policy-rc.d varsa dokunulmaz.
pub struct HizmetDondurucu {
    bizim: bool,
}

impl HizmetDondurucu {
    pub fn kur() -> Self {
        if Path::new(POLICY_RC_D).exists() {
            if !bizim_mi() {
                crate::uyari!("{POLICY_RC_D} zaten var (bizim değil), olduğu gibi bırakıldı");
                return HizmetDondurucu { bizim: false };
            }
        }
        let icerik = format!(
            "#!/bin/sh\n{POLICY_IMZA} güncelleme kurulurken hizmetler yeniden başlatılmaz.\n\
             # Güncelleme bitince silinir; kalmışsa bacak-guncel onar siler.\nexit 101\n"
        );
        let yaz = || -> io::Result<()> {
            crate::durum::atomik_yaz(Path::new(POLICY_RC_D), icerik.as_bytes())?;
            fs::set_permissions(POLICY_RC_D, fs::Permissions::from_mode(0o755))
        };
        match yaz() {
            Ok(()) => HizmetDondurucu { bizim: true },
            Err(e) => {
                crate::uyari!("{POLICY_RC_D} yazılamadı: {e}");
                HizmetDondurucu { bizim: false }
            }
        }
    }
}

impl Drop for HizmetDondurucu {
    fn drop(&mut self) {
        if self.bizim {
            eski_dondurucuyu_kaldir();
        }
    }
}

fn bizim_mi() -> bool {
    fs::read_to_string(POLICY_RC_D)
        .map(|s| s.contains(POLICY_IMZA))
        .unwrap_or(false)
}

/// Yarıda kalmış bir kurulumdan kalan policy-rc.d'yi kaldırır.
pub fn eski_dondurucuyu_kaldir() {
    if bizim_mi() {
        match fs::remove_file(POLICY_RC_D) {
            Ok(()) => {}
            Err(e) => crate::uyari!("{POLICY_RC_D} silinemedi: {e}"),
        }
    }
}

/// Güncelleme sonrası yeniden başlatma gerektiğini Debian düzeninde bildirir.
pub fn yeniden_baslatma_gerekli(paketler: &[String]) {
    let _ = fs::write("/run/reboot-required", "*** BacakOS güncellendi: yeniden başlatın ***\n");
    let mut liste = fs::read_to_string("/run/reboot-required.pkgs").unwrap_or_default();
    for p in paketler {
        let ad = p.split_whitespace().next().unwrap_or(p);
        if !liste.lines().any(|l| l == ad) {
            liste.push_str(ad);
            liste.push('\n');
        }
    }
    let _ = fs::write("/run/reboot-required.pkgs", liste);
}

#[cfg(test)]
mod testler {
    use super::*;

    fn kaynak(d: &Path, ad: &str, alanlar: &[(&str, &str)]) {
        let p = d.join(ad);
        fs::create_dir_all(&p).unwrap();
        for (k, v) in alanlar {
            fs::write(p.join(k), format!("{v}\n")).unwrap();
        }
    }

    #[test]
    fn pil_kurallari() {
        let d = std::env::temp_dir().join(format!("bacak-guncel-guc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        // Pil yok → yeterli.
        assert!(guc_yeterli_dizinde(&d, 40));
        kaynak(&d, "BAT0", &[("type", "Battery"), ("capacity", "25")]);
        kaynak(&d, "AC", &[("type", "Mains"), ("online", "0")]);
        assert!(!guc_yeterli_dizinde(&d, 40));
        assert!(guc_yeterli_dizinde(&d, 20));
        // Fare pili sayılmaz, prize takılınca yeterli.
        kaynak(&d, "hid-fare", &[("type", "Battery"), ("scope", "Device"), ("capacity", "5")]);
        assert!(guc_yeterli_dizinde(&d, 20));
        fs::write(d.join("AC/online"), "1\n").unwrap();
        assert!(guc_yeterli_dizinde(&d, 90));
        fs::remove_dir_all(d).unwrap();
    }
}
