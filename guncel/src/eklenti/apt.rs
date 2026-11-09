//! Yerleşik apt eklentisi: Debian + BacakOS deposundaki paketler.
//!
//! Güvenlik kuralları:
//! - Yalnız `upgrade --with-new-pkgs`: hiçbir paket kaldırılmaz; kaldırma
//!   gerektiren güncellemeler apt tarafından geri tutulur.
//! - İndirme `--download-only`; ardından her .deb'in önbellekte boyutu ve
//!   özeti tek tek denetlenir. Kurulum `--no-download` ile yapılır: eksik bir
//!   dosya varsa apt dpkg'ye hiç geçmeden durur.
//! - Kurulumda policy-rc.d hizmet yeniden başlatmalarını engeller,
//!   systemd-inhibit kapatma/uykuyu bloklar, SIGTERM kurulumu kesmez.
//! - İndirilen paketler başarıyla kurulana kadar önbellekte kalır: yarıda
//!   kalırsa açılışta ağsız onarım (`dpkg --configure -a`, `apt-get -f
//!   install --no-download`) bunları kullanır.

use super::{Eklenti, Plan};
use crate::komut::{self, calistir, calistir_basarili, hata, Hata, Korumali, Sonuc};
use crate::koruma::HizmetDondurucu;
use std::path::Path;
use std::process::Command;

const ARSIV: &str = "/var/cache/apt/archives";

pub struct Apt {
    kilit_bekleme_sn: u64,
    indirme_bayt: u64,
}

impl Apt {
    pub fn new(kilit_bekleme_sn: u64) -> Self {
        Apt { kilit_bekleme_sn, indirme_bayt: 0 }
    }

    fn komut(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        c.env("LC_ALL", "C")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("DEBCONF_NONINTERACTIVE_SEEN", "true")
            .env("APT_LISTCHANGES_FRONTEND", "none")
            .env("APT_LISTBUGS_FRONTEND", "none")
            .env("NEEDRESTART_MODE", "l")
            .env("UCF_FORCE_CONFFOLD", "1");
        c
    }

    fn apt_get(&self) -> Command {
        let mut c = self.komut("apt-get");
        c.arg("-y")
            .arg("-q")
            .arg(format!("-oDPkg::Lock::Timeout={}", self.kilit_bekleme_sn))
            // Değiştirilmiş yapılandırma dosyaları korunur, soru sorulmaz.
            .arg("-oDpkg::Options::=--force-confdef")
            .arg("-oDpkg::Options::=--force-confold")
            .arg("-oAPT::Get::Show-Upgraded=true");
        c
    }

    fn yukseltme(&self) -> Command {
        let mut c = self.apt_get();
        c.args(["upgrade", "--with-new-pkgs"]);
        c
    }

    fn dpkg_bozuk_mu(&self) -> Sonuc<bool> {
        let c = calistir(self.komut("dpkg").arg("--audit"), true)?;
        Ok(!c.basarili() || !c.stdout.trim().is_empty())
    }

    /// İndirilmesi gereken dosyalar (önbellekte olsun olmasın hepsi).
    fn dosyalar(&self) -> Sonuc<Vec<Dosya>> {
        let mut c = self.yukseltme();
        c.arg("--print-uris");
        let cikti = calistir_basarili(&mut c, true)?;
        Ok(uri_listesi(&cikti.stdout))
    }
}

impl Eklenti for Apt {
    fn ad(&self) -> String {
        "apt".into()
    }

    fn onar(&mut self, cevrimdisi: bool) -> Sonuc<()> {
        crate::koruma::eski_dondurucuyu_kaldir();
        let yarim = crate::durum::yarim_var();
        if !yarim && !self.dpkg_bozuk_mu()? {
            return Ok(());
        }
        crate::uyari!("apt: yarım kalmış kurulum onarılıyor");
        let _k = Korumali::al();
        let _d = HizmetDondurucu::kur();
        let c = calistir(self.komut("dpkg").args(["--configure", "-a", "--force-confdef", "--force-confold"]), false)?;
        if !c.basarili() {
            crate::uyari!("apt: dpkg --configure -a {} koduyla bitti", c.kod);
        }
        let mut f = self.apt_get();
        f.args(["-f", "install"]);
        if cevrimdisi {
            f.arg("--no-download");
        }
        let c = calistir(&mut f, false)?;
        if !c.basarili() {
            crate::uyari!("apt: apt-get -f install {} koduyla bitti", c.kod);
        }
        if self.dpkg_bozuk_mu()? {
            return hata("apt: paket veritabanı hâlâ tutarsız (dpkg --audit)");
        }
        crate::bilgi!("apt: onarım tamam");
        Ok(())
    }

    fn denetle(&mut self) -> Sonuc<Option<Plan>> {
        let mut g = self.apt_get();
        g.arg("update");
        let c = calistir(&mut g, false)?;
        if !c.basarili() {
            return hata(format!("apt-get update {} koduyla bitti (ağ yok mu?)", c.kod));
        }
        let mut s = self.yukseltme();
        s.arg("-s");
        let cikti = calistir_basarili(&mut s, true)?;
        let (ogeler, kaldirilan) = benzetimi_coz(&cikti.stdout);
        if !kaldirilan.is_empty() {
            // upgrade kaldırmaz; yine de görülürse hiçbir şeye dokunma.
            return hata(format!("plan paket kaldırıyor ({}), atlandı", kaldirilan.join(", ")));
        }
        if ogeler.is_empty() {
            return Ok(None);
        }
        Ok(Some(Plan { ogeler }))
    }

    fn indir(&mut self, _plan: &Plan) -> Sonuc<()> {
        let mut c = self.yukseltme();
        c.arg("--download-only");
        calistir_basarili(&mut c, false)?;
        Ok(())
    }

    fn dogrula(&mut self, _plan: &Plan) -> Sonuc<bool> {
        let dosyalar = self.dosyalar()?;
        self.indirme_bayt = dosyalar.iter().map(|d| d.boyut).sum();
        let mut tam = true;
        for d in &dosyalar {
            if komut::durduruldu() {
                return Err(Hata::Kesildi);
            }
            if let Err(neden) = d.dogrula(Path::new(ARSIV)) {
                crate::uyari!("apt: {} eksik/bozuk: {neden}", d.ad);
                tam = false;
            }
        }
        if tam {
            crate::bilgi!("apt: {} paket ({} MB) indirildi ve doğrulandı", dosyalar.len(), self.indirme_bayt >> 20);
        }
        Ok(tam)
    }

    fn gereken_alan(&self, _plan: &Plan) -> u64 {
        // Açılmış paketler arşivden büyük olur; kaba bir üst sınır.
        self.indirme_bayt.saturating_mul(3)
    }

    fn uygula(&mut self, plan: &Plan) -> Sonuc<()> {
        let _k = Korumali::al();
        let _d = HizmetDondurucu::kur();
        crate::bilgi!("apt: {} paket kuruluyor", plan.ogeler.len());
        let mut c = if komut::var_mi("systemd-inhibit") {
            let mut c = self.komut("systemd-inhibit");
            c.args([
                "--what=shutdown:sleep:handle-power-key:handle-suspend-key",
                "--who=BacakOS güncelleyici",
                "--why=Güncelleme kuruluyor, birkaç dakika sürebilir",
                "--mode=block",
            ]);
            let ic = self.yukseltme();
            c.arg(ic.get_program()).args(ic.get_args());
            c
        } else {
            self.yukseltme()
        };
        c.arg("--no-download");
        let sonuc = calistir(&mut c, false)?;
        if !sonuc.basarili() || self.dpkg_bozuk_mu()? {
            // Önbellek temizlenmez: onarım aynı dosyaları kullanacak.
            return hata(format!("apt: kurulum {} koduyla bitti, onarım gerekecek", sonuc.kod));
        }
        drop(_d);
        let _ = calistir(self.apt_get().arg("clean"), false);
        Ok(())
    }

    fn yeniden_baslatma_ister(&self) -> bool {
        true
    }
}

#[derive(Debug, PartialEq)]
struct Dosya {
    ad: String,
    boyut: u64,
    /// ("SHA256", "abc…")
    ozet: Option<(String, String)>,
}

impl Dosya {
    fn dogrula(&self, arsiv: &Path) -> Result<(), String> {
        if self.ad.contains('/') || self.ad.starts_with('.') {
            return Err("geçersiz dosya adı".into());
        }
        let yol = arsiv.join(&self.ad);
        let m = std::fs::metadata(&yol).map_err(|_| "önbellekte yok".to_string())?;
        if self.boyut > 0 && m.len() != self.boyut {
            return Err(format!("boyut {} ≠ {}", m.len(), self.boyut));
        }
        let Some((tur, beklenen)) = &self.ozet else {
            return Ok(());
        };
        let program = match tur.to_ascii_uppercase().as_str() {
            "SHA512" => "sha512sum",
            "SHA256" => "sha256sum",
            "SHA1" => "sha1sum",
            "MD5SUM" | "MD5" => "md5sum",
            _ => return Ok(()),
        };
        let c = calistir(Command::new(program).arg(&yol), true).map_err(|e| e.to_string())?;
        let gercek = c.stdout.split_whitespace().next().unwrap_or("");
        if !c.basarili() || !gercek.eq_ignore_ascii_case(beklenen) {
            return Err(format!("{tur} özeti uyuşmuyor"));
        }
        Ok(())
    }
}

/// `'uri' dosya boyut [TÜR:özet]` satırları.
fn uri_listesi(cikti: &str) -> Vec<Dosya> {
    cikti
        .lines()
        .filter(|l| l.starts_with('\''))
        .filter_map(|l| {
            let geri = &l[l[1..].find('\'')? + 2..];
            let mut p = geri.split_whitespace();
            let ad = p.next()?.to_string();
            let boyut = p.next().and_then(|b| b.parse().ok()).unwrap_or(0);
            let ozet = p
                .next()
                .and_then(|o| o.split_once(':'))
                .map(|(t, h)| (t.to_string(), h.to_string()));
            Some(Dosya { ad, boyut, ozet })
        })
        .collect()
}

/// `apt-get -s` çıktısından (güncellenen/yeni, kaldırılan) listeleri.
fn benzetimi_coz(cikti: &str) -> (Vec<String>, Vec<String>) {
    let mut ogeler = Vec::new();
    let mut kaldirilan = Vec::new();
    for l in cikti.lines() {
        if let Some(r) = l.strip_prefix("Inst ") {
            let ad = r.split_whitespace().next().unwrap_or("").to_string();
            let eski = r
                .split_once(" [")
                .filter(|(on, _)| !on.contains('('))
                .and_then(|(_, s)| s.split_once(']'))
                .map(|(v, _)| v.to_string());
            let yeni = r
                .split_once(" (")
                .and_then(|(_, s)| s.split_whitespace().next())
                .unwrap_or("?")
                .to_string();
            ogeler.push(match eski {
                Some(e) => format!("{ad} {e} -> {yeni}"),
                None => format!("{ad} (yeni) {yeni}"),
            });
        } else if let Some(r) = l.strip_prefix("Remv ") {
            kaldirilan.push(r.split_whitespace().next().unwrap_or("").to_string());
        }
    }
    (ogeler, kaldirilan)
}

#[cfg(test)]
mod testler {
    use super::*;

    #[test]
    fn benzetim() {
        let c = "Reading package lists...\n\
                 Inst code [1.145.0-1] (1.145.2-2 code stable:stable [amd64])\n\
                 Inst libyeni1 (2.0-1 Debian:13.7/stable [amd64])\n\
                 Conf code (1.145.2-2 code stable:stable [amd64])\n";
        let (o, k) = benzetimi_coz(c);
        assert_eq!(o, vec!["code 1.145.0-1 -> 1.145.2-2", "libyeni1 (yeni) 2.0-1"]);
        assert!(k.is_empty());
        let (_, k) = benzetimi_coz("Remv eski [1.0]\n");
        assert_eq!(k, vec!["eski"]);
    }

    #[test]
    fn uriler() {
        let c = "Need to get 2 MB\n\
                 'http://deb.debian.org/debian/pool/main/b/bash/bash_5.2%2bb10_amd64.deb' bash_5.2+b10_amd64.deb 1500792 MD5Sum:27b1\n\
                 'https://x/code_1.deb' code_1.deb 263203070 \n";
        let d = uri_listesi(c);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].ad, "bash_5.2+b10_amd64.deb");
        assert_eq!(d[0].boyut, 1500792);
        assert_eq!(d[0].ozet, Some(("MD5Sum".into(), "27b1".into())));
        assert_eq!(d[1].ozet, None);
    }

    #[test]
    fn dosya_dogrulama() {
        let a = std::env::temp_dir().join(format!("bacak-guncel-arsiv-{}", std::process::id()));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("p_1_all.deb"), b"merhaba").unwrap();
        let mut d = Dosya {
            ad: "p_1_all.deb".into(),
            boyut: 7,
            ozet: Some(("SHA256".into(), "ad1b8b72d3e5b2d4d8eb8a4ec8be7a38ce4f6a9a36a5b3f78b55f1d9dc8a3a46".into())),
        };
        assert!(d.dogrula(&a).is_err(), "yanlış özet kabul edildi");
        d.ozet = Some((
            "SHA256".into(),
            "4C6BCDD55F3153E1939669AB1EC039E4059174DC25ABDFCB2F58868849B4D61B".into(),
        ));
        assert!(d.dogrula(&a).is_ok());
        d.ozet = None;
        assert!(d.dogrula(&a).is_ok());
        d.boyut = 8;
        assert!(d.dogrula(&a).is_err(), "eksik dosya kabul edildi");
        d.boyut = 7;
        d.ad = "../p_1_all.deb".into();
        assert!(d.dogrula(&a).is_err());
        d.ad = "yok.deb".into();
        assert!(d.dogrula(&a).is_err());
        std::fs::remove_dir_all(a).unwrap();
    }
}
