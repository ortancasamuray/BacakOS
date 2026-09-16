// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Validation rules for a manual partition layout.
//!
//! Every rule here is a precondition of a later install stage. Checking them
//! before the confirmation screen means a manual layout can never fail *during*
//! the install, when the disk has already been modified.

use thiserror::Error;

use super::{Layout, Role};
use crate::backend::disk::{format_bytes, GIB, MIB};

/// The smallest ESP firmware tolerates in practice. Reusing anything below this
/// leaves no room for a second kernel's fallback image.
const MIN_ESP_BYTES: u64 = 100 * MIB;

/// Root needs the base system, a desktop and room to apply updates.
const MIN_ROOT_BYTES: u64 = 12 * GIB;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("Bir kök bölümü (/) seçmelisiniz")]
    NoRoot,
    #[error("Yalnızca bir kök bölümü seçebilirsiniz")]
    MultipleRoots,
    #[error("Kök bölümü en az {} olmalı", format_bytes(MIN_ROOT_BYTES))]
    RootTooSmall,
    #[error("Kök bölümü biçimlendirilmeden kullanılamaz")]
    RootNotFormatted,
    #[error("UEFI sisteminde bir EFI sistem bölümü seçmelisiniz")]
    NoEsp,
    #[error("Yalnızca bir EFI sistem bölümü seçebilirsiniz")]
    MultipleEsps,
    #[error("EFI sistem bölümü en az {} olmalı", format_bytes(MIN_ESP_BYTES))]
    EspTooSmall,
    #[error("Korunacak EFI bölümü FAT biçiminde olmalı; biçimlendirmeyi seçin")]
    EspNotFat,
    #[error("Yalnızca bir takas alanı seçebilirsiniz")]
    MultipleSwaps,
    #[error("`{0}` şu anda bağlı; önce ayırın")]
    DeviceMounted(String),
    #[error("`{0}` artık disk üzerinde yok")]
    UnknownDevice(String),
}

impl Layout {
    /// Check every rule the install stages later depend on.
    ///
    /// A role may be held by either a kept existing partition or a newly
    /// created one — `count` and the min-size checks below look at both, since
    /// the user can freely pick either kind for root/ESP/swap.
    pub fn validate(&self, uefi: bool) -> Result<(), PlanError> {
        let Layout::Manual(m) = self else {
            // An automatic layout is constructed by us and correct by design.
            return Ok(());
        };

        let count = |role: Role| {
            m.keep.iter().filter(|a| a.role == role).count()
                + m.create.iter().filter(|c| c.role == role).count()
        };

        match count(Role::Root) {
            0 => return Err(PlanError::NoRoot),
            1 => {}
            _ => return Err(PlanError::MultipleRoots),
        }
        if count(Role::Swap) > 1 {
            return Err(PlanError::MultipleSwaps);
        }

        let root_size = m
            .keep
            .iter()
            .find(|a| a.role == Role::Root)
            .map(|a| a.size_bytes)
            .or_else(|| m.create.iter().find(|c| c.role == Role::Root).map(|c| c.size_bytes))
            .expect("checked above");
        if root_size < MIN_ROOT_BYTES {
            return Err(PlanError::RootTooSmall);
        }
        // A kept root over a populated filesystem leaves the old
        // distribution's files behind and produces an unbootable hybrid; a
        // created root is always freshly formatted, so only the kept case
        // needs checking.
        if let Some(root) = m.keep.iter().find(|a| a.role == Role::Root) {
            if !root.format {
                return Err(PlanError::RootNotFormatted);
            }
        }

        if uefi {
            match count(Role::Esp) {
                0 => return Err(PlanError::NoEsp),
                1 => {}
                _ => return Err(PlanError::MultipleEsps),
            }
            if let Some(esp) = m.keep.iter().find(|a| a.role == Role::Esp) {
                if esp.size_bytes < MIN_ESP_BYTES {
                    return Err(PlanError::EspTooSmall);
                }
                // Keeping another OS's ESP is the normal dual-boot case, but
                // only if it really is FAT — GRUB cannot write anywhere else.
                let is_fat = esp.existing_fstype.as_deref().is_some_and(|fs| fs.starts_with("vfat"));
                if !esp.format && !is_fat {
                    return Err(PlanError::EspNotFat);
                }
            } else if let Some(esp) = m.create.iter().find(|c| c.role == Role::Esp) {
                if esp.size_bytes < MIN_ESP_BYTES {
                    return Err(PlanError::EspTooSmall);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::plan::tests::assign;
    use crate::backend::plan::{Assignment, ManualCreate, ManualLayout};

    fn manual(keep: Vec<Assignment>) -> Layout {
        Layout::Manual(ManualLayout::keep_only(keep))
    }

    fn esp(size: u64, fstype: Option<&str>, format: bool) -> Assignment {
        assign("/dev/sda1", size, fstype, Role::Esp, format)
    }

    fn root(size: u64, format: bool) -> Assignment {
        assign("/dev/sda2", size, Some("ext4"), Role::Root, format)
    }

    fn valid() -> Vec<Assignment> {
        vec![esp(512 * MIB, Some("vfat"), false), root(100 * GIB, true)]
    }

    #[test]
    fn a_valid_manual_layout_passes() {
        assert!(manual(valid()).validate(true).is_ok());
    }

    #[test]
    fn automatic_layouts_are_always_valid() {
        assert!(Layout::Automatic(Vec::new()).validate(true).is_ok());
    }

    #[test]
    fn requires_exactly_one_root() {
        let none = vec![esp(512 * MIB, Some("vfat"), false)];
        assert_eq!(manual(none).validate(true), Err(PlanError::NoRoot));

        let mut two = valid();
        two.push(assign("/dev/sda3", 50 * GIB, None, Role::Root, true));
        assert_eq!(manual(two).validate(true), Err(PlanError::MultipleRoots));
    }

    #[test]
    fn root_must_be_formatted() {
        let assignments = vec![esp(512 * MIB, Some("vfat"), false), root(100 * GIB, false)];
        assert_eq!(manual(assignments).validate(true), Err(PlanError::RootNotFormatted));
    }

    #[test]
    fn root_must_be_large_enough() {
        let assignments = vec![esp(512 * MIB, Some("vfat"), false), root(4 * GIB, true)];
        assert_eq!(manual(assignments).validate(true), Err(PlanError::RootTooSmall));
    }

    #[test]
    fn preserved_esp_must_already_be_fat() {
        let assignments = vec![esp(512 * MIB, Some("ext4"), false), root(100 * GIB, true)];
        assert_eq!(manual(assignments).validate(true), Err(PlanError::EspNotFat));
    }

    #[test]
    fn non_fat_esp_is_fine_when_it_will_be_formatted() {
        let assignments = vec![esp(512 * MIB, Some("ext4"), true), root(100 * GIB, true)];
        assert!(manual(assignments).validate(true).is_ok());
    }

    #[test]
    fn esp_must_be_large_enough() {
        let assignments = vec![esp(32 * MIB, Some("vfat"), false), root(100 * GIB, true)];
        assert_eq!(manual(assignments).validate(true), Err(PlanError::EspTooSmall));
    }

    #[test]
    fn bios_boot_needs_no_esp() {
        let assignments = vec![root(100 * GIB, true)];
        assert!(manual(assignments.clone()).validate(false).is_ok());
        assert_eq!(manual(assignments).validate(true), Err(PlanError::NoEsp));
    }

    #[test]
    fn at_most_one_swap() {
        let mut assignments = valid();
        assignments.push(assign("/dev/sda3", GIB, None, Role::Swap, true));
        assignments.push(assign("/dev/sda4", GIB, None, Role::Swap, true));
        assert_eq!(manual(assignments).validate(true), Err(PlanError::MultipleSwaps));
    }

    fn create(role: Role, size_bytes: u64) -> ManualCreate {
        ManualCreate { role, number: 3, start_bytes: 0, size_bytes }
    }

    #[test]
    fn root_and_esp_can_both_be_freshly_created_partitions() {
        let layout = Layout::Manual(ManualLayout {
            keep: Vec::new(),
            delete: Vec::new(),
            create: vec![create(Role::Esp, 512 * MIB), create(Role::Root, 100 * GIB)],
            ..ManualLayout::keep_only(Vec::new())
        });
        assert!(layout.validate(true).is_ok());
    }

    #[test]
    fn created_root_needs_no_format_check_but_still_needs_size() {
        let layout = Layout::Manual(ManualLayout {
            keep: vec![esp(512 * MIB, Some("vfat"), false)],
            delete: Vec::new(),
            create: vec![create(Role::Root, 4 * GIB)],
            ..ManualLayout::keep_only(Vec::new())
        });
        assert_eq!(layout.validate(true), Err(PlanError::RootTooSmall));
    }

    #[test]
    fn created_esp_must_be_large_enough() {
        let layout = Layout::Manual(ManualLayout {
            keep: vec![root(100 * GIB, true)],
            delete: Vec::new(),
            create: vec![create(Role::Esp, 32 * MIB)],
            ..ManualLayout::keep_only(Vec::new())
        });
        assert_eq!(layout.validate(true), Err(PlanError::EspTooSmall));
    }
}
