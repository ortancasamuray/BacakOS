// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Per-page controllers: the Slint callbacks for steps 0–3.
//!
//! `main.rs` owns the wizard shell (navigation, the install bridge, the shared
//! state); this module owns what happens *inside* a page. Each `wire_*` function
//! is called once at startup and installs that page's callbacks.

use crate::backend::disk::{self, Disk};
use crate::backend::locale;
use crate::backend::plan::Plan;
use crate::backend::timezone;
use crate::backend::user::Account;
use crate::wizard::RoleChoice;
use crate::{refresh_can_advance, strings, with_state, AppState, MainWindow, PartitionModel};

use slint::{ComponentHandle, ModelRc, VecModel};

// ---- Language ---------------------------------------------------------------

pub fn wire_language(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_locale_changed(move || {
        let ui = weak.unwrap();
        let index = ui.get_locale_index() as usize;
        with_state(|state| sync_locale_derived(&ui, state, index));
    });
}

/// Pick the keyboard layout and time zone implied by the chosen locale.
///
/// Only hints: both screens let the user override them, and we re-derive on
/// every locale change because a user who switches language before touching
/// either screen expects the defaults to follow.
pub fn sync_locale_derived(ui: &MainWindow, state: &mut AppState, locale_index: usize) {
    let Some(chosen) = state.locales.get(locale_index) else {
        return;
    };
    let code = chosen.code.clone();

    if let Some(keymap_index) = locale::keymap_for_locale(&code, &state.keymaps) {
        ui.set_keymap_index(keymap_index as i32);
    }
    if let Some(zone_index) = timezone::default_for_locale(&code, &state.zones) {
        state.chosen_zone = zone_index;
        refresh_zone_list(ui, state);
    }
}

// ---- Time zone --------------------------------------------------------------

pub fn wire_timezone(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_zone_filter_changed(move |query| {
        let ui = weak.unwrap();
        with_state(|state| {
            let needle = query.to_lowercase();
            state.visible_zones = state
                .zones
                .iter()
                .enumerate()
                .filter(|(_, zone)| zone.name.to_lowercase().contains(&needle))
                .map(|(i, _)| i)
                .collect();
            refresh_zone_list(&ui, state);
        });
    });

    let weak = ui.as_weak();
    ui.on_zone_selected(move |visible_index| {
        let ui = weak.unwrap();
        with_state(|state| {
            // The list is filtered, so map the row back to the real zone.
            if let Some(&zone_index) = state.visible_zones.get(visible_index as usize) {
                state.chosen_zone = zone_index;
                refresh_zone_list(&ui, state);
            }
        });
        refresh_can_advance(&ui);
    });
}

/// Rebuild the visible zone list and keep the highlighted row pointing at the
/// user's choice — which may have been filtered out of view entirely.
pub fn refresh_zone_list(ui: &MainWindow, state: &AppState) {
    let names =
        state.visible_zones.iter().filter_map(|&i| state.zones.get(i)).map(|z| z.name.clone());
    ui.set_zones(strings(names));

    let highlighted = state.visible_zones.iter().position(|&i| i == state.chosen_zone);
    ui.set_zone_index(highlighted.map_or(-1, |i| i as i32));
    ui.set_selected_zone(state.zones.get(state.chosen_zone).map_or("-", |z| z.name.as_str()).into());
}

// ---- Disks & partitioning ---------------------------------------------------

pub fn wire_disks(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_disk_selected(move |index| {
        let ui = weak.unwrap();
        with_state(|state| {
            let Some(disk) = state.disks.get(index as usize) else { return };
            // Guard the callback, not just the TouchArea: the live medium must
            // never become an install target.
            if disk.is_live_medium {
                log::warn!("{} kurulum ortamı, hedef olamaz", disk.path);
                return;
            }
            state.selection.select_disk(index as usize, disk);
        });
        refresh_disk_view(&ui);
    });

    let weak = ui.as_weak();
    ui.on_manual_toggled(move |manual| {
        let ui = weak.unwrap();
        with_state(|state| state.selection.manual = manual);
        refresh_disk_view(&ui);
    });

    let weak = ui.as_weak();
    ui.on_partition_role_changed(move |index, role| {
        let ui = weak.unwrap();
        with_state(|state| state.selection.set_role(index as usize, RoleChoice::from_index(role)));
        refresh_disk_view(&ui);
    });

    let weak = ui.as_weak();
    ui.on_partition_format_toggled(move |index, format| {
        let ui = weak.unwrap();
        with_state(|state| state.selection.set_format(index as usize, format));
        refresh_disk_view(&ui);
    });
}

/// Re-render the disk page from `Selection`, including the live plan error.
pub fn refresh_disk_view(ui: &MainWindow) {
    with_state(|state| {
        ui.set_disk_index(state.selection.disk.map_or(-1, |i| i as i32));
        ui.set_manual_mode(state.selection.manual);

        let rows: Vec<PartitionModel> = state
            .selection
            .rows(&state.disks)
            .map(|row| PartitionModel {
                path: row.partition.path.as_str().into(),
                description: row.partition.describe().into(),
                role_index: row.role.index(),
                format: row.format,
                // Root always gets a fresh filesystem; the box would be a lie.
                format_enabled: row.role != RoleChoice::Unused && row.role != RoleChoice::Root,
            })
            .collect();
        ui.set_partitions(ModelRc::new(VecModel::from(rows)));

        // Show why the plan is not yet valid, but stay quiet before the user has
        // done anything on this page.
        let message = match build_plan(state) {
            Ok(_) => String::new(),
            Err(_) if state.selection.disk.is_none() => String::new(),
            Err(error) => error,
        };
        ui.set_plan_error(message.into());
    });
    refresh_can_advance(ui);
}

/// Build the plan the install would execute, or the reason it cannot be built.
///
/// Called on every UI change, so it must stay cheap and must not touch the disk.
pub fn build_plan(state: &AppState) -> Result<Plan, String> {
    let index = state.selection.disk.ok_or("Bir disk seçin")?;
    let disk: &Disk = state.disks.get(index).ok_or("Seçili disk kayboldu")?;

    if state.selection.manual {
        let assignments = state.selection.assignments(disk);
        Plan::manual(disk, assignments, disk::is_uefi_boot()).map_err(|error| error.to_string())
    } else {
        Plan::automatic(disk, disk::total_ram_bytes(), disk::is_uefi_boot())
            .map_err(|error| format!("{error:#}"))
    }
}

// ---- Account ----------------------------------------------------------------

pub fn wire_account(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_user_changed(move || {
        let ui = weak.unwrap();
        ui.set_password_score(crate::backend::user::password_score(&ui.get_password()) as i32);

        // Show the error only once the user has typed a confirmation; nagging
        // about "parolalar eşleşmiyor" on the first keystroke is noise.
        let account = account_from_ui(&ui);
        let confirm = ui.get_password_confirm();
        let message = match account.validate(&confirm) {
            Ok(()) => String::new(),
            Err(_) if confirm.is_empty() && !account.password.is_empty() => String::new(),
            Err(error) => error.to_string(),
        };
        ui.set_validation_error(message.into());
        refresh_can_advance(&ui);
    });
}

pub fn account_from_ui(ui: &MainWindow) -> Account {
    Account {
        hostname: ui.get_hostname().to_string(),
        username: ui.get_username().to_string(),
        password: ui.get_password().to_string(),
        root_same_password: ui.get_root_same_password(),
    }
}
