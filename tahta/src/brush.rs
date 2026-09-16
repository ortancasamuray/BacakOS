//! Pedagogical pen/brush presets: what a stroke *is* (ink vs. highlighter
//! vs. a self-erasing laser pointer) as opposed to *how* it's smoothed and
//! meshed ([`crate::stroke`]) or *which tool routes touches to it*
//! ([`crate::input_handler`]).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushType {
    /// Fixed/lightly speed-responsive crisp ink — writing, formulas, math.
    Ballpoint,
    /// Speed-driven variable-width stroke — headings, emphasis.
    Calligraphy,
    /// Semi-transparent, Max-blended so overlaps never darken past one
    /// layer's opacity and underlying ink stays legible.
    Highlighter,
    /// Fades to transparent and is dropped from the buffer after
    /// [`LASER_LIFETIME_SECS`] — a pointer, not permanent ink.
    LaserPointer,
}

/// Fixed cycle order for the toolbar's touch-only brush-cycle button — this
/// panel has no keyboard, so every mode must be reachable by tapping alone.
pub const BRUSH_CYCLE: [BrushType; 4] =
    [BrushType::Ballpoint, BrushType::Calligraphy, BrushType::Highlighter, BrushType::LaserPointer];

impl BrushType {
    pub fn next(self) -> Self {
        let i = BRUSH_CYCLE.iter().position(|&b| b == self).unwrap_or(0);
        BRUSH_CYCLE[(i + 1) % BRUSH_CYCLE.len()]
    }
}

/// Fixed cycle order for the toolbar's touch-only color-cycle button.
pub const PALETTE: [[f32; 4]; 5] = [
    [0.92, 0.92, 0.95, 1.0], // white/chalk
    [0.95, 0.25, 0.25, 1.0], // red
    [0.25, 0.55, 0.95, 1.0], // blue
    [0.30, 0.85, 0.35, 1.0], // green
    [0.98, 0.78, 0.15, 1.0], // yellow
];

/// Next color after `current` in [`PALETTE`], matching on RGB only (brush
/// presets like Highlighter override alpha, so an exact match would never
/// hit). Falls back to index 0 if `current` isn't a palette color at all
/// (e.g. it came from the radial menu's black swatch).
pub fn next_palette_color(current: [f32; 4]) -> [f32; 4] {
    let found = PALETTE.iter().position(|c| c[0] == current[0] && c[1] == current[1] && c[2] == current[2]);
    match found {
        Some(i) => PALETTE[(i + 1) % PALETTE.len()],
        None => PALETTE[0],
    }
}

/// How long a laser-pointer stroke's points live before being pruned, in
/// seconds.
pub const LASER_LIFETIME_SECS: f64 = 1.8;

#[derive(Debug, Clone, PartialEq)]
pub struct PenSettings {
    pub brush_type: BrushType,
    pub color: [f32; 4],
    pub base_width: f32,
    /// Speed-driven width interpolation (used by Calligraphy; ignored by
    /// Ballpoint/Highlighter, implicit for LaserPointer's own age-fade).
    pub dynamic_stroke: bool,
    /// Reserved for a future geometric shape-snap pass (circle/line/
    /// rectangle recognition) — not implemented yet, kept as a settings
    /// field so the toolbar/radial-menu wiring doesn't need to change
    /// shape later.
    pub auto_shape_detection: bool,
}

impl PenSettings {
    pub fn ballpoint(color: [f32; 4]) -> Self {
        Self {
            brush_type: BrushType::Ballpoint,
            color,
            base_width: 4.0,
            dynamic_stroke: false,
            auto_shape_detection: false,
        }
    }

    pub fn calligraphy(color: [f32; 4]) -> Self {
        Self {
            brush_type: BrushType::Calligraphy,
            color,
            base_width: 10.0,
            dynamic_stroke: true,
            auto_shape_detection: false,
        }
    }

    pub fn highlighter(color: [f32; 4]) -> Self {
        Self {
            brush_type: BrushType::Highlighter,
            color: [color[0], color[1], color[2], 0.35],
            base_width: 22.0,
            dynamic_stroke: false,
            auto_shape_detection: false,
        }
    }

    pub fn laser_pointer(color: [f32; 4]) -> Self {
        Self {
            brush_type: BrushType::LaserPointer,
            color,
            base_width: 6.0,
            dynamic_stroke: false,
            auto_shape_detection: false,
        }
    }

    /// Build the preset for `brush_type` with a specific color — used when
    /// the radial menu or a color hotkey changes color without resetting
    /// the currently selected brush.
    pub fn for_brush(brush_type: BrushType, color: [f32; 4]) -> Self {
        match brush_type {
            BrushType::Ballpoint => Self::ballpoint(color),
            BrushType::Calligraphy => Self::calligraphy(color),
            BrushType::Highlighter => Self::highlighter(color),
            BrushType::LaserPointer => Self::laser_pointer(color),
        }
    }
}
