//! /etc/bacak-guncel/bacak-guncel.conf — `anahtar = değer` satırları, `#` yorum.

use std::path::Path;

pub const AYAR_DOSYASI: &str = "/etc/bacak-guncel/bacak-guncel.conf";

#[derive(Debug, Clone, PartialEq)]
pub struct Ayar {
    /// hayir → hiçbir şey yapılmaz (onarım hariç).
    pub etkin: bool,
    /// Pille çalışırken kurulum için gereken en az doluluk (%). Prize takılıysa
    /// ya da pil yoksa denetlenmez. İndirme her durumda yapılır.
    pub en_az_pil: u8,
    /// Kurulumdan önce kök dosya sisteminde istenen en az boş alan (MB).
    pub en_az_bos_mb: u64,
    /// Başka bir apt/dpkg işlemi sürüyorsa kilit için en çok bekleme (sn).
    pub kilit_bekleme_sn: u64,
    /// Çalışacak eklentiler, sırayla. `apt` yerleşik; `harici` eklenti.d
    /// altındaki çalıştırılabilir dosyaların hepsi.
    pub eklentiler: Vec<String>,
}

impl Default for Ayar {
    fn default() -> Self {
        Ayar {
            etkin: true,
            en_az_pil: 40,
            en_az_bos_mb: 500,
            kilit_bekleme_sn: 600,
            eklentiler: vec!["apt".into(), "harici".into()],
        }
    }
}

fn evet_mi(d: &str) -> Option<bool> {
    match d.to_lowercase().as_str() {
        "evet" | "e" | "1" | "true" | "yes" | "acik" | "açık" => Some(true),
        "hayir" | "hayır" | "h" | "0" | "false" | "no" | "kapali" | "kapalı" => Some(false),
        _ => None,
    }
}

impl Ayar {
    /// BACAK_GUNCEL_AYAR ortam değişkeni (sınama için) yoksa AYAR_DOSYASI.
    pub fn yukle() -> Ayar {
        match std::env::var_os("BACAK_GUNCEL_AYAR") {
            Some(y) => Self::dosyadan(Path::new(&y)),
            None => Self::dosyadan(Path::new(AYAR_DOSYASI)),
        }
    }

    pub fn dosyadan(yol: &Path) -> Ayar {
        match std::fs::read_to_string(yol) {
            Ok(s) => Self::coz(&s),
            Err(_) => Ayar::default(),
        }
    }

    pub fn coz(metin: &str) -> Ayar {
        let mut a = Ayar::default();
        for (no, satir) in metin.lines().enumerate() {
            let satir = satir.split('#').next().unwrap_or("").trim();
            if satir.is_empty() {
                continue;
            }
            let Some((k, d)) = satir.split_once('=') else {
                crate::uyari!("ayar satır {}: '=' yok, atlandı", no + 1);
                continue;
            };
            let (k, d) = (k.trim(), d.trim());
            let tamam = match k {
                "etkin" => evet_mi(d).map(|v| a.etkin = v).is_some(),
                "en_az_pil" => d.parse().map(|v: u8| a.en_az_pil = v.min(100)).is_ok(),
                "en_az_bos_mb" => d.parse().map(|v| a.en_az_bos_mb = v).is_ok(),
                "kilit_bekleme_sn" => d.parse().map(|v| a.kilit_bekleme_sn = v).is_ok(),
                "eklentiler" => {
                    a.eklentiler = d
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    true
                }
                _ => false,
            };
            if !tamam {
                crate::uyari!("ayar satır {}: '{k} = {d}' anlaşılmadı, atlandı", no + 1);
            }
        }
        a
    }
}

#[cfg(test)]
mod testler {
    use super::*;

    #[test]
    fn varsayilan_ve_cozum() {
        assert_eq!(Ayar::coz(""), Ayar::default());
        let a = Ayar::coz(
            "# yorum\netkin = hayır\nen_az_pil=150\nen_az_bos_mb = 900 # satır sonu\n\
             eklentiler = apt\nbilinmeyen = 3\nbozuk satır\n",
        );
        assert!(!a.etkin);
        assert_eq!(a.en_az_pil, 100);
        assert_eq!(a.en_az_bos_mb, 900);
        assert_eq!(a.eklentiler, vec!["apt"]);
        assert_eq!(a.kilit_bekleme_sn, 600);
    }
}
