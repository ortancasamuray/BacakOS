//! Just enough HTTP/1.1: one request per connection, `Content-Length`
//! bodies on the way in; `Content-Length` or chunked bodies on the way back
//! from llama-server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Debug)]
pub enum Hata {
    Bicim,
    CokBuyuk,
    Gc(std::io::Error),
}

impl From<std::io::Error> for Hata {
    fn from(e: std::io::Error) -> Self {
        Hata::Gc(e)
    }
}

impl std::fmt::Display for Hata {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Hata::Bicim => write!(f, "HTTP biçim hatası"),
            Hata::CokBuyuk => write!(f, "gövde çok büyük"),
            Hata::Gc(e) => write!(f, "{e}"),
        }
    }
}

pub struct Istek {
    pub yontem: String,
    pub yol: String,
    sorgu: String,
    pub govde: Vec<u8>,
}

impl Istek {
    pub fn oku(akis: &mut TcpStream, max_govde: usize) -> Result<Self, Hata> {
        let mut r = BufReader::new(akis);
        let ilk = satir(&mut r)?;
        let mut parca = ilk.split(' ');
        let yontem = parca.next().ok_or(Hata::Bicim)?.to_string();
        let hedef = parca.next().ok_or(Hata::Bicim)?;
        let (yol, sorgu) = hedef.split_once('?').unwrap_or((hedef, ""));
        let (yol, sorgu) = (yol.to_string(), sorgu.to_string());
        let mut uzunluk = 0usize;
        for _ in 0..64 {
            let s = satir(&mut r)?;
            if s.is_empty() {
                let mut govde = vec![0; uzunluk];
                r.read_exact(&mut govde)?;
                return Ok(Istek { yontem, yol, sorgu, govde });
            }
            if let Some((k, d)) = s.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    uzunluk = d.trim().parse().map_err(|_| Hata::Bicim)?;
                    if uzunluk > max_govde {
                        return Err(Hata::CokBuyuk);
                    }
                }
            }
        }
        Err(Hata::Bicim)
    }

    pub fn sorgu(&self, ad: &str) -> Option<String> {
        self.sorgu.split('&').find_map(|kv| {
            let (k, d) = kv.split_once('=')?;
            (k == ad).then(|| d.to_string())
        })
    }
}

fn satir(r: &mut impl BufRead) -> Result<String, Hata> {
    let mut s = String::new();
    // Header lines are short; a client streaming one endless line is junk.
    if r.by_ref().take(8192).read_line(&mut s)? == 0 || !s.ends_with('\n') {
        return Err(Hata::Bicim);
    }
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

/// POSTs JSON to `127.0.0.1:port` and returns `(status, body)`.
pub fn json_gonder(port: u16, yol: &str, govde: &[u8], zaman_asimi: Duration) -> Result<(u16, Vec<u8>), Hata> {
    let mut akis = TcpStream::connect(("127.0.0.1", port))?;
    akis.set_read_timeout(Some(zaman_asimi))?;
    write!(
        akis,
        "POST {yol} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        govde.len()
    )?;
    akis.write_all(govde)?;
    yanit_oku(&mut akis)
}

pub fn get(port: u16, yol: &str) -> Result<(u16, Vec<u8>), Hata> {
    let mut akis = TcpStream::connect(("127.0.0.1", port))?;
    akis.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(akis, "GET {yol} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
    yanit_oku(&mut akis)
}

fn yanit_oku(akis: &mut TcpStream) -> Result<(u16, Vec<u8>), Hata> {
    let mut r = BufReader::new(akis);
    let ilk = satir(&mut r)?;
    let durum: u16 = ilk.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or(Hata::Bicim)?;
    let (mut uzunluk, mut parcali) = (None, false);
    loop {
        let s = satir(&mut r)?;
        if s.is_empty() {
            break;
        }
        if let Some((k, d)) = s.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                uzunluk = d.trim().parse::<usize>().ok();
            } else if k.eq_ignore_ascii_case("transfer-encoding") && d.to_ascii_lowercase().contains("chunked") {
                parcali = true;
            }
        }
    }
    let mut govde = Vec::new();
    if parcali {
        loop {
            let boy = usize::from_str_radix(satir(&mut r)?.split(';').next().unwrap_or("").trim(), 16).map_err(|_| Hata::Bicim)?;
            if boy == 0 {
                break;
            }
            let bas = govde.len();
            govde.resize(bas + boy, 0);
            r.read_exact(&mut govde[bas..])?;
            satir(&mut r)?;
        }
    } else if let Some(n) = uzunluk {
        govde.resize(n, 0);
        r.read_exact(&mut govde)?;
    } else {
        r.read_to_end(&mut govde)?;
    }
    Ok((durum, govde))
}
