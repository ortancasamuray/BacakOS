//! `bacak-common` — shared, privilege-free building blocks for the
//! Bacak Display Manager (BDM).
//!
//! This crate is deliberately free of system-library bindings (no PAM, no
//! libsystemd, no Wayland) so that the security-critical parsing and policy
//! logic can be unit-tested on any machine and reused by every component:
//! the root daemon, the unprivileged greeter and the session launcher.
//!
//! Modules:
//! * [`config`]  — `/etc/bacak-display-manager.conf` parsing and defaults.
//! * [`users`]   — enumeration of *login-capable* local users.
//! * [`sessions`]— discovery of Wayland / X11 desktop session entries.
//! * [`ipc`]     — the line-delimited JSON protocol spoken between the
//!   privileged daemon and the unprivileged greeter.
//! * [`power`]   — power-action model (logind verbs + polkit action ids).

pub mod config;
pub mod error;
pub mod i18n;
pub mod ipc;
pub mod power;
pub mod sessions;
pub mod users;

pub use error::{Error, Result};
