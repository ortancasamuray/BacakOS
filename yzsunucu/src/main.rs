//! YZsunucu — the school's own AI server for tahta's "Sor" card.
//!
//! A board POSTs the PNG of a framed board region to `/sor?kip=coz|acikla|ipucu`
//! and gets the answer back as plain UTF-8 text. Behind it runs llama.cpp's
//! `llama-server` with an open vision-language model, started and supervised
//! by this process and bound to 127.0.0.1 only.
//!
//! Privacy (KVKK): images and answers are never written to disk or to the
//! log; only sizes and timings are logged.

mod ayar;
mod http;
mod metin;
mod model;

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Concurrent questions beyond this get "meşgul" instead of queueing for
/// minutes behind one GPU.
const MAX_BEKLEYEN: usize = 6;
/// Larger uploads are refused; tahta sends at most ~1 MB.
const MAX_GOVDE: usize = 8 * 1024 * 1024;

fn main() {
    let yol = std::env::args().nth(1).unwrap_or_else(|| ayar::VARSAYILAN_YOL.to_string());
    let ayar = match ayar::Ayar::oku(&yol) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("yzsunucu: {yol}: {e}");
            std::process::exit(2);
        }
    };
    let model = Arc::new(model::Model::baslat(&ayar).unwrap_or_else(|e| {
        eprintln!("yzsunucu: model başlatılamadı: {e}");
        std::process::exit(1);
    }));
    let dinleyici = TcpListener::bind(&ayar.dinle).unwrap_or_else(|e| {
        eprintln!("yzsunucu: {} dinlenemedi: {e}", ayar.dinle);
        std::process::exit(1);
    });
    eprintln!("yzsunucu: {} dinleniyor", ayar.dinle);
    let bekleyen = Arc::new(AtomicUsize::new(0));
    for baglanti in dinleyici.incoming() {
        let Ok(akis) = baglanti else { continue };
        let (model, bekleyen) = (model.clone(), bekleyen.clone());
        std::thread::spawn(move || {
            let _ = akis.set_read_timeout(Some(Duration::from_secs(20)));
            let _ = akis.set_write_timeout(Some(Duration::from_secs(20)));
            isle(akis, &model, &bekleyen);
        });
    }
}

fn isle(mut akis: TcpStream, model: &model::Model, bekleyen: &AtomicUsize) {
    let istek = match http::Istek::oku(&mut akis, MAX_GOVDE) {
        Ok(i) => i,
        Err(http::Hata::CokBuyuk) => return yanit(&mut akis, 413, "Görüntü çok büyük."),
        Err(_) => return yanit(&mut akis, 400, "Geçersiz istek."),
    };
    match (istek.yontem.as_str(), istek.yol.as_str()) {
        ("GET", "/durum") => {
            if model.hazir() {
                yanit(&mut akis, 200, "hazır")
            } else {
                yanit(&mut akis, 503, "Model yükleniyor.")
            }
        }
        ("POST", "/sor") => {
            if !istek.govde.starts_with(b"\x89PNG\r\n\x1a\n") {
                return yanit(&mut akis, 415, "PNG görüntü bekleniyor.");
            }
            let kip = match istek.sorgu("kip").as_deref() {
                None | Some("coz") => model::Kip::Coz,
                Some("acikla") => model::Kip::Acikla,
                Some("ipucu") => model::Kip::Ipucu,
                Some(_) => return yanit(&mut akis, 400, "Bilinmeyen kip."),
            };
            if bekleyen.fetch_add(1, Ordering::SeqCst) >= MAX_BEKLEYEN {
                bekleyen.fetch_sub(1, Ordering::SeqCst);
                return yanit(&mut akis, 503, "Sunucu şu an meşgul, biraz sonra yeniden deneyin.");
            }
            let basla = Instant::now();
            let sonuc = model.sor(&istek.govde, kip);
            bekleyen.fetch_sub(1, Ordering::SeqCst);
            match sonuc {
                Ok(cevap) => {
                    eprintln!("soru: {:?}, {} KB, {:.1} s", kip, istek.govde.len() / 1024, basla.elapsed().as_secs_f32());
                    yanit(&mut akis, 200, &metin::duzelt(&cevap))
                }
                Err(e) => {
                    eprintln!("soru başarısız ({:.1} s): {e}", basla.elapsed().as_secs_f32());
                    yanit(&mut akis, 502, "Model yanıt veremedi.")
                }
            }
        }
        _ => yanit(&mut akis, 404, "Bulunamadı."),
    }
}

fn yanit(akis: &mut TcpStream, durum: u16, govde: &str) {
    let ad = match durum {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        502 => "Bad Gateway",
        _ => "Service Unavailable",
    };
    let _ = write!(
        akis,
        "HTTP/1.1 {durum} {ad}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{govde}",
        govde.len()
    );
}
