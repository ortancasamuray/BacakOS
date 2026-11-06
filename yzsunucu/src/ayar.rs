//! `/etc/yzsunucu/yzsunucu.conf` — `anahtar = değer` lines, `#` comments.

pub const VARSAYILAN_YOL: &str = "/etc/yzsunucu/yzsunucu.conf";

pub struct Ayar {
    /// Address boards connect to.
    pub dinle: String,
    pub llama_server: String,
    pub model: String,
    pub mmproj: String,
    /// Loopback port for llama-server.
    pub llama_port: u16,
    /// Model layers on the GPU (99 = all).
    pub gpu_katman: u32,
    pub baglam: u32,
    /// Answer length cap in tokens.
    pub en_cok_belirtec: u32,
}

impl Ayar {
    pub fn oku(yol: &str) -> Result<Self, String> {
        let metin = std::fs::read_to_string(yol).map_err(|e| e.to_string())?;
        Self::ayristir(&metin)
    }

    fn ayristir(metin: &str) -> Result<Self, String> {
        let mut a = Ayar {
            dinle: "0.0.0.0:8790".into(),
            llama_server: "/opt/yzsunucu/llama/llama-server".into(),
            model: "/var/lib/yzsunucu/model/Qwen3VL-8B-Instruct-Q4_K_M.gguf".into(),
            mmproj: "/var/lib/yzsunucu/model/mmproj-Qwen3VL-8B-Instruct-F16.gguf".into(),
            llama_port: 8791,
            gpu_katman: 99,
            baglam: 8192,
            en_cok_belirtec: 900,
        };
        for (no, satir) in metin.lines().enumerate() {
            let satir = satir.split('#').next().unwrap_or("").trim();
            if satir.is_empty() {
                continue;
            }
            let Some((k, d)) = satir.split_once('=') else {
                return Err(format!("{}. satır: 'anahtar = değer' bekleniyor", no + 1));
            };
            let d = d.trim().trim_matches('"').to_string();
            let sayi = |d: &str| d.parse::<u32>().map_err(|_| format!("{}. satır: sayı bekleniyor", no + 1));
            match k.trim() {
                "dinle" => a.dinle = d,
                "llama_server" => a.llama_server = d,
                "model" => a.model = d,
                "mmproj" => a.mmproj = d,
                "llama_port" => a.llama_port = sayi(&d)? as u16,
                "gpu_katman" => a.gpu_katman = sayi(&d)?,
                "baglam" => a.baglam = sayi(&d)?,
                "en_cok_belirtec" => a.en_cok_belirtec = sayi(&d)?,
                k => return Err(format!("{}. satır: bilinmeyen anahtar '{k}'", no + 1)),
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varsayilanlar_ve_yorumlar() {
        let a = Ayar::ayristir("# yorum\ndinle = 127.0.0.1:9000  # satır sonu\nmodel=\"/m.gguf\"\n").unwrap();
        assert_eq!(a.dinle, "127.0.0.1:9000");
        assert_eq!(a.model, "/m.gguf");
        assert_eq!(a.llama_port, 8791);
        assert!(Ayar::ayristir("bilinmez = 1").is_err());
        assert!(Ayar::ayristir("baglam = çok").is_err());
    }
}
