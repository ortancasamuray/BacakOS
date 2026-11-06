# El yazısı sembol modeli

Tahta'nın "Tahtadan oku" özelliğinin (`src/handwriting.rs`) kullandığı
küçük sembol sınıflandırıcısını (`assets/model/semboller.bin`) üreten
betikler. Model tahtada yalnız istendiğinde, çevrimdışı çalışır.

## Sınıflar

`0–9`, `x y a b c s i n o t l g e`, `+ ( )`, `√`, `π` (28 sınıf).
`−`, `=` ve `.` biçimlerinden kurallarla tanınır, modelde yoktur.

## Model

Evrişim 1→16 (3×3) + ReLU + 2×2 havuz → evrişim 16→32 (3×3) + ReLU +
2×2 havuz → tam bağlı 1568→128 + ReLU → 128→28. Yaklaşık 0,8 MB (f32).
Girdi: mürekkep kutusuna kırpılmış, uzun kenarı 20 piksele sığdırılmış,
28×28 içinde ortalanmış görüntü (Rust tarafı aynı kuralla çizer).

## Ön işleme

Her örnek önce orta çizgisine indirilir (görüntüler Zhang–Suen ile tek
piksele inceltilir, sentetik çizgiler zaten çizgidir) ve tahtanın
kullandığı kalemle (2,4 px) yeniden çizilir. Böylece model MNIST/EMNIST
arasındaki doku farkını değil yalnız şekli öğrenir; eğitim görüntüsü
`handwriting.rs::render`'ın çizdiğiyle aynıdır. Veri çoğaltma (döndürme,
eğme, en-boy, titreme, kalem kalınlığı) noktalara uygulanır.

## Sonuç (15 epok)

Test: %86 doğru, ilk 3 aday içinde %98,9. Kalan hataların çoğu görüntüden
ayırt edilemeyen çiftler (`l`/`1`, `o`/`0`, `g`/`9`, `s`/`5`); bunları
`handwriting.rs` bağlamla (formül ayrıştırıcı + yazım önbilgisi) çözer.

## Yeniden üretme

```sh
# veri: MNIST, EMNIST (gzip.zip içinden byclass), HASYv2 → <veri>/{mnist,emnist/gzip,hasy}
python3 hazirla.py <veri> semboller.npz
python3 egit.py semboller.npz semboller.bin 15   # numpy, ~30 dk (32 çekirdek)
cp semboller.bin semboller.bin.probe ../../assets/model/
```

`semboller.bin.probe`, Rust çıkarımının eğitimle aynı sonucu verdiğini
denetleyen test girdisidir (`cargo test`).

## Veri kaynakları ve lisanslar

- **MNIST** — Yann LeCun, Corinna Cortes, Christopher J.C. Burges;
  CC BY-SA 3.0.
- **EMNIST** — Cohen, Afshar, Tapson, van Schaik (2017), NIST Special
  Database 19'dan türetilmiştir (ABD hükümeti verisi).
- **HASYv2** — Martin Thoma (2017), "The HASYv2 dataset",
  arXiv:1701.08380; Open Database License (ODbL).
- `(`, `)`, ek `+`, kancalı `1` ve çizgili `7` (Türk el yazısı) örnekleri
  betikte sentetik üretilir.

Model ağırlıkları bu verilerden üretilmiş türev bir çalışmadır; yukarıdaki
atıflar korunarak dağıtılır.
