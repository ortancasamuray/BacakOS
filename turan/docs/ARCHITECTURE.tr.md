# BDM Mimarisi — Türkçe Özet

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Bu, [ARCHITECTURE.md](ARCHITECTURE.md) dosyasının kısa Türkçe özetidir.

## 1. Bileşen ve yetki genel bakışı

Üç yetki alanı, tam olarak iki kez geçilir:

1. **root → greeter kullanıcısı**: daemon fork eder ve giriş arayüzünü
   çalıştırmak için `bacak-greeter`'a düşer. Arayüz geri yalnızca IPC soketi
   üzerinden konuşur.
2. **root → hedef kullanıcı**: kimlik doğrulamadan sonra daemon fork eder ve
   oturum başlatıcıyı çalıştırmak için kimliği doğrulanan kullanıcıya düşer.

Greeter asla root tutmaz ve asla PAM'a bağlanmaz. logind/polkit, güç ve
seat politikasını BDM'den bağımsız olarak uygular.

## 2. Rust modül yapısı

```
bacak-common (kütüphane, sistem bağımlılığı yok — her yerde birim testli)
├── config      Config + alt yapılar, TOML yükleme, varsayılanlar, deny_unknown_fields
├── users       UserProvider trait'i, PasswdProvider, UID-aralığı/shell/gizli politikası
├── sessions    .desktop ayrıştırma, Wayland/X11 keşfi, tekilleştirme + öncelik
├── ipc         Request/Response enum'ları (newline-delimited JSON), PROTOCOL_VERSION
├── power       PowerAction ↔ logind metodu ↔ polkit eylemi, yapılandırma geçidi
└── error       crate Error/Result

bacak-pam (kütüphane)
├── lib         Authenticator + Conversation trait'leri, Prompt, AuthError, PAM_SERVICE
├── mock        MockAuthenticator (test/geliştirme), bellek içi kimlik bilgileri
└── system      SystemAuthenticator (system-pam özelliği) — gerçek libpam

bacak-display-manager (ikili, root)
├── main        root kontrolü, yapılandırma yükleme, autologin-mi-greeter-mi, ana döngü
├── seat        çalışma zamanı dizini, greeter/kullanıcı id araması, XDG_RUNTIME_DIR
├── ipc         GreeterServer (tekini kabul et), Conn çerçeveleme, Outcome, dokunmatik ekran probu
├── auth        AuthSlot, IpcConversation (PAM↔IPC röle), arka uç seçimi
├── launch      yetki düşüren spawn'lar: greeter, oturum, autologin
└── power       logind delegasyonu (systemctl / zbus), yapılandırma + polkit geçidi

bacak-greeter (ikili + kütüphane, yetkisiz)
├── lib         GreeterClient (bağlan, listele, auth döngüsü, oturum başlat, güç)
└── main        referans TTY arayüzü; gui modülü `gui` özelliğinin arkasında

bacak-session-launcher (ikili, kullanıcı)
└── main        Exec-satırı kelime ayrımı, D-Bus oturum sarmalama, oturumun execve'si
```

## 3. Giriş akışı (mutlu yol)

```
daemon                         greeter                         PAM / logind
  │  greeter'ı başlat (uid düşür) │                                  │
  │─────────────────────────────▶                                  │
  │              Hello           │                                  │
  │◀─────────────────────────────                                  │
  │      Welcome{dokunmatik,     │                                  │
  │        son_kullanıcı, ...}   │                                  │
  │─────────────────────────────▶ render: logo, kullanıcı ızgarası │
  │         ListUsers/ListSessions                                  │
  │◀──────────────▶ Kullanıcılar / Oturumlar                       │
  │      StartAuth{kullanıcıadı} │                                  │
  │◀─────────────────────────────                                  │
  │   pam_start + pam_authenticate ───────────────────────────────▶│
  │      AuthPrompt{"Parola:", echo=hayır}                          │
  │─────────────────────────────▶ (maskeli alan / sanal klavye)    │
  │      AuthResponse{gizli}      │                                  │
  │◀─────────────────────────────                                  │
  │   gizli değeri PAM'a besle ───────────────────────────────────▶│
  │   pam_acct_mgmt OK                                              │
  │      AuthResult{başarılı}     │                                  │
  │─────────────────────────────▶                                  │
  │      StartSession{oturum_id} │                                  │
  │◀─────────────────────────────                                  │
  │   pam_setcred + pam_open_session ─────────────────────────────▶│ (logind
  │      SessionStarting          │                                  oturumu
  │─────────────────────────────▶ greeter kapanır                  kaydeder)
  │   fork → kullanıcıya düş → bacak-session-launcher'ı execve et → compositor
```

Hata dalları: yanlış bir gizli değer, kasıtlı olarak genel bir mesajla
`AuthResult{success=false}` üretir; greeter alana geri döner. `CancelAuth`,
devam eden bir konuşmayı iptal eder. Daemon tarafı bir hata (bilinmeyen
oturum, reddedilen güç eylemi), greeter'ı kapatmadan `Error{message}`
olarak bildirilir.

## 4. Eşzamanlılık modeli

Daemon tek-greeter'lı ve tamamen senkrondur: tek bir `accept()`, tek bir
sun-tamamlanana-kadar döngüsü. Bu, yetki sınırını denetlenebilir tutar. Her
iki PAM arka ucu da (mock ve gerçek libpam FFI) konuşmayı bu thread
üzerinde senkron çağırır — PAM bir gizli değer istediğinde, konuşma bir
`AuthPrompt` yazar ve aynı soket üzerinde `AuthResponse` okumayı bloklar.
Hiçbir worker thread veya kanal yoktur, bu yüzden akıl yürütülmesi gereken
paylaşılan değişebilir durum yoktur.

## 5. Çoklu monitör

Greeter, `bacak-compositor`'ın bir Wayland istemcisidir. Çıkış (output)
yönetimi compositor'ın işidir:

- **Yansıtılmış giriş (varsayılan)**: compositor, greeter yüzeyini bağlı
  her çıkışa yansıtır, böylece giriş kartı her birinde ortalanmış görünür.
- **Bağımsız çıkışlar**: compositor bunun yerine kartı birincil çıkışa
  yerleştirebilir, diğerlerinde yalnızca duvar kağıdını gösterebilir.
- Hotplug (bağlan/ayrıl), compositor'ın çıkış yöneticisi tarafından
  yönetilir; greeter yalnızca `wl_output` değişikliklerinde yeniden
  yerleşir.

Görsel tasarım için [GREETER_UI.md](GREETER_UI.md) dosyasına bakın.
