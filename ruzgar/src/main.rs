//! Rüzgar — a web browser for interactive whiteboards: tabs and the
//! address bar sit at the bottom where a hand reaches, and the open page
//! can be written and drawn on (see `cizim`, `pencere`).

mod adres;
mod cizim;
mod pencere;

use gtk::prelude::*;
use gtk::{gio, glib};

/// Also the `.desktop` file's name and the Wayland app_id the dock pins.
pub const UYGULAMA_KIMLIGI: &str = "tr.org.anadolupanteri.Ruzgar";

fn main() -> glib::ExitCode {
    // One browser per session: links opened later (Tahta's globe, a file
    // manager) arrive as `open` in the running one and become tabs.
    let app = gtk::Application::builder()
        .application_id(UYGULAMA_KIMLIGI)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_startup(|_| pencere::stil_yukle());
    app.connect_activate(|app| {
        let t = pencere::Tarayici::al(app);
        if t.sekme_sayisi() == 0 {
            t.yeni_sekme(adres::ANASAYFA);
        }
        t.goster();
    });
    app.connect_open(|app, dosyalar, _| {
        let t = pencere::Tarayici::al(app);
        for d in dosyalar {
            t.yeni_sekme(&d.uri());
        }
        t.goster();
    });
    app.run()
}
