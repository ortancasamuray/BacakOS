# YZsunucu

Okulun kendi sunucusunda çalışan yapay zekâ hizmeti. Tahtanın **Sor**
kartı, öğretmenin çerçevelediği bölümü PNG olarak buraya gönderir. Sunucu
görüntüyü llama.cpp ile çalışan açık bir görüntü-dil modeline sorar
(varsayılan **Qwen3-VL 8B Instruct**) ve adım adım yanıtı düz metin olarak
geri yollar.

- **Gizlilik (KVKK):** görüntü okul ağından çıkmaz, diske ya da günlüğe
  yazılmaz. Günlükte yalnız kip, boyut ve süre bulunur.
- **Kipler:** `coz` (adım adım çöz), `acikla` (konuyu açıkla, çözme),
  `ipucu` (çözümü vermeden tek ipucu).

## Kurulum

```sh
sudo apt install ./yzsunucu_0.1.0-1_amd64.deb
sudo yzsunucu-model-indir        # llama.cpp (Vulkan) + model, ~6 GB
sudo systemctl enable --now yzsunucu
curl http://localhost:8790/durum # "hazır" (yükleme ~30 sn sürer)
```

Her tahtada sunucu adresi `/etc/tahta/yzsunucu` dosyasına yazılır:

```
http://192.168.1.10:8790
```

Dosya yoksa tahta `http://yzsunucu.local:8790` adresini dener.
`TAHTA_YZSUNUCU` ortam değişkeni ikisinin de önüne geçer.

## Donanım

8–12 GB belleği olan bir GPU önerilir (NVIDIA, AMD ya da Intel; Vulkan
kullanılır). RTX 4080 Laptop'ta bir yanıt 1–3 saniye sürer. GPU yoksa
`gpu_katman = 0` ile işlemcide de çalışır, ancak yanıt dakikalar sürebilir.

## Ayarlar

`/etc/yzsunucu/yzsunucu.conf`: dinleme adresi, model yolları, GPU katman
sayısı, bağlam ve yanıt uzunluğu. Başka bir GGUF görüntü-dil modeli
(`model` + `mmproj`) yazılabilir.

## Protokol

```
POST /sor?kip=coz|acikla|ipucu
Content-Type: image/png
<PNG gövdesi, en çok 8 MB>

200 text/plain; charset=utf-8   yanıt metni
503                              model yükleniyor ya da sunucu meşgul
GET /durum                       "hazır" ya da 503
```

Bağlantı okul ağında düz HTTP'dir. Sunucuyu yalnız okul ağına açın.

## Sınırlar

8B model basit denklem ve fonksiyonlarda iyi sonuç verir. Uzun ya da
karmaşık sorularda yanılabilir; yanıtı öğretmen denetlemelidir.

## Lisanslar

- YZsunucu: GPL-3.0-or-later
- llama.cpp: MIT (ggml-org)
- Qwen3-VL-8B-Instruct: Apache 2.0 (Alibaba Qwen)
