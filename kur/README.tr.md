# kur — BacakOS Sistem Kurulumcusu

🌐 **Türkçe** · [English](README.md)

`kur`, BacakOS'un Debian 13 (Trixie) için grafik sistem kurulumcusudur: Slint
tabanlı sihirbaz arayüzü — yerel ayar, saat dilimi, disk bölümleme, hesap
oluşturma, kurulum — güvenlik açısından kritik hiçbir işlem için kabuğa
(shell) çıkmayan saf Rust bir arka uç üzerinde çalışır. Modül haritası ve
iş parçacığı modeli için [ARCHITECTURE.md](ARCHITECTURE.md) (İngilizce) /
[ARCHITECTURE.tr.md](ARCHITECTURE.tr.md) (Türkçe özet) dosyalarına bakın.

## Neden yazılım tabanlı render

Arayüz yalnızca Slint'in yazılım tabanlı render motorunu kullanır — GPU
sürücüsü yüklenmemiş bir canlı (live) imajda çalışacağı garanti edilen tek
motor budur, ki kurulumcunun tam olarak çalıştığı ortam da budur. `altay`
ile aynı derlenmiş Slint bağımlılığını paylaşır.

## Derleme ve çalıştırma

```sh
cargo build --release
cargo test                    # arka uç, herhangi bir UI/ekrandan bağımsız birim testlidir
```

```sh
KUR_HEADLESS=1 ./target/release/kur   # önceden ayarlanmış (preseed) tarzda, ortam değişkeni tabanlı, penceresiz kurulum
```

## Canlı imajda başlatma

Doğrudan ikili dosyayı değil, her zaman **`kur-baslat`**'ı çalıştırın:

```sh
/usr/bin/kur-baslat
```

`kur` diskleri bölümlediği için root gerektirir — ama burada `pkexec`
çalışmaz: `auth_admin` polkit politikası PAM üzerinden gerçek bir parola
ister, canlı oturumun autologin kullanıcısının ise hiç parolası yoktur.
Bunun yerine `kur-baslat`, `sudo` üzerinden yeniden çalıştırır (canlı
kullanıcı zaten parolasız sudo yetkisine sahiptir); `sudo`'nun sildiği
`WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`/`DISPLAY` değişkenlerini argüman olarak
aktarır, ve root tarafındaki karşılığı olan `kur-root`, root'un dosya
izinlerinden bağımsız olarak `/run/user/<uid>` altındaki kullanıcının
Wayland soketini açabilme yeteneğini kullanarak devam eder.

## Gerçek diske karşı test

```sh
scripts/test-vm-boot.sh       # canlı imajı bir VM'de önyükler
scripts/test-vm-install.sh    # VM loop device'ına karşı tam bir kurulum çalıştırır
```

Birim testleri doğrulama ve planlama mantığını (hostname/hesap kuralları,
locale/saat dilimi ayrıştırma, bölümleme planlaması) hiçbir blok aygıta
dokunmadan kapsar; asıl `backend::install`'ı gerçek (sanal) donanıma karşı
çalıştıran VM betikleridir.

## Paketleme

```sh
dpkg-buildpackage -us -uc -b
```

Bkz. `debian/changelog` ve `debian/lintian-overrides`.

## Lisans

GPL-3.0-or-later.
