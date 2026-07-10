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
}

impl Layout {
    /// Check every rule the install stages later depend on.
    pub fn validate(&self, uefi: bool) -> Result<(), PlanError> {
        let Layout::Manual(assignments) = self else {
            // An automatic layout is constructed by us and correct by design.
            return Ok(());
        };

        let count = |role: Role| assignments.iter().filter(|a| a.role == role).count();

        match count(Role::Root) {
            0 => return Err(PlanError::NoRoot),
            1 => {}
            _ => return Err(PlanError::MultipleRoots),
        }
        if count(Role::Swap) > 1 {
            return Err(PlanError::MultipleSwaps);
        }

        let root = assignments.iter().find(|a| a.role == Role::Root).expect("checked above");
        if root.size_bytes < MIN_ROOT_BYTES {
            return Err(PlanError::RootTooSmall);
        }
        // Installing over a populated filesystem leaves the old distribution's
        // files behind and produces an unbootable hybrid.
        if !root.format {
            return Err(PlanError::RootNotFormatted);
        }

        if uefi {
            match count(Role::Esp) {
                0 => return Err(PlanError::NoEsp),
                1 => {}
                _ => return Err(PlanError::MultipleEsps),
            }
            let esp = assignments.iter().find(|a| a.role == Role::Esp).expect("checked above");
            if esp.size_bytes < MIN_ESP_BYTES {
                return Err(PlanError::EspTooSmall);
            }
            // Keeping another OS's ESP is the normal dual-boot case, but only
            // if it really is FAT — GRUB cannot write anywhere else.
            let is_fat = esp.existing_fstype.as_deref().is_some_and(|fs| fs.starts_with("vfat"));
            if !esp.format && !is_fat {
                return Err(PlanError::EspNotFat);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::plan::tests::assign;
    use crate::backend::plan::Assignment;

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
        assert!(Layout::Manual(valid()).validate(true).is_ok());
    }

    #[test]
    fn automatic_layouts_are_always_valid() {
        assert!(Layout::Automatic(Vec::new()).validate(true).is_ok());
    }

    #[test]
    fn requires_exactly_one_root() {
        let none = vec![esp(512 * MIB, Some("vfat"), false)];
        assert_eq!(Layout::Manual(none).validate(true), Err(PlanError::NoRoot));

        let mut two = valid();
        two.push(assign("/dev/sda3", 50 * GIB, None, Role::Root, true));
        assert_eq!(Layout::Manual(two).validate(true), Err(PlanError::MultipleRoots));
    }

    #[test]
    fn root_must_be_formatted() {
        let assignments = vec![esp(512 * MIB, Some("vfat"), false), root(100 * GIB, false)];
        assert_eq!(Layout::Manual(assignments).validate(true), Err(PlanError::RootNotFormatted));
    }

    #[test]
    fn root_must_be_large_enough() {
        let assignments = vec![esp(512 * MIB, Some("vfat"), false), root(4 * GIB, true)];
        assert_eq!(Layout::Manual(assignments).validate(true), Err(PlanError::RootTooSmall));
    }

    #[test]
    fn preserved_esp_must_already_be_fat() {
        let assignments = vec![esp(512 * MIB, Some("ext4"), false), root(100 * GIB, true)];
        assert_eq!(Layout::Manual(assignments).validate(true), Err(PlanError::EspNotFat));
    }

    #[test]
    fn non_fat_esp_is_fine_when_it_will_be_formatted() {
        let assignments = vec![esp(512 * MIB, Some("ext4"), true), root(100 * GIB, true)];
        assert!(Layout::Manual(assignments).validate(true).is_ok());
    }

    #[test]
    fn esp_must_be_large_enough() {
        let assignments = vec![esp(32 * MIB, Some("vfat"), false), root(100 * GIB, true)];
        assert_eq!(Layout::Manual(assignments).validate(true), Err(PlanError::EspTooSmall));
    }

    #[test]
    fn bios_boot_needs_no_esp() {
        let assignments = vec![root(100 * GIB, true)];
        assert!(Layout::Manual(assignments.clone()).validate(false).is_ok());
        assert_eq!(Layout::Manual(assignments).validate(true), Err(PlanError::NoEsp));
    }

    #[test]
    fn at_most_one_swap() {
        let mut assignments = valid();
        assignments.push(assign("/dev/sda3", GIB, None, Role::Swap, true));
        assignments.push(assign("/dev/sda4", GIB, None, Role::Swap, true));
        assert_eq!(Layout::Manual(assignments).validate(true), Err(PlanError::MultipleSwaps));
    }
}
