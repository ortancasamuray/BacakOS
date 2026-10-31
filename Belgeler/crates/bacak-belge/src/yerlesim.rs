//! Zoom and pan arithmetic, kept free of Slint so it can be tested.
//!
//! Sizes are logical pixels. Pan is the offset of the content's centre from
//! the view's centre, as the UI applies it.

pub const ZOOM_MIN: f32 = 0.1;
pub const ZOOM_MAX: f32 = 8.0;

/// How the content is sized to the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sigdir {
    /// The whole page/picture is visible.
    Sayfa,
    /// The page fills the view's width (reading).
    Genislik,
    /// A zoom the user chose.
    Serbest,
}

impl Sigdir {
    /// What the fit button switches to next.
    pub fn sonraki(self) -> Sigdir {
        match self {
            Sigdir::Sayfa => Sigdir::Genislik,
            Sigdir::Genislik | Sigdir::Serbest => Sigdir::Sayfa,
        }
    }

    pub fn etiket(self) -> &'static str {
        match self {
            Sigdir::Sayfa => "Sayfa",
            Sigdir::Genislik => "Genişlik",
            Sigdir::Serbest => "",
        }
    }
}

/// Empty space kept around fitted content, so the page edge stays visible.
const KENAR: f32 = 16.0;

/// The zoom at which content of `(w, h)` (logical px at zoom 1) fits the view.
/// `buyutme`: may a small picture be enlarged beyond 100 %?
pub fn sigdir_zoom(icerik: (f32, f32), gorunum: (f32, f32), kip: Sigdir, buyutme: bool) -> Option<f32> {
    let (w, h) = icerik;
    let (vw, vh) = ((gorunum.0 - 2.0 * KENAR).max(1.0), (gorunum.1 - 2.0 * KENAR).max(1.0));
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let z = match kip {
        Sigdir::Sayfa => (vw / w).min(vh / h),
        Sigdir::Genislik => vw / w,
        Sigdir::Serbest => return None,
    };
    let z = if buyutme { z } else { z.min(1.0) };
    Some(z.clamp(ZOOM_MIN, ZOOM_MAX))
}

/// Keep the content inside the view: when it is smaller it stays centred,
/// when larger its edges can't be dragged past the view's edges.
pub fn sinirla(pan: f32, icerik: f32, gorunum: f32) -> f32 {
    let m = ((icerik - gorunum) / 2.0).max(0.0);
    pan.clamp(-m, m)
}

/// Zoom stepping (buttons, keys, Ctrl+wheel).
pub fn adim(zoom: f32, buyut: bool) -> f32 {
    let z = if buyut { zoom * 1.25 } else { zoom / 1.25 };
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Natural order for file names: "resim2" before "resim10".
pub fn dogal_sira(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na: String = std::iter::from_fn(|| a.next_if(|c| c.is_ascii_digit())).collect();
                let nb: String = std::iter::from_fn(|| b.next_if(|c| c.is_ascii_digit())).collect();
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a4_fits_the_window() {
        // A4 at 96 dpi in a 1000×700 view: height is the limit.
        let z = sigdir_zoom((794.0, 1123.0), (1000.0, 700.0), Sigdir::Sayfa, true).unwrap();
        assert!((1123.0 * z - (700.0 - 2.0 * KENAR)).abs() < 0.5);
        let zw = sigdir_zoom((794.0, 1123.0), (1000.0, 700.0), Sigdir::Genislik, true).unwrap();
        assert!((794.0 * zw - (1000.0 - 2.0 * KENAR)).abs() < 0.5);
        assert!(sigdir_zoom((0.0, 10.0), (100.0, 100.0), Sigdir::Sayfa, true).is_none());
    }

    #[test]
    fn small_pictures_are_not_blown_up_unless_asked() {
        assert_eq!(sigdir_zoom((100.0, 80.0), (1000.0, 700.0), Sigdir::Sayfa, false), Some(1.0));
        assert!(sigdir_zoom((100.0, 80.0), (1000.0, 700.0), Sigdir::Sayfa, true).unwrap() > 1.0);
    }

    #[test]
    fn pan_stays_inside() {
        assert_eq!(sinirla(50.0, 400.0, 800.0), 0.0, "küçük içerik ortada kalır");
        assert_eq!(sinirla(500.0, 1200.0, 800.0), 200.0);
        assert_eq!(sinirla(-500.0, 1200.0, 800.0), -200.0);
        assert_eq!(sinirla(10.0, 1200.0, 800.0), 10.0);
    }

    #[test]
    fn zoom_steps_are_bounded() {
        assert!(adim(ZOOM_MAX, true) <= ZOOM_MAX);
        assert!(adim(ZOOM_MIN, false) >= ZOOM_MIN);
        assert!((adim(1.0, true) - 1.25).abs() < 1e-6);
    }

    #[test]
    fn natural_file_order() {
        let mut v = vec!["resim10.jpg", "Resim2.jpg", "resim1.jpg", "a.png", "resim02b.jpg"];
        v.sort_by(|a, b| dogal_sira(a, b));
        assert_eq!(v, ["a.png", "resim1.jpg", "Resim2.jpg", "resim02b.jpg", "resim10.jpg"]);
    }

    #[test]
    fn fit_button_cycles() {
        assert_eq!(Sigdir::Sayfa.sonraki(), Sigdir::Genislik);
        assert_eq!(Sigdir::Genislik.sonraki(), Sigdir::Sayfa);
        assert_eq!(Sigdir::Serbest.sonraki(), Sigdir::Sayfa);
    }
}
