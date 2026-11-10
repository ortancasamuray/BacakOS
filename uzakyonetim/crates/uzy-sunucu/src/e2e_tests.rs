// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end security tests of the agent gateway over real TLS, using the
//! agent's actual TLS code (`uzy-ajan/src/tls.rs`).

#[path = "../../uzy-ajan/src/tls.rs"]
#[allow(dead_code)]
mod ajan_tls;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use uzy_proto::{read_frame, write_frame, AgentMsg, Command, Hello, Payload, Policy, ServerMsg, UserInfo};

use crate::{auth, ca, db, files, gateway, App, Config};

struct Fixture {
    app: Arc<App>,
    addr: String,
    _dir: tempfile::TempDir,
}

async fn start() -> Fixture {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let ca = ca::Ca::create(dir.path()).unwrap();
    let (crt, key) = ca.issue_server(&["127.0.0.1".into()]).unwrap();
    let cfg = gateway::tls_config(&ca.cert_pem, &crt, &key).unwrap();
    let app = Arc::new(App {
        db: db::Db::open(&dir.path().join("t.db")).unwrap(),
        ca_pem: ca.cert_pem.clone(),
        ca_fp: ca.fingerprint(),
        ca: Mutex::new(ca),
        agents: Default::default(),
        enroll_limit: Default::default(),
        sessions: auth::Sessions::default(),
        login_limit: Default::default(),
        recover_limit: auth::LoginLimiter::with_max(3),
        argon_izin: tokio::sync::Semaphore::new(crate::ARGON_ESZAMANLI),
        pending_admins: Default::default(),
        cfg: Config {
            adresler: vec!["127.0.0.1".into()],
            ajan_portu: 0,
            panel_adresi: String::new(),
            panel_sertifika: None,
            panel_anahtar: None,
        },
        data: dir.path().to_path_buf(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(gateway::serve(app.clone(), listener, cfg));
    Fixture { app, addr, _dir: dir }
}

fn csr() -> (rcgen::KeyPair, String) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, "lab-01");
    // A hostile CSR asking for serverAuth (to impersonate the server) must
    // not get it: the server builds every field itself.
    p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let pem = p.serialize_request(&key).unwrap().pem().unwrap();
    (key, pem)
}

fn token(app: &App) -> String {
    let t = files::random_token();
    app.db.add_token(&files::sha256_hex(&t), 600, "test").unwrap();
    t
}

async fn enroll(f: &Fixture, fp: &str, jeton: &str, csr_pem: &str) -> Result<ServerMsg, String> {
    let pinned = Arc::new(ajan_tls::PinnedCa::new(fp));
    let mut c = ajan_tls::connect_enroll(&f.addr, pinned).await?;
    write_frame(&mut c, &AgentMsg::Kayit { jeton: jeton.into(), csr_pem: csr_pem.into(), makine_adi: "lab-01".into() })
        .await
        .unwrap();
    read_frame(&mut c).await.map_err(|e| e.to_string())?.ok_or("kapandı".into())
}

fn hello() -> AgentMsg {
    AgentMsg::Merhaba(Hello {
        protokol: 1,
        makine_adi: "lab-01".into(),
        isletim_sistemi: "BacakOS".into(),
        ajan_surumu: "test".into(),
        politika: Policy::default(),
    })
}

#[tokio::test]
async fn enrollment_mtls_commands_and_revocation() {
    let f = start().await;
    let (key, csr_pem) = csr();
    let jeton = token(&f.app);

    let ServerMsg::KayitTamam { sertifika_pem, ca_pem, makine_id } = enroll(&f, &f.app.ca_fp, &jeton, &csr_pem).await.unwrap()
    else {
        panic!("kayıt reddedildi")
    };
    assert_eq!(ca_pem, f.app.ca_pem);

    // The issued cert is a non-CA leaf with only the clientAuth EKU and our
    // machine id as CN, whatever the CSR asked for.
    let parsed = rcgen::CertificateParams::from_ca_cert_pem(&sertifika_pem).unwrap();
    assert!(matches!(parsed.is_ca, rcgen::IsCa::ExplicitNoCa | rcgen::IsCa::NoCa));
    assert_eq!(parsed.extended_key_usages, vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth]);
    let cn = parsed.distinguished_name.get(&rcgen::DnType::CommonName).cloned();
    assert!(matches!(cn, Some(rcgen::DnValue::Utf8String(ref v)) if *v == makine_id) || format!("{cn:?}").contains(&makine_id));

    // Token is single-use.
    let (_, csr2) = csr();
    assert!(matches!(enroll(&f, &f.app.ca_fp, &jeton, &csr2).await.unwrap(), ServerMsg::KayitRed { .. }));

    // mTLS session: hello → settings → a command round-trip.
    let mut c = ajan_tls::connect_mtls(&f.addr, ca_pem.as_bytes(), sertifika_pem.as_bytes(), key.serialize_pem().as_bytes())
        .await
        .unwrap();
    write_frame(&mut c, &hello()).await.unwrap();
    let first: ServerMsg = read_frame(&mut c).await.unwrap().unwrap();
    assert!(matches!(first, ServerMsg::Ayarlar { .. }));
    for _ in 0..50 {
        if f.app.agents.online(&makine_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(f.app.agents.online(&makine_id));

    let app = f.app.clone();
    let id = makine_id.clone();
    let call = tokio::spawn(async move { app.agents.call(&id, Command::KullanicilariListele, Duration::from_secs(5)).await });
    let ServerMsg::Komut { id: req, komut: Command::KullanicilariListele } = read_frame(&mut c).await.unwrap().unwrap() else {
        panic!("komut bekleniyordu")
    };
    let user = UserInfo { kullanici: "ayse".into(), tam_ad: "Ayşe".into(), uid: 1001, bacakonay: false, yonetici: false };
    write_frame(&mut c, &AgentMsg::Yanit { id: req, sonuc: Ok(Payload::Kullanicilar { liste: vec![user.clone()] }) })
        .await
        .unwrap();
    match call.await.unwrap().unwrap() {
        Payload::Kullanicilar { liste } => assert_eq!(liste, vec![user]),
        other => panic!("{other:?}"),
    }

    // Revoke → existing session dropped, reconnect refused.
    assert!(f.app.db.revoke(&makine_id));
    f.app.agents.disconnect(&makine_id);
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match read_frame::<_, ServerMsg>(&mut c).await {
                Ok(Some(_)) => continue, // settings already in flight
                _ => break,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "iptal edilen makinenin açık bağlantısı hemen kapanmalı");
    let mut c2 = ajan_tls::connect_mtls(&f.addr, ca_pem.as_bytes(), sertifika_pem.as_bytes(), key.serialize_pem().as_bytes())
        .await
        .unwrap();
    write_frame(&mut c2, &hello()).await.unwrap();
    let r: std::io::Result<Option<ServerMsg>> = read_frame(&mut c2).await;
    assert!(matches!(r, Ok(None) | Err(_)), "iptal edilen makine oturum açamamalı");
}

#[tokio::test]
async fn wrong_pin_is_refused_before_anything_is_sent() {
    let f = start().await;
    let (_, csr_pem) = csr();
    let jeton = token(&f.app);
    let err = enroll(&f, &"0".repeat(64), &jeton, &csr_pem).await.unwrap_err();
    assert!(err.contains("parmak izi"), "{err}");
    // The token was never presented, so it's still usable.
    assert!(matches!(enroll(&f, &f.app.ca_fp, &jeton, &csr_pem).await.unwrap(), ServerMsg::KayitTamam { .. }));
}

#[tokio::test]
async fn hello_without_certificate_is_dropped() {
    let f = start().await;
    let pinned = Arc::new(ajan_tls::PinnedCa::new(&f.app.ca_fp));
    let mut c = ajan_tls::connect_enroll(&f.addr, pinned).await.unwrap();
    write_frame(&mut c, &hello()).await.unwrap();
    let r: std::io::Result<Option<ServerMsg>> = read_frame(&mut c).await;
    assert!(matches!(r, Ok(None) | Err(_)));
}

#[tokio::test]
async fn certificate_from_another_ca_is_rejected() {
    let f = start().await;
    // An attacker's own CA + client cert: TLS handshake must fail.
    let other = tempfile::tempdir().unwrap();
    let evil = ca::Ca::create(other.path()).unwrap();
    let (key, csr_pem) = csr();
    let (cert, _) = evil.sign_agent(&csr_pem, "deadbeefdeadbeef").unwrap();
    let res = async {
        let mut c = ajan_tls::connect_mtls(&f.addr, f.app.ca_pem.as_bytes(), cert.as_bytes(), key.serialize_pem().as_bytes()).await?;
        write_frame(&mut c, &hello()).await.map_err(|e| e.to_string())?;
        read_frame::<_, ServerMsg>(&mut c).await.map_err(|e| e.to_string())
    }
    .await;
    assert!(matches!(res, Err(_) | Ok(None)), "yabancı CA'nın sertifikası kabul edilmemeli");
}

#[tokio::test]
async fn re_enrollment_with_the_current_certificate_renews_the_same_machine() {
    let f = start().await;
    let (key, csr_pem) = csr();
    let ServerMsg::KayitTamam { sertifika_pem, ca_pem, makine_id } =
        enroll(&f, &f.app.ca_fp, &token(&f.app), &csr_pem).await.unwrap()
    else {
        panic!("kayıt reddedildi")
    };
    let mtls = |cert: String, key: String| {
        let (addr, ca_pem) = (f.addr.clone(), ca_pem.clone());
        async move { ajan_tls::connect_mtls(&addr, ca_pem.as_bytes(), cert.as_bytes(), key.as_bytes()).await.unwrap() }
    };
    let mut live = mtls(sertifika_pem.clone(), key.serialize_pem()).await;
    write_frame(&mut live, &hello()).await.unwrap();
    assert!(matches!(read_frame::<_, ServerMsg>(&mut live).await.unwrap(), Some(ServerMsg::Ayarlar { .. })));

    // `kaydol --zorla`: a new key, a new token, sent over the current certificate.
    let (key2, csr2) = csr();
    let mut c = mtls(sertifika_pem.clone(), key.serialize_pem()).await;
    write_frame(&mut c, &AgentMsg::Kayit { jeton: token(&f.app), csr_pem: csr2, makine_adi: "lab-01".into() })
        .await
        .unwrap();
    let ServerMsg::KayitTamam { sertifika_pem: cert2, makine_id: id2, .. } = read_frame(&mut c).await.unwrap().unwrap() else {
        panic!("yeniden kayıt reddedildi")
    };
    assert_eq!(id2, makine_id, "aynı makine yeni kimlik almamalı");
    assert_eq!(f.app.db.machines().len(), 1, "panelde tek kart kalmalı");

    // The session on the old certificate is closed and the old certificate is dead.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(_)) = read_frame::<_, ServerMsg>(&mut live).await {}
    })
    .await;
    assert!(ended.is_ok(), "eski sertifikalı oturum kapanmalı");
    let mut old = mtls(sertifika_pem, key.serialize_pem()).await;
    write_frame(&mut old, &hello()).await.unwrap();
    assert!(matches!(read_frame::<_, ServerMsg>(&mut old).await, Ok(None) | Err(_)), "eski sertifika artık geçmemeli");
    let mut new = mtls(cert2.clone(), key2.serialize_pem()).await;
    write_frame(&mut new, &hello()).await.unwrap();
    assert!(matches!(read_frame::<_, ServerMsg>(&mut new).await.unwrap(), Some(ServerMsg::Ayarlar { .. })));

    // A machine taken out of the fleet that enrolls again comes back as a new
    // machine, never by reviving the revoked record.
    assert!(f.app.db.revoke(&makine_id));
    f.app.agents.disconnect(&makine_id);
    let (_, csr3) = csr();
    let mut c = mtls(cert2, key2.serialize_pem()).await;
    write_frame(&mut c, &AgentMsg::Kayit { jeton: token(&f.app), csr_pem: csr3, makine_adi: "lab-01".into() })
        .await
        .unwrap();
    let ServerMsg::KayitTamam { makine_id: id3, .. } = read_frame(&mut c).await.unwrap().unwrap() else {
        panic!("iptal sonrası kayıt reddedildi")
    };
    assert_ne!(id3, makine_id);
    assert_eq!(f.app.db.machines().len(), 2);
}

#[tokio::test]
async fn removing_a_machine_removes_its_screens() {
    use base64::Engine;
    let f = start().await;
    f.app.db.add_machine("00000000000000aa", "lab-01", "fp1").unwrap();
    let shot = uzy_proto::Screenshot {
        zaman: crate::db::now() as u64,
        oturum: Some("ayse".into()),
        jpeg_b64: base64::engine::general_purpose::STANDARD.encode([0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
    };
    gateway::store_screenshot(&f.app, "00000000000000aa", &shot).unwrap();
    let dir = f.app.screens_dir("00000000000000aa").unwrap();
    assert!(dir.join("son.jpg").exists());

    assert!(f.app.db.revoke("00000000000000aa"));
    gateway::forget_screens(&f.app, "00000000000000aa");
    assert!(!dir.exists(), "filodan çıkarılan makinenin ekranları silinmeli");
    assert!(f.app.db.machines().iter().all(|m| m.son_ekran == 0));

    // A frame that was still in flight doesn't bring the screen back.
    assert!(gateway::store_screenshot(&f.app, "00000000000000aa", &shot).is_err());
    assert!(!dir.exists());
}
