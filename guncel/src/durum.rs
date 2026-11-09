//! Kalıcı durum: /var/lib/bacak-guncel/durum.json (bilgi amaçlı) ve
//! /var/lib/bacak-guncel/yarim işareti. İşaret kurulum başlamadan diske
//! yazılır (fsync), kurulum ve denetim temiz bitince silinir; açılışta varsa
//! bacak-guncel-onar.service giriş ekranından önce onarım yapar.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DIZIN: &str = "/var/lib/bacak-guncel";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Evre {
    #[default]
    Bosta,
    Onariliyor,
    Denetleniyor,
    Indiriliyor,
    /// Her şey indirildi, kurulum koşulları (pil, boş alan) bekleniyor.
    Indirildi,
    Uygulaniyor,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Durum {
    pub evre: Evre,
    /// Evrenin ait olduğu eklenti.
    pub eklenti: Option<String>,
    pub son_deneme: u64,
    pub son_basari: Option<u64>,
    pub son_hata: Option<String>,
    pub ardisik_hata: u32,
    /// Son başarılı kurulumda güncellenenler ("paket eski -> yeni").
    pub son_guncellenen: Vec<String>,
    /// İndirilmiş, kurulmayı bekleyenler.
    pub bekleyen: Vec<String>,
}

pub fn simdi() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dizin() -> PathBuf {
    std::env::var_os("BACAK_GUNCEL_DIZIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DIZIN))
}

pub fn yarim_isareti() -> PathBuf {
    dizin().join("yarim")
}

/// Dosyayı geçici adla yazar, fsync'ler, yerine taşır ve dizini fsync'ler:
/// elektrik kesilse de ya eski ya yeni içerik kalır.
pub fn atomik_yaz(yol: &Path, icerik: &[u8]) -> io::Result<()> {
    let ust = yol.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(ust)?;
    let gecici = yol.with_extension("yeni");
    {
        let mut f = File::create(&gecici)?;
        f.write_all(icerik)?;
        f.sync_all()?;
    }
    fs::rename(&gecici, yol)?;
    File::open(ust)?.sync_all()
}

pub fn yarim_isaretle(aciklama: &str) -> io::Result<()> {
    atomik_yaz(&yarim_isareti(), aciklama.as_bytes())
}

pub fn yarim_kaldir() -> io::Result<()> {
    let y = yarim_isareti();
    match fs::remove_file(&y) {
        Ok(()) => File::open(y.parent().unwrap_or(Path::new("/")))?.sync_all(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn yarim_var() -> bool {
    yarim_isareti().exists()
}

impl Durum {
    fn yol() -> PathBuf {
        dizin().join("durum.json")
    }

    pub fn yukle() -> Durum {
        fs::read(Self::yol())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn kaydet(&self) {
        let json = serde_json::to_vec_pretty(self).unwrap_or_default();
        if let Err(e) = atomik_yaz(&Self::yol(), &json) {
            crate::uyari!("durum kaydedilemedi: {e}");
        }
    }

    pub fn evre(&mut self, evre: Evre, eklenti: Option<&str>) {
        self.evre = evre;
        self.eklenti = eklenti.map(str::to_string);
        self.kaydet();
    }
}

#[cfg(test)]
mod testler {
    use super::*;

    #[test]
    fn atomik_yazma_ve_eski_alanlar() {
        let d = std::env::temp_dir().join(format!("bacak-guncel-test-{}", std::process::id()));
        let y = d.join("durum.json");
        atomik_yaz(&y, b"{\"evre\":\"uygulaniyor\",\"bilinmeyen\":1}").unwrap();
        let durum: Durum = serde_json::from_slice(&fs::read(&y).unwrap()).unwrap();
        assert_eq!(durum.evre, Evre::Uygulaniyor);
        assert!(!y.with_extension("yeni").exists());
        fs::remove_dir_all(d).unwrap();
    }
}
