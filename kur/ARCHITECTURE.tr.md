# kur — Mimari Özeti

🌐 **Türkçe özet** · [English (full)](ARCHITECTURE.md)

Bu, [ARCHITECTURE.md](ARCHITECTURE.md) dosyasının kısa Türkçe özetidir.
Saf Rust bir kurulumcu arka ucu üzerinde çalışan bir Slint sihirbaz arayüzü.
Bu kod tabanının üzerine kurulduğu sert kural, `main.rs`'nin kendi
doc-comment'inde yazıyor: **arka uç asla arayüzü import etmez, arayüz asla
bir komut çalıştırmaz.**

## Modül haritası

```
kur/
├─ src/
│  ├─ main.rs        # kontrolcü kabuğu: sihirbaz durumu, adım gezinmesi,
│  │                  # kurulum thread'ini Slint olay döngüsüne bağlar
│  ├─ pages.rs        # sayfa başına Slint callback bağlantıları (0–3. adımlar)
│  ├─ wizard.rs        # disk sayfasının değişebilir seçim durumu + kuralları
│  ├─ headless.rs      # ortam değişkeni tabanlı, gözetimsiz kurulum — aynı motor, penceresiz
│  └─ backend/         # sisteme dokunan her şey — arayüzden bağımsız, birim testli
│     ├─ disk.rs        # `lsblk -J` ile blok aygıt keşfi
│     ├─ plan/          # bölümleme planlaması — donanımsız, incelenebilir `Plan`
│     ├─ install.rs     # kurulum motoru: aşama tablosu, thread'leme, ilerleme hesabı
│     ├─ stages.rs      # çalışma sırasına göre tek tek kurulum aşamaları
│     ├─ medium.rs      # kur'un üzerinden çalıştığı squashfs imaj(lar)ını bulur
│     ├─ locale.rs      # yerel ayar/klavye düzeni keşfi (locales/xkb-data'dan okur)
│     ├─ timezone.rs    # saat dilimi keşfi (tzdata'nın zone1970.tab'ını ayrıştırır)
│     ├─ user.rs        # hesap/hostname doğrulaması (adduser(8) + RFC 1123 kuralları)
│     └─ cmd.rs         # ortak subprocess çağırma yardımcıları
├─ ui/                  # Slint arayüzü
├─ scripts/
│  ├─ kur-baslat         # kullanıcı tarafı başlatıcı — gerçek giriş noktası
│  ├─ kur-root           # kur-baslat'ın sudo ile çağırdığı root tarafı sarmalayıcı
│  └─ test-vm-*.sh       # VM tabanlı boot/kurulum testleri
└─ debian/               # paketleme: changelog, lintian-overrides
```

## Arayüz ↔ arka uç sınırı

`main.rs` sihirbaz kabuğunu (hangi adımın aktif olduğu, paylaşılan durum,
arka ucun kurulum thread'i ile Slint'in olay döngüsü arasındaki köprü)
sahiplenir. `pages.rs` bir sayfanın *içinde* ne olduğunu sahiplenir — her
adım için başlangıçta bir kez çağrılan bir `wire_*` fonksiyonu. `backend/`
yalnızca düz veri tipleriyle uğraşır ve ilerlemeyi callback'ler üzerinden
bildirir — hiçbir backend modülü arayüzün varlığından haberdar değildir, bu
da aynı motorun `headless.rs`'i hiç pencere olmadan çalıştırabilmesini
sağlar.

## Disk planlama ve yürütme

`backend::plan::Plan`, bir diske ne yapılacağının tam, incelenebilir bir
tanımıdır — donanıma hiç dokunmadan oluşturulur, doğrulanır, metne
dönüştürülür, tamamen birim test edilebilir. `backend::install` bir
`Plan`'ı yürüten tek modüldür.

## Kurulum motoru thread modeli

`backend::install::spawn` tüm kurulumu düz bir `std::thread`'e taşır ve
`Fn(Progress) + Send` bir callback üzerinden geri bildirim yapar — burada
async runtime yoktur. `stages.rs`, sırayla çalıştırılan aşama tablosunu
tanımlar; her aşamanın silinmiş bir diskten yeniden başlatılabilir olması
beklenir — hiçbiri yarım kalmış bir hedefe karşı idempotent olmaya
çalışmaz.

## Root yetki el sıkışması

```
kur-baslat (kullanıcı, canlı oturum)
    │  WAYLAND_DISPLAY / XDG_RUNTIME_DIR / DISPLAY'i argv olarak iletir
    ▼
sudo kur-root $WAYLAND_DISPLAY $XDG_RUNTIME_DIR $DISPLAY
    │  root, dosya izinlerinden bağımsız olarak kullanıcının
    │  /run/user/<uid> altındaki Wayland soketini açabilir
    ▼
kur (root olarak çalışır, kullanıcının compositor oturumuna render eder)
```

## Headless mod

`headless.rs`, sihirbazın preseed tarzı karşılığıdır: aynı motor, tamamen
ortam değişkenleriyle yönetilir (`KUR_HEADLESS=1`, herhangi bir Slint
penceresi oluşturulmadan önce `main`'de bu moda geçirir). CI'da veya bir
VM'de, ekran olmadan uçtan uca test edilebilmesi için vardır.

## Test

```sh
cargo test                      # disk/plan/locale/timezone/user doğrulama mantığı
scripts/test-vm-boot.sh         # canlı imaj önyükleme duman testi
scripts/test-vm-install.sh      # gerçek (sanal) diske karşı tam kurulum
```
