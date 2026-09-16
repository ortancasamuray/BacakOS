//! Apps-menu plugin — the application launcher grid (category sidebar +
//! scrollable icon grid). Dispatches to `render_apps_menu` and the
//! `apps_menu_*` state. A modal overlay above the keyboard.
use std::time::Instant;

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::animation::Spring;
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::{OutputId, Rect};

// --- plugin-owned data types (Phase-2). Field instances + the `impl AppsMenu`
// methods stay on/near `BacakState`; the definitions live here. ---

/// One cell in the applications grid menu. Its on-screen `rect` is derived from
/// its index + the menu's scroll offset (see `AppsMenu::item_rect`), so
/// scrolling never re-lays-out — only the search query rebuilds the list.
pub struct AppMenuItem {
    pub app: String,
    /// Pre-rasterised name label + its pixel size, built when the (filtered)
    /// list is assembled so the render path never re-rasterises per frame.
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
}

/// One category tab in the apps-menu sidebar. `mask` is the [`crate::icons`]
/// category bitmask an app must match to appear under this tab; the all-tab
/// ("Tümü") uses `mask == 0`, which matches every app.
pub struct AppCategoryTab {
    pub mask: u8,
    pub rect: Rect,
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
}

/// The applications menu opened from the dock's apps button: a left category
/// sidebar, a search field, and a vertically-scrollable icon grid. `all` is the
/// full app list (kept for re-filtering); `items` is the current filtered set.
pub struct AppsMenu {
    pub output: OutputId,
    pub panel: Rect,
    /// Left category column.
    pub sidebar: Rect,
    /// Pre-rasterised "Kategoriler" sidebar heading.
    pub header: Option<(MemoryRenderBuffer, usize, usize)>,
    pub cats: Vec<AppCategoryTab>,
    /// Index into `cats` of the selected tab.
    pub selected: usize,
    pub search: Rect,
    /// Grid origin (first cell's top-left) and visible viewport height.
    pub grid_x: f32,
    pub grid_y: f32,
    pub grid_h: f32,
    pub cols: usize,
    pub cell: f32,
    /// Rows that fit in the viewport (for the wheel step + scroll clamp).
    pub visible_rows: usize,
    /// Scroll offset in PIXELS from the top of the content, as a [`Spring`] so a
    /// flick coasts to rest. Read the live offset via `scroll.pos`.
    pub scroll: Spring,
    pub query: String,
    pub query_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub all: Vec<(String, String, u8)>,
    pub items: Vec<AppMenuItem>,
}

/// In-flight single-finger drag-scroll of the [`AppsMenu`] grid. Pixel-smooth
/// and finger-following; `moved` flips once the finger passes the tap slop
/// (press → scroll vs launch); `velocity` (EMA, px/s) seeds the release fling.
pub struct AppsMenuDrag {
    pub start_y: f32,
    pub start_scroll_y: f32,
    pub press_x: f32,
    pub press_y: f32,
    pub moved: bool,
    pub last_y: f32,
    pub last_t: Instant,
    pub velocity: f32,
}

/// Outcome of an apps-menu touch-down, telling the backend how to route the
/// rest of the touch sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppsTouch {
    /// No menu open — fall through to the normal touch chain.
    None,
    /// Handled fully on press (dismiss / category switch); nothing follows.
    Consumed,
    /// A grid drag-scroll started — claim this slot for motion/up.
    Drag,
}

pub struct AppsMenuPlugin;

impl Plugin for AppsMenuPlugin {
    fn id(&self) -> &'static str {
        "apps_menu"
    }

    fn z(&self) -> i32 {
        60
    }

    fn input_z(&self) -> i32 {
        70
    }

    fn enabled(&self, state: &BacakState) -> bool {
        state.config.dock
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().apps_menu_left_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        // A grid drag already owns a finger → swallow any extra finger.
        if st.apps_menu_touch_slot.is_some() {
            return true;
        }
        // A press in the grid starts a single-finger drag-scroll (claims the
        // slot); launch-on-tap is deferred to the touch-up.
        match st.apps_menu_touch_down(tx, ty) {
            crate::state::AppsTouch::Drag => {
                st.apps_menu_touch_slot = Some(slot);
                true
            }
            crate::state::AppsTouch::Consumed => true,
            crate::state::AppsTouch::None => false,
        }
    }

    fn on_touch_motion(&self, ctx: &mut PluginCtx, _tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.apps_menu_touch_slot == Some(slot) {
            st.apps_menu_touch_motion(ty);
            true
        } else {
            false
        }
    }

    fn on_touch_up(&self, ctx: &mut PluginCtx, slot: i32) -> bool {
        let st = ctx.state();
        if st.apps_menu_touch_slot == Some(slot) {
            st.apps_menu_touch_up();
            st.apps_menu_touch_slot = None;
            true
        } else {
            false
        }
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
        crate::render::render_apps_menu(state, renderer, output, scale, off_x, off_y, out);
    }
}
