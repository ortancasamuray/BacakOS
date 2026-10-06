# Bacak Onay

🌐 **Türkçe** · [English](README.md)

BacakOS için iki adımlı giriş: **Bacak Onay** Android uygulaması (TOTP/HOTP
doğrulayıcı) + **Turan** (Bacak Ekran Yöneticisi, `bacak-display-manager`) giriş
ekranında kod soran `pam_bacakonay.so` modülü.

```
 Telefon (Bacak Onay)                     BacakOS bilgisayar
 ┌─────────────────────┐   QR (otpauth://)  ┌──────────────────────────────┐
 │ Android Keystore    │◄───────────────────│ sudo bacakonay kur           │
 │  └ sarma anahtarı   │   (bir kez)        │  └ /var/lib/bacakonay/<user> │
 │ AES-GCM kasa        │                    │      (root, 0600)            │
 │ TOTP (RFC 6238)     │  6 haneli kod      │ Turan giriş ekranı           │
 │  "123 456" ◔ 18 sn  │──── kullanıcı ────►│  parola → pam_unix           │
 └─────────────────────┘     yazar          │  kod    → pam_bacakonay      │
                                            └──────────────────────────────┘
```

Ağ bağlantısı yok: telefon ile bilgisayar yalnızca eşleme QR'ı üzerinden bir kez
"konuşur", kodlar iki tarafta da saatten hesaplanır. Uygulamanın internet izni
bile yoktur.

## Dizin yapısı

```
android/                         Bacak Onay (Kotlin, Jetpack Compose, Material 3)
  app/src/main/kotlin/org/anadolupanteri/bacakonay/
    domain/                      saf Kotlin — Android'e bağımlı değil
      model/OtpAccount.kt          hesap, algoritma, kod modeli
      otp/TotpGenerator.kt         RFC 4226 HOTP / RFC 6238 TOTP (javax.crypto.Mac)
      otp/OtpAuthUri.kt            otpauth:// ayrıştırıcı
      otp/Base32.kt                RFC 4648 base32
      repository/AccountRepository.kt
    data/
      crypto/CryptoManager.kt      Keystore sarma anahtarı + AES-256-GCM
      vault/VaultStore.kt          şifreli kasa dosyaları (AtomicFile)
      vault/VaultManager.kt        kur / kilit aç durum makinesi
      repository/VaultAccountRepository.kt
    ui/
      lock/                        BiometricPrompt + CryptoObject, kilit ekranı
      codes/                       kod listesi, geri sayım halkası, kopyalama
      scan/QrScanScreen.kt         CameraX + ZXing QR tarama
      add/ManualEntryScreen.kt     elle ekleme
  app/src/test/                  RFC test vektörleri, URI, kasa kodlaması
linux/                           Rust workspace
  crates/bacakonay-core/         OTP, base32, otpauth URI, kayıt deposu
  crates/bacakonay-cli/          `bacakonay` komutu (+ deb paketi)
  crates/pam-bacakonay/          pam_bacakonay.so (+ gerçek libpam ile uçtan uca test)
  pam/                           diğer PAM servisleri için örnek satırlar
```

Turan tarafında değişiklik yalnızca `turan/pam/bacak-display-manager` dosyasına
eklenen iki satırdır; greeter, PAM'in ikinci sorusunu ("Bacak Onay kodu:") zaten
ayrı bir giriş adımı olarak gösteriyor.

## Güvenlik modeli

**Telefon**
- Gizli anahtarlar `filesDir/vault.bin` içinde AES-256-GCM ile şifreli. Kasa
  anahtarı, Android Keystore'da (varsa StrongBox, yoksa TEE) üretilen, dışarı
  çıkarılamayan bir anahtarla sarılı.
- Sarma anahtarı **her kullanımda** kullanıcı doğrulaması ister ve
  `BiometricPrompt.CryptoObject` ile açılır: parmak izi/yüz (Android 11+'da
  PIN/desen de) doğrulaması kriptografik olarak şifre çözmeye bağlıdır, yalnızca
  bir arayüz kapısı değildir.
- Uygulama arka plana geçince kasa kilitlenir, çözülmüş anahtarlar bellekten
  silinir. `FLAG_SECURE`: ekran görüntüsü, kayıt, yayın ve son uygulamalar
  önizlemesi engelli. Yedekleme kapalı (geri yüklenen kasa zaten açılamaz).
- Kopyalanan kod Android 13+'da "hassas" işaretlenir, 30 sn sonra panodan silinir.
- Yeni parmak izi eklemek kasayı bozmaz; **ekran kilidini kaldırmak** Keystore
  anahtarını kalıcı olarak siler → hesaplar yeniden eklenmelidir.

**Bilgisayar**
- Kayıtlar `/var/lib/bacakonay/<kullanıcı>` — root'a ait, dizin 0700, dosya 0600.
  Modül sahibi/izinleri her okumada denetler; uymayan, sembolik bağlantı olan
  ya da bozuk kayıtta **girişi reddeder** (fail closed).
- Kullanıcı kendi gizli anahtarını okuyamaz ya da değiştiremez; kayıt yalnızca
  `sudo` ile yapılır.
- Kullanılan kod tüketilir (`last_step`): aynı kod 30 sn içinde bile ikinci kez
  geçmez. Doğrula-ve-tüket işlemi `flock` altında, eşzamanlı iki giriş aynı kodu
  harcayamaz.
- ±1 adım (±30 sn) saat kayması kabul edilir (`pencere=N` ile değişir).
- Yanlış kod `pam_faillock authfail` satırına düşer: kaba kuvvet denemeleri
  parola denemeleriyle birlikte sayılır ve hesabı kilitler.
- Kaydı olmayan kullanıcı etkilenmez (`PAM_IGNORE`); `zorunlu` seçeneğiyle
  herkese zorunlu yapılabilir.
- Paket kurulu değilse Turan'daki satır girişi **bozmaz**: eksik modül
  `PAM_MODULE_UNKNOWN` döndürür, `module_unknown=1` bu durumda satırı atlar
  (`linux/crates/pam-bacakonay/tests/pam_stack.rs` bunu gerçek libpam ile test eder).

## Derleme

```sh
# Linux (CLI + PAM modülü + deb)
cd linux
cargo test                                   # RFC vektörleri + gerçek libpam testleri
cargo build --release
cargo deb --no-build -p bacakonay-cli        # → target/debian/bacakonay_0.1.0-1_amd64.deb

# Android
cd android
./gradlew testDebugUnitTest                  # RFC vektörleri, URI, kasa
./gradlew assembleRelease                    # keystore.properties varsa imzalı
```

Android imzalama uzakel-android ile aynı düzendedir: git'e girmeyen
`android/keystore.properties` (`storeFile`, `storePassword`, `keyAlias`,
`keyPassword`).

## BacakOS'ta uçtan uca kurulum

1. **Paketleri kur.** `bacakonay` paketini ve güncel `bacak-display-manager`'ı
   (yeni PAM satırını içeren) kurun:
   ```sh
   sudo apt install ./bacakonay_0.1.0-1_amd64.deb
   ```
   Halihazırda kurulu bir sistemde `/etc/pam.d/bacak-display-manager` dosyası
   düzenlenmiş bir yapılandırma dosyasıdır; dpkg yeni sürümü sorabilir. Elle
   eklemek için `linux/pam/bacak-display-manager.snippet`'teki iki satırı
   `pam_unix.so` ve onu izleyen `authfail` satırının hemen altına koyun.

2. **Telefonu hazırla.** Bacak Onay'ı kurun, açın, parmak izi/PIN ile kasayı
   oluşturun. Cihazda ekran kilidi olmalı.

3. **Kullanıcıyı kaydet** (o kullanıcının oturumunda, terminalde):
   ```sh
   sudo bacakonay kur
   ```
   Terminalde QR çıkar. Telefonda **QR tara** ile okutun, uygulamada beliren
   6 haneli kodu terminale yazın. Kod tutarsa kayıt etkinleşir; tutmazsa hiçbir
   şey değişmez (yanlış okutma sizi kilitleyemez). Seçenekler:
   `--algoritma SHA256`, `--hane 8`, başka kullanıcı için `sudo bacakonay kur ayse`.

4. **Dene** (oturumu kapatmadan önce):
   ```sh
   sudo bacakonay dogrula        # telefondaki kodu sorar; kodu tüketmez
   sudo bacakonay durum
   ```

5. **Giriş.** Oturumu kapatın. Turan'da parolayı yazın → "Bacak Onay kodu:"
   ekranı gelir → telefondaki kodu yazın.

6. **Geri alma.** `sudo bacakonay kaldir` iki adımlı doğrulamayı kapatır.
   Telefonu kaybederseniz başka bir yönetici hesabıyla ya da kurtarma
   kipinde (root) `bacakonay kaldir <kullanıcı>` çalıştırın.

### Sorun giderme

| Belirti | Neden / çözüm |
|---|---|
| "Bacak Onay kodu geçersiz" | Telefonun saati otomatik mi? ±30 sn'den fazla kayma reddedilir. Aynı kod ikinci kez kullanılamaz — sonraki kodu bekleyin. |
| Kod hiç sorulmuyor | `sudo bacakonay durum`; PAM satırı `/etc/pam.d/bacak-display-manager`'da mı? |
| Kodu doğru olsa da giriş reddediliyor | `journalctl -t bacak-display-manager \| grep bacakonay` — "güvensiz kayıt" görüyorsanız `/var/lib/bacakonay` izinlerini düzeltin (`chmod 700`, dosyalar `600`, sahibi root). |
| Uygulama "Kasa anahtarı geçersiz" diyor | Ekran kilidi kaldırılmış; yeni kasa oluşturup `sudo bacakonay kur --zorla` ile yeniden eşleyin. |
| Çok sayıda hatalı deneme sonrası giriş yok | `pam_faillock` kilidi: `sudo faillock --user <kullanıcı> --reset`. |

## Sonraki adımlar

- Kayıt arayüzünü Kontrol Merkezi'ne taşımak (compositor'daki mevcut QR
  çizicisiyle), böylece terminal gerekmez.
- Ekran kilidi (oturum kilitleme) için aynı PAM satırı.
- Şifreli dışa/içe aktarma (telefon değişimi).
