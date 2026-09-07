//! Desktop Settings plugin — görünüm (duvar kağıdı, karanlık mod) ve
//! root parolası gerektiren sistem ayarları (bilgisayar adı, otomatik giriş,
//! kullanıcı parolası).
//!
//! Enabled when `/usr/share/bacak/plugins/desktop-settings.plugin` is installed.

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::{OutputId, Rect};

const MANIFEST: &str = "/usr/share/bacak/plugins/desktop-settings.plugin";

/// Preset wallpaper solid colours. The compositor draws the chosen colour as
/// the bottommost frame element when no Background layer-shell client is running.
pub const WALLPAPER_PRESETS: [[u8; 3]; 8] = [
    [10, 14, 22],    // Gece Mavisi (varsayılan)
    [28, 65, 110],   // Okyanus Mavisi
    [20, 78, 60],    // Çam Yeşili
    [88, 42, 114],   // Mor
    [128, 32, 48],   // Bordo
    [176, 96, 22],   // Kehribar
    [72, 76, 84],    // Kurşun Grisi
    [225, 225, 215], // Açık Krem
];

/// What a settings row does when tapped.
#[derive(Clone)]
pub enum DsAction {
    ToggleDarkMode,
    SetWallpaper(usize),
    /// Open a text-entry for the wallpaper image path (PNG/JPEG).
    SetWallpaperImage,
    /// Open the resolution list page for the panel's output.
    OpenResolutionList,
    /// Apply mode `i` from [`crate::state::OutputModes::modes`] to this output
    /// (persisted to `compositor.json` `outputs.<connector>.mode`).
    SetResolution(usize),
    /// Clear the mode override — auto-pick the highest supported mode.
    ResolutionAuto,
    /// Open the font list page.
    OpenFontList,
    /// Apply font `i` from [`crate::state::BacakState::ds_font_list`]
    /// (persisted to `compositor.json` `font`).
    SetFont(usize),
    /// Clear the font override — auto-detect a system font.
    FontAuto,
    /// Open the icon-theme list page.
    OpenIconThemeList,
    /// Apply icon theme `i` from
    /// [`crate::state::BacakState::ds_icon_theme_list`] (persisted to
    /// `compositor.json` `icon_theme`).
    SetIconTheme(usize),
    /// Clear the icon-theme override — auto-detect.
    IconThemeAuto,
    /// Return from a list page to the main settings page.
    BackToMain,
    /// Root auth required: change the system hostname.
    EditHostname,
    /// Root auth required: toggle auto-login for the session user.
    ToggleAutoLogin,
    /// Root auth required: change the session user's password.
    ChangePassword,
}

/// Desktop settings panel mode.
pub enum DsMode {
    /// Main settings list.
    Main,
    /// Root password prompt — once verified, run `pending`.
    Auth { pending: DsAction },
    /// Hostname text entry (post-auth). `original` remembers the old name.
    HostnameEntry { original: String },
    /// Wallpaper image path entry.
    WallpaperImageEntry,
    /// Password change: phase 0 = new password, phase 1 = confirm.
    PwChange { phase: u8, new_pw: String },
}

/// One row in the settings panel.
pub struct DsRow {
    pub rect: Rect,
    pub action: DsAction,
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub value_label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Filled for SetWallpaper rows — the swatch colour.
    pub swatch: Option<[u8; 3]>,
    /// True when the row should render an ON/OFF indicator.
    pub is_toggle: bool,
    pub toggled: bool,
}

/// Result from a background worker thread.
pub enum DsResult {
    /// Root auth passed; carry the action and the verified password forward.
    AuthOk(DsAction, String),
    /// Root auth failed (wrong password).
    AuthFail,
    /// Hostname change finished. `true` = success.
    HostnameSet(bool),
    /// Auto-login toggle finished.
    AutoLoginSet(bool),
    /// Password change finished.
    PasswordSet(bool),
}

/// The desktop settings panel (open when `Some` on `BacakState`).
pub struct DesktopSettingsPanel {
    pub output: OutputId,
    pub panel: Rect,
    pub rows: Vec<DsRow>,
    pub mode: DsMode,
    /// Live text buffer for hostname / password entry.
    pub text_buf: String,
    /// Root password being typed in Auth mode.
    pub auth_buf: String,
    /// Root password stored after successful verification (used for subsequent privileged commands).
    pub root_pw: String,
    pub title: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Status line ("Hata: yanlış parola", "Uygulandi" …).
    pub status: Option<(MemoryRenderBuffer, usize, usize)>,
    pub status_ok: bool,
    // Auth overlay geometry (drawn when mode == Auth).
    pub auth_title_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub auth_field: Rect,
    pub auth_pw_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub auth_ok_rect: Rect,
    pub auth_cancel_rect: Rect,
    pub auth_ok_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub auth_cancel_label: Option<(MemoryRenderBuffer, usize, usize)>,
    // Entry sub-flow geometry (hostname / pw change).
    pub entry_title_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub entry_field: Rect,
    pub entry_field_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub entry_ok_rect: Rect,
    pub entry_cancel_rect: Rect,
    pub entry_ok_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub entry_cancel_label: Option<(MemoryRenderBuffer, usize, usize)>,
}

// ---------------------------------------------------------------------------
// File browser

/// One entry in the file browser list.
pub struct FbEntry {
    /// Display name (file/dir name).
    pub name: String,
    /// Absolute path.
    pub path: std::path::PathBuf,
    pub is_dir: bool,
    /// PNG / JPEG / WEBP — selectable as wallpaper.
    pub is_image: bool,
    /// Pre-rasterised label.
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
}

/// File-browser panel opened when the user taps "Resim Yolu".
pub struct FileBrowserPanel {
    pub output: OutputId,
    pub panel: Rect,
    /// Scrollable content area (clipped to this rect in the renderer).
    pub list_rect: Rect,
    pub current_dir: std::path::PathBuf,
    pub entries: Vec<FbEntry>,
    /// Vertical scroll offset in logical pixels (0 = top).
    pub scroll_y: f32,
    /// Maximum scroll offset (content_h - list_rect.h).
    pub scroll_max: f32,
    /// Height of one row in the list (px).
    pub row_h: f32,
    /// Touch/pointer drag: (start_pointer_y, scroll_at_start).
    pub drag_start: Option<(f32, f32)>,
    /// Which touch slot owns the scroll drag.
    pub drag_slot: Option<i32>,
    /// Set to true once the finger/pointer moved > 8px (suppresses click).
    pub drag_moved: bool,
    /// Entry index pressed (for click vs drag detection).
    pub pressed_idx: Option<usize>,
    // Labels
    pub title_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub path_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub cancel_rect: Rect,
    pub cancel_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub up_rect: Rect,
    pub up_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub home_rect: Rect,
    pub home_label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Scroll-up arrow button (tapping scrolls list up by ~3 rows).
    pub scroll_up_rect: Rect,
    pub scroll_up_label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Scroll-down arrow button (tapping scrolls list down by ~3 rows).
    pub scroll_dn_rect: Rect,
    pub scroll_dn_label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Scrollbar track rect (right side of list — visual indicator).
    pub scrollbar_rect: Rect,
    /// Pre-rendered 28×28 folder icon (shared for all dir entries).
    pub icon_folder: Option<MemoryRenderBuffer>,
    /// Pre-rendered 28×28 image icon (shared for all image entries).
    pub icon_image: Option<MemoryRenderBuffer>,
}

impl FileBrowserPanel {
    /// Returns true if `path` has an image extension we can decode.
    pub fn is_image_path(path: &std::path::Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
            Some("png" | "jpg" | "jpeg" | "webp")
        )
    }
}

// ---------------------------------------------------------------------------

pub struct DesktopSettingsPlugin;

impl Plugin for DesktopSettingsPlugin {
    fn id(&self) -> &'static str { "desktop_settings" }

    fn z(&self) -> i32 { 62 }

    fn input_z(&self) -> i32 { 74 }

    fn enabled(&self, _state: &BacakState) -> bool {
        std::path::Path::new(MANIFEST).exists()
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        let st = ctx.state();
        if st.file_browser.is_some() { return st.fb_press(gx as f32, gy as f32); }
        st.ds_panel_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.file_browser.is_some() { return st.fb_touch_press(tx, ty, slot); }
        st.ds_panel_press(tx, ty)
    }

    fn on_pointer_motion(&self, ctx: &mut PluginCtx, _gx: f64, gy: f64) -> bool {
        let st = ctx.state();
        if st.file_browser.is_some() { return st.fb_pointer_motion(gy as f32); }
        false
    }

    fn on_pointer_release(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        let st = ctx.state();
        if st.file_browser.is_some() { return st.fb_pointer_release(gx as f32, gy as f32); }
        false
    }

    fn on_touch_motion(&self, ctx: &mut PluginCtx, _tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.file_browser.is_some() { return st.fb_touch_motion(ty, slot); }
        false
    }

    fn on_touch_up(&self, ctx: &mut PluginCtx, slot: i32) -> bool {
        let st = ctx.state();
        if st.file_browser.is_some() { return st.fb_touch_up(slot); }
        false
    }

    fn render(
        &self,
        state: &BacakState,
        renderer: &mut GlesRenderer,
        output: OutputId,
        scale: i32,
        off_x: i32,
        off_y: i32,
        out: &mut Vec<BacakElements>,
    ) {
        if state.file_browser.is_some() {
            crate::render::render_file_browser(state, renderer, output, scale, off_x, off_y, out);
        } else {
            crate::render::render_desktop_settings(state, renderer, output, scale, off_x, off_y, out);
        }
    }
}
