# Bacak OS — Tasarım Sistemi

🌐 **Türkçe** · [English](DESIGN_SYSTEM.md)

Premium, teknoloji-öncelikli bir görsel dil: Ege gradyanı üzerinde
glassmorphism, Rust turuncusu vurgu rengiyle. Dokunmatik-öncelikli
etkileşim alanları, yay fiziği (spring physics) hareketi ve baştan sona
8pt ritim.

---

## 1. Token'lar

### 1.1 Renk

| Rol | Token | Değer |
| ------------------ | ------------------ | ---------- |
| Rust vurgu (birincil)  | `--rust-500`   | `#D96C2D`  |
| Rust vurgu (hover)    | `--rust-400`   | `#EF8348`  |
| Rust vurgu (soluk)   | `--rust-300`   | `#F9A679`  |
| Rust parıltısı              | `--rust-glow`  | `rgba(217,108,45,0.55)` |
| Ege — derin         | `--aegean-deep`| `#07334A`  |
| Ege — orta          | `--aegean-mid` | `#1A6C8A`  |
| Ege — yumuşak         | `--aegean-soft`| `#5CB0C4`  |
| Ege — soluk         | `--aegean-pale`| `#A8DDE6`  |
| Ege — köpük         | `--aegean-foam`| `#E6F6F8`  |

**Arka plan.** Her zaman çok durakl bir radial bileşim — asla düz bir renk
değil. Varsayılan masaüstü:

```css
background:
  radial-gradient(120% 80% at 80% 10%, #2c8aa3 0%, transparent 55%),
  radial-gradient( 90% 70% at 10% 90%, #0f5970 0%, transparent 60%),
  linear-gradient(180deg, #0c4e6b 0%, #0a3c55 45%, #062a3d 100%);
```

`filter: blur(80px)` ile üç yüzen orb, yavaş bir paralaks ekler (22 sn
döngü). Tam ekranda görünecek bant efektini (banding) yok eden, %4
opaklıkta, `mix-blend-mode: overlay` ile 3 px'lik radial-nokta grenli bir
doku.

### 1.2 Cam yüzeyler

| Yüzey        | Arka plan                    | Kenarlık                       | Bulanıklık     |
| -------------- | ------------------------------ | ----------------------------- | -------- |
| Dock           | `rgba(255,255,255,0.10)`      | `rgba(255,255,255,0.22)`     | 30 px + saturate(160%) |
| Pencere çerçevesi  | `rgba(12,38,56,0.55)`         | `rgba(255,255,255,0.22)`     | 28 px + saturate(140%) |
| Klavye          | `rgba(255,255,255,0.10)`      | `rgba(255,255,255,0.22)`     | 34 px + saturate(160%) |
| Araç ipucu / menü | `rgba(8,26,38,0.92)` (opak)  | `rgba(255,255,255,0.14)`     | — |

Kural: *masaüstünün üzerinde yüzen* her şey bulanıklaşır; *metin taşıyan*
hiçbir şey bulanıklaşmaz (okunabilirlik kazanır). Firefox içerik alanı
kasıtlı olarak bulanıklaştırılmaz — yalnızca çerçevesi bulanıklaşır.

### 1.3 Gölge ve derinlik hiyerarşisi

| Yükseklik | Kullanım                | Gölge                                                                              |
| --------- | ------------------- | ----------------------------------------------------------------------------------- |
| 0         | Masaüstü duvar kağıdı  | yok                                                                                |
| 1         | Etkin olmayan pencere    | `0 18px 60px -18px rgba(2,28,42,0.55), 0 2px 12px -4px rgba(2,28,42,0.35)`         |
| 2         | Odaktaki pencere     | `0 24px 72px -16px rgba(2,28,42,0.70), 0 0 36px -4px var(--rust-glow)`              |
| 3         | Dock / Ekran klavyesi | 1'i miras alır + iç highlight `inset 0 1px 0 rgba(255,255,255,0.18)`                 |

Odak = yumuşak, Rust tonlu bir parıltı, asla sert bir çerçeve değil. Etkin
olmayan pencereler hem parıltıyı kaybeder *hem de* `opacity: 0.92 +
filter: saturate(85%)`'e düşer — var ama sessiz.

### 1.4 Boşluk (8pt)

`4 · 8 · 12 · 16 · 20 · 24 · 32 · 40` → `--s-1 … --s-10`. Yarım adım
`4 px` yalnızca ikon-içi dolgu için izinlidir. Bunun dışındaki her şey
8'lik ızgaraya oturur.

### 1.5 Yarıçap

| Token        | Değer | Kullanım                          |
| ------------ | ----- | ---------------------------- |
| `--r-sm`     | 8 px  | girdi hapları, tuş kapağı içi  |
| `--r-md`     | 14 px | kartlar, dock ikonları            |
| `--r-lg`     | 22 px | pencereler, yerleştirme önizlemesi         |
| `--r-xl`     | 32 px | dock konteyneri, ekran klavyesi kabuğu    |
| `--r-pill`   | 999   | adres çubuğu, etiket çipleri       |

### 1.6 Tipografi

- **Aile.** Önce `Inter`, sonra SF Pro Text, system-ui'ye düşer.
- **Ölçek.** 28 (display) · 18 (h2) · 15 (büyük gövde) · 14 (gövde) · 13 (kompakt) · 12 (başlık altı) · 10 (meta).
- **Ağırlık.** Gövde için 400, arayüz etiketleri için 500, vurgu için 600, hero başlıklar için ayrılmış 700.
- **Harf aralığı.** Display başlığı için negatif (`-0.02em`); dijital saatte iki noktanın ortalı kalması için +0.3 px.

Hero `<h1>` beyazdan `--rust-300`'e %90'da bir gradyan metin doldurması
kullanır — soğuk tipografiye sıcak vurguyu karıştırdığımız *tek* yer.

### 1.7 Hareket

| Eğri            | Değer                                  | Kullanım                                  |
| ---------------- | --------------------------------------- | ------------------------------------ |
| `--ease-spring`  | `cubic-bezier(0.34, 1.56, 0.64, 1)`    | Pencere yerleştirme, dock büyütme, tuş basışı |
| `--ease-out`     | `cubic-bezier(0.16, 1, 0.3, 1)`        | Opaklık, renk, yerleştirme önizleme solması    |
| `--ease-inout`   | `cubic-bezier(0.65, 0, 0.35, 1)`       | Çalışma alanı kaydırma, orb sürüklenmesi |

| Süre | Token       | Kullanım                                |
| -------- | ----------- | ----------------------------------- |
| 140 ms   | `--t-fast`  | Hover, araç ipucu, tuş basışı          |
| 280 ms   | `--t-med`   | Pencere taşıma/yerleştirme, büyütme    |
| 460 ms   | `--t-slow`  | Ekran klavyesi aç/kapat, çalışma alanı geçişi   |

Kural: **asla doğrusal değil.** Sistemdeki tek doğrusal hareket saattir.

---

## 2. Bileşenler

### 2.1 Dock

```
┌────────────────────────────────────────────────────────────┐
│  ▢  ▣  ▢  ▢  ▢  │  📶  🔋  🔊  │  14:22                   │
│   ·  •  ·  ·  ·                       Pzt, 11 Mayıs         │
└────────────────────────────────────────────────────────────┘
```

- Cam kabuğun içinde 48 px uygulama karoları, 8 px boşluk, 12 px dolgu.
- **Büyütme.** Hover, odaktaki ikonu 1.18× büyütür ve 10 px yukarı kaldırır;
  *komşular* `:has(+ .dock-app:hover)` ile 1.06×'e büyür. Büyütme için JS
  gerekmez, tamamen CSS.
- **Gösterge.** İkonun 6 px altında, Rust vurgusunda yumuşak parıltılı
  4 px'lik bir nokta. Gösterge yalnızca uygulamanın en az bir açık
  penceresi varken görünür.
- **Durumlar.**
  - *Boşta.* Tam opaklık, tam cam.
  - *Uygulama odakta.* Dock `opacity: 0.72`'ye kararır (`.dock--dimmed`).
  - *Tam ekran.* Dock `Y+100% + 24` taşır ve `opacity: 0`. Kenar-açığa
    çıkarma: ekran altında 6 px'lik bir dokunma alanı hover'da görünür
    kılar.
- **Tepsi.** Ağ, pil, ses, saat. Araç ipuçları hover'da 140 ms solma ile
  üstte belirir.
- **Sağ-tık (planlanan).** Son kullanılanlar, sabitlenmiş eylemler,
  "Tüm pencereleri göster" içeren bağlam menüsü → uygulamaya göre
  önceden filtrelenmiş görev değiştiriciyi tetikler.

### 2.2 Pencere

```
┌────────────────────────────────────────────────────┐
│ ●●●   Mozilla Firefox                              │  ← başlık çubuğu (36 px)
├────────────────────────────────────────────────────┤
│ ‹ › ⟳   [ ⌬  https://_____________ ]   ≡          │  ← uygulama çerçevesi
├────────────────────────────────────────────────────┤
│                                                    │
│              ( uygulama görünüm alanı )             │
│                                                    │
│                                              ╲╲   │  ← yeniden boyutlandırma tutamağı
└────────────────────────────────────────────────────┘
```

- Başlık çubuğu pencereyi sürükler. Kontroller (kapat/küçült/büyüt) solda,
  mac tarzı — glassmorphic tasarımlar için en yaygın çerçeve deseni.
- Odak durumu Rust tonlu parıltıyı ekler; blur durumu doygunluğu azaltır
  ve %92'ye kararır.
- **Yerleştirme (snap) bölgeleri.** 24 px'lik sıcak kenarlar. Sürükleme
  yaklaştığında → yay eğrisiyle turuncu tonlu bir önizleme dikdörtgeni
  belirir, sonra pencere `pointerup`'ta kararını verir. Bölgeler:
  sol/sağ yarım, üst = büyüt, dört köşe = çeyrek döşeme.
- **Yeniden boyutlandırma.** Prototip için sağ-alt tutamak; production
  8 yöne de, hover'a kadar görünmeyen imleçle uyumlu tutamaklarla destek
  verecek.

### 2.3 Ekran klavyesi

```
┌──────────────────────────────────────────────────────┐
│  [https://] [bacak.dev] [rust-lang.org]  …           │ ← öneriler (yuvarlak haplar)
│  1 2 3 4 5 6 7 8 9 0                                 │
│  q w e r t y u i o p                                 │
│   a s d f g h j k l                                  │
│  ⇧  z x c v b n m  ⌫                                │
│  ⎚   @  .  [ boşluk ]  /  ⏎                          │
└──────────────────────────────────────────────────────┘
```

- Girdi odaklandığında `--ease-spring` ile 460 ms'de alttan kayar.
- 44 px tuş yüksekliği — Apple'ın minimum dokunma hedefi. Değiştirici
  tuşlar (`⇧ ⌫ ⏎ ⎚`) `key--wide` alır; boşluk `key--space` alır
  (flex-grow 6).
- **Yerleşime duyarlı.** Açıldığında, ekran klavyesi WM'ye bir
  "alan ayır" olayı gönderir. Odaktaki pencerenin içeriği, odaktaki girdi
  klavyenin en az 16 px üstünde kalacak şekilde kaydırılır — asla
  yalnızca üzerini örtmez.
- **Öngörülü öneriler.** Tuşların üstünde hap satırı. Dokunma öneriyi
  ekler. v1'de bunlar statiktir; mimari, ileride cihaz-üstü bir n-gram
  veya küçük bir ONNX dil modeline yer bırakır.
- **Modlar.** Varsayılan (tam genişlik), yüzen (üst kenarda sürükleme
  tutamacı), bölünmüş (tablet baş parmak yazımı için alt köşelere
  yerleşmiş iki yarım). Mod değiştirme `⎚` uzun basışında.

### 2.4 Görev değiştirici (genel bakış)

- 3 parmak yukarı kaydırma veya `Alt+Tab`, uygulamaya göre gruplanmış
  canlı pencere küçük resimlerinden oluşan tam ekran bir ızgara açar.
- Her kart, sol altta uygulama ikonu bindirilmiş, ~240 px genişliğinde
  gerçek zamanlı bir önizleme gösterir.
- Yatay kaydırma çalışma alanları arasında gezinir; komşu çalışma
  alanlarındaki kartlar kenarlardan kayarak girer.

### 2.5 Dosya yöneticisi

- **Yerleşim.** Uyarlanabilir: dar genişliklerde tek panel, 1280 px
  üzerinde çift panel (isteğe bağlı Miller sütunları).
- **Arşivler.** Bir `.zip` veya `.7z`, küçük bir kurdele rozetiyle bir
  dizin ikonu olarak render edilir. Çift tıklama içine gider; breadcrumb
  `archive.7z › docs › api.md` gösterir.
- **Dokunma alanları.** Satır yüksekliği 56 px; uzun basış (550 ms)
  kayarak açılan onay kutularıyla çoklu-seçime girer.
- **Sürükle-bırak sıkıştırma.** Bir seçimi bir `.zip`'e bırakma →
  "Arşive ekle" sayfası; boş bir panele bir `.7z` bırakma → "Buraya çıkar".
- **Önizlemeler.** Bir resmin üzerine hover (veya tap-and-hold), EXIF
  tarih bilgisiyle 320 px'lik bir önizleme kabarcığı gösterir; video,
  kaydırılabilir bir küçük resim şeridi gösterir.

---

## 3. Etkileşim desenleri

### 3.1 Yerleştirme (snap)

1. Sürükleme başlar → pencere "geçici sürükleme" durumuna girer;
   opaklık 0.95, hafif küçültme 0.98×.
2. İmleç yerleştirme eşiğini geçer → önizleme katmanı solarak belirir
   (`140 ms`, `--ease-out`).
3. İmleç eşikten çıkar → önizleme solarak kaybolur (`140 ms`).
4. Eşik içinde bırakılırsa → pencere önizlemenin geometrisine yay ile
   sıçrar (`280 ms`, `--ease-spring`). Önizlemenin kendisi geçiş
   sırasında pencerenin içinde erir.

### 3.2 Büyütme

Tamamen CSS. Bir dock karosuna hover, `translateY(-10px) scale(1.18)`
uygular; kardeş kombinatörü (`:has(+ .dock-app:hover)` ve
`.dock-app:hover + .dock-app`) komşulara daha yumuşak bir `scale(1.06)`
yayar. JS yok, rAF yok, takılma yok.

### 3.3 Jestler

| Jest                | Eylem                       |
| ---------------------- | ----------------------------- |
| 3 parmak kaydırma ←/→     | Çalışma alanını değiştir              |
| 3 parmak kaydırma ↑       | Görev genel bakışı                 |
| 4 parmak pinch         | Masaüstünü göster                 |
| Alttan kenar kaydırma | Tam ekranda dock'u ortaya çıkar     |
| Uzun basış (dokunmatik)     | Çoklu-seçim / bağlam menüsü   |
| İki parmak dokunuş         | Sağ-tık eşdeğeri   |

### 3.4 Odak ve karartma

- Etkin pencere: tam opaklık, Rust tonlu gölge parıltısı, keskin.
- Etkin olmayan pencere: `opacity: 0.92`, `saturate(85%)`, parıltı yok.
  İnce ama fark edilir.
- Dock aynı grameri izler: odaktaki-uygulama bağlamı = dock'u %72'ye
  karart.

---

## 4. Erişilebilirlik

- Tüm etkileşimli öğeler ≥ 44 × 44 mantıksal piksel (WCAG 2.5.5 hedefi).
- Araç ipuçları `role="tooltip"` taşır ve hedeflerinde `aria-describedby`
  ile referans verilir.
- Ekran klavyesi, odaktaki alanın `inputmode` ve `enterkeyhint`'ine saygı
  duyar ve eylem tuşunu yeniden render eder (`⏎`, "Git", "Ara" vb. olur).
- `prefers-reduced-motion`, her geçişi 1 ms'ye indirir.
- Renk: tüm metin için Ege arka planına karşı WCAG AA (ön plan
  `--glass-fg` = %92 beyaz, 13 px+ için AA).
- Klavye gezinmesi: her dock ve pencere kontrolü, görünür bir odak
  halkasıyla (`box-shadow: 0 0 0 3px rgba(217,108,45,0.45)`)
  `Tab`-erişilebilirdir.

---

## 5. İkonografi

- 1.5 px kalınlıkta, optik-yuvarlatılmış birleşimli, 24 × 24 ızgarada
  outline-stil monoline ikonlar.
- Uygulama ikonları 2 renkli: birincil şekil Rust veya Ege soluğunda,
  ikincil detay %60 beyazda.
- Sistem tepsisi glyph'leri (ağ, pil, ses) `currentColor` kullanır,
  böylece dock'un karartma durumu yayılır.

---

## 6. Token haritası (Tailwind ön ayarı, alıntı)

```ts
// ui/src/design/tokens.ts
export const tokens = {
  color: {
    rust:   { 300: "#F9A679", 400: "#EF8348", 500: "#D96C2D" },
    aegean: { 050: "#E6F6F8", 100: "#A8DDE6", 300: "#5CB0C4",
              500: "#1A6C8A", 700: "#07334A" },
    glass:  { fg: "rgba(255,255,255,0.92)",
              fgDim: "rgba(255,255,255,0.62)",
              bg: "rgba(255,255,255,0.10)",
              border: "rgba(255,255,255,0.22)" },
  },
  radius: { sm: 8, md: 14, lg: 22, xl: 32, pill: 9999 },
  space:  [0, 4, 8, 12, 16, 20, 24, 32, 40],
  motion: {
    spring: "cubic-bezier(0.34, 1.56, 0.64, 1)",
    out:    "cubic-bezier(0.16, 1, 0.3, 1)",
    inout:  "cubic-bezier(0.65, 0, 0.35, 1)",
  },
  duration: { fast: 140, med: 280, slow: 460 },
  blur:     { dock: 30, window: 28, osk: 34 },
} as const;
```

Bu dosya tek gerçek kaynaktır — `styles.css`'teki CSS değişkenleri ve
Tailwind yapılandırmasının ikisi de bunu tüketir. Yeni bir vurgu veya
yarıçap eklemek buradaki tek satırlık bir değişikliktir.

> **Not.** Bu belge, `tokens.ts` / Tailwind referanslarıyla birlikte
> orijinal (Tauri + React tabanlı) tasarımdan devralınmıştır; gerçek
> `bacak-compositor`'ın Slint arayüzü bu token'ların birebir aynısını
> uygulamıyor olabilir. Renk/boşluk/hareket değerleri BacakOS'un görsel
> kimliği için hâlâ geçerli referanstır, ama bir Tailwind/React
> pipeline'ı varsayan kısımlar (ör. §6) compositor'ın gerçek render
> koduna göre güncellenmeyi bekliyor.
