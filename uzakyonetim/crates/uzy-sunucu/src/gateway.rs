// SPDX-License-Identifier: GPL-3.0-or-later
//! Agent gateway: the mTLS port agents dial out to.
//!
//! A connection either carries **no** client certificate and exactly one
//! `Kayit` (enrollment with a one-time token), or a certificate issued by our
//! CA whose fingerprint maps to a non-revoked machine — anything else is
//! dropped. Commands are request/response with ids; screenshots are pushed.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_rustls::TlsAcceptor;
use uzy_proto::{read_frame, write_frame, AgentMsg, Command, Payload, ServerMsg};

use crate::ca::fingerprint_der;
use crate::files;
use crate::App;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Payload, String>>>>>;

struct Conn {
    tx: mpsc::Sender<ServerMsg>,
    pending: Pending,
    /// Distinguishes a replaced connection from the current one on cleanup.
    epoch: u64,
}

#[derive(Default)]
pub struct Agents {
    map: Mutex<HashMap<String, Conn>>,
    next_id: AtomicU64,
    epoch: AtomicU64,
}

impl Agents {
    pub fn online(&self, id: &str) -> bool {
        self.map.lock().unwrap().contains_key(id)
    }

    pub async fn call(&self, id: &str, cmd: Command, timeout: Duration) -> Result<Payload, String> {
        let (tx, pending) = {
            let map = self.map.lock().unwrap();
            let c = map.get(id).ok_or("makine şu an çevrimdışı")?;
            (c.tx.clone(), c.pending.clone())
        };
        let req = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (otx, orx) = oneshot::channel();
        pending.lock().unwrap().insert(req, otx);
        if tx.send(ServerMsg::Komut { id: req, komut: cmd }).await.is_err() {
            pending.lock().unwrap().remove(&req);
            return Err("bağlantı koptu".into());
        }
        match tokio::time::timeout(timeout, orx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("bağlantı koptu".into()),
            Err(_) => {
                pending.lock().unwrap().remove(&req);
                Err("makine zamanında yanıt vermedi".into())
            }
        }
    }

    pub async fn broadcast_settings(&self, interval: u32) {
        let txs: Vec<_> = self.map.lock().unwrap().values().map(|c| c.tx.clone()).collect();
        for tx in txs {
            let _ = tx.send(ServerMsg::Ayarlar { ekran_araligi_sn: interval }).await;
        }
    }

    pub fn disconnect(&self, id: &str) {
        self.map.lock().unwrap().remove(id);
    }
}

/// Failed enrollments per source IP (brute-forcing tokens is pointless at
/// 256 bits, but don't let anyone hammer the CA either).
#[derive(Default)]
pub struct EnrollLimiter(Mutex<HashMap<IpAddr, (u32, Instant)>>);

impl EnrollLimiter {
    const MAX: u32 = 10;
    const WINDOW: Duration = Duration::from_secs(600);

    fn blocked(&self, ip: IpAddr) -> bool {
        let mut m = self.0.lock().unwrap();
        m.retain(|_, (_, t)| t.elapsed() < Self::WINDOW);
        m.get(&ip).is_some_and(|(n, _)| *n >= Self::MAX)
    }

    fn fail(&self, ip: IpAddr) {
        let mut m = self.0.lock().unwrap();
        let e = m.entry(ip).or_insert((0, Instant::now()));
        e.0 += 1;
    }
}

pub fn tls_config(ca_pem: &str, leaf_pem: &str, key_pem: &str) -> Result<Arc<ServerConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    for c in rustls_pemfile::certs(&mut ca_pem.as_bytes()) {
        roots.add(c.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .allow_unauthenticated()
        .build()
        .map_err(|e| e.to_string())?;
    let mut chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut leaf_pem.as_bytes()).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    // Send the CA too: agents pin it by fingerprint during enrollment.
    chain.extend(rustls_pemfile::certs(&mut ca_pem.as_bytes()).collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?);
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut key_pem.as_bytes()).map_err(|e| e.to_string())?.ok_or("anahtar yok")?;
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(cfg))
}

pub async fn serve(app: Arc<App>, listener: TcpListener, cfg: Arc<ServerConfig>) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(cfg);
    log::info!("ajan kapısı dinleniyor: {}", listener.local_addr()?);
    loop {
        let (tcp, peer) = listener.accept().await?;
        let app = app.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            if app.enroll_limit.blocked(peer.ip()) {
                return;
            }
            let tls = match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(tcp)).await {
                Ok(Ok(t)) => t,
                _ => return,
            };
            if let Err(e) = handle(app, tls, peer).await {
                log::debug!("{peer}: {e}");
            }
        });
    }
}

async fn handle(app: Arc<App>, tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, peer: SocketAddr) -> Result<(), String> {
    let cert_fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| fingerprint_der(c));
    let (mut rd, mut wr) = tokio::io::split(tls);
    let first: AgentMsg = tokio::time::timeout(Duration::from_secs(30), read_frame(&mut rd))
        .await
        .map_err(|_| "ilk çerçeve gelmedi")?
        .map_err(|e| e.to_string())?
        .ok_or("bağlantı kapandı")?;

    match (first, cert_fp) {
        (AgentMsg::Kayit { jeton, csr_pem, makine_adi }, None) => {
            let reply = enroll(&app, &jeton, &csr_pem, &makine_adi, peer);
            write_frame(&mut wr, &reply).await.map_err(|e| e.to_string())
        }
        (AgentMsg::Merhaba(hello), Some(fp)) => {
            let id = app.db.machine_by_cert(&fp).ok_or("tanınmayan ya da iptal edilmiş sertifika")?;
            let policy = serde_json::to_string(&hello.politika).unwrap_or_default();
            let name: String = hello.makine_adi.chars().filter(|c| !c.is_control()).take(64).collect();
            app.db.touch_machine(&id, &name, &hello.isletim_sistemi, &hello.ajan_surumu, &policy);
            log::info!("ajan bağlandı: {name} ({id}) {peer}");
            session(app, id, rd, wr).await
        }
        _ => Err("protokol ihlali".into()),
    }
}

fn enroll(app: &App, token: &str, csr: &str, name: &str, peer: SocketAddr) -> ServerMsg {
    if !app.db.consume_token(&files::sha256_hex(token)) {
        app.enroll_limit.fail(peer.ip());
        log::warn!("geçersiz katılım jetonu: {peer}");
        return ServerMsg::KayitRed { neden: "katılım kodu geçersiz, kullanılmış ya da süresi dolmuş".into() };
    }
    let id: String = files::random_bytes::<8>().iter().map(|b| format!("{b:02x}")).collect();
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let signed = app.ca.lock().unwrap().sign_agent(csr, &id);
    match signed.and_then(|(pem, fp)| app.db.add_machine(&id, &name, &fp).map(|_| pem)) {
        Ok(pem) => {
            app.db.audit("ajan", &id, "makine_kaydi", &format!("{name} ({peer})"), "tamam");
            ServerMsg::KayitTamam { sertifika_pem: pem, ca_pem: app.ca_pem.clone(), makine_id: id }
        }
        Err(e) => ServerMsg::KayitRed { neden: e },
    }
}

async fn session<R, W>(app: Arc<App>, id: String, mut rd: R, mut wr: W) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(32);
    let pending: Pending = Arc::default();
    let epoch = app.agents.epoch.fetch_add(1, Ordering::Relaxed);
    app.agents.map.lock().unwrap().insert(id.clone(), Conn { tx: tx.clone(), pending: pending.clone(), epoch });

    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if write_frame(&mut wr, &m).await.is_err() {
                break;
            }
        }
    });
    // Settings now and every 45 s (doubles as a NAT keepalive).
    let ka_app = app.clone();
    let ka_tx = tx.clone();
    let keepalive = tokio::spawn(async move {
        loop {
            let interval = ka_app.db.screenshot_interval();
            if ka_tx.send(ServerMsg::Ayarlar { ekran_araligi_sn: interval }).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(45)).await;
        }
    });

    let result = loop {
        let msg: AgentMsg = match tokio::time::timeout(Duration::from_secs(180), read_frame(&mut rd)).await {
            Err(_) => break Err("ajan sessiz kaldı".to_string()),
            Ok(Err(e)) => break Err(e.to_string()),
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some(m))) => m,
        };
        app.db.seen(&id);
        match msg {
            AgentMsg::Yanit { id: req, sonuc } => {
                if let Some(t) = pending.lock().unwrap().remove(&req) {
                    let _ = t.send(sonuc);
                }
            }
            AgentMsg::Ekran(shot) => {
                if let Err(e) = store_screenshot(&app, &id, &shot) {
                    log::warn!("{id}: ekran kaydedilemedi: {e}");
                }
            }
            _ => break Err("protokol ihlali".to_string()),
        }
    };
    keepalive.abort();
    writer.abort();
    {
        let mut map = app.agents.map.lock().unwrap();
        if map.get(&id).is_some_and(|c| c.epoch == epoch) {
            map.remove(&id);
        }
    }
    log::info!("ajan ayrıldı: {id}");
    result
}

/// Keep the newest screenshot plus a short history per machine.
const HISTORY: usize = 30;

pub fn store_screenshot(app: &App, id: &str, shot: &uzy_proto::Screenshot) -> Result<(), String> {
    let jpeg = base64::engine::general_purpose::STANDARD.decode(&shot.jpeg_b64).map_err(|e| e.to_string())?;
    if !jpeg.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Err("JPEG değil".into());
    }
    let dir = app.screens_dir(id).ok_or("geçersiz makine kimliği")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let at = shot.zaman.min(crate::db::now() as u64 + 60);
    files::write_private(&dir.join(format!("{at}.jpg")), &jpeg)?;
    files::write_private(&dir.join("son.jpg"), &jpeg)?;
    let mut hist: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.parse::<u64>().is_ok()))
        .collect();
    hist.sort();
    while hist.len() > HISTORY {
        let _ = std::fs::remove_file(hist.remove(0));
    }
    app.db.set_screenshot(id, at as i64, shot.oturum.as_deref());
    Ok(())
}
