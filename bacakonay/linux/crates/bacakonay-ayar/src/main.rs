// SPDX-License-Identifier: GPL-3.0-or-later
//! `bacakonay-ayar` — turn Bacak Onay two-step login on or off from the
//! desktop, without a terminal: the pairing QR is drawn large for the phone
//! to scan and the confirmation code comes from the on-screen keyboard.
//!
//! Nothing here touches the enrollment store. Every step runs
//! `sudo -S -k bacakonay arayuz …` (protocol in `protokol`) with the
//! administrator password typed into the first page, so the security model
//! stays the CLI's: root-only store, and the CLI acts on `$SUDO_USER` alone.

mod protokol;

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use qrcode::{Color, EcLevel, QrCode};
use zeroize::Zeroizing;

use protokol::{Durum, Satir};

const UYGULAMA_KIMLIGI: &str = "tr.org.anadolupanteri.BacakOnay";

/// How long `kur` may take to show the QR before we give up on it (a sudo
/// that is still waiting for a password would otherwise hang the page).
const QR_ZAMAN_ASIMI: u32 = 15;

const STIL: &str = r#"
window { background: #1f2229; color: #e8eaf0; }
.baslik { font-size: 24px; font-weight: bold; }
.aciklama { color: #aab2c4; font-size: 15px; }
.durum-acik { color: #4ade80; font-size: 22px; font-weight: bold; }
.durum-kapali { color: #fbbf24; font-size: 22px; font-weight: bold; }
.ileti { font-size: 15px; }
.ileti.hata { color: #f87171; }
.ileti.iyi { color: #4ade80; }
.anahtar { font-family: monospace; font-size: 14px; color: #aab2c4; }
button { min-height: 52px; font-size: 17px; border-radius: 12px; border: none; box-shadow: none;
         background: #353a45; color: #e8eaf0; }
button:hover { background: #434957; }
button.ana { background: #3b82f6; color: white; }
button.tehlike { background: #b91c1c; color: white; }
entry, passwordentry { min-height: 52px; font-size: 20px; background: #2a2e37; color: #f2f4f8; }
"#;

fn main() -> glib::ExitCode {
    // A parent that ignores SIGCHLD (older bacak-compositor builds did, and
    // the setting survives exec) makes every child's exit status unreadable:
    // a successful `sudo` would look like a refused password.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
    let app = gtk::Application::builder().application_id(UYGULAMA_KIMLIGI).build();
    app.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(STIL);
        if let Some(ekran) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&ekran, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    app.connect_activate(|app| match app.active_window() {
        Some(w) => w.present(),
        None => Pencere::yeni(app).pencere.present(),
    });
    app.run()
}

struct Pencere {
    pencere: gtk::ApplicationWindow,
    yigin: gtk::Stack,
    parola: gtk::PasswordEntry,
    parola_dugme: gtk::Button,
    parola_ileti: gtk::Label,
    durum_baslik: gtk::Label,
    durum_aciklama: gtk::Label,
    esle_dugme: gtk::Button,
    kaldir_dugme: gtk::Button,
    durum_ileti: gtk::Label,
    qr: gtk::DrawingArea,
    anahtar: gtk::Label,
    kod: gtk::Entry,
    onayla: gtk::Button,
    esle_ileti: gtk::Label,
    /// The administrator password, kept for the later steps of this window.
    sifre: RefCell<Zeroizing<String>>,
    durum: Cell<Durum>,
    /// QR module grid (side, dark?) of the pairing link on screen.
    qr_modulleri: RefCell<Option<(usize, Vec<bool>)>>,
    /// The running `arayuz kur`, while pairing.
    surec: RefCell<Option<gio::Subprocess>>,
    /// "Kapat" was tapped once; the second tap within a few seconds acts.
    kaldir_onay: Cell<bool>,
}

fn dikey() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 14);
    b.set_margin_top(28);
    b.set_margin_bottom(28);
    b.set_margin_start(28);
    b.set_margin_end(28);
    b
}

/// The narrow centred column of the password and status pages.
fn sayfa() -> gtk::Box {
    let b = dikey();
    b.set_width_request(460);
    b.set_halign(gtk::Align::Center);
    b
}

fn etiket(metin: &str, sinif: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(metin)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .justify(gtk::Justification::Center)
        // Bounds the natural width, so the narrow pages stay narrow.
        .max_width_chars(44)
        .build();
    if !sinif.is_empty() {
        l.add_css_class(sinif);
    }
    l
}

fn dugme(metin: &str, sinif: &str) -> gtk::Button {
    let d = gtk::Button::with_label(metin);
    if !sinif.is_empty() {
        d.add_css_class(sinif);
    }
    d
}

fn ileti(l: &gtk::Label, metin: &str, sinif: &str) {
    l.set_text(metin);
    if sinif.is_empty() {
        l.set_css_classes(&["ileti"]);
    } else {
        l.set_css_classes(&["ileti", sinif]);
    }
}

/// `sudo -S -k bacakonay arayuz <args>`: `-k` makes sudo always read the
/// password line we send first, so it can never fall through to bacakonay
/// as a confirmation code. stderr is merged so errors arrive with the replies.
fn sudo_bacakonay(args: &[&str]) -> Result<gio::Subprocess, glib::Error> {
    let mut argv: Vec<&OsStr> =
        ["sudo", "-S", "-k", "-p", "", "bacakonay", "arayuz"].into_iter().map(OsStr::new).collect();
    argv.extend(args.iter().map(OsStr::new));
    gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
    )
}

/// Run a one-shot `arayuz` command to completion: (succeeded, output).
async fn calistir(sifre: &str, komut: &str) -> (bool, String) {
    let p = match sudo_bacakonay(&[komut]) {
        Ok(p) => p,
        Err(e) => return (false, format!("bacakonay: {e}")),
    };
    let cikti = match p.communicate_utf8_future(Some(format!("{sifre}\n"))).await {
        Ok((cikti, _)) => cikti.map(|c| c.to_string()).unwrap_or_default(),
        Err(e) => return (false, format!("bacakonay: {e}")),
    };
    // The pipes close before the process is reaped: wait for the exit status
    // itself, or a success read too early looks like a refused password.
    let ok = p.wait_future().await.is_ok() && p.is_successful();
    (ok, cikti)
}

impl Pencere {
    fn yeni(app: &gtk::Application) -> Rc<Self> {
        let pencere = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Bacak Onay")
            .default_width(800)
            .default_height(560)
            .build();
        let yigin = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();

        // 1. Administrator password.
        let p1 = sayfa();
        p1.append(&etiket("Bacak Onay", "baslik"));
        p1.append(&etiket(
            "İki adımlı giriş: giriş ekranında paroladan sonra telefonunuzdaki Bacak Onay \
             uygulamasının ürettiği 6 haneli kod da sorulur. Parolanız ele geçse bile \
             telefonunuz olmadan oturum açılamaz.",
            "aciklama",
        ));
        p1.append(&etiket("Ayarı görmek ve değiştirmek için yönetici parolanızı girin:", "aciklama"));
        let parola = gtk::PasswordEntry::builder().show_peek_icon(true).build();
        p1.append(&parola);
        let parola_dugme = dugme("Devam", "ana");
        p1.append(&parola_dugme);
        let parola_ileti = etiket("", "ileti");
        p1.append(&parola_ileti);
        yigin.add_named(&p1, Some("parola"));

        // 2. Status.
        let p2 = sayfa();
        p2.append(&etiket("Bacak Onay", "baslik"));
        let durum_baslik = etiket("", "");
        p2.append(&durum_baslik);
        let durum_aciklama = etiket("", "aciklama");
        p2.append(&durum_aciklama);
        let esle_dugme = dugme("", "ana");
        p2.append(&esle_dugme);
        let kaldir_dugme = dugme("", "");
        p2.append(&kaldir_dugme);
        let durum_ileti = etiket("", "ileti");
        p2.append(&durum_ileti);
        let kapat = dugme("Kapat", "");
        kapat.set_margin_top(12);
        p2.append(&kapat);
        yigin.add_named(&p2, Some("durum"));

        // 3. Pairing: the QR on the left, the steps and the code on the right
        // — short enough that the on-screen keyboard (which opens with the
        // code field focused) doesn't cover the buttons or the reply.
        let p3 = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        p3.set_margin_top(24);
        p3.set_margin_bottom(24);
        p3.set_margin_start(24);
        p3.set_margin_end(24);
        let sol = gtk::Box::new(gtk::Orientation::Vertical, 10);
        let qr = gtk::DrawingArea::builder().content_width(320).content_height(320).build();
        sol.append(&qr);
        let anahtar = etiket("", "anahtar");
        sol.append(&anahtar);
        p3.append(&sol);
        let sag = gtk::Box::new(gtk::Orientation::Vertical, 14);
        sag.set_hexpand(true);
        sag.append(&etiket("Telefonu eşle", "baslik"));
        sag.append(&etiket("1. Telefonda Bacak Onay'ı açın, + → QR tara ile soldaki kodu okutun.", "aciklama"));
        sag.append(&etiket("2. Uygulamada beliren kodu yazın:", "aciklama"));
        let kod = gtk::Entry::builder()
            .input_purpose(gtk::InputPurpose::Digits)
            .max_length(9)
            .placeholder_text("123 456")
            .xalign(0.5)
            .build();
        sag.append(&kod);
        let satir = gtk::Box::builder().spacing(12).homogeneous(true).build();
        let vazgec = dugme("Vazgeç", "");
        satir.append(&vazgec);
        let onayla = dugme("Onayla", "ana");
        satir.append(&onayla);
        sag.append(&satir);
        let esle_ileti = etiket("", "ileti");
        sag.append(&esle_ileti);
        p3.append(&sag);
        yigin.add_named(&p3, Some("esle"));

        // Scrolls when the on-screen keyboard shrinks the window.
        let kaydir = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&yigin)
            .build();
        pencere.set_child(Some(&kaydir));

        let t = Rc::new(Pencere {
            pencere,
            yigin,
            parola,
            parola_dugme,
            parola_ileti,
            durum_baslik,
            durum_aciklama,
            esle_dugme,
            kaldir_dugme,
            durum_ileti,
            qr,
            anahtar,
            kod,
            onayla,
            esle_ileti,
            sifre: RefCell::new(Zeroizing::new(String::new())),
            durum: Cell::new(Durum::Kapali),
            qr_modulleri: RefCell::new(None),
            surec: RefCell::new(None),
            kaldir_onay: Cell::new(false),
        });

        let w = Rc::downgrade(&t);
        t.qr.set_draw_func(move |_, cr, gen, yuk| {
            let Some(t) = w.upgrade() else { return };
            let moduller = t.qr_modulleri.borrow();
            let Some((n, koyu)) = moduller.as_ref() else { return };
            // A 4-module quiet zone around, as the QR spec wants.
            let kenar = n + 8;
            let m = (gen.min(yuk) as usize / kenar).max(1) as f64;
            let boy = m * kenar as f64;
            let (x0, y0) = ((gen as f64 - boy) / 2.0, (yuk as f64 - boy) / 2.0);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.rectangle(x0, y0, boy, boy);
            let _ = cr.fill();
            cr.set_source_rgb(0.0, 0.0, 0.0);
            for (i, &k) in koyu.iter().enumerate() {
                if k {
                    let (x, y) = ((i % n + 4) as f64, (i / n + 4) as f64);
                    cr.rectangle(x0 + x * m, y0 + y * m, m, m);
                }
            }
            let _ = cr.fill();
        });

        let w = Rc::downgrade(&t);
        let gonder = move || {
            if let Some(t) = w.upgrade() {
                t.parola_gonder();
            }
        };
        let g = gonder.clone();
        t.parola.connect_activate(move |_| g());
        t.parola_dugme.connect_clicked(move |_| gonder());

        let w = Rc::downgrade(&t);
        t.esle_dugme.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.esle();
            }
        });
        let w = Rc::downgrade(&t);
        t.kaldir_dugme.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.kaldir();
            }
        });
        let w = Rc::downgrade(&t);
        kapat.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.pencere.close();
            }
        });
        let w = Rc::downgrade(&t);
        let onay = move || {
            if let Some(t) = w.upgrade() {
                t.kod_gonder();
            }
        };
        let o = onay.clone();
        t.kod.connect_activate(move |_| o());
        t.onayla.connect_clicked(move |_| onay());
        let w = Rc::downgrade(&t);
        vazgec.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.vazgec();
            }
        });

        // Closing mid-pairing must not leave a root process behind.
        let w = Rc::downgrade(&t);
        t.pencere.connect_close_request(move |_| {
            if let Some(p) = w.upgrade().and_then(|t| t.surec.take()) {
                p.force_exit();
            }
            glib::Propagation::Proceed
        });
        // The window's closures hold only weak references; it keeps `t`.
        let tut = t.clone();
        t.pencere.connect_destroy(move |_| {
            let _ = &tut;
        });
        t
    }

    fn parola_gonder(self: &Rc<Self>) {
        let sifre = Zeroizing::new(self.parola.text().to_string());
        if sifre.is_empty() {
            return;
        }
        self.parola.set_text("");
        self.parola_dugme.set_sensitive(false);
        ileti(&self.parola_ileti, "Denetleniyor…", "");
        let t = self.clone();
        glib::spawn_future_local(async move {
            let (ok, cikti) = calistir(&sifre, "durum").await;
            t.parola_dugme.set_sensitive(true);
            match (ok, protokol::durum(&cikti)) {
                (true, Some(d)) => {
                    *t.sifre.borrow_mut() = sifre;
                    ileti(&t.parola_ileti, "", "");
                    t.durum_goster(d, "", "");
                }
                _ => {
                    ileti(&t.parola_ileti, &protokol::hata(&cikti), "hata");
                    t.parola.grab_focus();
                }
            }
        });
    }

    fn durum_goster(&self, d: Durum, metin: &str, sinif: &str) {
        self.durum.set(d);
        self.kaldir_onay.set(false);
        match d {
            Durum::Acik => {
                self.durum_baslik.set_text("İki adımlı giriş AÇIK");
                self.durum_baslik.set_css_classes(&["durum-acik"]);
                self.durum_aciklama.set_text(
                    "Girişte paroladan sonra telefondaki kod sorulur. Yeni bir telefona \
                     geçtiyseniz yeniden eşleyin; eski telefondaki kayıt geçersiz olur.",
                );
                self.esle_dugme.set_label("Yeni telefonla yeniden eşle");
                self.kaldir_dugme.set_label("İki adımlı girişi kapat");
                self.kaldir_dugme.set_css_classes(&[]);
                self.kaldir_dugme.set_visible(true);
            }
            Durum::Kapali => {
                self.durum_baslik.set_text("İki adımlı giriş KAPALI");
                self.durum_baslik.set_css_classes(&["durum-kapali"]);
                self.durum_aciklama.set_text(
                    "Açmak için telefona Bacak Onay uygulamasını kurun, uygulamada kasayı \
                     oluşturun (telefonda ekran kilidi olmalı) ve aşağıdaki düğmeye basın.",
                );
                self.esle_dugme.set_label("Telefonu eşle ve aç");
                self.kaldir_dugme.set_visible(false);
            }
        }
        ileti(&self.durum_ileti, metin, sinif);
        self.yigin.set_visible_child_name("durum");
    }

    fn esle(self: &Rc<Self>) {
        let zorla = self.durum.get() == Durum::Acik;
        let args: &[&str] = if zorla { &["kur", "--zorla"] } else { &["kur"] };
        let p = match sudo_bacakonay(args) {
            Ok(p) => p,
            Err(e) => return ileti(&self.durum_ileti, &format!("Başlatılamadı: {e}"), "hata"),
        };
        let (Some(girdi), Some(cikti)) = (p.stdin_pipe(), p.stdout_pipe()) else {
            p.force_exit();
            return;
        };
        self.surec.replace(Some(p.clone()));
        self.qr_modulleri.replace(None);
        self.qr.queue_draw();
        self.anahtar.set_text("");
        self.kod.set_text("");
        self.onayla.set_sensitive(true);
        ileti(&self.durum_ileti, "QR hazırlanıyor…", "");
        ileti(&self.esle_ileti, "", "");

        let w = Rc::downgrade(self);
        let bekleyen = p.clone();
        glib::timeout_add_seconds_local_once(QR_ZAMAN_ASIMI, move || {
            let Some(t) = w.upgrade() else { return };
            let ayni = t.surec.borrow().as_ref() == Some(&bekleyen);
            if ayni && t.qr_modulleri.borrow().is_none() {
                t.bitir("QR oluşturulamadı (zaman aşımı). Yeniden deneyin.", "hata");
            }
        });

        let sifre = Zeroizing::new(format!("{}\n", self.sifre.borrow().as_str()).into_bytes());
        let t = self.clone();
        glib::spawn_future_local(async move {
            let _ = girdi.write_all_future(sifre.to_vec(), glib::Priority::DEFAULT).await;
            let okuyucu = gio::DataInputStream::new(&cikti);
            let mut diger = String::new();
            while let Ok(Some(satir)) = okuyucu.read_line_utf8_future(glib::Priority::DEFAULT).await {
                match protokol::satir(&satir) {
                    Satir::Uri(uri) => t.qr_goster(&uri),
                    Satir::Anahtar(a) => t.anahtar.set_text(&format!("Kamera yoksa uygulamada \"Elle ekle\" ile girin:\n{a}")),
                    Satir::Hatali(n) => {
                        t.kod.set_text("");
                        t.onayla.set_sensitive(true);
                        ileti(
                            &t.esle_ileti,
                            &format!("Kod tutmadı ({n}). Telefonun saati otomatik mi? Yeni kodu yazın."),
                            "hata",
                        );
                    }
                    Satir::Tamam => {
                        t.surec.take();
                        t.qr_modulleri.replace(None);
                        t.durum_goster(
                            Durum::Acik,
                            "✓ Etkin. Bir sonraki girişte paroladan sonra Bacak Onay kodu sorulacak.",
                            "iyi",
                        );
                        return;
                    }
                    Satir::Iptal => return t.bitir("Vazgeçildi; hiçbir şey değişmedi.", ""),
                    Satir::Vazgecildi => {
                        return t.bitir("3 hatalı deneme; hiçbir şey değişmedi. Yeniden deneyebilirsiniz.", "hata")
                    }
                    Satir::Kayitli => return t.bitir("Bu hesapta iki adımlı giriş zaten açık.", ""),
                    Satir::Diger(d) => {
                        diger.push_str(&d);
                        diger.push('\n');
                    }
                }
            }
            // Ended without a final reply: it failed (sudo or bacakonay).
            if t.surec.borrow().as_ref() == Some(&p) {
                t.bitir(&protokol::hata(&diger), "hata");
            }
        });
    }

    fn qr_goster(&self, uri: &str) {
        let Ok(kod) = QrCode::with_error_correction_level(uri.as_bytes(), EcLevel::M) else {
            return self.bitir("QR oluşturulamadı.", "hata");
        };
        let koyu = kod.to_colors().into_iter().map(|c| c == Color::Dark).collect();
        self.qr_modulleri.replace(Some((kod.width(), koyu)));
        self.qr.queue_draw();
        ileti(&self.durum_ileti, "", "");
        self.yigin.set_visible_child_name("esle");
    }

    fn kod_gonder(&self) {
        let kod = self.kod.text();
        if kod.trim().is_empty() || !self.onayla.is_sensitive() {
            return;
        }
        let Some(girdi) = self.surec.borrow().as_ref().and_then(|p| p.stdin_pipe()) else { return };
        self.onayla.set_sensitive(false);
        ileti(&self.esle_ileti, "Denetleniyor…", "");
        let veri = format!("{}\n", kod.trim()).into_bytes();
        glib::spawn_future_local(async move {
            let _ = girdi.write_all_future(veri, glib::Priority::DEFAULT).await;
        });
    }

    fn vazgec(&self) {
        // An empty line is bacakonay's "cancel"; it answers IPTAL and exits.
        match self.surec.borrow().as_ref().and_then(|p| p.stdin_pipe()) {
            Some(girdi) => {
                glib::spawn_future_local(async move {
                    let _ = girdi.write_all_future(b"\n".to_vec(), glib::Priority::DEFAULT).await;
                });
            }
            None => self.bitir("", ""),
        }
    }

    /// Leave pairing (whatever state it was in) and go back to the status.
    fn bitir(&self, metin: &str, sinif: &str) {
        if let Some(p) = self.surec.take() {
            p.force_exit();
        }
        self.qr_modulleri.replace(None);
        self.anahtar.set_text("");
        self.durum_goster(self.durum.get(), metin, sinif);
    }

    fn kaldir(self: &Rc<Self>) {
        if !self.kaldir_onay.get() {
            self.kaldir_onay.set(true);
            self.kaldir_dugme.set_label("Emin misiniz? Kapatmak için yeniden dokunun");
            self.kaldir_dugme.set_css_classes(&["tehlike"]);
            let w = Rc::downgrade(self);
            glib::timeout_add_seconds_local_once(4, move || {
                if let Some(t) = w.upgrade() {
                    if t.kaldir_onay.get() && t.durum.get() == Durum::Acik {
                        t.durum_goster(Durum::Acik, "", "");
                    }
                }
            });
            return;
        }
        self.kaldir_onay.set(false);
        self.kaldir_dugme.set_sensitive(false);
        let sifre = self.sifre.borrow().clone();
        let t = self.clone();
        glib::spawn_future_local(async move {
            let (ok, cikti) = calistir(&sifre, "kaldir").await;
            t.kaldir_dugme.set_sensitive(true);
            if ok {
                t.durum_goster(
                    Durum::Kapali,
                    "İki adımlı giriş kapatıldı. Telefondaki kaydı da silebilirsiniz.",
                    "iyi",
                );
            } else {
                t.durum_goster(t.durum.get(), &protokol::hata(&cikti), "hata");
            }
        });
    }
}
