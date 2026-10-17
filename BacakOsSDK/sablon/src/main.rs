//! __BASLIK__ — BacakOsSDK ile oluşturuldu.

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    // Dock ve uygulama menüsü pencereyi bu kimlikle .desktop dosyasına eşler.
    let _ = slint::set_xdg_app_id("__AD__");

    let ui = Uygulama::new()?;
    bacak_sdk_slint::tema_bagla!(ui, BacakTema);

    ui.on_selam(|| {
        let _ = bacak_sdk::bildir("__BASLIK__", "Merhaba!");
    });

    ui.run()
}
