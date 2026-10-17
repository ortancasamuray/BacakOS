//! BacakOS theme and widgets for Slint applications.
//!
//! `build.rs` (with this crate in `[build-dependencies]`, feature `derleme`):
//!
//! ```ignore
//! fn main() {
//!     bacak_sdk_slint::derle("ui/app.slint").unwrap();
//! }
//! ```
//!
//! `ui/app.slint`:
//!
//! ```slint,ignore
//! import { BacakTema, BacakPencere, BacakDugme } from "@bacak";
//! export { BacakTema }   // so Rust can reach it
//! export component Uygulama inherits BacakPencere { … }
//! ```
//!
//! `main.rs`:
//!
//! ```ignore
//! slint::include_modules!();
//! let ui = Uygulama::new()?;
//! bacak_sdk_slint::tema_bagla!(ui, BacakTema);   // follows the desktop dark mode
//! ui.run()
//! ```

use std::path::PathBuf;

pub use bacak_sdk;
pub use slint;

/// The directory holding `bacak.slint`.
pub fn ui_dizini() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

/// Compiles `giris` with `import … from "@bacak"` available. For `build.rs`.
#[cfg(feature = "derleme")]
pub fn derle(giris: impl AsRef<std::path::Path>) -> Result<(), slint_build::CompileError> {
    derle_ayarla(giris, slint_build::CompilerConfiguration::new())
}

/// Like [`derle`] with the application's own compiler settings (style,
/// further library paths…).
#[cfg(feature = "derleme")]
pub fn derle_ayarla(
    giris: impl AsRef<std::path::Path>,
    ayar: slint_build::CompilerConfiguration,
) -> Result<(), slint_build::CompileError> {
    let lib = ui_dizini().join("bacak.slint");
    println!("cargo:rerun-if-changed={}", lib.display());
    let ayar = ayar.with_library_paths(std::collections::HashMap::from([("bacak".to_string(), lib)]));
    slint_build::compile_with_config(giris, ayar)
}

/// Binds the app's exported `BacakTema` global to the desktop-wide dark mode:
/// sets it now and keeps following changes (on a background thread that hops
/// back to the Slint event loop).
///
/// ```ignore
/// bacak_sdk_slint::tema_bagla!(ui, BacakTema);
/// ```
#[macro_export]
macro_rules! tema_bagla {
    ($ui:expr, $tema:ident) => {{
        use $crate::slint::ComponentHandle as _;
        let ui = &$ui;
        ui.global::<$tema>().set_koyu($crate::bacak_sdk::Gorunum::load().koyu);
        let zayif = ui.as_weak();
        $crate::bacak_sdk::gorunum::izle(move |g| {
            let _ = zayif.upgrade_in_event_loop(move |ui| ui.global::<$tema>().set_koyu(g.koyu));
        });
    }};
}

/// For a dark-mode switch inside the app: changes it for the whole desktop
/// (the app itself follows through [`tema_bagla!`]).
pub fn koyu_mod_ayarla(koyu: bool) {
    // Runs off the UI thread: the IPC call may wait up to its timeout.
    std::thread::spawn(move || {
        if let Err(e) = bacak_sdk::gorunum::ayarla(koyu) {
            eprintln!("bacak-sdk: koyu mod ayarlanamadı: {e}");
        }
    });
}
