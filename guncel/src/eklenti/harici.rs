//! Harici eklentiler: /usr/lib/bacak-guncel/eklenti.d ve
//! /etc/bacak-guncel/eklenti.d altındaki çalıştırılabilir dosyalar (aynı ad
//! /etc'de varsa o kullanılır; boş /etc dosyası eklentiyi kapatır). Ad
//! sırasıyla çalışırlar. Sözleşme (paket/eklenti.d/README):
//!
//!   <dosya> onar [--cevrimdisi]  0 = tamam
//!   <dosya> denetle              0 = güncelleme var (stdout: satır satır özet)
//!                                100 = güncel
//!   <dosya> indir                0 = tamam
//!   <dosya> dogrula              0 = hepsi indirildi ve doğru, 1 = eksik
//!   <dosya> uygula               0 = tamam (kapatma sinyali bu adımı kesmez)
//!
//! Ortam: BACAK_GUNCEL_DIZIN = eklentiye özel kalıcı dizin
//! (/var/lib/bacak-guncel/eklenti/<ad>), LC_ALL=C.

use super::{Eklenti, Plan};
use crate::komut::{calistir, hata, Korumali, Sonuc};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const DIZINLER: [&str; 2] = ["/usr/lib/bacak-guncel/eklenti.d", "/etc/bacak-guncel/eklenti.d"];

pub struct Harici {
    ad: String,
    yol: PathBuf,
}

pub fn bul() -> Vec<Harici> {
    bul_dizinlerde(&DIZINLER.map(Path::new))
}

fn bul_dizinlerde(dizinler: &[&Path]) -> Vec<Harici> {
    // Sonraki dizin öncekini ezer (BTreeMap → ad sırası).
    let mut bulunan: BTreeMap<String, PathBuf> = BTreeMap::new();
    for d in dizinler {
        let Ok(g) = std::fs::read_dir(d) else { continue };
        for e in g.flatten() {
            let ad = e.file_name().to_string_lossy().into_owned();
            if ad.starts_with('.') || ad.ends_with(".dpkg-new") || ad.ends_with(".dpkg-old") || ad == "README" {
                continue;
            }
            bulunan.insert(ad, e.path());
        }
    }
    bulunan
        .into_iter()
        .filter(|(_, yol)| {
            std::fs::metadata(yol)
                .map(|m| m.is_file() && m.len() > 0 && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
        .map(|(ad, yol)| Harici { ad, yol })
        .collect()
}

impl Harici {
    fn komut(&self, adim: &str) -> Command {
        let dizin = Path::new(crate::durum::DIZIN).join("eklenti").join(&self.ad);
        let _ = std::fs::create_dir_all(&dizin);
        let mut c = Command::new(&self.yol);
        c.arg(adim).env("LC_ALL", "C").env("BACAK_GUNCEL_DIZIN", dizin);
        c
    }

    fn calis(&self, adim: &str, ek: &[&str]) -> Sonuc<()> {
        let c = calistir(self.komut(adim).args(ek), false)?;
        if !c.basarili() {
            return hata(format!("{}: '{adim}' {} koduyla bitti", self.ad, c.kod));
        }
        Ok(())
    }
}

impl Eklenti for Harici {
    fn ad(&self) -> String {
        self.ad.clone()
    }

    fn onar(&mut self, cevrimdisi: bool) -> Sonuc<()> {
        let _k = Korumali::al();
        self.calis("onar", if cevrimdisi { &["--cevrimdisi"] } else { &[] })
    }

    fn denetle(&mut self) -> Sonuc<Option<Plan>> {
        let c = calistir(&mut self.komut("denetle"), true)?;
        match c.kod {
            0 => Ok(Some(Plan {
                ogeler: c.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect(),
            })),
            100 => Ok(None),
            k => hata(format!("{}: 'denetle' {k} koduyla bitti", self.ad)),
        }
    }

    fn indir(&mut self, _plan: &Plan) -> Sonuc<()> {
        self.calis("indir", &[])
    }

    fn dogrula(&mut self, _plan: &Plan) -> Sonuc<bool> {
        let c = calistir(&mut self.komut("dogrula"), false)?;
        match c.kod {
            0 => Ok(true),
            1 => Ok(false),
            k => hata(format!("{}: 'dogrula' {k} koduyla bitti", self.ad)),
        }
    }

    fn uygula(&mut self, _plan: &Plan) -> Sonuc<()> {
        let _k = Korumali::al();
        self.calis("uygula", &[])
    }
}

#[cfg(test)]
mod testler {
    use super::*;
    use std::fs;

    #[test]
    fn bulma_ve_ezme() {
        let k = std::env::temp_dir().join(format!("bacak-guncel-eklenti-{}", std::process::id()));
        let (usr, etc) = (k.join("usr"), k.join("etc"));
        fs::create_dir_all(&usr).unwrap();
        fs::create_dir_all(&etc).unwrap();
        let yaz = |yol: PathBuf, icerik: &str, kip: u32| {
            fs::write(&yol, icerik).unwrap();
            fs::set_permissions(&yol, fs::Permissions::from_mode(kip)).unwrap();
        };
        yaz(usr.join("20-model"), "#!/bin/sh\n", 0o755);
        yaz(usr.join("10-tema"), "#!/bin/sh\n", 0o755);
        yaz(usr.join("30-calismaz"), "#!/bin/sh\n", 0o644);
        yaz(usr.join("README"), "x", 0o755);
        yaz(usr.join("40-kapali"), "#!/bin/sh\n", 0o755);
        yaz(etc.join("40-kapali"), "", 0o755);
        yaz(etc.join("20-model"), "#!/bin/sh\n# yerel\n", 0o755);
        let b = bul_dizinlerde(&[&usr, &etc]);
        let adlar: Vec<_> = b.iter().map(|h| h.ad.as_str()).collect();
        assert_eq!(adlar, vec!["10-tema", "20-model"]);
        assert_eq!(b[1].yol, etc.join("20-model"));
        fs::remove_dir_all(k).unwrap();
    }
}
