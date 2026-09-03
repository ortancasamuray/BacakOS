// This crate is a packaging-only crate.
// It ships BacakOS's default desktop settings (dock açık + sabitlenmiş
// uygulamalar, duvar kağıdı, ikon teması) to /etc/skel/.config/bacak/ and
// /usr/share/backgrounds/bacakos/ — no compiled code. New user accounts
// (adduser'ın skel kopyalaması, canlı ISO kullanıcısı dahil) bu varsayılan
// compositor.json ile açılır; bacak-compositor::config::CompositorConfig
// kullanıcı dosyasını bulamazsa sert kodlanmış (dock kapalı) varsayılana
// düşer.
