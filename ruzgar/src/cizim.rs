//! The ink drawn over a page: strokes in *page* coordinates (CSS px from
//! the document's top-left), so they stay on what they mark while the page
//! scrolls under them.

use gtk::cairo;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arac {
    Kalem,
    Fosforlu,
    Silgi,
}

/// The palette: dark, red, blue, green, orange — readable on white pages.
pub const RENKLER: [[f64; 3]; 5] = [
    [0.10, 0.10, 0.12],
    [0.86, 0.15, 0.15],
    [0.12, 0.40, 0.90],
    [0.10, 0.62, 0.25],
    [0.96, 0.55, 0.05],
];

/// Pen widths (CSS px); the highlighter is several times wider.
pub const KALINLIKLAR: [f64; 3] = [3.0, 6.0, 12.0];

const FOSFORLU_KAT: f64 = 4.0;
const FOSFORLU_SAYDAMLIK: f64 = 0.35;
/// How close (CSS px) the eraser must come to a stroke to take it.
pub const SILGI_YARICAP: f64 = 18.0;

/// The tool in hand and its settings.
#[derive(Clone, Copy, Debug)]
pub struct Kalem {
    pub arac: Arac,
    pub renk: [f64; 3],
    pub kalinlik: f64,
}

impl Default for Kalem {
    fn default() -> Self {
        Self { arac: Arac::Kalem, renk: RENKLER[1], kalinlik: KALINLIKLAR[1] }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cizgi {
    pub noktalar: Vec<(f64, f64)>,
    pub renk: [f64; 3],
    pub kalinlik: f64,
    pub fosforlu: bool,
}

impl Cizgi {
    pub fn yeni(kalem: &Kalem, nokta: (f64, f64)) -> Self {
        let fosforlu = kalem.arac == Arac::Fosforlu;
        let kalinlik = if fosforlu { kalem.kalinlik * FOSFORLU_KAT } else { kalem.kalinlik };
        Self { noktalar: vec![nokta], renk: kalem.renk, kalinlik, fosforlu }
    }

    /// Adds a point, skipping ones too close to the last to matter.
    pub fn ekle(&mut self, nokta: (f64, f64)) {
        if let Some(&(x, y)) = self.noktalar.last() {
            if (nokta.0 - x).hypot(nokta.1 - y) < 1.0 {
                return;
            }
        }
        self.noktalar.push(nokta);
    }

    pub fn ciz(&self, cr: &cairo::Context) {
        let [r, g, b] = self.renk;
        let a = if self.fosforlu { FOSFORLU_SAYDAMLIK } else { 1.0 };
        cr.set_source_rgba(r, g, b, a);
        cr.set_line_width(self.kalinlik);
        cr.set_line_cap(if self.fosforlu { cairo::LineCap::Square } else { cairo::LineCap::Round });
        cr.set_line_join(cairo::LineJoin::Round);
        let Some(&(x0, y0)) = self.noktalar.first() else { return };
        cr.move_to(x0, y0);
        if self.noktalar.len() == 1 {
            // A tap: a dot.
            cr.line_to(x0 + 0.01, y0);
        }
        for &(x, y) in &self.noktalar[1..] {
            cr.line_to(x, y);
        }
        let _ = cr.stroke();
    }

    /// Whether the eraser at `p` touches this stroke.
    pub fn degiyor_mu(&self, p: (f64, f64)) -> bool {
        let r = SILGI_YARICAP + self.kalinlik / 2.0;
        match self.noktalar.as_slice() {
            [] => false,
            [a] => uzaklik(p, *a, *a) <= r,
            n => n.windows(2).any(|w| uzaklik(p, w[0], w[1]) <= r),
        }
    }
}

/// Distance from `p` to the segment `a`–`b`.
fn uzaklik(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// Removes the strokes the eraser at `p` touches; whether any went.
pub fn sil(cizgiler: &mut Vec<Cizgi>, p: (f64, f64)) -> bool {
    let once = cizgiler.len();
    cizgiler.retain(|c| !c.degiyor_mu(p));
    cizgiler.len() != once
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eraser_takes_only_strokes_it_touches() {
        let kalem = Kalem::default();
        let mut a = Cizgi::yeni(&kalem, (0.0, 0.0));
        a.ekle((100.0, 0.0));
        let b = Cizgi::yeni(&kalem, (0.0, 200.0));
        let mut cizgiler = vec![a, b.clone()];
        assert!(!sil(&mut cizgiler, (50.0, 60.0)));
        assert!(sil(&mut cizgiler, (50.0, 10.0)));
        assert_eq!(cizgiler, vec![b]);
    }

    #[test]
    fn highlighter_is_wide_and_close_points_are_dropped() {
        let kalem = Kalem { arac: Arac::Fosforlu, ..Kalem::default() };
        let mut c = Cizgi::yeni(&kalem, (0.0, 0.0));
        assert!(c.fosforlu && c.kalinlik == KALINLIKLAR[1] * FOSFORLU_KAT);
        c.ekle((0.4, 0.3));
        c.ekle((5.0, 0.0));
        assert_eq!(c.noktalar.len(), 2);
    }
}
