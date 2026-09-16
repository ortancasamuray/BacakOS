#[cfg(feature = "runtime")]
fn main() {
    for theme in &["Adwaita", "hicolor"] {
        let r = freedesktop_icons::lookup("altay")
            .with_size(64)
            .with_theme(theme)
            .find();
        println!("freedesktop[{theme}] size=64: {r:?}");
    }
    
    let sizes = &["64x64", "48x48", "128x128", "32x32", "256x256"];
    for size in sizes {
        let p = std::path::PathBuf::from(format!("/usr/share/icons/hicolor/{size}/apps/altay.png"));
        println!("manual [{size}]: exists={}", p.is_file());
    }
    
    let p64 = std::path::Path::new("/usr/share/icons/hicolor/64x64/apps/altay.png");
    match image::ImageReader::open(p64) {
        Ok(r) => match r.decode() {
            Ok(img) => println!("decode OK: {}x{}", img.width(), img.height()),
            Err(e) => println!("decode FAILED: {e}"),
        },
        Err(e) => println!("open FAILED: {e}"),
    }
    
    // Full resolve
    let result = bacak_compositor::icons::resolve_icon_rgba("altay");
    println!("resolve_icon_rgba(\"altay\"): {}", if result.is_some() { "Some" } else { "None" });
}

#[cfg(not(feature = "runtime"))]
fn main() {
    println!("need --features runtime");
}
