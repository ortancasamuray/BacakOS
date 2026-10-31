//! Bacak Belge — the BacakOS document and picture viewer: PDF (MuPDF),
//! EPUB (as text) and pictures, with pen/highlighter notes on PDF pages and
//! pictures.

mod cizim;
mod metin;
mod yerlesim;

use anyhow::{anyhow, bail, Context, Result};
use slint::{Color, ComponentHandle, Image, Model, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use yerlesim::Sigdir;

slint::include_modules!();

const RESIM_UZANTILARI: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif", "ico", "qoi", "hdr", "pbm", "pgm", "ppm", "pnm",
];

/// Above this many pixels a PDF page is rendered smaller and scaled up
/// instead: a huge zoom must not take gigabytes or freeze the window.
const EN_COK_PIKSEL: f32 = 40.0e6;

/// Pen and highlighter: diameter in logical px on screen when the stroke is
/// drawn, and opacity. The highlighter's whole stroke is one path, so where
/// it crosses itself it does not get darker.
const KALEM: (f32, u8) = (4.0, 235);
const FOSFORLU: (f32, u8) = (24.0, 100);

fn main() -> Result<()> {
    let ui = AppWindow::new()?;
    // The EPUB reading mode follows the desktop's dark mode (BacakOsSDK);
    // its button switches it for this window only.
    ui.set_dark_mode(bacak_sdk::Gorunum::load().koyu);
    ui.on_dosya_yoneticisi(dosya_yoneticisini_ac);

    // Dock/menu launches pass no file: still show a window, never exit.
    let Some(arg) = std::env::args_os().nth(1).map(PathBuf::from) else {
        ui.set_mode("bos".into());
        ui.set_status("Açık belge yok. PDF, EPUB ya da resim dosyasını dosya yöneticisinde açın.".into());
        ui.run()?;
        return Ok(());
    };
    let path = arg.canonicalize().unwrap_or(arg);
    ui.set_filename(dosya_adi(&path).into());

    let sonuc = match tur(&path) {
        Some(Tur::Pdf) => pdf_ac(&ui, path),
        Some(Tur::Epub) => epub_ac(&ui, &path),
        Some(Tur::Resim) => resim_ac(&ui, path),
        None => Err(anyhow!("Bu dosya türü açılamıyor: {}", dosya_adi(&path))),
    };
    if let Err(e) = sonuc {
        hata_goster(&ui, &e);
    }
    ui.run()?;
    Ok(())
}

fn hata_goster(ui: &AppWindow, e: &anyhow::Error) {
    ui.set_mode("error".into());
    ui.set_status(format!("{e:#}").into());
}

fn dosya_adi(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn dosya_yoneticisini_ac() {
    let ev = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    let acildi = bacak_sdk::masaustu::uygulama("altay").is_some_and(|u| u.baslat(&[ev.as_path()]).is_ok());
    if !acildi {
        let _ = std::process::Command::new("altay").arg(&ev).spawn();
    }
}

enum Tur {
    Pdf,
    Epub,
    Resim,
}

fn tur(path: &Path) -> Option<Tur> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "pdf" => Some(Tur::Pdf),
        "epub" => Some(Tur::Epub),
        e if RESIM_UZANTILARI.contains(&e) => Some(Tur::Resim),
        _ => None,
    }
}

// ── View: zoom, fit, pan ────────────────────────────────────────────────────

/// The view's size; before the first layout the window's.
fn gorunum(ui: &AppWindow) -> (f32, f32) {
    let (w, h) = (ui.get_view_w(), ui.get_view_h());
    if w > 1.0 && h > 1.0 {
        return (w, h);
    }
    let s = ui.window().size().to_logical(ui.window().scale_factor());
    ((s.width).max(400.0), (s.height - 56.0).max(300.0))
}

struct Gorus {
    zoom: f32,
    kip: Sigdir,
    /// May the fit enlarge small content (PDF pages yes, pictures no)?
    buyutme: bool,
}

impl Gorus {
    /// Zoom for content of `boyut` (logical px at 100 %) in the current fit.
    fn uygula(&mut self, boyut: (f32, f32), view: (f32, f32)) {
        if let Some(z) = yerlesim::sigdir_zoom(boyut, view, self.kip, self.buyutme) {
            self.zoom = z;
        }
    }
}

/// Shows content of logical size `boyut` (at 100 %) at the current zoom and
/// keeps the pan inside the view.
fn olcu_yaz(ui: &AppWindow, g: &Gorus, boyut: (f32, f32)) {
    let (w, h) = (boyut.0 * g.zoom, boyut.1 * g.zoom);
    ui.set_icerik_w(w);
    ui.set_icerik_h(h);
    ui.set_zoom(g.zoom);
    ui.set_sigdir_kipi(g.kip.etiket().into());
    let (vw, vh) = gorunum(ui);
    ui.set_pan_x(yerlesim::sinirla(ui.get_pan_x(), w, vw));
    ui.set_pan_y(yerlesim::sinirla(ui.get_pan_y(), h, vh));
}

/// After a page change: horizontally centred, at the top of the page.
fn sayfa_basina(ui: &AppWindow) {
    ui.set_pan_x(0.0);
    ui.set_pan_y(f32::MAX);
}

/// Zoom buttons/keys keep the point at the view's centre where it was.
fn zoom_degisti(ui: &AppWindow, eski: f32, yeni: f32) {
    let k = yeni / eski.max(1e-3);
    ui.set_pan_x(ui.get_pan_x() * k);
    ui.set_pan_y(ui.get_pan_y() * k);
}

/// Re-fit after the window was resized, once it has settled.
fn yeniden_boyut_baglanti(ui: &AppWindow, mut yenile: impl FnMut() + 'static) {
    let zamanlayici = Rc::new(slint::Timer::default());
    let yenile = Rc::new(RefCell::new(move || yenile()));
    ui.on_view_resized(move || {
        let y = yenile.clone();
        zamanlayici.start(slint::TimerMode::SingleShot, Duration::from_millis(120), move || (y.borrow_mut())());
    });
}

// ── Notes ───────────────────────────────────────────────────────────────────

struct NotDurumu {
    notlar: cizim::Notlar,
    sayfa: usize,
    /// The page's height / width.
    oran: f32,
    model: Rc<VecModel<Cizgi>>,
    renk: [u8; 3],
    fosforlu: bool,
}

impl NotDurumu {
    fn new(ui: &AppWindow, notlar: cizim::Notlar) -> Rc<RefCell<NotDurumu>> {
        let model = Rc::new(VecModel::default());
        ui.set_cizgiler(model.clone().into());
        Rc::new(RefCell::new(NotDurumu { notlar, sayfa: 0, oran: 1.0, model, renk: [0xe5, 0x39, 0x35], fosforlu: false }))
    }

    fn ui_cizgi(&self, c: &cizim::Cizgi) -> Cizgi {
        let [r, g, b, a] = c.renk;
        Cizgi { komut: c.komut(self.oran).into(), renk: Color::from_argb_u8(a, r, g, b), kalinlik: c.kalinlik }
    }

    /// Shows the notes of `sayfa`, whose height/width is `oran`.
    fn sayfa(&mut self, ui: &AppWindow, sayfa: usize, oran: f32) {
        self.notlar.bitir();
        self.sayfa = sayfa;
        self.oran = oran;
        self.yenile(ui);
    }

    fn yenile(&self, ui: &AppWindow) {
        let v: Vec<Cizgi> = self.notlar.cizgiler(self.sayfa).iter().map(|c| self.ui_cizgi(c)).collect();
        self.model.set_vec(v);
        self.durum(ui);
    }

    fn durum(&self, ui: &AppWindow) {
        ui.set_not_var(self.notlar.var_mi(self.sayfa));
        ui.set_geri_alinabilir(self.notlar.geri_alinabilir(self.sayfa));
    }
}

fn notlari_bagla(ui: &AppWindow, st: &Rc<RefCell<NotDurumu>>) {
    let ui_h = ui.as_weak();
    let s = st.clone();
    ui.on_set_tool(move |arac| {
        let Some(ui) = ui_h.upgrade() else { return };
        // Tapping the active tool again leaves drawing mode.
        if ui.get_draw_mode() && ui.get_active_tool() == arac {
            ui.set_draw_mode(false);
            return;
        }
        let mut st = s.borrow_mut();
        st.fosforlu = arac.as_str() == "marker";
        // A highlighter is yellow unless chosen otherwise.
        if st.fosforlu && st.renk == [0xe5, 0x39, 0x35] {
            st.renk = [0xfd, 0xd8, 0x35];
        } else if !st.fosforlu && st.renk == [0xfd, 0xd8, 0x35] {
            st.renk = [0xe5, 0x39, 0x35];
        }
        let [r, g, b] = st.renk;
        ui.set_pen_color(Color::from_rgb_u8(r, g, b));
        ui.set_active_tool(arac);
        ui.set_draw_mode(true);
    });

    let ui_h = ui.as_weak();
    let s = st.clone();
    ui.on_pick_color(move |c| {
        s.borrow_mut().renk = [c.red(), c.green(), c.blue()];
        if let Some(ui) = ui_h.upgrade() {
            ui.set_pen_color(c);
        }
    });

    let s = st.clone();
    ui.on_draw_start(move |x, y, w| {
        let mut st = s.borrow_mut();
        let (cap, alfa) = if st.fosforlu { FOSFORLU } else { KALEM };
        let [r, g, b] = st.renk;
        let sayfa = st.sayfa;
        st.notlar.basla(sayfa, (x, y), [r, g, b, alfa], cap / w.max(1.0));
        let c = st.notlar.cizgiler(sayfa).last().map(|c| st.ui_cizgi(c));
        if let Some(c) = c {
            st.model.push(c);
        }
    });

    let s = st.clone();
    ui.on_draw_move(move |x, y, w| {
        let mut st = s.borrow_mut();
        // Points closer than ~0.7 px on screen add nothing but path length.
        let Some(c) = st.notlar.devam((x, y), 0.7 / w.max(1.0)).cloned() else { return };
        let ui_c = st.ui_cizgi(&c);
        let n = st.model.row_count();
        if n > 0 {
            st.model.set_row_data(n - 1, ui_c);
        }
    });

    let ui_h = ui.as_weak();
    let s = st.clone();
    ui.on_draw_end(move || {
        let mut st = s.borrow_mut();
        if st.notlar.bitir() {
            if let Some(ui) = ui_h.upgrade() {
                st.durum(&ui);
            }
        }
    });

    let ui_h = ui.as_weak();
    let s = st.clone();
    ui.on_undo(move || {
        let Some(ui) = ui_h.upgrade() else { return };
        let mut st = s.borrow_mut();
        let sayfa = st.sayfa;
        if st.notlar.geri_al(sayfa) {
            st.yenile(&ui);
        }
    });

    let ui_h = ui.as_weak();
    let s = st.clone();
    ui.on_clear_annotations(move || {
        let Some(ui) = ui_h.upgrade() else { return };
        let mut st = s.borrow_mut();
        let sayfa = st.sayfa;
        st.notlar.temizle(sayfa);
        st.yenile(&ui);
    });
}

// ── PDF ─────────────────────────────────────────────────────────────────────

const DPI: f32 = 96.0;

struct Pdf {
    doc: mupdf::Document,
    sayfa: i32,
    adet: i32,
    gorus: Gorus,
}

/// A page's size in logical px at 100 % (96 dpi).
fn pdf_sayfa_boyutu(doc: &mupdf::Document, i: i32) -> Result<(f32, f32)> {
    let b = doc.load_page(i)?.bounds()?;
    Ok(((b.x1 - b.x0) * DPI / 72.0, (b.y1 - b.y0) * DPI / 72.0))
}

/// Renders page `i` so that 1 pixel is 1 physical pixel at `zoom` on a
/// screen of `olcek` (HiDPI) — sharp, never blurry from upscaling.
fn pdf_ciz(doc: &mupdf::Document, i: i32, zoom: f32, olcek: f32) -> Result<Image> {
    let page = doc.load_page(i)?;
    let b = page.bounds()?;
    let mut s = zoom * olcek * DPI / 72.0;
    let piksel = (b.x1 - b.x0) * (b.y1 - b.y0) * s * s;
    if piksel > EN_COK_PIKSEL {
        s *= (EN_COK_PIKSEL / piksel).sqrt();
    }
    let px = page.to_pixmap(&mupdf::Matrix::new_scale(s, s), &mupdf::Colorspace::device_rgb(), 0.0, true)?;
    let (w, h, n) = (px.width(), px.height(), px.n() as usize);
    if n < 3 {
        bail!("beklenmeyen renk biçimi");
    }
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    for (d, s) in buf.make_mut_slice().iter_mut().zip(px.samples().chunks_exact(n)) {
        *d = Rgba8Pixel { r: s[0], g: s[1], b: s[2], a: 255 };
    }
    Ok(Image::from_rgba8(buf))
}

fn pdf_ac(ui: &AppWindow, path: PathBuf) -> Result<()> {
    let yol = path.to_str().context("dosya yolu UTF-8 değil")?;
    let doc = mupdf::Document::open(yol)
        .map_err(|e| anyhow!("PDF açılamadı: dosya bozuk ya da PDF değil.\n\n({e})"))?;
    let gerekli = doc.needs_password().unwrap_or(false);
    let pdf = Rc::new(RefCell::new(None::<Pdf>));
    let notlar = NotDurumu::new(ui, cizim::Notlar::ac(&path));
    notlari_bagla(ui, &notlar);

    let goster: Rc<dyn Fn()> = {
        let ui_h = ui.as_weak();
        let pdf = pdf.clone();
        let notlar = notlar.clone();
        Rc::new(move || {
            let Some(ui) = ui_h.upgrade() else { return };
            let mut p = pdf.borrow_mut();
            let Some(p) = p.as_mut() else { return };
            let sonuc = pdf_sayfa_boyutu(&p.doc, p.sayfa).and_then(|boyut| {
                p.gorus.uygula(boyut, gorunum(&ui));
                let img = pdf_ciz(&p.doc, p.sayfa, p.gorus.zoom, ui.window().scale_factor())?;
                Ok((boyut, img))
            });
            match sonuc {
                Ok((boyut, img)) => {
                    ui.set_content_image(img);
                    olcu_yaz(&ui, &p.gorus, boyut);
                    ui.set_page_num(p.sayfa + 1);
                    ui.set_page_count(p.adet);
                    ui.set_has_prev(p.sayfa > 0);
                    ui.set_has_next(p.sayfa + 1 < p.adet);
                    ui.set_status("".into());
                    ui.set_mode("pdf".into());
                    let mut n = notlar.borrow_mut();
                    if n.sayfa != p.sayfa as usize || n.model.row_count() == 0 {
                        n.sayfa(&ui, p.sayfa as usize, boyut.1 / boyut.0);
                    }
                }
                Err(e) => {
                    ui.set_content_image(Image::default());
                    ui.set_status(format!("Sayfa {} gösterilemedi: {e}", p.sayfa + 1).into());
                    ui.set_mode("pdf".into());
                }
            }
        })
    };

    // Opens the document once it can be read (after the password, if any).
    let baslat: Rc<dyn Fn(mupdf::Document) -> Result<()>> = {
        let pdf = pdf.clone();
        let goster = goster.clone();
        Rc::new(move |doc: mupdf::Document| {
            let adet = doc.page_count().map_err(|e| anyhow!("PDF okunamadı: {e}"))?;
            if adet <= 0 {
                bail!("Belgede gösterilecek sayfa yok.");
            }
            *pdf.borrow_mut() = Some(Pdf { doc, sayfa: 0, adet, gorus: Gorus { zoom: 1.0, kip: Sigdir::Sayfa, buyutme: true } });
            let g = goster.clone();
            // After the window is laid out, so the fit sees its real size.
            slint::Timer::single_shot(Duration::ZERO, move || g());
            Ok(())
        })
    };

    if gerekli {
        ui.set_mode("parola".into());
        let ui_h = ui.as_weak();
        let bekleyen = RefCell::new(Some(doc));
        let baslat = baslat.clone();
        ui.on_parola_gir(move |parola| {
            let Some(ui) = ui_h.upgrade() else { return };
            let mut b = bekleyen.borrow_mut();
            let Some(doc) = b.as_mut() else { return };
            match doc.authenticate(&parola) {
                Ok(true) => {
                    ui.set_parola_hatasi("".into());
                    ui.set_mode("loading".into());
                    let doc = b.take().expect("belge var");
                    if let Err(e) = baslat(doc) {
                        hata_goster(&ui, &e);
                    }
                }
                _ => ui.set_parola_hatasi("Parola yanlış.".into()),
            }
        });
    } else {
        baslat(doc)?;
    }

    let sayfaya_git = {
        let pdf = pdf.clone();
        let goster = goster.clone();
        let ui_h = ui.as_weak();
        move |hedef: Box<dyn Fn(i32, i32) -> i32>| {
            let degisti = {
                let mut p = pdf.borrow_mut();
                let Some(p) = p.as_mut() else { return };
                let yeni = hedef(p.sayfa, p.adet).clamp(0, p.adet - 1);
                let d = yeni != p.sayfa;
                p.sayfa = yeni;
                d
            };
            if degisti {
                if let Some(ui) = ui_h.upgrade() {
                    sayfa_basina(&ui);
                }
                goster();
            }
        }
    };
    let git = Rc::new(sayfaya_git);
    let g = git.clone();
    ui.on_next_action(move || g(Box::new(|s, _| s + 1)));
    let g = git.clone();
    ui.on_prev_action(move || g(Box::new(|s, _| s - 1)));
    let g = git.clone();
    ui.on_first_page(move || g(Box::new(|_, _| 0)));
    let g = git.clone();
    ui.on_last_page(move || g(Box::new(|_, n| n - 1)));

    let zoomla = {
        let pdf = pdf.clone();
        let goster = goster.clone();
        let ui_h = ui.as_weak();
        Rc::new(move |yeni: Box<dyn Fn(&mut Gorus)>| {
            let (eski, z) = {
                let mut p = pdf.borrow_mut();
                let Some(p) = p.as_mut() else { return };
                let eski = p.gorus.zoom;
                yeni(&mut p.gorus);
                (eski, p.gorus.zoom)
            };
            if let Some(ui) = ui_h.upgrade() {
                zoom_degisti(&ui, eski, z);
            }
            goster();
        })
    };
    let z = zoomla.clone();
    ui.on_zoom_in(move || z(Box::new(|g| {
        g.zoom = yerlesim::adim(g.zoom, true);
        g.kip = Sigdir::Serbest;
    })));
    let z = zoomla.clone();
    ui.on_zoom_out(move || z(Box::new(|g| {
        g.zoom = yerlesim::adim(g.zoom, false);
        g.kip = Sigdir::Serbest;
    })));
    let z = zoomla.clone();
    ui.on_pinch_ended(move |v| z(Box::new(move |g| {
        g.zoom = v.clamp(yerlesim::ZOOM_MIN, yerlesim::ZOOM_MAX);
        g.kip = Sigdir::Serbest;
    })));
    let z = zoomla.clone();
    let ui_h = ui.as_weak();
    ui.on_fit(move || {
        if let Some(ui) = ui_h.upgrade() {
            sayfa_basina(&ui);
        }
        z(Box::new(|g| g.kip = g.kip.sonraki()));
    });

    let g = goster.clone();
    let p = pdf.clone();
    yeniden_boyut_baglanti(ui, move || {
        // Only a fitted page changes with the window.
        if p.borrow().as_ref().is_some_and(|p| p.gorus.kip != Sigdir::Serbest) {
            g();
        }
    });
    Ok(())
}

// ── Pictures ────────────────────────────────────────────────────────────────

/// The pictures in the same folder, in natural order.
fn kardes_resimler(path: &Path) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| matches!(tur(p), Some(Tur::Resim)) && p.is_file())
        .collect();
    if !v.iter().any(|p| p == path) {
        v.push(path.to_path_buf());
    }
    v.sort_by(|a, b| yerlesim::dogal_sira(&dosya_adi(a), &dosya_adi(b)));
    v
}

/// Decodes a picture upright: phone photos carry their rotation in EXIF.
fn resim_yukle(path: &Path) -> Result<(Image, (u32, u32))> {
    use image::ImageDecoder;
    let mut dec = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let yon = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec)?;
    img.apply_orientation(yon);
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), w, h);
    Ok((Image::from_rgba8(buf), (w, h)))
}

struct Resimler {
    liste: Vec<PathBuf>,
    sira: usize,
    boyut: (f32, f32),
    gorus: Gorus,
}

fn resim_ac(ui: &AppWindow, path: PathBuf) -> Result<()> {
    let liste = kardes_resimler(&path);
    let sira = liste.iter().position(|p| *p == path).unwrap_or(0);
    let r = Rc::new(RefCell::new(Resimler {
        liste,
        sira,
        boyut: (0.0, 0.0),
        gorus: Gorus { zoom: 1.0, kip: Sigdir::Sayfa, buyutme: false },
    }));
    let notlar = NotDurumu::new(ui, cizim::Notlar::default());
    notlari_bagla(ui, &notlar);
    ui.set_mode("image".into());

    let yukle = {
        let ui_h = ui.as_weak();
        let r = r.clone();
        let notlar = notlar.clone();
        Rc::new(move || {
            let Some(ui) = ui_h.upgrade() else { return };
            let mut r = r.borrow_mut();
            let path = r.liste[r.sira].clone();
            let (sira, adet) = (r.sira, r.liste.len());
            ui.set_filename(dosya_adi(&path).into());
            ui.set_has_prev(sira > 0);
            ui.set_has_next(sira + 1 < adet);
            ui.set_pan_x(0.0);
            ui.set_pan_y(0.0);
            ui.set_draw_mode(false);
            let mut n = notlar.borrow_mut();
            n.notlar.bitir();
            match resim_yukle(&path) {
                Ok((img, (w, h))) => {
                    r.boyut = (w as f32, h as f32);
                    r.gorus.kip = Sigdir::Sayfa;
                    let boyut = r.boyut;
                    r.gorus.uygula(boyut, gorunum(&ui));
                    ui.set_content_image(img);
                    olcu_yaz(&ui, &r.gorus, boyut);
                    ui.set_status(format!("{w}×{h}   {}/{adet}", sira + 1).into());
                    n.notlar = cizim::Notlar::ac(&path);
                    n.sayfa(&ui, 0, h as f32 / w.max(1) as f32);
                }
                Err(e) => {
                    ui.set_content_image(Image::default());
                    ui.set_status(format!("Resim açılamadı: {e}   {}/{adet}", sira + 1).into());
                    n.notlar = cizim::Notlar::default();
                    n.sayfa(&ui, 0, 1.0);
                }
            }
        })
    };
    let y = yukle.clone();
    slint::Timer::single_shot(Duration::ZERO, move || y());

    let git = {
        let r = r.clone();
        let yukle = yukle.clone();
        Rc::new(move |hedef: Box<dyn Fn(usize, usize) -> usize>| {
            let degisti = {
                let mut r = r.borrow_mut();
                let yeni = hedef(r.sira, r.liste.len()).min(r.liste.len() - 1);
                let d = yeni != r.sira;
                r.sira = yeni;
                d
            };
            if degisti {
                yukle();
            }
        })
    };
    let g = git.clone();
    ui.on_next_action(move || g(Box::new(|s, _| s + 1)));
    let g = git.clone();
    ui.on_prev_action(move || g(Box::new(|s, _| s.saturating_sub(1))));
    let g = git.clone();
    ui.on_first_page(move || g(Box::new(|_, _| 0)));
    let g = git.clone();
    ui.on_last_page(move || g(Box::new(|_, n| n.saturating_sub(1))));

    let zoomla = {
        let r = r.clone();
        let ui_h = ui.as_weak();
        Rc::new(move |yeni: Box<dyn Fn(&mut Gorus)>| {
            let Some(ui) = ui_h.upgrade() else { return };
            let mut r = r.borrow_mut();
            let eski = r.gorus.zoom;
            yeni(&mut r.gorus);
            let boyut = r.boyut;
            r.gorus.uygula(boyut, gorunum(&ui));
            zoom_degisti(&ui, eski, r.gorus.zoom);
            olcu_yaz(&ui, &r.gorus, boyut);
        })
    };
    let z = zoomla.clone();
    ui.on_zoom_in(move || z(Box::new(|g| {
        g.zoom = yerlesim::adim(g.zoom, true);
        g.kip = Sigdir::Serbest;
    })));
    let z = zoomla.clone();
    ui.on_zoom_out(move || z(Box::new(|g| {
        g.zoom = yerlesim::adim(g.zoom, false);
        g.kip = Sigdir::Serbest;
    })));
    let z = zoomla.clone();
    ui.on_pinch_ended(move |v| z(Box::new(move |g| {
        g.zoom = v.clamp(yerlesim::ZOOM_MIN, yerlesim::ZOOM_MAX);
        g.kip = Sigdir::Serbest;
    })));
    let z = zoomla.clone();
    ui.on_fit(move || z(Box::new(|g| {
        // A picture: whole picture ↔ 100 %.
        g.kip = if g.kip == Sigdir::Sayfa { Sigdir::Serbest } else { Sigdir::Sayfa };
        if g.kip == Sigdir::Serbest {
            g.zoom = 1.0;
        }
    })));

    let z = zoomla.clone();
    yeniden_boyut_baglanti(ui, move || z(Box::new(|_| {})));
    Ok(())
}

// ── EPUB ────────────────────────────────────────────────────────────────────

/// Chapter titles from the table of contents; a spine item without an entry
/// of its own (a chapter split into files) keeps the title before it.
fn bolum_basliklari(book: &epub::doc::EpubDoc<std::io::BufReader<std::fs::File>>) -> Vec<String> {
    fn duzle(v: &[epub::doc::NavPoint], out: &mut Vec<(String, PathBuf)>) {
        for n in v {
            let yol = n.content.to_string_lossy();
            let yol = yol.split('#').next().unwrap_or("").to_string();
            out.push((n.label.trim().to_string(), PathBuf::from(yol)));
            duzle(&n.children, out);
        }
    }
    let mut girdiler = Vec::new();
    duzle(&book.toc, &mut girdiler);
    let mut basliklar = vec![String::new(); book.get_num_chapters()];
    for (ad, yol) in girdiler {
        if let Some(i) = book.resource_uri_to_chapter(&yol) {
            if basliklar[i].is_empty() && !ad.is_empty() {
                basliklar[i] = ad;
            }
        }
    }
    let mut onceki = String::new();
    for b in basliklar.iter_mut() {
        if b.is_empty() {
            b.clone_from(&onceki);
        } else {
            onceki.clone_from(b);
        }
    }
    basliklar
}

fn epub_ac(ui: &AppWindow, path: &Path) -> Result<()> {
    let book = epub::doc::EpubDoc::new(path)
        .map_err(|e| anyhow!("EPUB açılamadı: dosya bozuk ya da EPUB değil.\n\n({e})"))?;
    let adet = book.get_num_chapters();
    if adet == 0 {
        bail!("Kitapta okunacak bölüm yok.");
    }
    let baslik = book
        .mdata("title")
        .map(|m| m.value.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    let basliklar = bolum_basliklari(&book);
    ui.set_filename(baslik.into());
    ui.set_mode("epub".into());

    let book = Rc::new(RefCell::new(book));
    let sira = Rc::new(Cell::new(0usize));
    let yukle = {
        let ui_h = ui.as_weak();
        let book = book.clone();
        let sira = sira.clone();
        Rc::new(move |i: usize| {
            let Some(ui) = ui_h.upgrade() else { return };
            let i = i.min(adet - 1);
            sira.set(i);
            let mut b = book.borrow_mut();
            b.set_current_chapter(i);
            let metin = b
                .get_current_str()
                .map(|(html, _)| metin::html_to_text(&html))
                .unwrap_or_else(|| "(Bu bölüm okunamadı.)".to_string());
            ui.set_chapter_title(basliklar[i].clone().into());
            ui.set_body_text(metin.into());
            ui.set_status(format!("Bölüm {}/{adet}", i + 1).into());
            ui.set_has_prev(i > 0);
            ui.set_has_next(i + 1 < adet);
        })
    };
    let y = yukle.clone();
    slint::Timer::single_shot(Duration::ZERO, move || y(0));

    let (y, s) = (yukle.clone(), sira.clone());
    ui.on_next_action(move || y(s.get() + 1));
    let (y, s) = (yukle.clone(), sira.clone());
    ui.on_prev_action(move || y(s.get().saturating_sub(1)));
    let y = yukle.clone();
    ui.on_first_page(move || y(0));
    let y = yukle.clone();
    ui.on_last_page(move || y(adet - 1));

    let ui_h = ui.as_weak();
    ui.on_font_larger(move || {
        if let Some(ui) = ui_h.upgrade() {
            ui.set_font_size((ui.get_font_size() + 2).min(40));
        }
    });
    let ui_h = ui.as_weak();
    ui.on_font_smaller(move || {
        if let Some(ui) = ui_h.upgrade() {
            ui.set_font_size((ui.get_font_size() - 2).max(10));
        }
    });
    let ui_h = ui.as_weak();
    ui.on_toggle_dark(move || {
        if let Some(ui) = ui_h.upgrade() {
            ui.set_dark_mode(!ui.get_dark_mode());
        }
    });
    let ui_h = ui.as_weak();
    ui.on_pinch_ended(move |olcek| {
        if let Some(ui) = ui_h.upgrade() {
            ui.set_font_size(((ui.get_font_size() as f32 * olcek).round() as i32).clamp(10, 40));
        }
    });
    Ok(())
}
