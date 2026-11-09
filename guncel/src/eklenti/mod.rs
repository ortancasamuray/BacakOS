//! Güncelleme kaynakları eklentidir. Her eklenti aynı adımları uygular;
//! yönetici (main.rs) hepsini aynı güvenlik kurallarıyla çalıştırır:
//!
//! 1. `onar`    — yarıda kalmış önceki kurulumu toparla (her çalışmada önce)
//! 2. `denetle` — güncelleme var mı; varsa plan
//! 3. `indir`   — planın tamamını indir (kesilebilir)
//! 4. `dogrula` — indirilenlerin tamamı yerinde ve özetleri doğru mu
//! 5. `uygula`  — yalnız 4 "tam" dediyse; kapatma sinyali bu adımı kesmez
//!
//! Yeni bir kaynak eklemek için ya bu trait'i Rust'ta uygulayıp `yukle`ye
//! eklenir ya da /usr/lib/bacak-guncel/eklenti.d (veya /etc/…) altına aynı
//! adımları komut satırı argümanı olarak alan bir çalıştırılabilir konur
//! (bkz. harici.rs, paket/eklenti.d/README).

pub mod apt;
pub mod harici;

use crate::ayar::Ayar;
use crate::komut::Sonuc;

#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// İnsan okunur satırlar ("paket eski -> yeni").
    pub ogeler: Vec<String>,
}

pub trait Eklenti {
    fn ad(&self) -> String;
    /// `cevrimdisi`: açılışta ağ yokken, yalnız yerel önbellekle onarım.
    fn onar(&mut self, cevrimdisi: bool) -> Sonuc<()>;
    fn denetle(&mut self) -> Sonuc<Option<Plan>>;
    fn indir(&mut self, plan: &Plan) -> Sonuc<()>;
    /// true: planın her parçası indirilmiş ve doğrulanmış.
    fn dogrula(&mut self, plan: &Plan) -> Sonuc<bool>;
    /// Kurulum için gereken ek boş alan tahmini (bayt).
    fn gereken_alan(&self, _plan: &Plan) -> u64 {
        0
    }
    fn uygula(&mut self, plan: &Plan) -> Sonuc<()>;
    /// Kurulum yeniden başlatma gerektiriyor mu?
    fn yeniden_baslatma_ister(&self) -> bool {
        false
    }
}

pub fn yukle(ayar: &Ayar) -> Vec<Box<dyn Eklenti>> {
    let mut v: Vec<Box<dyn Eklenti>> = Vec::new();
    for ad in &ayar.eklentiler {
        match ad.as_str() {
            "apt" => v.push(Box::new(apt::Apt::new(ayar.kilit_bekleme_sn))),
            "harici" => v.extend(harici::bul().into_iter().map(|e| Box::new(e) as Box<dyn Eklenti>)),
            diger => crate::uyari!("bilinmeyen eklenti '{diger}' atlandı"),
        }
    }
    v
}
