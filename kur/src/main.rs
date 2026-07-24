// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! `kur` — the BacakOS system installer.
//!
//! `main.rs` is the controller shell: it owns the wizard state, the step
//! navigation and the bridge between the install thread and the Slint event
//! loop. The per-page callbacks live in [`pages`]. Between them they are the
//! *only* code that knows about both the UI and [`backend`]; the backend never
//! imports the UI, and the UI never runs a command.

mod backend;
mod headless;
mod pages;
mod wizard;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use anyhow::Result;
use slint::{Model, ModelRc, SharedString, VecModel};

use backend::disk::{self, Disk};
use backend::install::{self, Progress};
use backend::locale::{self, Locale};
use backend::timezone::{self, Zone};
use wizard::{RoleChoice, Selection};

slint::include_modules!();

/// How many log lines the install pane keeps. Enough to show a failing
/// package's error, small enough that the `String` rebuild stays cheap.
const LOG_LINES: usize = 200;

/// The last step index; `InstallPage` lives here and owns its own buttons.
const STEP_INSTALL: i32 = 4;

/// Wizard state that Slint properties cannot hold — the typed originals behind
/// the flattened display strings in the UI.
pub struct AppState {
    pub locales: Vec<Locale>,
    pub keymaps: Vec<String>,
    pub zones: Vec<Zone>,
    /// Indices into `zones` currently shown, after the search filter.
    pub visible_zones: Vec<usize>,
    /// Index into `zones` (not `visible_zones`) of the user's choice.
    pub chosen_zone: usize,

    pub disks: Vec<Disk>,
    /// Per-partition role and format choices for the selected disk.
    pub selection: Selection,
    /// The selected disk's partition table, fetched once per selection so
    /// `pages::build_plan` — run on every keystroke — never shells out to
    /// `sfdisk` itself. `None` until a disk has been picked.
    pub table: Option<disk::PartitionTable>,

    log: VecDeque<String>,
    /// `Some` only while an install is running; dropping it does not cancel.
    install: Option<install::Handle>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Unattended path: install straight from environment variables, no window.
    // Used by the VM/loop-device end-to-end test. See `headless`.
    if headless::requested() {
        return headless::run();
    }

    let ui = MainWindow::new()?;
    ui.global::<Theme>().set_dark(true);

    let locales = locale::list_locales();
    let keymaps = locale::list_keymaps()?;
    let zones = timezone::list_zones().unwrap_or_else(|error| {
        log::error!("saat dilimleri okunamadı: {error:#}");
        Vec::new()
    });
    // A missing disk list is not fatal: the user may have booted with no drive
    // attached and can plug one in — but we surface it in the log.
    let disks = disk::list_disks().unwrap_or_else(|error| {
        log::error!("diskler listelenemedi: {error:#}");
        Vec::new()
    });

    let state = Rc::new(RefCell::new(AppState {
        visible_zones: (0..zones.len()).collect(),
        chosen_zone: 0,
        locales,
        keymaps,
        zones,
        disks,
        selection: Selection::default(),
        table: None,
        log: VecDeque::with_capacity(LOG_LINES),
        install: None,
    }));
    APP.with(|app| *app.borrow_mut() = Some(state.clone()));

    populate(&ui, &mut state.borrow_mut());
    wire_navigation(&ui);
    pages::wire_language(&ui);
    pages::wire_timezone(&ui);
    pages::wire_disks(&ui);
    pages::wire_account(&ui);
    wire_install(&ui);

    ui.run()?;
    Ok(())
}

/// Push the initial model data into the UI.
fn populate(ui: &MainWindow, state: &mut AppState) {
    let steps = [
        ("Dil ve Klavye", "🌐"),
        ("Saat Dilimi", "🕐"),
        ("Bölümleme", "💽"),
        ("Kullanıcı", "👤"),
        ("Kurulum", "⚙"),
    ]
    .map(|(title, glyph)| StepModel { title: title.into(), glyph: glyph.into(), done: false });
    ui.set_steps(ModelRc::new(VecModel::from(steps.to_vec())));

    ui.set_locale_labels(strings(state.locales.iter().map(|l| l.label.clone())));
    ui.set_keymaps(strings(state.keymaps.iter().cloned()));
    ui.set_partition_roles(strings(RoleChoice::ALL.iter().map(|r| r.label().to_string())));
    ui.set_create_roles(strings(
        wizard::CREATE_ROLES.iter().map(|r| RoleChoice::from_role(*r).label().to_string()),
    ));

    let default_locale = locale::default_locale_index(&state.locales);
    ui.set_locale_index(default_locale as i32);
    pages::sync_locale_derived(ui, state, default_locale);

    let rows: Vec<DiskModel> = state
        .disks
        .iter()
        .map(|d| DiskModel {
            path: d.path.as_str().into(),
            model: d.model.as_str().into(),
            size_text: d.size_text().into(),
            removable: d.removable,
            is_live_medium: d.is_live_medium,
        })
        .collect();
    ui.set_disks(ModelRc::new(VecModel::from(rows)));

    pages::refresh_zone_list(ui, state);

    // Step 0 needs no input, so the user starts able to advance.
    ui.set_can_advance(true);
}

/// Wrap an iterator of owned `String`s as a Slint string model.
pub fn strings(items: impl Iterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.map(SharedString::from).collect::<Vec<_>>()))
}

// ---- Navigation -------------------------------------------------------------

/// Whether the current step's input is complete enough to move on.
fn can_advance(ui: &MainWindow, state: &AppState) -> bool {
    match ui.get_current_step() {
        0 => true,
        1 => !state.zones.is_empty(),
        2 => pages::build_plan(state).is_ok(),
        3 => pages::account_from_ui(ui).validate(&ui.get_password_confirm()).is_ok(),
        _ => false,
    }
}

pub fn refresh_can_advance(ui: &MainWindow) {
    let allowed = with_state(|state| can_advance(ui, state)).unwrap_or(false);
    ui.set_can_advance(allowed);
}

fn wire_navigation(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_next_clicked(move || {
        let ui = weak.unwrap();
        if !ui.get_can_advance() {
            return;
        }

        let current = ui.get_current_step();
        mark_step_done(&ui, current);

        let next = (current + 1).min(STEP_INSTALL);
        ui.set_current_step(next);
        ui.set_max_reachable(ui.get_max_reachable().max(next));

        if next == STEP_INSTALL {
            enter_install_step(&ui);
        }
        refresh_can_advance(&ui);
    });

    let weak = ui.as_weak();
    ui.on_back_clicked(move || {
        let ui = weak.unwrap();
        ui.set_current_step((ui.get_current_step() - 1).max(0));
        refresh_can_advance(&ui);
    });

    let weak = ui.as_weak();
    ui.on_step_selected(move |index| {
        let ui = weak.unwrap();
        // The sidebar already refuses clicks beyond `max-reachable`; re-check
        // here because callbacks are a public surface.
        if index <= ui.get_max_reachable() && !ui.get_install_running() {
            ui.set_current_step(index);
            if index == STEP_INSTALL {
                enter_install_step(&ui);
            }
            refresh_can_advance(&ui);
        }
    });
}

/// Tick the sidebar checkmark for a completed step.
fn mark_step_done(ui: &MainWindow, index: i32) {
    let steps = ui.get_steps();
    if let Some(mut step) = steps.row_data(index as usize) {
        step.done = true;
        steps.set_row_data(index as usize, step);
    }
}

// ---- Install ----------------------------------------------------------------

/// Entering the last step: render the summary.
fn enter_install_step(ui: &MainWindow) {
    with_state(|state| ui.set_install_summary(build_summary(ui, state).into()));
    refresh_can_start(ui);
}

fn build_summary(ui: &MainWindow, state: &AppState) -> String {
    let plan = match pages::build_plan(state) {
        Ok(plan) => plan,
        Err(error) => return error,
    };
    let locale = state.locales.get(ui.get_locale_index() as usize).map_or("-", |l| l.label.as_str());
    let keymap = state.keymaps.get(ui.get_keymap_index() as usize).map_or("-", String::as_str);
    let zone = state.zones.get(state.chosen_zone).map_or("-", |z| z.name.as_str());

    format!(
        "{}\nDil: {locale}\nKlavye: {keymap}\nSaat dilimi: {zone}\n\
         Bilgisayar adı: {}\nKullanıcı: {}\nÖnyükleme: {}\n\n\
         ⚠ Yukarıda biçimlendirilecek olarak işaretlenen bölümlerdeki TÜM veriler silinecek.",
        plan.describe(),
        ui.get_hostname(),
        ui.get_username(),
        if disk::is_uefi_boot() { "UEFI" } else { "BIOS (eski)" },
    )
}

/// The start button demands a valid plan and a valid account. Installing from
/// the ISO's own squashfs needs no network, so there is nothing else to gate on.
fn refresh_can_start(ui: &MainWindow) {
    let ready = with_state(|state| {
        pages::build_plan(state).is_ok()
            && pages::account_from_ui(ui).validate(&ui.get_password_confirm()).is_ok()
    })
    .unwrap_or(false);
    ui.set_can_start(ready && !ui.get_install_running());
}

fn wire_install(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_install_start(move || {
        let ui = weak.unwrap();
        if ui.get_install_running() {
            return;
        }

        let Some(config) = build_config(&ui) else {
            return;
        };

        ui.set_install_running(true);
        ui.set_install_failed(false);
        ui.set_can_start(false);
        ui.set_install_progress(0.0);

        // `slint::Weak` is `Send`, but `MainWindow` is not. The only legal way
        // to touch the window from another thread is `upgrade_in_event_loop`,
        // which queues the closure onto the UI thread. Everything below runs
        // *there*, so it may freely borrow `Rc`/`RefCell` state.
        let ui_weak = ui.as_weak();
        let handle = install::spawn(config, move |progress| {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| apply_progress(&ui, progress));
        });

        with_state(|state| state.install = Some(handle));
    });

    let weak = ui.as_weak();
    ui.on_install_cancel(move || {
        let ui = weak.unwrap();
        with_state(|state| {
            if let Some(handle) = state.install.as_ref() {
                handle.cancel();
            }
        });
        ui.set_install_stage("İptal ediliyor…".into());
    });

    ui.on_reboot(|| {
        let cancel = backend::cmd::Cancel::new();
        if let Err(error) = backend::cmd::run("systemctl", &["reboot"], &cancel) {
            log::error!("yeniden başlatılamadı: {error:#}");
        }
    });
}

/// Assemble the engine's config, or `None` when the wizard is inconsistent.
///
/// Every failure here is already prevented by `can-start`; reaching one means a
/// callback fired out of order, so we log rather than surface it.
fn build_config(ui: &MainWindow) -> Option<install::Config> {
    with_state(|state| {
        let plan = pages::build_plan(state)
            .inspect_err(|error| log::error!("plan oluşturulamadı: {error}"))
            .ok()?;

        let account = pages::account_from_ui(ui);
        account
            .validate(&ui.get_password_confirm())
            .inspect_err(|error| log::error!("hesap geçersiz: {error}"))
            .ok()?;

        Some(install::Config {
            plan,
            account,
            locale: state
                .locales
                .get(ui.get_locale_index() as usize)
                .map_or_else(|| "en_US.UTF-8".into(), |l| l.code.clone()),
            keymap: state
                .keymaps
                .get(ui.get_keymap_index() as usize)
                .cloned()
                .unwrap_or_else(|| "us".into()),
            timezone: state
                .zones
                .get(state.chosen_zone)
                .map_or_else(|| "Etc/UTC".into(), |z| z.name.clone()),
        })
    })
    .flatten()
}

/// Apply one [`Progress`] event to the window. Runs on the UI thread.
///
/// This is deliberately the *only* function that mutates install-related
/// properties, so the terminal-state invariant (nothing after `Finished`) is
/// checkable by reading one function.
fn apply_progress(ui: &MainWindow, progress: Progress) {
    match progress {
        Progress::Stage { label } => ui.set_install_stage(label.into()),

        Progress::Percent(value) => ui.set_install_progress(value),

        Progress::Log(line) => {
            // Rebuilding the whole string each line is O(LOG_LINES) but bounded,
            // and lines arrive far slower than a frame.
            let tail = with_state(|state| {
                if state.log.len() == LOG_LINES {
                    state.log.pop_front();
                }
                state.log.push_back(line);
                state.log.iter().cloned().collect::<Vec<_>>().join("\n")
            });
            if let Some(tail) = tail {
                ui.set_install_log(tail.into());
            }
        }

        Progress::Finished(outcome) => {
            ui.set_install_running(false);
            with_state(|state| state.install = None);

            match outcome {
                Ok(()) => {
                    ui.set_install_finished(true);
                    ui.set_install_progress(1.0);
                    mark_step_done(ui, STEP_INSTALL);
                }
                Err(message) => {
                    // Stay on the install page with the log visible: the failure
                    // reason is the last thing in it.
                    ui.set_install_failed(true);
                    ui.set_install_stage(format!("Kurulum başarısız: {message}").into());
                    refresh_can_start(ui);
                }
            }
        }
    }
}

thread_local! {
    /// Lets callbacks dispatched into the event loop without captured state —
    /// `apply_progress` — reach `AppState`. The non-`Send` `Rc` never crosses a
    /// thread this way.
    static APP: RefCell<Option<Rc<RefCell<AppState>>>> = const { RefCell::new(None) };
}

/// Run `f` against the thread-local `AppState`, if it has been installed.
pub fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> Option<R> {
    APP.with(|app| app.borrow().as_ref().map(|state| f(&mut state.borrow_mut())))
}
