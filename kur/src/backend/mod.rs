// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Installer backend — everything that touches the system.
//!
//! No module here knows the UI exists. They deal in plain data types and report
//! progress through callbacks, which keeps them unit-testable and means the
//! same logic could drive a headless/preseed installer later.

pub mod cmd;
pub mod disk;
pub mod install;
pub mod locale;
pub mod medium;
pub mod plan;
pub mod stages;
pub mod timezone;
pub mod user;
