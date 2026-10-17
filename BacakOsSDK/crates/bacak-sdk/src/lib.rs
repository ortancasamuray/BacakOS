//! BacakOS application SDK.
//!
//! What every BacakOS application needs and used to re-implement:
//!
//! * [`gorunum`] — the desktop-wide dark mode: read it, follow changes, set it.
//! * [`renk`] — the BacakOS colour palette for both modes.
//! * [`masaustu`] — `.desktop` entries: list/find applications, build their
//!   command line, launch them (terminal apps inside a terminal).
//! * [`simge`] — icon-theme lookup (configured theme → BacakOS → common full
//!   themes → hicolor → pixmaps).
//! * [`mime`] — a file's MIME type, its default application, "Open With".
//! * [`ipc`] — the compositor client: notifications, Control Center, window
//!   and output lists, events.
//!
//! Nothing here needs a running compositor except [`ipc`]; every other part
//! works from the files on disk, so applications start fine anywhere.
//! Slint applications also get theme-aware widgets from `bacak-sdk-slint`.

pub mod gorunum;
pub mod ipc;
pub mod masaustu;
pub mod mime;
pub mod renk;
pub mod simge;
mod xdg;

/// The raw protocol types, for applications that talk to the socket directly.
pub use bacak_ipc as protokol;
pub use bacak_ipc::{Ekran, Gorunum, Olay, Pencere};
pub use ipc::{bildir, Istemci};
pub use masaustu::Uygulama;
pub use renk::{Palet, Renk};
