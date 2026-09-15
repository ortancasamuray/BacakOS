fn main() {
    // Embeds `resources/app.ico` (via `resources/app.rc`) as the .exe's icon
    // — Explorer/taskbar, the pairing window's title bar, and the tray icon
    // (`gui.rs` loads it back out with `nwg::Icon::from_embed`). Windows-only:
    // this crate builds cross-platform, but the resource itself only exists
    // for the Win32 GUI.
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        embed_resource::compile("resources/app.rc", embed_resource::NONE).manifest_required().unwrap();
    }
}
