// SPDX-License-Identifier: GPL-3.0-or-later
//! Bacak Onay core: everything the PAM module and the `bacakonay` CLI share.
//!
//! * [`base32`] — RFC 4648 base32 (the `secret=` encoding in `otpauth://`).
//! * [`otp`] — RFC 4226 HOTP / RFC 6238 TOTP with SHA-1/256/512.
//! * [`uri`] — build the `otpauth://` URI the phone app scans.
//! * [`store`] — the per-user enrollment file under `/var/lib/bacakonay`,
//!   root-owned, with replay protection (`last_step`).
//!
//! Deliberately small and dependency-light: this code is linked into the
//! login daemon through `pam_bacakonay.so`.

pub mod base32;
pub mod otp;
pub mod store;
pub mod uri;

pub use otp::{Algorithm, OtpParams};
