//! BacakOS compositor IPC protocol and the desktop-wide settings files that
//! the compositor and applications share.
//!
//! This crate is deliberately tiny (only `serde` + `serde_json`) because the
//! compositor depends on it too. Applications normally use the higher-level
//! client in `bacak-sdk` instead of these types directly.
//!
//! # Wire format
//!
//! A Unix stream socket at [`socket_path`], one JSON object per line (UTF-8,
//! `\n`-terminated, at most [`MAX_LINE`] bytes):
//!
//! ```text
//! → {"id":1,"istek":"bildirim","baslik":"Kaydedildi","ayrinti":"rapor.pdf"}
//! ← {"id":1,"tamam":true}
//! → {"id":2,"istek":"pencereler"}
//! ← {"id":2,"tamam":true,"veri":[{"id":7,"uygulama":"altay",...}]}
//! → {"id":3,"istek":"abone"}
//! ← {"id":3,"tamam":true}
//! ← {"olay":"gorunum","koyu":false}          (pushed later, no id)
//! ```
//!
//! Requests carry a client-chosen `id` echoed in the reply; events never carry
//! one. Unknown fields are ignored so either side can grow without breaking the
//! other; bump [`PROTOCOL_VERSION`] only for incompatible changes.

pub mod gorunum;
mod paths;
pub mod proto;

pub use gorunum::Gorunum;
pub use paths::{config_dir, socket_path, socket_path_for};
pub use proto::*;

/// Incompatible protocol revisions. Returned by [`Istek::Surum`].
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest accepted request/response line, in bytes. Longer lines close the
/// connection — nothing in the protocol needs more.
pub const MAX_LINE: usize = 64 * 1024;

/// Environment variable the compositor exports to its children with the socket
/// path, overriding the default location.
pub const SOCKET_ENV: &str = "BACAK_IPC";
