//! Pen and highlighter notes, kept as vector strokes in page coordinates.
//!
//! A point is stored as a fraction of the page's width and height and a
//! stroke's width as a fraction of the page's width, so notes stay on the
//! words they mark whatever the zoom, pan or window size, and each page (or
//! picture) has its own. The UI draws every stroke as a `Path`.
//!
//! Notes are saved next to nothing the user owns: one JSON file per
//! document under `$XDG_DATA_HOME/bacak-belge/notlar/`, written after every
//! change, so they are back the next time the document is opened.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Width of the path viewbox; its height is this times the page's h/w.
pub const VIEWBOX_W: f32 = 1000.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cizgi {
    /// Points, as fractions of the page width/height.
    #[serde(rename = "n")]
    pub noktalar: Vec<(f32, f32)>,
    /// RGBA.
    #[serde(rename = "r")]
    pub renk: [u8; 4],
    /// Stroke width as a fraction of the page width.
    #[serde(rename = "k")]
    pub kalinlik: f32,
}

impl Cizgi {
    /// SVG path commands in a `VIEWBOX_W × VIEWBOX_W*oran` viewbox.
    pub fn komut(&self, oran: f32) -> String {
        let mut s = String::with_capacity(self.noktalar.len() * 14);
        let sy = VIEWBOX_W * oran;
        for (i, (x, y)) in self.noktalar.iter().enumerate() {
            let (x, y) = (x * VIEWBOX_W, y * sy);
            s.push_str(if i == 0 { "M " } else { " L " });
            s.push_str(&format!("{x:.1} {y:.1}"));
        }
        // A tap is one point: a hair-long segment so the round cap shows a dot.
        if let [(x, y)] = self.noktalar[..] {
            s.push_str(&format!(" L {:.1} {:.1}", x * VIEWBOX_W + 0.1, y * sy));
        }
        s
    }
}

enum Islem {
    Eklendi,
    Silindi(Vec<Cizgi>),
}

/// The notes of one document.
#[derive(Default)]
pub struct Notlar {
    sayfalar: BTreeMap<usize, Vec<Cizgi>>,
    /// Per page, what "undo" reverts.
    gecmis: BTreeMap<usize, Vec<Islem>>,
    /// The stroke being drawn: its page.
    suren: Option<usize>,
    dosya: Option<PathBuf>,
    /// The document's path, written into the note file for whoever looks.
    belge: String,
}

#[derive(Serialize, Deserialize)]
struct Kayit {
    belge: String,
    sayfalar: BTreeMap<usize, Vec<Cizgi>>,
}

impl Notlar {
    /// Notes for `belge`, read back from where they were saved (empty if none).
    pub fn ac(belge: &Path) -> Notlar {
        let dosya = kayit_yolu(belge);
        let sayfalar = dosya
            .as_deref()
            .and_then(|d| std::fs::read(d).ok())
            .and_then(|b| serde_json::from_slice::<Kayit>(&b).ok())
            .map(|k| k.sayfalar)
            .unwrap_or_default();
        Notlar { sayfalar, dosya, belge: belge.to_string_lossy().into_owned(), ..Default::default() }
    }

    /// In memory only (tests).
    #[cfg(test)]
    pub fn bellekte() -> Notlar {
        Notlar::default()
    }

    pub fn cizgiler(&self, sayfa: usize) -> &[Cizgi] {
        self.sayfalar.get(&sayfa).map_or(&[], Vec::as_slice)
    }

    pub fn var_mi(&self, sayfa: usize) -> bool {
        !self.cizgiler(sayfa).is_empty()
    }

    pub fn geri_alinabilir(&self, sayfa: usize) -> bool {
        self.gecmis.get(&sayfa).is_some_and(|g| !g.is_empty())
    }

    pub fn basla(&mut self, sayfa: usize, nokta: (f32, f32), renk: [u8; 4], kalinlik: f32) {
        self.bitir();
        let c = Cizgi { noktalar: vec![sinirli(nokta)], renk, kalinlik };
        self.sayfalar.entry(sayfa).or_default().push(c);
        self.suren = Some(sayfa);
    }

    /// Extends the running stroke; `None` if the point was too close to the
    /// last one to matter (`esik`, as a fraction of the page width).
    pub fn devam(&mut self, nokta: (f32, f32), esik: f32) -> Option<&Cizgi> {
        let sayfa = self.suren?;
        let c = self.sayfalar.get_mut(&sayfa)?.last_mut()?;
        let n = sinirli(nokta);
        let son = *c.noktalar.last()?;
        if (n.0 - son.0).hypot(n.1 - son.1) < esik {
            return None;
        }
        c.noktalar.push(n);
        Some(c)
    }

    /// Ends the running stroke. Returns whether one was ended.
    pub fn bitir(&mut self) -> bool {
        let Some(sayfa) = self.suren.take() else { return false };
        self.gecmis.entry(sayfa).or_default().push(Islem::Eklendi);
        self.kaydet();
        true
    }

    pub fn temizle(&mut self, sayfa: usize) {
        self.bitir();
        if let Some(v) = self.sayfalar.remove(&sayfa).filter(|v| !v.is_empty()) {
            self.gecmis.entry(sayfa).or_default().push(Islem::Silindi(v));
            self.kaydet();
        }
    }

    pub fn geri_al(&mut self, sayfa: usize) -> bool {
        self.bitir();
        let Some(islem) = self.gecmis.get_mut(&sayfa).and_then(Vec::pop) else { return false };
        match islem {
            Islem::Eklendi => {
                if let Some(v) = self.sayfalar.get_mut(&sayfa) {
                    v.pop();
                }
            }
            Islem::Silindi(v) => {
                self.sayfalar.insert(sayfa, v);
            }
        }
        self.kaydet();
        true
    }

    fn kaydet(&self) {
        let Some(dosya) = &self.dosya else { return };
        self.sayfalar_kaydet(dosya);
    }

    fn sayfalar_kaydet(&self, dosya: &Path) {
        let sayfalar: BTreeMap<usize, Vec<Cizgi>> =
            self.sayfalar.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (*k, v.clone())).collect();
        if sayfalar.is_empty() {
            let _ = std::fs::remove_file(dosya);
            return;
        }
        let kayit = Kayit { belge: self.belge.clone(), sayfalar };
        let Ok(json) = serde_json::to_vec(&kayit) else { return };
        if let Some(d) = dosya.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = dosya.with_extension("tmp");
        if std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, dosya)).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn sinirli((x, y): (f32, f32)) -> (f32, f32) {
    (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
}

/// `$XDG_DATA_HOME/bacak-belge/notlar/<hash of the document path>.json`.
fn kayit_yolu(belge: &Path) -> Option<PathBuf> {
    let veri = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(veri.join("bacak-belge/notlar").join(format!("{:016x}.json", fnv1a(belge.as_os_str().as_encoded_bytes()))))
}

/// Stable across builds (unlike `DefaultHasher`), so notes keep their file.
fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, x| (h ^ *x as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIRMIZI: [u8; 4] = [255, 0, 0, 230];

    #[test]
    fn strokes_belong_to_their_page() {
        let mut n = Notlar::bellekte();
        n.basla(0, (0.1, 0.1), KIRMIZI, 0.01);
        n.devam((0.5, 0.5), 0.001);
        n.bitir();
        assert_eq!(n.cizgiler(0).len(), 1);
        assert!(n.cizgiler(1).is_empty(), "başka sayfada görünmemeli");
        assert_eq!(n.cizgiler(0)[0].noktalar, vec![(0.1, 0.1), (0.5, 0.5)]);
    }

    #[test]
    fn close_points_are_dropped_and_points_clamped() {
        let mut n = Notlar::bellekte();
        n.basla(0, (-0.5, 0.2), KIRMIZI, 0.01);
        assert!(n.devam((0.0001, 0.2), 0.01).is_none());
        assert!(n.devam((0.5, 1.7), 0.01).is_some());
        assert_eq!(n.cizgiler(0)[0].noktalar, vec![(0.0, 0.2), (0.5, 1.0)]);
    }

    #[test]
    fn undo_reverts_strokes_and_clearing() {
        let mut n = Notlar::bellekte();
        for i in 0..2 {
            n.basla(3, (0.1 * i as f32, 0.1), KIRMIZI, 0.01);
            n.bitir();
        }
        n.temizle(3);
        assert!(!n.var_mi(3));
        assert!(n.geri_al(3), "temizleme geri alınabilmeli");
        assert_eq!(n.cizgiler(3).len(), 2);
        assert!(n.geri_al(3));
        assert_eq!(n.cizgiler(3).len(), 1);
        assert!(n.geri_al(3));
        assert!(!n.geri_al(3));
        assert!(!n.geri_alinabilir(3));
    }

    #[test]
    fn path_commands_use_the_page_aspect() {
        let c = Cizgi { noktalar: vec![(0.0, 0.0), (1.0, 1.0)], renk: KIRMIZI, kalinlik: 0.01 };
        assert_eq!(c.komut(1.5), "M 0.0 0.0 L 1000.0 1500.0");
        let nokta = Cizgi { noktalar: vec![(0.5, 0.5)], ..c };
        assert_eq!(nokta.komut(1.0), "M 500.0 500.0 L 500.1 500.0");
    }

    #[test]
    fn notes_survive_reopening() {
        let dir = std::env::temp_dir().join(format!("bacak-belge-test-{}", std::process::id()));
        let dosya = dir.join("not.json");
        let mut n = Notlar { dosya: Some(dosya.clone()), ..Default::default() };
        n.basla(2, (0.25, 0.75), KIRMIZI, 0.02);
        n.devam((0.5, 0.5), 0.0);
        n.bitir();
        let b = std::fs::read(&dosya).unwrap();
        let k: Kayit = serde_json::from_slice(&b).unwrap();
        assert_eq!(k.sayfalar[&2], n.cizgiler(2));
        n.temizle(2);
        assert!(!dosya.exists(), "notu kalmayan belgenin dosyası silinmeli");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn note_file_name_is_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"/a/b.pdf"), fnv1a(b"/a/b.pdf"));
        assert_ne!(fnv1a(b"/a/b.pdf"), fnv1a(b"/a/c.pdf"));
    }
}
