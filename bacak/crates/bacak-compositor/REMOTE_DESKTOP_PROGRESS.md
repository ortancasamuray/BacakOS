# Uzak Masaüstü — Oturum Durumu (2026-09-10)

Bu dosya, "Uzak Masaüstü" (`bacak-remote` + compositor plugin) özelliğinin
o anki geliştirme oturumundaki durumunu kaydeder — oturum kesilirse
kaldığı yerden devam edebilmek için. Kalıcı mimari belgeleri değil,
bunlar sırasıyla `ARCHITECTURE.md` §3 ve `uzakel/uzakel-pc/README.md`'de.

## Genel özet

`uzakel-pc/bacak-remote-*` (PC ekran/girdi köprüsü) artık **hem** bağımsız
bir `bacak-remote-client` uygulaması **hem de** gerçek `bacak-compositor`
içine gömülü bir Control Center paneli ("🖥 Uzak Masaüstü") olarak
çalışıyor. İkinci yol — compositor plugin'i — bu oturumda eklendi ve
**gerçek üretim masaüstünde, gerçek bir Windows PC ile** uçtan uca
doğrulandı.

## Tamamlanan işler (bu oturumda)

1. **Güvenlik katmanı** (`uzakel-pc/bacak-remote-proto/src/crypto.rs`):
   uzakel'in PIN + X25519 ECDH + HKDF-SHA256 + ChaCha20-Poly1305 şeması
   aynen taşındı; video ve girdi kanalları için bağımsız türetilmiş
   anahtarlar (`SessionMaterial::channel_keys`).
2. **Gerçek donanım testleri**: Windows 10 (VM) ↔ bu Linux makinesi,
   gerçek LAN üzerinden; ayrıca gerçek BacakOS Wayland masaüstünde
   (Xvfb değil) gerçek fare/touchpad girdisi.
3. **Üç gerçek bug bulundu ve düzeltildi** (Wayland masaüstü testinde):
   - `tracing_subscriber` filtresi `RUST_LOG=debug`'ı sessizce eziyordu
     (`.add_directive("info")` sırası hatası).
   - `winit`'in Wayland arka ucu `DeviceEvent::MouseMotion` üretmiyor;
     `WindowEvent::CursorMoved`'dan delta hesaplamaya geçildi.
   - Sunucu, girdi soketindeki paketleri video soketinin `SocketAddr`'ı
     (port dahil) ile karşılaştırıyordu; yalnızca IP karşılaştırmasına
     geçirildi (`sess.addr.ip() != from.ip()`).
4. **`bacak-compositor` plugin'i** (`crates/bacak-compositor/src/
   remote_desktop.rs` + `plugins/remote_desktop.rs`):
   - Cross-workspace path bağımlılığı: `uzakel-pc/bacak-remote-proto`.
   - Arka planda tokio runtime'lı eşleşme/alım thread'i.
   - `MemoryRenderBuffer`/GLES üzerinden video kare render'ı.
   - Control Center'a "🖥 Uzak Masaüstü" tile'ı (`CcAction::
     RemoteDesktopConnect`).
   - v1: PIN/IP `~/.config/bacak-remote/pair.json`'dan okunuyor (ekran
     içi giriş formu yok).
5. **Tam ekran + overlay z-sıralama düzeltmesi**: video artık tüm ekranı
   kaplıyor (`video_rect = bounds`, kenar boşluğu yok); "Kapat" düğmesi ve
   durum yazısı video'nun *üzerinde* görünen bir overlay olarak render
   ediliyor (önceki bir sürümde bunlar video karesinin ARKASINDA
   kalıyordu — push sırası hatası, uzakel'in QR panelindeki aynı sınıf
   hatayla aynı kökten).
6. **Gerçek `.deb` paketleyip kurma** — iki kez, üretim oturumuna
   uygulandı, ikisinde de dpkg.log + checksum ile doğrulandı. İlk ikisinde
   oturum kendiliğinden yeniden başladı; üçüncüsünde başlamadı, kullanıcı
   Control Center'ın "Çıkış" düğmesiyle manuel çıkış yapmaya çalıştı ama
   **tam ekran video + eski (düzeltilmeden önceki) binary nedeniyle kapat
   düğmesine ulaşamadı** — süreç `kill -TERM` ile sonlandırılıp greeter'a
   düşürülerek kurtarıldı, kullanıcı yeniden giriş yaptı, düzeltilmiş
   binary'nin çalıştığı doğrulandı (Kapat düğmesi + durum yazısı artık
   görünüyor).

## Kapat düğmesi alta taşındı — TAMAMLANDI ✅

Kullanıcı **"kapat düğmesi altta olmalı"** dedi — düğme sağ üstteydi,
sağ alta (durum yazısıyla aynı alt şerit) taşındı. `remote_desktop.rs`'de
`close_rect` artık `bounds.y + bounds.h - m - 36.0`'da. Derlendi,
`.deb` paketlendi, **kuruldu ve doğrulandı**:

- Checksum: `d6532d668a30ba92ad3a13ca61ed8306` (`/usr/bin/bacak-compositor`
  ile derlenen binary eşleşiyor)
- `dpkg.log`: `2026-09-10 00:11:24 status installed bacak-compositor:amd64 0.1.0-1`
- Oturum bu kurulumda **otomatik yeniden başladı** (PID 37734, tam kurulum
  saatinde) — kullanıcı müdahalesi gerekmedi, masaüstü ekran görüntüsüyle
  sağlıklı olduğu doğrulandı.

**Henüz yapılmayan:** yeni buton konumunun gerçek bir Windows oturumuyla
panel açılarak görsel doğrulaması (önceki adımda pair.json'daki PIN eski
kalmış olabilir, server'ın yeniden başlatılması ve PIN'in güncellenmesi
gerekiyor — bkz. "Test ortamı notları").

### Kaldığımız yerden devam etmek için (bir sonraki adım):

```sh
# 1. Windows'ta sunucuyu yeniden başlat (kendi konsolundan, SSH'tan değil —
#    SendInput SSH'ta engelleniyor):
#    C:\Users\os6\Desktop\bacak-remote-server.exe --fps 15 >
#      C:\Users\os6\Desktop\server-log.txt 2>&1
# 2. Yeni PIN'i oradan oku (SSH ile):
sshpass -e ssh os6@192.168.1.55 "type C:\\Users\\os6\\Desktop\\server-log.txt"
# 3. ~/.config/bacak-remote/pair.json'ı güncelle (ip zaten 192.168.1.55,
#    sadece pin'i güncel PIN ile değiştir).
# 4. Kullanıcıya BacakOS'ta Control Center -> "🖥 Uzak Masaüstü" tekrar
#    açtırıp yeni buton konumunu (sağ alt) görsel olarak doğrulat.
```

## Geri alma planı (hâlâ geçerli)

- Git etiketi: `pre-remote-desktop-ui-20260909-211713`
- Bilinen-çalışan binary yedeği: `/home/os2/backups/bacak-compositor.known-good.20260909-211757`
- Geri alma: `sudo cp /home/os2/backups/bacak-compositor.known-good.20260909-211757 /usr/bin/bacak-compositor`

## Test ortamı notları

- Windows sunucusu: `192.168.1.55`, `os6` kullanıcısı, `sshpass -e ssh
  os6@192.168.1.55` ile erişiliyor (parola `$SSHPASS` env'inde,
  konuşmada geçti — kalıcı olarak saklanmadı).
  `C:\Users\os6\Desktop\bacak-remote-server.exe --fps 15 >
  C:\Users\os6\Desktop\server-log.txt 2>&1` ile başlatılıyor, PIN
  dosyadan okunuyor.
- `~/.config/bacak-remote/pair.json` şu an gerçek Windows makinesine
  işaret ediyor (`{"ip": "192.168.1.55", "pin": <son PIN>}`) — PIN her
  server yeniden başlatmasında değişir, panel açılmadan önce güncellenmesi
  gerekir.
- Henüz yapılmayan: Faz 3 (Windows tarafı gerçek GUI/tepsi uygulaması),
  ekran içi PIN/IP giriş formu (compositor tarafında).
