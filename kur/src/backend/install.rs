// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! The installation engine: it owns the stage table, the threading and the
//! progress arithmetic. The stages themselves live in [`super::stages`].
//!
//! # Threading model
//!
//! [`spawn`] moves the whole install onto a plain `std::thread` and reports back
//! through a `Fn(Progress) + Send` callback. There is no `tokio` here on
//! purpose: the work is a strictly sequential chain of *subprocesses*, not
//! concurrent I/O. An async runtime would buy nothing and would force every
//! stage to be a future for no reason.
//!
//! The callback runs on the worker thread. `main.rs` wraps it in
//! `slint::Weak::upgrade_in_event_loop`, which is the documented, `Send`-safe
//! way to touch the UI from another thread.
//!
//! # Progress model
//!
//! Each stage carries a *weight* — its share of the total. Overall progress is
//! `completed_weight + current_weight * stage_fraction`. Only the package
//! stages can report a meaningful intra-stage fraction; the rest jump 0 → 1.
//! The weights come from wall-clock measurements on a mid-range SATA SSD, so
//! the bar advances at a roughly constant rate rather than sitting at 40% for
//! four minutes.

use anyhow::{Context, Result};

use super::cmd::Cancel;
use super::plan::Plan;
use super::stages;
use super::user::Account;

/// Where the target filesystem is mounted while we populate it.
pub const TARGET: &str = "/mnt/kur-target";

/// Messages sent from the worker thread to the UI.
#[derive(Debug, Clone)]
pub enum Progress {
    /// A new stage began. `label` is user-facing Turkish text.
    Stage { label: String },
    /// Overall completion, 0.0..=1.0.
    Percent(f32),
    /// A line of subprocess output, for the log pane.
    Log(String),
    /// Terminal message. Exactly one is sent, and nothing follows it.
    Finished(Result<(), String>),
}

/// Everything the engine needs. Assembled by the UI once the user confirms.
#[derive(Debug, Clone)]
pub struct Config {
    pub plan: Plan,
    pub account: Account,
    pub locale: String,
    pub keymap: String,
    /// tzdata zone name, e.g. `Europe/Istanbul`.
    pub timezone: String,
}

/// Reports intra-stage progress (`0.0..=1.0`) and, optionally, a log line.
/// Passed as `&mut dyn` so every stage is a plain `fn` and [`STAGES`] can be a
/// `const` table.
pub type Reporter<'a> = dyn FnMut(f32, &str) + 'a;

/// A named unit of work and its share of the progress bar.
struct Stage {
    label: &'static str,
    weight: f32,
    run: fn(&Config, &Cancel, &mut Reporter) -> Result<()>,
}

/// Weights sum to 1.0 — asserted by a unit test. Keep them in sync when adding
/// a stage.
const STAGES: &[Stage] = &[
    Stage { label: "Disk bölümleniyor…",              weight: 0.03, run: stages::partition },
    Stage { label: "Dosya sistemleri oluşturuluyor…", weight: 0.04, run: stages::format },
    Stage { label: "Hedef bağlanıyor…",               weight: 0.01, run: stages::mount },
    Stage { label: "Taban sistem kuruluyor…",         weight: 0.62, run: stages::extract_squashfs },
    Stage { label: "Sistem yapılandırılıyor…",        weight: 0.10, run: stages::configure },
    Stage { label: "Kullanıcı oluşturuluyor…",        weight: 0.02, run: stages::account },
    Stage { label: "Önyükleyici kuruluyor…",          weight: 0.15, run: stages::bootloader },
    Stage { label: "Temizleniyor…",                   weight: 0.03, run: stages::cleanup },
];

/// A handle the UI keeps so the "İptal" button has something to talk to.
pub struct Handle {
    cancel: Cancel,
}

impl Handle {
    /// Request cancellation. The worker stops after the current subprocess is
    /// killed, then emits `Progress::Finished(Err(..))`.
    ///
    /// Note the target disk is left partitioned-but-empty; the UI must tell the
    /// user their disk was already modified.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Start the install on a background thread.
///
/// `report` is invoked from that thread for every [`Progress`] event. It must
/// be `Send + 'static`; see the module docs for how `main.rs` satisfies that
/// while still updating a Slint window.
pub fn spawn<F>(config: Config, report: F) -> Handle
where
    F: Fn(Progress) + Send + 'static,
{
    let cancel = Cancel::new();
    let handle = Handle { cancel: cancel.clone() };

    std::thread::Builder::new()
        .name("kur-install".into())
        .spawn(move || {
            let result = run_all(&config, &cancel, &report);

            // Normalise to a String: `anyhow::Error` is not `Clone`, and the UI
            // only ever displays the message.
            let outcome = match result {
                Ok(()) => {
                    report(Progress::Percent(1.0));
                    Ok(())
                }
                Err(error) => {
                    log::error!("kurulum başarısız: {error:#}");
                    Err(format!("{error:#}"))
                }
            };
            report(Progress::Finished(outcome));
        })
        .expect("kurulum iş parçacığı oluşturulamadı");

    handle
}

/// Drive every stage in order, translating per-stage fractions into overall
/// progress. Stops at the first error, unmounting on the way out.
fn run_all<F>(config: &Config, cancel: &Cancel, report: &F) -> Result<()>
where
    F: Fn(Progress) + Send + 'static,
{
    let mut completed = 0.0_f32;

    for stage in STAGES {
        report(Progress::Stage { label: stage.label.to_string() });
        report(Progress::Percent(completed));

        // Handed to each stage so it can report intra-stage progress and logs.
        let mut on_progress = |fraction: f32, line: &str| {
            if !line.is_empty() {
                report(Progress::Log(line.to_string()));
            }
            report(Progress::Percent(completed + stage.weight * fraction.clamp(0.0, 1.0)));
        };

        let result = (stage.run)(config, cancel, &mut on_progress);

        if let Err(error) = result {
            // Always unmount, even on cancellation: a bind-mounted /dev in a
            // half-built chroot will block the user's next attempt.
            stages::unmount_all();
            return Err(error).with_context(|| format!("aşama başarısız: {}", stage.label));
        }

        completed += stage.weight;
        report(Progress::Percent(completed));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::disk::{Disk, GIB};
    use std::sync::mpsc;

    /// Turn on the dry-run guard for this whole test binary.
    ///
    /// `KUR_DRY_RUN` is process-global and cargo runs tests in threads, so this
    /// leaks into every other test. That is harmless: nothing else in the suite
    /// branches on it, and no test may execute a real command anyway.
    ///
    /// The logger is initialised too, so `cargo test -- --nocapture` with
    /// `RUST_LOG=info` prints the exact argv of every command the installer
    /// *would* have run. That transcript is the point of the dry-run mode.
    fn enable_dry_run() {
        std::env::set_var("KUR_DRY_RUN", "1");
        let _ = env_logger::builder().is_test(false).try_init();
        assert!(super::super::cmd::is_dry_run());
    }

    fn test_config(uefi: bool) -> Config {
        let disk = Disk {
            path: "/dev/vda".into(),
            model: "QEMU HARDDISK".into(),
            size_bytes: 120 * GIB,
            removable: false,
            is_live_medium: false,
            partitions: Vec::new(),
        };
        Config {
            plan: Plan::automatic(&disk, 4 * GIB, uefi).expect("plan"),
            account: Account {
                hostname: "bacakos".into(),
                username: "kullanici".into(),
                password: "cokGizliParola1!".into(),
                root_same_password: true,
            },
            locale: "tr_TR.UTF-8".into(),
            keymap: "tr".into(),
            timezone: "Europe/Istanbul".into(),
        }
    }

    /// Drive the whole stage table end to end and return every event emitted.
    fn run_dry(uefi: bool) -> (anyhow::Result<()>, Vec<Progress>) {
        enable_dry_run();
        let (tx, rx) = mpsc::channel();
        let report = move |progress: Progress| tx.send(progress).expect("receiver alive");

        let result = run_all(&test_config(uefi), &Cancel::new(), &report);

        // `report` owns the only `Sender`. Drop it, or the iterator below waits
        // forever for a message that can no longer be sent.
        drop(report);
        (result, rx.into_iter().collect())
    }

    /// The test the `KUR_DRY_RUN` flag exists for: every stage runs, in order,
    /// without touching a disk. Exercised for both firmware paths, since they
    /// take different partition and bootloader branches.
    #[test]
    fn dry_run_completes_every_stage() {
        for uefi in [true, false] {
            let (result, events) = run_dry(uefi);
            assert!(result.is_ok(), "kuru çalıştırma başarısız (uefi={uefi}): {:?}", result.err());
            assert!(events.iter().any(|e| matches!(e, Progress::Stage { .. })));
        }

        let (result, events) = run_dry(true);
        assert!(result.is_ok(), "kuru çalıştırma başarısız: {:?}", result.err());

        let stages: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Progress::Stage { label } => Some(label.clone()),
                _ => None,
            })
            .collect();

        let expected: Vec<&str> = STAGES.iter().map(|s| s.label).collect();
        assert_eq!(stages, expected, "aşamalar sırayla çalışmalı");
    }

    #[test]
    fn dry_run_progress_never_goes_backwards_and_reaches_one() {
        let (_, events) = run_dry(true);

        let percents: Vec<f32> = events
            .iter()
            .filter_map(|e| match e {
                Progress::Percent(value) => Some(*value),
                _ => None,
            })
            .collect();

        assert!(!percents.is_empty());
        for pair in percents.windows(2) {
            assert!(pair[1] >= pair[0] - 1e-4, "ilerleme geri gitti: {} → {}", pair[0], pair[1]);
        }
        // Float weights sum to 1.0 only approximately; `spawn` forces the exact
        // 1.0 the bar shows, so accept anything that rounds to 100%.
        let last = *percents.last().unwrap();
        assert!(last >= 0.999, "son ilerleme {last}, 1.0 olmalı");
        assert!(percents.iter().all(|p| (0.0..=1.0001).contains(p)));
    }

    /// A cancelled install must stop at the first stage rather than partition.
    #[test]
    fn cancelling_before_the_first_stage_aborts() {
        enable_dry_run();
        let cancel = Cancel::new();
        cancel.cancel();

        let (tx, _rx) = mpsc::channel();
        let report = move |progress: Progress| {
            let _ = tx.send(progress);
        };

        let error = run_all(&test_config(true), &cancel, &report).expect_err("iptal edilmeliydi");
        assert!(format!("{error:#}").contains("iptal"), "beklenmeyen hata: {error:#}");
    }

    #[test]
    fn stage_weights_sum_to_one() {
        let total: f32 = STAGES.iter().map(|s| s.weight).sum();
        assert!((total - 1.0).abs() < 1e-5, "ağırlıklar toplamı {total}, 1.0 olmalı");
    }

    #[test]
    fn every_stage_has_a_label() {
        assert!(STAGES.iter().all(|s| !s.label.is_empty()));
    }

    #[test]
    fn cleanup_runs_last() {
        // `unmount_all` in any earlier position would break the chroot stages.
        assert_eq!(STAGES.last().unwrap().label, "Temizleniyor…");
    }
}
