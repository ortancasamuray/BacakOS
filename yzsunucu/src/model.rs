//! Supervises `llama-server` and asks it about a board image through its
//! OpenAI-compatible `/v1/chat/completions` endpoint.

use crate::ayar::Ayar;
use crate::http;
use serde_json::{json, Value};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kip {
    /// Solve step by step.
    Coz,
    /// Explain what's on the board, don't solve.
    Acikla,
    /// One hint for the class, no solution.
    Ipucu,
}

const SISTEM: &str = "Sen Türkiye'de ortaokul ve lisede ders veren deneyimli bir matematik öğretmenisin. \
Sana öğretmenin akıllı tahtaya elle yazdığı ya da çizdiği bir bölümün görüntüsü gelir. \
Önce görüntüde ne yazdığını dikkatle oku; el yazısı olduğunu unutma. \
Yanıtı Türkçe ve düz metin olarak ver: Markdown, LaTeX, $ işareti, yıldız ya da diyez kullanma. \
Üsleri x², x³ ya da x^4 biçiminde, kesirleri a/b biçiminde, kökü √ ile, çarpmayı · ile yaz. \
Matematiği sözcüklerle değil sembollerle yaz: 'x karesi eksi üç' değil 'x² − 3'. \
Kısa ve öğrencinin anlayacağı dilde yaz; her satır tek bir adım olsun, satırlar arasında boş satır bırakma, \
aynı şeyi tekrarlama. \
Görüntüde okuyamadığın bir yer varsa uydurma, bunu açıkça söyle.";

impl Kip {
    fn istem(self) -> &'static str {
        match self {
            Kip::Coz => "Görüntüdeki soruyu adım adım çöz. İlk satırda soruyu nasıl okuduğunu 'Okunan: ...' diye yaz. \
                         Görüntüde bir denklem değil yalnız bir fonksiyon ya da ifade varsa onu incele \
                         (tanım kümesi, kökler, tepe noktası ya da önemli noktalar, grafiğin biçimi). \
                         En çok 10 adım kullan. Son satırda sonucu 'Sonuç: ...' diye ver.",
            Kip::Acikla => "Görüntüdeki ifadeyi ya da şekli öğrencilere açıkla: ne olduğunu, hangi konuyla ilgili olduğunu \
                            ve neye dikkat etmeleri gerektiğini en çok 8 satırda anlat. Soruyu çözme.",
            Kip::Ipucu => "Görüntüdeki soru için çözümü ve sonucu vermeden öğrencilere tek bir ipucu ver: \
                           nereden ya da hangi yöntemle başlamalılar? En çok 3 satır.",
        }
    }
}

pub struct Model {
    port: u16,
    en_cok_belirtec: u32,
    hazir: Arc<AtomicBool>,
    _cocuk: Arc<Mutex<Child>>,
}

impl Model {
    /// Starts llama-server (loopback only) and returns at once; `hazir()`
    /// turns true once it answers `/health`. If it dies, this process exits
    /// so systemd restarts both.
    pub fn baslat(a: &Ayar) -> Result<Self, String> {
        for yol in [&a.llama_server, &a.model, &a.mmproj] {
            if !std::path::Path::new(yol).exists() {
                return Err(format!("{yol} yok (modeli indirmek için: yzsunucu-model-indir)"));
            }
        }
        let mut komut = Command::new(&a.llama_server);
        komut
            .args(["--host", "127.0.0.1", "--port", &a.llama_port.to_string()])
            .args(["-m", &a.model, "--mmproj", &a.mmproj])
            .args(["-ngl", &a.gpu_katman.to_string(), "-c", &a.baglam.to_string()])
            // Default verbosity logs timings, never prompt or answer text.
            .args(["--parallel", "2", "--no-webui"]);
        // SAFETY: prctl is async-signal-safe; runs in the forked child only.
        unsafe {
            komut.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let cocuk = Arc::new(Mutex::new(komut.spawn().map_err(|e| format!("{}: {e}", a.llama_server))?));
        let hazir = Arc::new(AtomicBool::new(false));
        {
            let (cocuk, hazir, port) = (cocuk.clone(), hazir.clone(), a.llama_port);
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(500));
                if let Ok(Some(durum)) = cocuk.lock().unwrap().try_wait() {
                    eprintln!("yzsunucu: llama-server kapandı ({durum})");
                    std::process::exit(1);
                }
                if !hazir.load(Ordering::Relaxed) && matches!(http::get(port, "/health"), Ok((200, _))) {
                    eprintln!("yzsunucu: model hazır");
                    hazir.store(true, Ordering::Relaxed);
                }
            });
        }
        Ok(Model { port: a.llama_port, en_cok_belirtec: a.en_cok_belirtec, hazir, _cocuk: cocuk })
    }

    pub fn hazir(&self) -> bool {
        self.hazir.load(Ordering::Relaxed)
    }

    pub fn sor(&self, png: &[u8], kip: Kip) -> Result<String, String> {
        if !self.hazir() {
            return Err("model henüz yüklenmedi".into());
        }
        let istek = json!({
            "messages": [
                {"role": "system", "content": SISTEM},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", base64(png))}},
                    {"type": "text", "text": kip.istem()},
                ]},
            ],
            "temperature": 0.2,
            "repeat_penalty": 1.1,
            "max_tokens": self.en_cok_belirtec,
        });
        let (durum, govde) = http::json_gonder(self.port, "/v1/chat/completions", istek.to_string().as_bytes(), Duration::from_secs(180))
            .map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&govde).map_err(|e| format!("yanıt JSON değil: {e}"))?;
        if durum != 200 {
            return Err(format!("llama-server {durum}: {}", v["error"]["message"].as_str().unwrap_or("?")));
        }
        v["choices"][0]["message"]["content"].as_str().map(str::to_string).ok_or_else(|| "yanıtta metin yok".into())
    }
}

fn base64(veri: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(veri.len().div_ceil(3) * 4);
    for p in veri.chunks(3) {
        let n = (p[0] as u32) << 16 | (*p.get(1).unwrap_or(&0) as u32) << 8 | *p.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= p.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_rfc4648() {
        for (g, c) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(super::base64(g.as_bytes()), c);
        }
    }
}
