#!/bin/sh
# BacakOsSDK şablonundan yeni bir BacakOS uygulaması oluşturur.
#
#   araclar/yeni-uygulama.sh <ad> ["Görünen ad"] [hedef dizin]
#
#   <ad>        paket, ikili ve .desktop adı: küçük harf, rakam, '-' (ör. not-defteri)
#   Görünen ad  menüde ve pencere başlığında görünen ad (varsayılan: <ad>)
#   hedef dizin varsayılan: ./<ad>
#
# Oluşan proje SDK'ya yol bağımlılığıyla bağlanır; `cargo run` ile çalışır,
# `cargo deb` ile .desktop dosyası ve simgesiyle birlikte paketlenir.
set -eu

SDK=$(cd "$(dirname "$0")/.." && pwd)
AD=${1:-}
case "$AD" in
    '' | -* | *[!a-z0-9-]*)
        echo "kullanım: $0 <ad> [\"Görünen ad\"] [hedef dizin]" >&2
        echo "  <ad>: küçük harf, rakam ve '-' (ör. not-defteri)" >&2
        exit 2
        ;;
esac
BASLIK=${2:-$AD}
case "$BASLIK" in
    *'"'* | *'\'* | *'|'*)
        echo "görünen ad \" \\ | içeremez" >&2
        exit 2
        ;;
esac
HEDEF=${3:-$PWD/$AD}
if [ -e "$HEDEF" ]; then
    echo "$HEDEF zaten var" >&2
    exit 1
fi

AD_KULLANICI=$(git config user.name 2>/dev/null || true)
EPOSTA=$(git config user.email 2>/dev/null || true)
YAZAR=${AD_KULLANICI:-${USER:-yazar}}
[ -n "$EPOSTA" ] && YAZAR="$YAZAR <$EPOSTA>"
case "$YAZAR" in *'"'* | *'|'*) YAZAR=${USER:-yazar} ;; esac

mkdir -p "$(dirname "$HEDEF")"
cp -r "$SDK/sablon" "$HEDEF"
mv "$HEDEF/Cargo.toml.sablon" "$HEDEF/Cargo.toml"
mv "$HEDEF/gitignore.sablon" "$HEDEF/.gitignore"
mv "$HEDEF/paket/__AD__.desktop" "$HEDEF/paket/$AD.desktop"
mv "$HEDEF/paket/__AD__.svg" "$HEDEF/paket/$AD.svg"

# sed ifadesi için & ve / kaçışı (ayırıcı |, yukarıda yasaklandı).
kac() { printf '%s' "$1" | sed 's/[&/\]/\\&/g'; }
find "$HEDEF" -type f | while read -r f; do
    sed -i \
        -e "s|__AD__|$(kac "$AD")|g" \
        -e "s|__BASLIK__|$(kac "$BASLIK")|g" \
        -e "s|__YAZAR__|$(kac "$YAZAR")|g" \
        -e "s|__SDK__|$(kac "$SDK")|g" \
        "$f"
done

echo "✓ $HEDEF oluşturuldu"
echo "  cd $HEDEF && cargo run          # çalıştır"
echo "  cargo install cargo-deb && cargo deb   # .deb paketi (target/debian/)"
