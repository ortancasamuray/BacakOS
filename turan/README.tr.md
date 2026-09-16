# Bacak Ekran Yöneticisi (BDM)

🌐 **Türkçe** · [English](README.md)

Bacak Masaüstü Ortamı için modern, güvenli, hafif, **Wayland-öncelikli** bir
ekran yöneticisi — LightDM, GDM ve SDDM'nin yerine, en baştan
dokunmatik-öncelikli ve çok-kullanıcılı tasarlandı.

> Durum: **çalışan iskelet**. Çekirdek (yapılandırma, kullanıcı numaralama,
> oturum keşfi, IPC protokolü, PAM soyutlaması, oturum başlatma, yetki
> ayrımı) uygulandı, derleniyor, birim/entegrasyon testli. Grafik greeter
> arayüzü ve gerçek `libpam` bağlantısı, altlarındaki her şeyin referans
> uygulamalarıyla birlikte özellik bayrağıyla (feature-gated) korunan
> dikişlerdir.

## Neden başka bir ekran yöneticisi?

| Hedef | BDM bunu nasıl yapıyor |
|---------------------|-----------------------------------------------------------|
| Wayland öncelikli | Greeter `bacak-compositor` üzerinde render edilir; X gerekmez. |
| Varsayılan olarak güvenli | Greeter tamamen yetkisizdir; yalnızca daemon root'tur. |
| Dokunmatik dostu | Dokunmatik ekran tespiti, ekran klavyesi + büyük dokunma hedeflerini tetikler. |
| Çok kullanıcılı | logind seat/oturumları; kullanıcı başına `XDG_RUNTIME_DIR`. |
| Hafif | Küçük Rust ikilileri, `panic=abort`, LTO release profili. |

## Bileşenler

```
bacak-display-manager   yetkili daemon    (root)      — PAM, seat, başlatma
bacak-greeter           giriş arayüzü     (yetkisiz)  — IPC istemcisi + arayüz
bacak-session-launcher  oturum çalıştırma (kullanıcı) — ortamı ayarlar, oturumu execve eder
bacak-common            paylaşılan kütüphane — yapılandırma, kullanıcılar, oturumlar, IPC, güç
bacak-pam               PAM soyutlaması   — konuşma modeli + arka uçlar
```

## Sistem akışı

```
önyükleme → systemd → bacak-display-manager (root)
                      │  /etc/bacak-display-manager.conf'u okur
                      ├─ autologin? ─evet→ bacak-session-launcher → bacak-compositor
                      └─hayır→ bacak-greeter'ı (yetkisiz) bacak-compositor üzerinde başlatır
                                 │  IPC, /run/bacak-display-manager/greeter.sock üzerinden
                                 │  daemon→PAM üzerinden kimlik doğrulama
                                 ↓ başarılı
                            bacak-session-launcher (kullanıcı olarak)
                                 ↓
                            bacak-compositor → masaüstü oturumu
```

## Depo yerleşimi

```
Cargo.toml                     workspace
crates/
  bacak-common/                yapılandırma, kullanıcılar, oturumlar, ipc, güç  (+15 birim testi)
  bacak-pam/                   Authenticator/Conversation; mock + system-pam arka uçları
  bacak-display-manager/       daemon: main, seat, ipc, auth, launch, power
  bacak-greeter/               GreeterClient çekirdeği + referans TTY arayüzü (+ e2e testi)
  bacak-session-launcher/      Exec-satırı ayrıştırıcı + oturum çalıştırma
config/bacak-display-manager.conf   açıklamalı örnek yapılandırma
systemd/bacak-display-manager.service
pam/bacak-display-manager           PAM servisi (etkileşimli)
pam/bacak-autologin                 PAM servisi (autologin)
sessions/bacak.desktop              örnek Wayland oturum girdisi
docs/                          tasarım belgeleri (aşağıya bakın)
```

## Belgeler (tasarım teslimatları)

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) (İngilizce) / [docs/ARCHITECTURE.tr.md](docs/ARCHITECTURE.tr.md) (Türkçe özet) — mimari diyagram, modül yapısı, giriş akışı.
- [docs/SECURITY.md](docs/SECURITY.md) — güvenlik modeli ve yetki ayrımı.
- [docs/PAM_INTEGRATION.md](docs/PAM_INTEGRATION.md) — PAM konuşma tasarımı.
- [docs/SYSTEMD.md](docs/SYSTEMD.md) — servis tasarımı ve VT/seat yönetimi.
- [docs/SESSION_STARTUP.md](docs/SESSION_STARTUP.md) — oturum başlatma/durdurma sırası.
- [docs/GREETER_UI.md](docs/GREETER_UI.md) — giriş arayüzü, dokunmatik, sanal klavye, tema, çoklu monitör, erişilebilirlik.
- [docs/PROTOTYPE.md](docs/PROTOTYPE.md) — BDM'yi bugün **weston** üzerinde çalıştırma (gerçek compositor henüz yazılmadı).

## Derleme ve test

```sh
cargo build                 # varsayılan: her şeyi mock auth arka ucuyla derler
cargo test                  # workspace genelinde 24 test

# Gerçek PAM ile production derlemesi (libpam.so.0'a doğrudan bağlanır —
# libpam0g-dev, bindgen veya libclang gerekmez; sadece çalışma zamanı libpam paketi):
cargo build --release -p bacak-display-manager --features system-pam
# Grafik greeter (egui/eframe; wayland/xkbcommon/fontconfig geliştirme kütüphaneleri gerekir):
cargo build --release -p bacak-greeter --features gui
# Root/PAM/compositor olmadan, paket içindeki mock daemon ile çalıştır:
#   BDM_GREETER_SOCKET=/tmp/bdm.sock cargo run -p bacak-greeter --example mock_daemon &
#   BDM_GREETER_SOCKET=/tmp/bdm.sock cargo run -p bacak-greeter --features gui
```

### Gerçek bir makineye kurulum

Paket, kurulum/geçiş/kurtarma/kaldırma işlemlerini bir arada sunan
**`bacak-dm-setup`** adlı bir kurulum yardımcısı içerir (kaynak ağacından
`packaging/install.sh` olarak da çalıştırılabilir):

```sh
sudo apt install ./target/debian/bacak-display-manager_*_amd64.deb   # bacak-dm-setup'ı verir
sudo bacak-dm-setup install                  # compositor + oturum + bağımlılıklar (güvenli; DM değişmez)
sudo bacak-dm-setup enable                    # BDM'yi ekran yöneticisi yap (onay ister; uygulamak için yeniden başlat)
sudo bacak-dm-setup enable --autologin <kullanıcı> # …autologin ile (en çok doğrulanmış mod)
sudo bacak-dm-setup status                    # ne kurulu/aktif
sudo bacak-dm-setup revert                     # KURTARMA: önceki DM'yi yeniden etkinleştir (Ctrl+Alt+F3'ten çalıştır)
sudo bacak-dm-setup uninstall                  # geri al + paketi kaldır
```

`install` mevcut ekran yöneticinize asla dokunmaz; yalnızca `enable`
dokunur ve kurtarma adımlarını yazdırıp önceki DM'yi hatırlayarak
`revert`'in onu geri yükleyebilmesini sağlar. Compositor:
`BDM_COMPOSITOR=/yol` veya `BDM_COMPOSITOR_SRC=/yol/to/bacak` geçin (yoksa
paket içindeki weston sarmalayıcısı kullanılır).

### Root olmadan protokolü deneme

Referans TTY greeter, GUI ile birebir aynı protokolü konuşur. Çalışan bir
daemon varken `$BDM_GREETER_SOCKET` üzerinden bağlanır;
`bacak-greeter/tests/protocol.rs` entegrasyon testi, geçici bir soket
üzerinden komut dosyalı bir daemon'a karşı tam giriş konuşmasını çalıştırır
— root, PAM veya compositor gerekmez.

## Sürekli entegrasyon

CI **GitHub'a bağlı değil** — `packaging/vm/smoke-test.sh`, sağlayıcıdan
bağımsız bir shell betiğidir; KVM'i olan herhangi bir runner çalıştırabilir.
İş akışları `.forgejo/workflows/` altında yaşar (Forgejo/Gitea Actions,
özgür yazılım, kendi kendine barındırılabilir; aynı YAML Gitea'da da
çalışır). Rust, adım içinde `rustup` ile kurulur, tek kullanılan action
`actions/checkout`'tur.

- **`ci.yml`** — her push/PR'de: `rustfmt --check`, `clippy -D warnings`,
  `cargo test`, `system-pam`/`gui` özellik derlemeleri ve bir `.deb`
  derlemesi + `lintian`. Herhangi bir Debian/Ubuntu runner'da çalışır
  (`runs-on: ubuntu-latest`).
- **`vm-smoke-test.yml`** — talep üzerine/haftalık/prototip değişince: bir
  virtio-gpu (virgl 3D) seat'li Debian VM önyükler ve **gerçek
  `bacak-compositor`** tarafından barındırılan bare-metal logind `greeter`
  oturumunu doğrular (DRM master + `$BACAK_STARTUP`; bkz.
  [docs/PROTOTYPE.md](docs/PROTOTYPE.md)). **KVM destekli bir runner**
  gerektirir (`runs-on: self-hosted`; host modu veya `--device /dev/kvm`
  ile bir konteyner). Compositor ayrı bir depodan derlenir — yapılandırma:
  `vars.BACAK_COMPOSITOR_REPO` (varsayılan `bacak-os/bacak`), isteğe bağlı
  `vars.BACAK_COMPOSITOR_REF` / `secrets.BACAK_REPO_TOKEN`.

Woodpecker CI kullananlar için `.woodpecker/` altında bir **Woodpecker CI**
varyantı da sağlanır (`ci.yml` + `vm-smoke-test.yml`):
- `ci.yml` — `rust:bookworm` konteynerlerinde derleme/test/lint/paketleme (KVM gerekmez).
- `vm-smoke-test.yml` — VM işi; KVM için ya `privileged: true` (docker
  arka ucu) ile *güvenilir* bir depo ya da bir KVM host'unda
  **local-backend** bir agent gerekir. Compositor tek bir gizli URL'den
  klonlanır (`bacak_compositor_clone_url`, özel depolar için token
  gömülebilir).

İkisi de yok mu? Betik her yerde çalışır: yerel olarak
(`packaging/vm/smoke-test.sh`, isteğe bağlı bir systemd timer/cron ile)
veya GitLab CI / Drone / Jenkins altında — her biri sadece o betiği bir
KVM host'unda çağırmalı. (Önceki GitHub Actions iş akışları
`.forgejo/`'ya taşındı; ikisini de istersen yeniden üretmemi iste.)

## Özellik bayrakları

| Crate | Özellik | Etki |
|-------------------------|---------------|------------------------------------------------|
| `bacak-pam` | `system-pam` | Küçük, elle yazılmış bir FFI üzerinden gerçek `libpam`. |
| `bacak-display-manager` | `system-pam` | Daemon gerçek PAM arka ucunu kullanır. |
| `bacak-greeter` | `gui` | egui/eframe grafik arayüzünü derler. |

Varsayılan derlemeler, akışın geliştirme için herhangi bir makinede uçtan
uca çalışması için **mock** bir doğrulayıcı kullanır (`bacak` parolasını
kabul eder). `system-pam` olmadan derlendiğinde daemon yüksek sesli bir
uyarı loglar; o derlemeyi asla canlıya almayın.

## Lisans

GPL-3.0-or-later.
