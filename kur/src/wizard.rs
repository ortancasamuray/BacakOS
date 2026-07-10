// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! The disk page's mutable selection state.
//!
//! Split out of `main.rs` because it is pure data with real rules — "assigning
//! a partition the root role forces a format", "clearing a role clears the
//! format box" — and those rules deserve tests rather than a scattering of `if`
//! statements among the Slint callbacks.

use crate::backend::disk::{Disk, PartitionInfo};
use crate::backend::plan::{Assignment, Role};

/// Build an [`Assignment`] from a listed partition and the user's choice.
fn assign(partition: &PartitionInfo, role: Role, format: bool) -> Assignment {
    Assignment {
        device: partition.path.clone(),
        role,
        format,
        size_bytes: partition.size_bytes,
        existing_fstype: partition.fstype.clone(),
    }
}

/// What the user picked in a partition row's combo box.
///
/// A superset of [`Role`]: the UI also needs "leave this partition alone",
/// which the plan has no concept of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoleChoice {
    #[default]
    Unused,
    Root,
    Esp,
    Swap,
}

impl RoleChoice {
    /// Order matters: this is the ComboBox model, and `index` is its row.
    pub const ALL: [RoleChoice; 4] =
        [RoleChoice::Unused, RoleChoice::Root, RoleChoice::Esp, RoleChoice::Swap];

    pub fn label(self) -> &'static str {
        match self {
            RoleChoice::Unused => "Kullanma",
            RoleChoice::Root => "Kök dizin (/)",
            RoleChoice::Esp => "EFI (/boot/efi)",
            RoleChoice::Swap => "Takas",
        }
    }

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|r| *r == self).unwrap_or(0) as i32
    }

    /// Out-of-range indices mean the UI and this table disagree; treat the row
    /// as unused rather than panicking inside a callback.
    pub fn from_index(index: i32) -> Self {
        usize::try_from(index).ok().and_then(|i| Self::ALL.get(i)).copied().unwrap_or_default()
    }

    pub fn to_role(self) -> Option<Role> {
        match self {
            RoleChoice::Unused => None,
            RoleChoice::Root => Some(Role::Root),
            RoleChoice::Esp => Some(Role::Esp),
            RoleChoice::Swap => Some(Role::Swap),
        }
    }

    /// Whether a freshly assigned partition should default to being formatted.
    ///
    /// Root and swap always want a clean filesystem. An ESP usually belongs to
    /// an existing OS and must be preserved, or dual-boot breaks.
    fn formats_by_default(self) -> bool {
        matches!(self, RoleChoice::Root | RoleChoice::Swap)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Choice {
    role: RoleChoice,
    format: bool,
}

/// One rendered row of the manual partitioning table.
pub struct Row<'a> {
    pub partition: &'a PartitionInfo,
    pub role: RoleChoice,
    pub format: bool,
}

/// Which disk is selected, whether the user is partitioning by hand, and what
/// they assigned to each of that disk's partitions.
#[derive(Debug, Default)]
pub struct Selection {
    /// Index into `AppState::disks`.
    pub disk: Option<usize>,
    pub manual: bool,
    /// Parallel to the selected disk's `partitions`. Reset when the disk changes.
    choices: Vec<Choice>,
}

impl Selection {
    /// Point at a new disk, discarding any assignments made against the old one
    /// — its partition indices mean nothing here.
    pub fn select_disk(&mut self, index: usize, disk: &Disk) {
        if self.disk == Some(index) {
            return;
        }
        self.disk = Some(index);
        self.choices = vec![Choice::default(); disk.partitions.len()];
    }

    pub fn set_role(&mut self, row: usize, role: RoleChoice) {
        if row >= self.choices.len() {
            return;
        }

        // A role may only be held by one partition. Clear whoever had it.
        if role != RoleChoice::Unused {
            for (index, other) in self.choices.iter_mut().enumerate() {
                if index != row && other.role == role {
                    *other = Choice::default();
                }
            }
        }

        let choice = &mut self.choices[row];
        choice.role = role;
        choice.format = role.formats_by_default();
    }

    /// Ignored for a role that has no say in the matter: root is always
    /// formatted, an unused partition never is.
    pub fn set_format(&mut self, row: usize, format: bool) {
        let Some(choice) = self.choices.get_mut(row) else {
            return;
        };
        match choice.role {
            RoleChoice::Unused | RoleChoice::Root => {}
            _ => choice.format = format,
        }
    }

    /// The rows to render, or nothing when no disk is selected.
    pub fn rows<'a>(&'a self, disks: &'a [Disk]) -> impl Iterator<Item = Row<'a>> {
        self.disk
            .and_then(|index| disks.get(index))
            .into_iter()
            .flat_map(move |disk| {
                disk.partitions.iter().enumerate().map(move |(i, partition)| {
                    let choice = self.choices.get(i).copied().unwrap_or_default();
                    Row { partition, role: choice.role, format: choice.format }
                })
            })
    }

    /// The assignments to hand [`crate::backend::plan::Plan::manual`].
    pub fn assignments(&self, disk: &Disk) -> Vec<Assignment> {
        disk.partitions
            .iter()
            .enumerate()
            .filter_map(|(i, partition)| {
                let choice = self.choices.get(i)?;
                let role = choice.role.to_role()?;
                Some(assign(partition, role, choice.format))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(path: &str, size: u64) -> PartitionInfo {
        PartitionInfo {
            path: path.into(),
            size_bytes: size,
            fstype: Some("ext4".into()),
            label: None,
            mountpoint: None,
        }
    }

    fn disk(partitions: usize) -> Disk {
        Disk {
            path: "/dev/sda".into(),
            model: "Test".into(),
            size_bytes: 500 * 1024 * 1024 * 1024,
            removable: false,
            is_live_medium: false,
            partitions: (1..=partitions).map(|i| part(&format!("/dev/sda{i}"), 1 << 30)).collect(),
        }
    }

    #[test]
    fn role_indices_round_trip() {
        for role in RoleChoice::ALL {
            assert_eq!(RoleChoice::from_index(role.index()), role);
        }
        // A combo box that has drifted out of sync must not panic.
        assert_eq!(RoleChoice::from_index(99), RoleChoice::Unused);
        assert_eq!(RoleChoice::from_index(-1), RoleChoice::Unused);
    }

    #[test]
    fn selecting_a_disk_sizes_the_choice_vector() {
        let mut selection = Selection::default();
        selection.select_disk(0, &disk(3));
        assert_eq!(selection.rows(&[disk(3)]).count(), 3);
    }

    #[test]
    fn changing_disks_discards_old_assignments() {
        let disks = [disk(3), disk(2)];
        let mut selection = Selection::default();
        selection.select_disk(0, &disks[0]);
        selection.set_role(1, RoleChoice::Root);

        selection.select_disk(1, &disks[1]);
        assert!(selection.rows(&disks).all(|r| r.role == RoleChoice::Unused));
    }

    #[test]
    fn reselecting_the_same_disk_keeps_assignments() {
        let disks = [disk(3)];
        let mut selection = Selection::default();
        selection.select_disk(0, &disks[0]);
        selection.set_role(1, RoleChoice::Root);

        selection.select_disk(0, &disks[0]);
        assert_eq!(selection.rows(&disks).nth(1).unwrap().role, RoleChoice::Root);
    }

    #[test]
    fn root_is_always_formatted_and_cannot_be_unticked() {
        let disks = [disk(2)];
        let mut selection = Selection::default();
        selection.select_disk(0, &disks[0]);

        selection.set_role(0, RoleChoice::Root);
        assert!(selection.rows(&disks).next().unwrap().format);

        selection.set_format(0, false);
        assert!(selection.rows(&disks).next().unwrap().format, "kök daima biçimlendirilir");
    }

    #[test]
    fn esp_defaults_to_preserved_but_can_be_formatted() {
        let disks = [disk(2)];
        let mut selection = Selection::default();
        selection.select_disk(0, &disks[0]);

        selection.set_role(0, RoleChoice::Esp);
        assert!(!selection.rows(&disks).next().unwrap().format, "var olan ESP korunmalı");

        selection.set_format(0, true);
        assert!(selection.rows(&disks).next().unwrap().format);
    }

    #[test]
    fn assigning_a_role_steals_it_from_the_previous_holder() {
        let disks = [disk(3)];
        let mut selection = Selection::default();
        selection.select_disk(0, &disks[0]);

        selection.set_role(0, RoleChoice::Root);
        selection.set_role(2, RoleChoice::Root);

        let roles: Vec<_> = selection.rows(&disks).map(|r| r.role).collect();
        assert_eq!(roles, [RoleChoice::Unused, RoleChoice::Unused, RoleChoice::Root]);
    }

    #[test]
    fn clearing_a_role_clears_its_format_flag() {
        let disks = [disk(2)];
        let mut selection = Selection::default();
        selection.select_disk(0, &disks[0]);

        selection.set_role(0, RoleChoice::Swap);
        assert!(selection.rows(&disks).next().unwrap().format);

        selection.set_role(0, RoleChoice::Unused);
        assert!(!selection.rows(&disks).next().unwrap().format);
    }

    #[test]
    fn assignments_skip_unused_partitions() {
        let target = disk(3);
        let mut selection = Selection::default();
        selection.select_disk(0, &target);
        selection.set_role(0, RoleChoice::Esp);
        selection.set_role(1, RoleChoice::Root);

        let assignments = selection.assignments(&target);
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].device, "/dev/sda1");
        assert_eq!(assignments[0].role, Role::Esp);
        assert!(!assignments[0].format);
        assert_eq!(assignments[1].role, Role::Root);
        assert!(assignments[1].format);
    }

    #[test]
    fn no_disk_selected_yields_no_rows() {
        let selection = Selection::default();
        assert_eq!(selection.rows(&[disk(3)]).count(), 0);
    }
}
