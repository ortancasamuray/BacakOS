# Uzak Yönetim

🌐 **Türkçe** · [English](README.md)

Farklı ağlardaki BacakOS bilgisayarlarını tek bir web panelinden yönetir:

- **Kullanıcı tanımlama:** uzak makinede hesap açar ve aynı anda **Bacak Onay**
  iki adımlı giriş kaydını yapar (QR panelde gösterilir, kişi telefonla okutur).
- **Ekran izleme:** belirlenen aralıkla (30 sn – 1 sa) ekran görüntüsü alır;
  izleme açıkken makinenin ekranında **"İzleniyor"** göstergesi görünür.
- **Denetim kaydı:** kim, ne zaman, hangi makinede ne yaptı.

```
 Yönetici (tarayıcı)                      Merkez sunucu                       BacakOS makineleri
 ┌──────────────────┐  HTTPS :8443   ┌───────────────────────┐   mTLS :8444  ┌──────────────────────┐
 │ Web paneli       │───────────────►│ uzakyonetim-sunucu    │◄──────────────│ uzakyonetim-ajan     │
 │ parola +         │                │  özel CA, SQLite,     │  ajan DIŞARI  │ (root servis)        │
 │ Bacak Onay kodu  │                │  denetim, ekranlar    │  bağlanır     │  hesap · Bacak Onay  │
 └──────────────────┘                └───────────────────────┘  (NAT sorun   │  · ekran (grim)      │
                                                                  değil)      └──────────────────────┘
```

Ajanlar sunucuya **dışarı doğru** bağlandığı için makinelerin bulunduğu ağlarda
port açmak gerekmez; yalnızca sunucunun 8444 (ajan) ve 8443 (panel) portları
erişilebilir olmalıdır.

## Yetkisiz kullanıma karşı

| Tehdit | Önlem |
|---|---|
| Sahte makinenin filoya katılması | Yalnızca panelin ürettiği **tek kullanımlık, süreli katılım koduyla** kayıt. Ajan anahtarını kendisi üretir; sunucu yalnızca CSR'ın açık anahtarını alır, sertifikanın tüm alanlarını (CN = makine kimliği, yalnız `clientAuth`, CA değil) kendisi belirler. Jetonun yalnızca SHA-256'sı saklanır. |
| Sahte sunucu / araya girme (MITM) | Katılım kodu sunucu CA'sının SHA-256 parmak izini taşır; ajan **ilk bağlantıda bile** yalnızca bu CA'ya güvenir. Sonrasında karşılıklı TLS (mTLS). |
| Panele yetkisiz giriş | Argon2id parola **ve** zorunlu Bacak Onay kodu (tekrar kullanılamaz). IP ve kullanıcı başına 5 hatada 15 dk kilit. Sunucu tarafı oturum (30 dk hareketsizlik / 8 sa üst sınır), `HttpOnly; Secure; SameSite=Strict` çerez, her değiştirici istekte CSRF jetonu, sıkı CSP. |
| Sunucunun ele geçirilmesi | Ajan **keyfi komut çalıştırmaz**; yalnızca sabit işlemler: kullanıcı listele, hesap aç (asla `sudo` grubuna değil), Bacak Onay başlat/onayla/kaldır, ekran al. Her makinede `/etc/uzakyonetim/politika.toml` bu işlemleri tek tek kapatabilir. |
| Gizli anahtarların sızması | Bacak Onay anahtarı **ajanda** üretilir, QR panelde bir kez gösterilir, sunucuda saklanmaz; telefon doğru kodu gönderene kadar etkinleşmez. Parolalar `chpasswd`'ye stdin'den verilir (argv'de görünmez). |
| Ele geçen/çalınan makine | Panelden **"Filodan çıkar"**: sertifika iptal edilir, bağlantı anında kesilir, bir daha bağlanamaz. |
| Gizli izleme | İzleme açıkken ekranda "İzleniyor" göstergesi (compositor çizer, ajan durunca systemd göstergeyi kaldırır). Göstergenin kendisi ekran görüntüsünde de görünür. |

Sunucu ayrı `uzakyonetim` sistem kullanıcısıyla, `ProtectSystem=strict`,
boş yetki kümesi ve sistem çağrısı filtresiyle çalışır.

## Bileşenler

```
crates/uzy-proto/    ajan ↔ sunucu protokolü (uzunluk önekli JSON çerçeveler)
crates/uzy-sunucu/   web paneli (axum, HTTPS) + ajan kapısı (rustls mTLS), CA (rcgen),
                     SQLite, oturum/CSRF, denetim; uçtan uca TLS güvenlik testleri
crates/uzy-ajan/     kayıt, mTLS oturumu, hesap/Bacak Onay işlemleri (bacakonay-core),
                     grim ile ekran görüntüsü, izleme göstergesi
web/                 panel arayüzü (çerçevesiz JS, CSP uyumlu)
paket/               systemd servisleri, politika dosyası, deb betikleri
```

"İzleniyor" göstergesi `bacak-compositor` içindedir
(`/run/uzakyonetim/izleniyor` dosyası varken sağ üstte çizilir).

## Kurulum

### 1. Sunucu (internetten ya da tüm ağlardan erişilebilir bir Debian/BacakOS makinesi)

```sh
sudo apt install ./uzakyonetim-sunucu_0.1.0-1_amd64.deb
sudo -u uzakyonetim uzakyonetim-sunucu kurulum --adres yonetim.okul.tr --adres 203.0.113.7
sudo -u uzakyonetim uzakyonetim-sunucu yonetici-ekle mudur     # parola + Bacak Onay QR
sudo systemctl enable --now uzakyonetim-sunucu
```

`--adres`: ajanların sunucuya ulaşacağı alan adı/IP'ler (sertifikaya yazılır;
ilki katılım kodlarında kullanılır). Güvenlik duvarında 8443/tcp (panel) ve
8444/tcp (ajan) açılmalı. Panel varsayılan olarak sunucunun kendi CA'sıyla
imzalı sertifika kullanır (tarayıcı uyarı verir; `ca.crt`'yi tarayıcıya
ekleyebilirsiniz). Genel geçer bir sertifika (ör. Let's Encrypt) için
`/var/lib/uzakyonetim-sunucu/ayar.json` içinde `panel_sertifika` /
`panel_anahtar` yollarını verin — ajanlar bundan etkilenmez, özel CA'yı kullanır.

### 2. Her BacakOS makinesi

1. Panel → **+ Makine ekle** → komutu kopyalayın (tek kullanımlık, 60 dk geçerli).
2. Makinede:
   ```sh
   sudo apt install ./uzakyonetim-ajan_0.1.0-1_amd64.deb     # bacakonay ve grim'i de çeker
   sudo uzakyonetim-ajan kaydol uzy1.eyJ…                    # panelden kopyalanan komut
   sudo systemctl enable --now uzakyonetim-ajan
   ```
3. Makine panelde çevrimiçi görünür.

İsterseniz kayıttan önce `/etc/uzakyonetim/politika.toml`'da işlemleri kapatın.

### 3. Kullanım

- **Ayarlar → Ekran izleme** aralığını seçin (varsayılan 1 dk; "Kapalı" göstergeyi de kaldırır).
- Makine kartına tıklayın: canlı/geçmiş ekranlar, kullanıcılar, **Yeni hesap**.
- "Bacak Onay iki adımlı girişi kur" işaretliyse hesap açılınca QR çıkar; kişi
  telefonunda Bacak Onay → **QR tara** ile okutur, uygulamadaki kodu panele
  girersiniz → kayıt etkinleşir. Kişi artık Turan giriş ekranında parolasından
  sonra kodu girer.

## Derleme ve test

```sh
cargo test        # protokol, oturum/limit, jeton, gerçek TLS ile kayıt/mTLS/iptal/MITM testleri
cargo build --release
cargo deb --no-build -p uzy-sunucu -o target/debian
cargo deb --no-build -p uzy-ajan   -o target/debian
```

## Bilinen sınırlar

- Ekran görüntüsü ajanın bağlı olduğu andaki seat0 oturumundan (ya da giriş
  ekranından) alınır; çoklu monitörde tüm çıkışlar tek görüntüde birleşir.
- Yönetici TOTP anahtarları sunucu veritabanında (0600, yalnızca `uzakyonetim`
  kullanıcısı) düz saklanır; sunucu diskini şifreleyin.
- Sertifika yenileme/rotasyon yok (ajan sertifikası 5 yıl); makine yeniden
  kaydedilerek yenilenir.
