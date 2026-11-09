# bacak-guncel — BacakOS otomatik güncelleyici

Açılıştan ~1,5 dakika sonra (sonra 6 saatte bir) arka planda, kullanıcıya
sormadan BacakOS'u günceller. Canlı (ISO) oturumda çalışmaz.

```
bacak-guncel durum        # son çalışma, bekleyen ve kurulan güncellemeler
sudo bacak-guncel denetle # bekleyen güncellemeleri listele (kurmaz)
sudo bacak-guncel calis   # döngüyü şimdi çalıştır
journalctl -u bacak-guncel -u bacak-guncel-onar
```

Ayarlar: `/etc/bacak-guncel/bacak-guncel.conf` (kapatma, pil eşiği, boş
alan, eklentiler).

## Döngü

1. **Onarım** — önceki kurulum yarım kaldıysa (`/var/lib/bacak-guncel/yarim`
   ya da `dpkg --audit` sorun gösteriyorsa) önce o toparlanır; toparlanamazsa
   yeni hiçbir şeye başlanmaz.
2. **Denetle** — `apt-get update` + `apt-get -s upgrade --with-new-pkgs`.
   Yalnız yükseltme: hiçbir paket kaldırılmaz.
3. **İndir** — `--download-only`. Kapatmada kesilir, sonraki çalışmada kaldığı
   yerden sürer.
4. **Doğrula** — her `.deb` önbellekte, boyutu ve depo özeti (SHA256/MD5)
   tek tek denetlenir. **Hepsi tam değilse kurulum başlamaz.**
5. **Ön koşullar** — pille çalışıyorsa en az %40, kökte yeterli boş alan.
   Tutmazsa paketler önbellekte bekler, kurulum sonraki çalışmada yapılır.
6. **Kur** — `yarim` işareti fsync'le diske yazılır, sonra
   `apt-get --no-download upgrade` (eksik dosya varsa dpkg'ye hiç geçilmez):
   - `policy-rc.d` hizmet (yeniden) başlatmalarını engeller: açık oturum,
     giriş ekranı ve birleştirici kurulum ortasında kapanmaz; yeni sürümler
     yeniden başlatınca çalışır (`/run/reboot-required` yazılır).
   - `systemd-inhibit` kapatma/uykuyu bloklar; hizmet `KillMode=process` ve
     SIGTERM'ü kurulumda yok sayar → systemd kapatmayı dpkg bitene kadar
     (en çok 30 dk) erteler.
   - Yapılandırma dosyalarında yerel değişiklikler korunur (`confold`).
7. **Bitir** — `dpkg --audit` temizse işaret silinir, önbellek temizlenir.
   Değilse paketler önbellekte kalır ve hemen onarım denenir.

## Yarıda kalırsa

Elektrik kesilirse `yarim` işareti kalır. Sonraki açılışta
`bacak-guncel-onar.service` **giriş ekranından önce** ağ gerektirmeden
(`dpkg --configure -a`, `apt-get -f install --no-download`, önbellekteki
paketlerle) sistemi tutarlı hâle getirir; ardından normal döngü kalan
güncellemeyi tamamlar.

## Eklentiler

Güncelleme kaynakları eklentidir (`src/eklenti/`). `apt` yerleşiktir.
`/usr/lib/bacak-guncel/eklenti.d/` (ya da `/etc/bacak-guncel/eklenti.d/`)
altındaki her çalıştırılabilir dosya aynı adımları (`onar`, `denetle`,
`indir`, `dogrula`, `uygula`) argüman olarak alan bir eklentidir ve aynı
kurallarla (tam indirilmeden kurulmaz, kurulum kesilmez, yarım kalırsa onar)
çalışır. Sözleşme: `eklenti.d/README`.
