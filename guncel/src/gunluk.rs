//! Günlük satırları. systemd altında (JOURNAL_STREAM tanımlı) satır başına
//! `<N>` öncelik öneki konur, böylece `journalctl -p warning` süzer; elle
//! çalıştırıldığında düz yazılır.

use std::sync::OnceLock;

fn onek(oncelik: u8) -> &'static str {
    static GUNLUKTE: OnceLock<bool> = OnceLock::new();
    if !*GUNLUKTE.get_or_init(|| std::env::var_os("JOURNAL_STREAM").is_some()) {
        return "";
    }
    match oncelik {
        3 => "<3>",
        4 => "<4>",
        _ => "<6>",
    }
}

pub fn yaz(oncelik: u8, mesaj: std::fmt::Arguments) {
    eprintln!("{}{}", onek(oncelik), mesaj);
}

#[macro_export]
macro_rules! bilgi { ($($a:tt)*) => { $crate::gunluk::yaz(6, format_args!($($a)*)) } }
#[macro_export]
macro_rules! uyari { ($($a:tt)*) => { $crate::gunluk::yaz(4, format_args!($($a)*)) } }
#[macro_export]
macro_rules! hata { ($($a:tt)*) => { $crate::gunluk::yaz(3, format_args!($($a)*)) } }
