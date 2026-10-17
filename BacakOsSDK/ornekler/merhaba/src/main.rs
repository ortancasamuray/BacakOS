//! BacakOsSDK örnek uygulaması.
//!
//!     cargo run -p bacak-merhaba
//!
//! BacakOS oturumu dışında da açılır; o zaman masaüstü düğmeleri durum
//! satırında "BacakOS oturumu yok" der.

use bacak_sdk::Istemci;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let _ = slint::set_xdg_app_id("bacak-merhaba");
    let ui = Uygulama::new()?;
    bacak_sdk_slint::tema_bagla!(ui, BacakTema);

    let durum = match Istemci::baglan().and_then(|mut c| c.surum()) {
        Ok((p, v)) => format!("compositor {v}, protokol {p}"),
        Err(e) => format!("BacakOS oturumu yok: {e}"),
    };
    ui.set_durum(durum.into());

    ui.on_koyu_degisti(bacak_sdk_slint::koyu_mod_ayarla);

    let zayif = ui.as_weak();
    ui.on_bildirim_gonder(move || {
        let sonuc = bacak_sdk::bildir("Merhaba!", "BacakOsSDK örnek uygulamasından");
        goster(&zayif, sonuc.map(|_| "Bildirim gönderildi".into()));
    });

    let zayif = ui.as_weak();
    ui.on_kontrol_merkezi(move || {
        let sonuc = Istemci::baglan().and_then(|mut c| c.kontrol_merkezi(None));
        goster(&zayif, sonuc.map(|_| "Kontrol Merkezi açıldı/kapandı".into()));
    });

    let zayif = ui.as_weak();
    ui.on_pencereleri_listele(move || {
        let Some(ui) = zayif.upgrade() else { return };
        match Istemci::baglan().and_then(|mut c| c.pencereler()) {
            Ok(list) => {
                let satirlar: Vec<String> = list
                    .iter()
                    .map(|p| {
                        let ad = bacak_sdk::masaustu::uygulama(&p.uygulama).map(|a| a.ad).unwrap_or(p.uygulama.clone());
                        format!("{}{} — {}", if p.odakli { "● " } else { "" }, ad, p.baslik)
                    })
                    .collect();
                ui.set_pencereler(satirlar.join("\n").into());
            }
            Err(e) => ui.set_durum(format!("Hata: {e}").into()),
        }
    });

    ui.run()
}

fn goster(zayif: &slint::Weak<Uygulama>, sonuc: std::io::Result<String>) {
    if let Some(ui) = zayif.upgrade() {
        ui.set_durum(sonuc.unwrap_or_else(|e| format!("Hata: {e}")).into());
    }
}
