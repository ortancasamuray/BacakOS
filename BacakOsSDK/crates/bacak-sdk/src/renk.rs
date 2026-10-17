//! The BacakOS colour palette. The dark values match the compositor's own
//! chrome (Control Center, panels), so applications blend with the desktop.
//! Slint applications get the same colours from `BacakTema` in
//! `bacak-sdk-slint`; keep the two in sync.

use crate::Gorunum;

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Renk {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Renk {
    pub const fn hex(v: u32) -> Renk {
        Renk { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    /// `#rrggbb`, e.g. for CSS or GTK.
    pub fn css(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Components in `0.0..=1.0`, for GPU renderers.
    pub fn f32(&self) -> [f32; 3] {
        [self.r as f32 / 255.0, self.g as f32 / 255.0, self.b as f32 / 255.0]
    }
}

/// The colour roles of a BacakOS interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palet {
    /// Window background.
    pub arka: Renk,
    /// Cards, panels, list rows.
    pub yuzey: Renk,
    /// Raised / hovered surfaces, inputs.
    pub yuzey2: Renk,
    /// Borders and separators.
    pub cizgi: Renk,
    /// Body text.
    pub metin: Renk,
    /// Secondary text, hints.
    pub soluk: Renk,
    /// Accent: primary buttons, selection, focus rings, "on" switches.
    pub vurgu: Renk,
    /// Text drawn on top of `vurgu`.
    pub vurgu_metin: Renk,
    /// Destructive actions and errors.
    pub tehlike: Renk,
    /// Warnings.
    pub uyari: Renk,
    /// Success.
    pub basari: Renk,
}

impl Palet {
    pub const fn koyu() -> Palet {
        Palet {
            arka: Renk::hex(0x0d1421),
            yuzey: Renk::hex(0x172131),
            yuzey2: Renk::hex(0x223047),
            cizgi: Renk::hex(0x2e3d55),
            metin: Renk::hex(0xe8ecf4),
            soluk: Renk::hex(0xaab2c4),
            vurgu: Renk::hex(0x5cb8ff),
            vurgu_metin: Renk::hex(0x04121f),
            tehlike: Renk::hex(0xeb4d4d),
            uyari: Renk::hex(0xffc74d),
            basari: Renk::hex(0x4caf7d),
        }
    }

    pub const fn acik() -> Palet {
        Palet {
            arka: Renk::hex(0xf5f6f9),
            yuzey: Renk::hex(0xffffff),
            yuzey2: Renk::hex(0xebedf2),
            cizgi: Renk::hex(0xd3d7df),
            metin: Renk::hex(0x1a1c22),
            soluk: Renk::hex(0x5f6573),
            vurgu: Renk::hex(0x1f7ae0),
            vurgu_metin: Renk::hex(0xffffff),
            tehlike: Renk::hex(0xc62828),
            uyari: Renk::hex(0xa86200),
            basari: Renk::hex(0x2e7d4f),
        }
    }

    pub const fn icin(g: Gorunum) -> Palet {
        if g.koyu {
            Palet::koyu()
        } else {
            Palet::acik()
        }
    }

    /// The palette for the current desktop appearance.
    pub fn simdiki() -> Palet {
        Palet::icin(Gorunum::load())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_css() {
        let r = Renk::hex(0x5cb8ff);
        assert_eq!((r.r, r.g, r.b), (0x5c, 0xb8, 0xff));
        assert_eq!(r.css(), "#5cb8ff");
        assert_eq!(Palet::icin(Gorunum { koyu: false }), Palet::acik());
    }
}
