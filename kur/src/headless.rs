// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Unattended installation, driven entirely by environment variables.
//!
//! This is the preseed-style counterpart to the wizard: same engine, no window.
//! It exists so the installer can be tested end to end against a loop device in
//! CI or a VM, where there is no display to click. `main` hands control here
//! whenever `KUR_HEADLESS=1` is set, before a Slint window is ever created.
//!
//! It is deliberately not documented as a user feature — the variable names are
//! a testing contract, not a stable UI.

use std::sync::mpsc;

use anyhow::{bail, Context, Result};

use crate::backend::disk::{self, Disk};
use crate::backend::install::{self, Config, Progress};
use crate::backend::plan::Plan;
use crate::backend::user::Account;

/// True when the process was asked to install without a UI.
pub fn requested() -> bool {
    std::env::var_os("KUR_HEADLESS").is_some_and(|v| v == "1")
}

/// Read a required variable, or fail with a message naming it.
fn required(key: &str) -> Result<String> {
    std::env::var(key).map_err(|_| anyhow::anyhow!("{key} ortam değişkeni gerekli"))
}

/// Build a [`Config`] from the environment.
///
/// The target disk is looked up in the real `lsblk` output rather than trusted
/// blindly: that reuses the exact live-medium and size guards the wizard
/// enforces, so an unattended run cannot wipe the running system either.
fn config_from_env() -> Result<Config> {
    let target = required("KUR_TARGET_DISK")?;

    let disks = disk::list_disks().context("diskler listelenemedi")?;
    let disk: &Disk = disks
        .iter()
        .find(|d| d.path == target)
        .with_context(|| format!("{target} bulunamadı (lsblk onu listelemiyor)"))?;

    if disk.is_live_medium {
        bail!("{target} kurulum ortamı — hedef olarak reddedildi");
    }

    // Firmware is forced by KUR_UEFI when set, else auto-detected. A loop device
    // has no firmware of its own, so tests pin it explicitly.
    let uefi = match std::env::var("KUR_UEFI").ok().as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => disk::is_uefi_boot(),
    };

    let plan = Plan::automatic(disk, disk::total_ram_bytes(), uefi)?;

    let account = Account {
        hostname: std::env::var("KUR_HOSTNAME").unwrap_or_else(|_| "bacakos".into()),
        username: required("KUR_USERNAME")?,
        password: required("KUR_PASSWORD")?,
        root_same_password: std::env::var("KUR_ROOT_SAME").ok().as_deref() != Some("0"),
    };
    account.validate(&account.password).context("hesap bilgileri geçersiz")?;

    Ok(Config {
        plan,
        account,
        locale: std::env::var("KUR_LOCALE").unwrap_or_else(|_| "tr_TR.UTF-8".into()),
        keymap: std::env::var("KUR_KEYMAP").unwrap_or_else(|_| "tr".into()),
        timezone: std::env::var("KUR_TIMEZONE").unwrap_or_else(|_| "Europe/Istanbul".into()),
    })
}

/// Run the install to completion, printing progress to stdout.
///
/// Returns `Ok(())` only when every stage succeeded; the caller turns that into
/// the process exit code so a VM test can assert on it.
pub fn run() -> Result<()> {
    let config = config_from_env()?;
    log::info!("başlıksız kurulum: hedef {}", config.plan.disk);
    println!("{}", config.plan.describe());

    // Reuse the real engine. Its worker thread reports back over a channel that
    // we drain here on the main thread until the terminal `Finished` arrives.
    let (tx, rx) = mpsc::channel();
    let handle = install::spawn(config, move |progress| {
        let _ = tx.send(progress);
    });
    // Keep the handle alive for the whole run; dropping it does not cancel, but
    // holding it documents that cancellation would be possible here.
    let _ = &handle;

    for progress in rx {
        match progress {
            Progress::Stage { label } => println!("\n== {label}"),
            Progress::Percent(value) => print!("\r{:>3}%", (value * 100.0) as u32),
            Progress::Log(line) => println!("  {line}"),
            Progress::Finished(Ok(())) => {
                println!("\nKurulum tamamlandı.");
                return Ok(());
            }
            Progress::Finished(Err(message)) => bail!("kurulum başarısız: {message}"),
        }
    }
    // The channel closed without a `Finished`: the worker panicked.
    bail!("kurulum iş parçacığı beklenmedik şekilde sonlandı")
}
