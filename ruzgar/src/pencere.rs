//! The browser window: a stack of web views (one per tab) with the ink
//! layer over them, and at the bottom the tab strip and the address bar.
//!
//! Drawing: the ink layer is always shown but only takes input while the
//! pen button is on (then the page underneath can't be clicked or
//! scrolled); off, touches go to the page as usual. Strokes are kept per
//! tab and per page address, in page coordinates — a small script in each
//! page reports its scroll position, so the ink moves with the content.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use webkit::prelude::*;

use crate::adres;
use crate::cizim::{self, Arac, Cizgi, Kalem};

/// Reports the page's scroll position to `ruzgarKaydirma` on every scroll
/// (top frame only — the ink is laid over the whole view).
const KAYDIRMA_BETIGI: &str = r#"
(function () {
  function bildir() {
    window.webkit.messageHandlers.ruzgarKaydirma.postMessage([window.scrollX, window.scrollY]);
  }
  addEventListener('scroll', bildir, { passive: true });
  addEventListener('load', bildir);
  addEventListener('pageshow', bildir);
  bildir();
})();
"#;

const STIL: &str = r#"
.alt-cubuk { background: #1f2229; padding: 6px; }
.alt-cubuk button, .araclar button { background: #353a45; color: #e8eaf0; border: none; box-shadow: none; }
.alt-cubuk button:hover, .araclar button:hover { background: #434957; }
.alt-cubuk button:disabled { color: #6b7180; }
.alt-cubuk button, .alt-cubuk entry { min-height: 48px; min-width: 48px; font-size: 18px; }
.alt-cubuk entry { background: #2a2e37; color: #f2f4f8; border-radius: 24px; padding-left: 16px; padding-right: 16px; }
.sekmeler { background: #2a2e37; padding: 4px 6px 0 6px; }
.sekme { background: #353a45; border-radius: 10px 10px 0 0; margin-right: 4px; }
.sekme.etkin { background: #1f2229; }
.sekme button { min-height: 44px; background: none; }
.sekme label { font-size: 15px; }
.yeni-sekme { min-height: 44px; min-width: 52px; }
.araclar { background: rgba(31, 34, 41, 0.94); border-radius: 18px; padding: 10px; }
.araclar button { min-width: 56px; min-height: 52px; font-size: 15px; }
.araclar button.secili, button.cizim-acik { background: #3b82f6; color: white; }
.araclar button.renk { border-radius: 26px; min-width: 44px; min-height: 44px; border: 3px solid transparent; }
.araclar button.renk.secili { border-color: white; }
.araclar button.renk-0 { background: #1a1a1f; }
.araclar button.renk-1 { background: #db2626; }
.araclar button.renk-2 { background: #1f66e6; }
.araclar button.renk-3 { background: #1a9e40; }
.araclar button.renk-4 { background: #f58c0d; }
"#;

pub fn stil_yukle() {
    let css = gtk::CssProvider::new();
    css.load_from_string(STIL);
    if let Some(ekran) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&ekran, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

struct Sekme {
    web: webkit::WebView,
    kutu: gtk::Box,
    etiket: gtk::Label,
    /// Ink per page address (`adres::sayfa_anahtari`), so going back to a
    /// page brings its drawing back.
    cizimler: RefCell<HashMap<String, Vec<Cizgi>>>,
    /// Undo steps for the page on screen (dropped on navigation).
    gecmis: RefCell<Vec<Vec<Cizgi>>>,
    kaydirma: Cell<(f64, f64)>,
}

impl Sekme {
    fn anahtar(&self) -> String {
        adres::sayfa_anahtari(&self.web.uri().map(|u| u.to_string()).unwrap_or_default()).to_string()
    }
}

pub struct Tarayici {
    pencere: gtk::ApplicationWindow,
    yigin: gtk::Stack,
    cizim_alani: gtk::DrawingArea,
    araclar: gtk::Box,
    alt: gtk::Box,
    sekme_kutusu: gtk::Box,
    adres: gtk::Entry,
    geri: gtk::Button,
    ileri: gtk::Button,
    yenile: gtk::Button,
    cizim_dugmesi: gtk::ToggleButton,
    sekmeler: RefCell<Vec<Rc<Sekme>>>,
    etkin: Cell<usize>,
    kalem: Cell<Kalem>,
    canli: RefCell<Option<Cizgi>>,
    arac_dugmeleri: RefCell<Vec<(gtk::Button, Arac)>>,
    renk_dugmeleri: RefCell<Vec<(gtk::Button, [f64; 3])>>,
    kalinlik_dugmeleri: RefCell<Vec<(gtk::Button, f64)>>,
}

thread_local! {
    static TARAYICI: RefCell<Option<Rc<Tarayici>>> = const { RefCell::new(None) };
}

impl Tarayici {
    /// The session's browser window, created on first use.
    pub fn al(app: &gtk::Application) -> Rc<Tarayici> {
        if let Some(t) = TARAYICI.with(|t| t.borrow().clone()) {
            return t;
        }
        let t = Self::olustur(app);
        TARAYICI.with(|s| *s.borrow_mut() = Some(t.clone()));
        t
    }

    pub fn goster(&self) {
        self.pencere.present();
    }

    pub fn sekme_sayisi(&self) -> usize {
        self.sekmeler.borrow().len()
    }

    fn olustur(app: &gtk::Application) -> Rc<Tarayici> {
        let pencere = gtk::ApplicationWindow::builder().application(app).title("Rüzgar").default_width(1280).default_height(800).build();
        pencere.maximize();

        let yigin = gtk::Stack::new();
        yigin.set_hexpand(true);
        yigin.set_vexpand(true);
        let cizim_alani = gtk::DrawingArea::new();
        cizim_alani.set_can_target(false);
        let araclar = gtk::Box::new(gtk::Orientation::Vertical, 6);
        araclar.add_css_class("araclar");
        araclar.set_halign(gtk::Align::End);
        araclar.set_valign(gtk::Align::Center);
        araclar.set_margin_end(14);
        araclar.set_visible(false);
        let katman = gtk::Overlay::new();
        katman.set_child(Some(&yigin));
        katman.add_overlay(&cizim_alani);
        katman.add_overlay(&araclar);

        let sekme_kutusu = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let yeni = gtk::Button::from_icon_name("tab-new-symbolic");
        yeni.add_css_class("yeni-sekme");
        yeni.set_tooltip_text(Some("Yeni sekme"));
        let sekme_kaydir = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&sekme_kutusu)
            .build();
        let sekme_cubugu = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        sekme_cubugu.add_css_class("sekmeler");
        sekme_cubugu.append(&sekme_kaydir);
        sekme_cubugu.append(&yeni);

        let geri = gtk::Button::from_icon_name("go-previous-symbolic");
        let ileri = gtk::Button::from_icon_name("go-next-symbolic");
        let yenile = gtk::Button::from_icon_name("view-refresh-symbolic");
        let ev = gtk::Button::from_icon_name("go-home-symbolic");
        geri.set_tooltip_text(Some("Geri"));
        ileri.set_tooltip_text(Some("İleri"));
        yenile.set_tooltip_text(Some("Yenile"));
        ev.set_tooltip_text(Some("Ana sayfa"));
        let adres = gtk::Entry::builder().hexpand(true).placeholder_text("Adres yazın ya da arayın").input_purpose(gtk::InputPurpose::Url).build();
        let cizim_dugmesi = gtk::ToggleButton::builder().label("✎  Çiz").tooltip_text("Sayfanın üzerine yaz ve çiz").build();
        let alt_satir = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for w in [geri.upcast_ref::<gtk::Widget>(), ileri.upcast_ref(), yenile.upcast_ref(), ev.upcast_ref(), adres.upcast_ref(), cizim_dugmesi.upcast_ref()] {
            alt_satir.append(w);
        }
        let alt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        alt.add_css_class("alt-cubuk");
        alt.append(&sekme_cubugu);
        alt.append(&alt_satir);

        let kok = gtk::Box::new(gtk::Orientation::Vertical, 0);
        kok.append(&katman);
        kok.append(&alt);
        pencere.set_child(Some(&kok));

        let t = Rc::new(Tarayici {
            pencere,
            yigin,
            cizim_alani,
            araclar,
            alt,
            sekme_kutusu,
            adres,
            geri,
            ileri,
            yenile,
            cizim_dugmesi,
            sekmeler: RefCell::default(),
            etkin: Cell::new(0),
            kalem: Cell::new(Kalem::default()),
            canli: RefCell::default(),
            arac_dugmeleri: RefCell::default(),
            renk_dugmeleri: RefCell::default(),
            kalinlik_dugmeleri: RefCell::default(),
        });
        t.araclari_kur();
        t.baglantilari_kur(&yeni, &ev);
        t.cizimi_kur();
        t
    }

    fn baglantilari_kur(self: &Rc<Self>, yeni: &gtk::Button, ev: &gtk::Button) {
        let w = Rc::downgrade(self);
        yeni.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.yeni_sekme(adres::ANASAYFA);
                t.adres.grab_focus();
            }
        });
        let w = Rc::downgrade(self);
        ev.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.ac(adres::ANASAYFA);
            }
        });
        let w = Rc::downgrade(self);
        self.geri.connect_clicked(move |_| {
            if let Some(s) = w.upgrade().and_then(|t| t.etkin_sekme()) {
                s.web.go_back();
            }
        });
        let w = Rc::downgrade(self);
        self.ileri.connect_clicked(move |_| {
            if let Some(s) = w.upgrade().and_then(|t| t.etkin_sekme()) {
                s.web.go_forward();
            }
        });
        let w = Rc::downgrade(self);
        self.yenile.connect_clicked(move |_| {
            if let Some(s) = w.upgrade().and_then(|t| t.etkin_sekme()) {
                if s.web.is_loading() {
                    s.web.stop_loading();
                } else {
                    s.web.reload();
                }
            }
        });
        let w = Rc::downgrade(self);
        self.adres.connect_activate(move |e| {
            if let Some(t) = w.upgrade() {
                t.ac(&adres::cozumle(&e.text()));
            }
        });
        let w = Rc::downgrade(self);
        self.cizim_dugmesi.connect_toggled(move |d| {
            if let Some(t) = w.upgrade() {
                let acik = d.is_active();
                t.cizim_alani.set_can_target(acik);
                t.araclar.set_visible(acik);
                if acik {
                    d.add_css_class("cizim-acik");
                } else {
                    d.remove_css_class("cizim-acik");
                }
            }
        });
    }

    /// Loads `uri` in the current tab (a new one if there's none).
    fn ac(self: &Rc<Self>, uri: &str) {
        match self.etkin_sekme() {
            Some(s) => {
                s.web.load_uri(uri);
                s.web.grab_focus();
            }
            None => {
                self.yeni_sekme(uri);
            }
        }
    }

    fn etkin_sekme(&self) -> Option<Rc<Sekme>> {
        self.sekmeler.borrow().get(self.etkin.get()).cloned()
    }

    fn web_olustur(&self, iliskili: Option<&webkit::WebView>) -> webkit::WebView {
        let icerik = webkit::UserContentManager::new();
        icerik.add_script(&webkit::UserScript::new(
            KAYDIRMA_BETIGI,
            webkit::UserContentInjectedFrames::TopFrame,
            webkit::UserScriptInjectionTime::End,
            &[],
            &[],
        ));
        icerik.register_script_message_handler("ruzgarKaydirma", None);
        let mut b = webkit::WebView::builder().user_content_manager(&icerik);
        if let Some(r) = iliskili {
            b = b.related_view(r);
        }
        let web = b.build();
        if let Some(ayar) = WebViewExt::settings(&web) {
            ayar.set_enable_smooth_scrolling(true);
            ayar.set_enable_back_forward_navigation_gestures(true);
        }
        web
    }

    /// Opens `uri` in a new tab and switches to it.
    pub fn yeni_sekme(self: &Rc<Self>, uri: &str) -> webkit::WebView {
        let web = self.web_olustur(None);
        self.sekme_ekle(web.clone());
        web.load_uri(uri);
        web
    }

    fn sekme_ekle(self: &Rc<Self>, web: webkit::WebView) {
        let etiket = gtk::Label::builder().label("Yeni sekme").ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(22).width_chars(10).build();
        let sec = gtk::Button::builder().child(&etiket).build();
        let kapat = gtk::Button::from_icon_name("window-close-symbolic");
        kapat.set_tooltip_text(Some("Sekmeyi kapat"));
        let kutu = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        kutu.add_css_class("sekme");
        kutu.append(&sec);
        kutu.append(&kapat);
        self.sekme_kutusu.append(&kutu);
        self.yigin.add_child(&web);

        let sekme = Rc::new(Sekme {
            web: web.clone(),
            kutu,
            etiket,
            cizimler: RefCell::default(),
            gecmis: RefCell::default(),
            kaydirma: Cell::new((0.0, 0.0)),
        });
        self.sekmeler.borrow_mut().push(sekme.clone());

        let (w, s) = (Rc::downgrade(self), Rc::downgrade(&sekme));
        sec.connect_clicked(move |_| {
            if let (Some(t), Some(s)) = (w.upgrade(), s.upgrade()) {
                t.sekmeye_gec(&s);
            }
        });
        let (w, s) = (Rc::downgrade(self), Rc::downgrade(&sekme));
        kapat.connect_clicked(move |_| {
            if let (Some(t), Some(s)) = (w.upgrade(), s.upgrade()) {
                t.sekme_kapat(&s);
            }
        });
        self.web_baglantilari(&sekme);
        self.sekmeye_gec(&sekme);
    }

    fn web_baglantilari(self: &Rc<Self>, sekme: &Rc<Sekme>) {
        let web = &sekme.web;
        let (w, s) = (Rc::downgrade(self), Rc::downgrade(sekme));
        web.connect_title_notify(move |web| {
            if let Some(s) = s.upgrade() {
                let baslik = web.title().map(|t| t.to_string()).filter(|t| !t.is_empty());
                s.etiket.set_label(baslik.as_deref().unwrap_or("Yeni sekme"));
                if let Some(t) = w.upgrade() {
                    t.basligi_guncelle();
                }
            }
        });
        let (w, s) = (Rc::downgrade(self), Rc::downgrade(sekme));
        web.connect_uri_notify(move |_| {
            if let (Some(t), Some(s)) = (w.upgrade(), s.upgrade()) {
                // Another page: its own ink (if it had some), a fresh undo list.
                s.gecmis.borrow_mut().clear();
                if t.etkin_mi(&s) {
                    t.cubugu_guncelle();
                    t.cizim_alani.queue_draw();
                }
            }
        });
        let (w, s) = (Rc::downgrade(self), Rc::downgrade(sekme));
        web.connect_estimated_load_progress_notify(move |web| {
            if let (Some(t), Some(s)) = (w.upgrade(), s.upgrade()) {
                if t.etkin_mi(&s) {
                    let p = web.estimated_load_progress();
                    t.adres.set_progress_fraction(if p >= 1.0 { 0.0 } else { p });
                }
            }
        });
        let (w, s) = (Rc::downgrade(self), Rc::downgrade(sekme));
        web.connect_load_changed(move |_, _| {
            if let (Some(t), Some(s)) = (w.upgrade(), s.upgrade()) {
                if t.etkin_mi(&s) {
                    t.cubugu_guncelle();
                }
            }
        });
        // Links that open a new window (target=_blank, window.open) become tabs.
        let w = Rc::downgrade(self);
        web.connect_create(move |web, _| {
            let t = w.upgrade()?;
            let yeni = t.web_olustur(Some(web));
            t.sekme_ekle(yeni.clone());
            Some(yeni.upcast())
        });
        let (w, s) = (Rc::downgrade(self), Rc::downgrade(sekme));
        web.connect_close(move |_| {
            if let (Some(t), Some(s)) = (w.upgrade(), s.upgrade()) {
                t.sekme_kapat(&s);
            }
        });
        // A video going fullscreen takes the whole screen, bars included.
        let w = Rc::downgrade(self);
        web.connect_enter_fullscreen(move |_| {
            if let Some(t) = w.upgrade() {
                t.alt.set_visible(false);
                t.cizim_alani.set_visible(false);
                t.pencere.fullscreen();
            }
            false
        });
        let w = Rc::downgrade(self);
        web.connect_leave_fullscreen(move |_| {
            if let Some(t) = w.upgrade() {
                t.alt.set_visible(true);
                t.cizim_alani.set_visible(true);
                t.pencere.unfullscreen();
            }
            false
        });
        if let Some(icerik) = web.user_content_manager() {
            let (w, s) = (Rc::downgrade(self), Rc::downgrade(sekme));
            icerik.connect_script_message_received(Some("ruzgarKaydirma"), move |_, deger| {
                let (Some(t), Some(s)) = (w.upgrade(), s.upgrade()) else { return };
                let Some(json) = deger.to_json(0) else { return };
                let mut sayilar = json.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|n| n.trim().parse::<f64>().ok());
                if let (Some(x), Some(y)) = (sayilar.next(), sayilar.next()) {
                    s.kaydirma.set((x, y));
                    if t.etkin_mi(&s) {
                        t.cizim_alani.queue_draw();
                    }
                }
            });
        }
    }

    fn etkin_mi(&self, sekme: &Rc<Sekme>) -> bool {
        self.etkin_sekme().is_some_and(|e| Rc::ptr_eq(&e, sekme))
    }

    fn sekmeye_gec(&self, sekme: &Rc<Sekme>) {
        let Some(i) = self.sekmeler.borrow().iter().position(|s| Rc::ptr_eq(s, sekme)) else { return };
        self.etkin.set(i);
        self.yigin.set_visible_child(&sekme.web);
        for (j, s) in self.sekmeler.borrow().iter().enumerate() {
            if j == i {
                s.kutu.add_css_class("etkin");
            } else {
                s.kutu.remove_css_class("etkin");
            }
        }
        self.canli.borrow_mut().take();
        self.cubugu_guncelle();
        self.basligi_guncelle();
        self.cizim_alani.queue_draw();
    }

    fn sekme_kapat(&self, sekme: &Rc<Sekme>) {
        let Some(i) = self.sekmeler.borrow().iter().position(|s| Rc::ptr_eq(s, sekme)) else { return };
        let etkindi = i == self.etkin.get();
        self.sekmeler.borrow_mut().remove(i);
        self.sekme_kutusu.remove(&sekme.kutu);
        self.yigin.remove(&sekme.web);
        sekme.web.try_close();
        let kalan = self.sekmeler.borrow().len();
        if kalan == 0 {
            // The last tab closed: start over at home rather than an empty window.
            if let Some(t) = TARAYICI.with(|t| t.borrow().clone()) {
                t.yeni_sekme(adres::ANASAYFA);
            }
            return;
        }
        let yeni = if etkindi { i.min(kalan - 1) } else if i < self.etkin.get() { self.etkin.get() - 1 } else { self.etkin.get() };
        let s = self.sekmeler.borrow()[yeni].clone();
        self.sekmeye_gec(&s);
    }

    fn cubugu_guncelle(&self) {
        let Some(s) = self.etkin_sekme() else { return };
        if !self.adres.has_focus() || s.web.is_loading() {
            self.adres.set_text(&s.web.uri().map(|u| u.to_string()).unwrap_or_default());
        }
        self.geri.set_sensitive(s.web.can_go_back());
        self.ileri.set_sensitive(s.web.can_go_forward());
        self.yenile.set_icon_name(if s.web.is_loading() { "process-stop-symbolic" } else { "view-refresh-symbolic" });
    }

    fn basligi_guncelle(&self) {
        let baslik = self.etkin_sekme().and_then(|s| s.web.title()).map(|t| format!("{t} — Rüzgar")).unwrap_or_else(|| "Rüzgar".into());
        self.pencere.set_title(Some(&baslik));
    }

    // --- Drawing --------------------------------------------------------

    fn araclari_kur(self: &Rc<Self>) {
        let satir = |dugmeler: &[gtk::Button]| {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            b.set_homogeneous(true);
            for d in dugmeler {
                b.append(d);
            }
            b
        };
        let araclar = [(Arac::Kalem, "Kalem"), (Arac::Fosforlu, "Fosforlu"), (Arac::Silgi, "Silgi")];
        for (arac, ad) in araclar {
            let d = gtk::Button::with_label(ad);
            let w = Rc::downgrade(self);
            d.connect_clicked(move |_| {
                if let Some(t) = w.upgrade() {
                    t.kalem.set(Kalem { arac, ..t.kalem.get() });
                    t.secimleri_goster();
                }
            });
            self.araclar.append(&d);
            self.arac_dugmeleri.borrow_mut().push((d, arac));
        }
        self.araclar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let mut renkler = Vec::new();
        for renk in cizim::RENKLER {
            let d = gtk::Button::new();
            d.add_css_class("renk");
            d.add_css_class(&format!("renk-{}", renkler.len())); // matches `cizim::RENKLER` in STIL
            let w = Rc::downgrade(self);
            d.connect_clicked(move |_| {
                if let Some(t) = w.upgrade() {
                    let k = t.kalem.get();
                    // Picking a colour while erasing means "write again".
                    let arac = if k.arac == Arac::Silgi { Arac::Kalem } else { k.arac };
                    t.kalem.set(Kalem { renk, arac, ..k });
                    t.secimleri_goster();
                }
            });
            renkler.push(d.clone());
            self.renk_dugmeleri.borrow_mut().push((d, renk));
        }
        self.araclar.append(&satir(&renkler[..3]));
        self.araclar.append(&satir(&renkler[3..]));
        self.araclar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let mut kalinliklar = Vec::new();
        for (kalinlik, ad) in cizim::KALINLIKLAR.into_iter().zip(["İnce", "Orta", "Kalın"]) {
            let d = gtk::Button::with_label(ad);
            let w = Rc::downgrade(self);
            d.connect_clicked(move |_| {
                if let Some(t) = w.upgrade() {
                    t.kalem.set(Kalem { kalinlik, ..t.kalem.get() });
                    t.secimleri_goster();
                }
            });
            kalinliklar.push(d.clone());
            self.kalinlik_dugmeleri.borrow_mut().push((d, kalinlik));
        }
        for d in kalinliklar {
            self.araclar.append(&d);
        }
        self.araclar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let geri_al = gtk::Button::with_label("Geri al");
        let w = Rc::downgrade(self);
        geri_al.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.geri_al();
            }
        });
        let temizle = gtk::Button::with_label("Temizle");
        let w = Rc::downgrade(self);
        temizle.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.degistir(|c| {
                    let bos = c.is_empty();
                    c.clear();
                    !bos
                });
            }
        });
        let kapat = gtk::Button::with_label("Bitir");
        let w = Rc::downgrade(self);
        kapat.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.cizim_dugmesi.set_active(false);
            }
        });
        self.araclar.append(&geri_al);
        self.araclar.append(&temizle);
        self.araclar.append(&kapat);
        self.secimleri_goster();
    }

    fn secimleri_goster(&self) {
        let k = self.kalem.get();
        let isaretle = |d: &gtk::Button, secili: bool| {
            if secili {
                d.add_css_class("secili");
            } else {
                d.remove_css_class("secili");
            }
        };
        for (d, a) in self.arac_dugmeleri.borrow().iter() {
            isaretle(d, *a == k.arac);
        }
        for (d, r) in self.renk_dugmeleri.borrow().iter() {
            isaretle(d, *r == k.renk && k.arac != Arac::Silgi);
        }
        for (d, w) in self.kalinlik_dugmeleri.borrow().iter() {
            isaretle(d, *w == k.kalinlik && k.arac != Arac::Silgi);
        }
    }

    /// Applies `f` to the current page's ink, keeping an undo step if it
    /// changed something.
    fn degistir(&self, f: impl FnOnce(&mut Vec<Cizgi>) -> bool) {
        let Some(s) = self.etkin_sekme() else { return };
        let anahtar = s.anahtar();
        let mut cizimler = s.cizimler.borrow_mut();
        let sayfa = cizimler.entry(anahtar).or_default();
        let once = sayfa.clone();
        if f(sayfa) {
            s.gecmis.borrow_mut().push(once);
            self.cizim_alani.queue_draw();
        }
    }

    fn geri_al(&self) {
        let Some(s) = self.etkin_sekme() else { return };
        let Some(once) = s.gecmis.borrow_mut().pop() else { return };
        s.cizimler.borrow_mut().insert(s.anahtar(), once);
        self.cizim_alani.queue_draw();
    }

    /// Widget point → page point (the page's scroll added back).
    fn sayfa_noktasi(&self, x: f64, y: f64) -> (f64, f64) {
        let (sx, sy) = self.etkin_sekme().map(|s| s.kaydirma.get()).unwrap_or_default();
        (x + sx, y + sy)
    }

    fn cizimi_kur(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        self.cizim_alani.set_draw_func(move |_, cr, _, _| {
            let Some(t) = w.upgrade() else { return };
            let Some(s) = t.etkin_sekme() else { return };
            let (sx, sy) = s.kaydirma.get();
            cr.translate(-sx, -sy);
            if let Some(cizgiler) = s.cizimler.borrow().get(&s.anahtar()) {
                for c in cizgiler {
                    c.ciz(cr);
                }
            }
            if let Some(c) = t.canli.borrow().as_ref() {
                c.ciz(cr);
            };
        });

        let surukle = gtk::GestureDrag::new();
        surukle.set_button(0);
        let w = Rc::downgrade(self);
        surukle.connect_drag_begin(move |_, x, y| {
            let Some(t) = w.upgrade() else { return };
            let p = t.sayfa_noktasi(x, y);
            let k = t.kalem.get();
            if k.arac == Arac::Silgi {
                t.degistir(|c| cizim::sil(c, p));
            } else {
                *t.canli.borrow_mut() = Some(Cizgi::yeni(&k, p));
                t.cizim_alani.queue_draw();
            }
        });
        let w = Rc::downgrade(self);
        surukle.connect_drag_update(move |g, dx, dy| {
            let Some(t) = w.upgrade() else { return };
            let Some((x0, y0)) = g.start_point() else { return };
            let p = t.sayfa_noktasi(x0 + dx, y0 + dy);
            if t.kalem.get().arac == Arac::Silgi {
                // One undo step per sweep, not per touched stroke.
                if let Some(s) = t.etkin_sekme() {
                    let mut cizimler = s.cizimler.borrow_mut();
                    if cizimler.get_mut(&s.anahtar()).is_some_and(|c| cizim::sil(c, p)) {
                        t.cizim_alani.queue_draw();
                    }
                }
            } else if let Some(c) = t.canli.borrow_mut().as_mut() {
                c.ekle(p);
                t.cizim_alani.queue_draw();
            }
        });
        let w = Rc::downgrade(self);
        surukle.connect_drag_end(move |_, _, _| {
            let Some(t) = w.upgrade() else { return };
            let Some(c) = t.canli.borrow_mut().take() else { return };
            t.degistir(move |cizgiler| {
                cizgiler.push(c);
                true
            });
        });
        self.cizim_alani.add_controller(surukle);

        // Keyboard: Ctrl+T new tab, Ctrl+W close, Ctrl+L address, F5 reload,
        // Escape leaves drawing.
        let tuslar = gtk::EventControllerKey::new();
        tuslar.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(self);
        tuslar.connect_key_pressed(move |_, tus, _, durum| {
            let Some(t) = w.upgrade() else { return glib::Propagation::Proceed };
            let ctrl = durum.contains(gdk::ModifierType::CONTROL_MASK);
            match (ctrl, tus.to_lower()) {
                (true, gdk::Key::t) => {
                    t.yeni_sekme(adres::ANASAYFA);
                    t.adres.grab_focus();
                }
                (true, gdk::Key::w) => {
                    if let Some(s) = t.etkin_sekme() {
                        t.sekme_kapat(&s);
                    }
                }
                (true, gdk::Key::l) => {
                    t.adres.grab_focus();
                }
                (false, gdk::Key::F5) => {
                    if let Some(s) = t.etkin_sekme() {
                        s.web.reload();
                    }
                }
                (false, gdk::Key::Escape) if t.cizim_dugmesi.is_active() => {
                    t.cizim_dugmesi.set_active(false);
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.pencere.add_controller(tuslar);
    }
}
