// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! The disk page's mutable selection state.
//!
//! Split out of `main.rs` because it is pure data with real rules — "assigning
//! a partition the root role forces a format", "clearing a role clears the
//! format box" — and those rules deserve tests rather than a scattering of `if`
//! statements among the Slint callbacks.

use crate::backend::disk::{format_bytes, Disk, FreeSpace, PartitionInfo, PartitionTable};
#[cfg(test)]
use crate::backend::disk::{GIB, MIB};
use crate::backend::plan::{Assignment, Layout, Plan, Role};

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
    /// Remove this partition before anything is created. Not a [`Role`] — the
    /// plan has no concept of a deleted partition holding one.
    Delete,
}

impl RoleChoice {
    /// Order matters: this is the ComboBox model, and `index` is its row. It
    /// also doubles as the "kind" the disk-bar segments hand the UI for
    /// colouring, so keep it stable.
    pub const ALL: [RoleChoice; 5] =
        [RoleChoice::Unused, RoleChoice::Root, RoleChoice::Esp, RoleChoice::Swap, RoleChoice::Delete];

    pub fn label(self) -> &'static str {
        match self {
            RoleChoice::Unused => "Kullanma",
            RoleChoice::Root => "Kök dizin (/)",
            RoleChoice::Esp => "EFI (/boot/efi)",
            RoleChoice::Swap => "Takas",
            RoleChoice::Delete => "Sil",
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
            RoleChoice::Unused | RoleChoice::Delete => None,
            RoleChoice::Root => Some(Role::Root),
            RoleChoice::Esp => Some(Role::Esp),
            RoleChoice::Swap => Some(Role::Swap),
        }
    }

    pub fn from_role(role: Role) -> Self {
        match role {
            Role::Root => RoleChoice::Root,
            Role::Esp => RoleChoice::Esp,
            Role::Swap => RoleChoice::Swap,
            // Manual mode never offers BIOS boot as a create/keep target.
            Role::BiosBoot => RoleChoice::Unused,
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

/// Roles offered when creating a partition in a free-space gap — no
/// "Kullanma"/"Sil", which only make sense for an existing partition.
pub const CREATE_ROLES: [Role; 3] = [Role::Root, Role::Esp, Role::Swap];

/// A new partition the user has asked to create in a free-space gap.
/// `gap` indexes the disk's [`PartitionTable::free_space`] list — stable for
/// as long as the table itself doesn't change, i.e. for the lifetime of one
/// visit to the disk page.
#[derive(Debug, Clone, Copy)]
pub struct PlannedCreate {
    pub gap: usize,
    pub role: Role,
    pub size_bytes: u64,
}

/// One coloured block of the manual-partitioning bar, left to right in
/// on-disk order. `role_kind` is a [`RoleChoice::index`] value, shared with
/// the ComboBox model so the UI can colour both from one lookup table.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub role_kind: i32,
    pub is_free: bool,
    pub fraction: f32,
    pub label: String,
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
    /// New partitions planned into free-space gaps. Reset when the disk changes.
    creates: Vec<PlannedCreate>,
}

impl Selection {
    /// Point at a new disk, discarding any assignments made against the old one
    /// — its partition indices and gap numbers mean nothing here.
    pub fn select_disk(&mut self, index: usize, disk: &Disk) {
        if self.disk == Some(index) {
            return;
        }
        self.disk = Some(index);
        self.choices = vec![Choice::default(); disk.partitions.len()];
        self.creates = Vec::new();
    }

    /// A [`Role`] may be held by exactly one holder — an existing partition
    /// kept with that role, or a planned new one. Clear whoever has it before
    /// handing it to someone else.
    fn clear_role_holders(&mut self, role: Role) {
        for choice in self.choices.iter_mut() {
            if choice.role.to_role() == Some(role) {
                *choice = Choice::default();
            }
        }
        self.creates.retain(|c| c.role != role);
    }

    pub fn set_role(&mut self, row: usize, role: RoleChoice) {
        if row >= self.choices.len() {
            return;
        }
        if let Some(taken) = role.to_role() {
            self.clear_role_holders(taken);
        }

        let choice = &mut self.choices[row];
        choice.role = role;
        choice.format = role.formats_by_default();
    }

    /// Ignored for a role that has no say in the matter: root is always
    /// formatted, an unused or deleted partition never is.
    pub fn set_format(&mut self, row: usize, format: bool) {
        let Some(choice) = self.choices.get_mut(row) else {
            return;
        };
        match choice.role {
            RoleChoice::Unused | RoleChoice::Root | RoleChoice::Delete => {}
            _ => choice.format = format,
        }
    }

    /// Plan a new partition in free-space gap `gap`, stealing `role` from
    /// whoever currently holds it.
    pub fn add_create(&mut self, gap: usize, role: Role, size_bytes: u64) {
        self.clear_role_holders(role);
        self.creates.push(PlannedCreate { gap, role, size_bytes });
    }

    /// Undo a [`Self::add_create`] by its position in [`Self::creates`].
    pub fn remove_create(&mut self, index: usize) {
        if index < self.creates.len() {
            self.creates.remove(index);
        }
    }

    pub fn creates(&self) -> &[PlannedCreate] {
        &self.creates
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

    /// The assignments to hand [`crate::backend::plan::Plan::manual`] as `keep`.
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

    /// Existing partitions marked for removal, for `Plan::manual`'s `delete`.
    pub fn deletions(&self, disk: &Disk) -> Vec<String> {
        disk.partitions
            .iter()
            .enumerate()
            .filter_map(|(i, partition)| {
                let choice = self.choices.get(i)?;
                (choice.role == RoleChoice::Delete).then(|| partition.path.clone())
            })
            .collect()
    }

    /// `(role, start_bytes, size_bytes)` for every planned new partition, for
    /// `Plan::manual`'s `create`. A planned size larger than its gap (stale
    /// after another change shrank it) is clamped rather than overflowing
    /// into the next partition.
    pub fn create_requests(&self, free: &[FreeSpace]) -> Vec<(Role, u64, u64)> {
        self.creates
            .iter()
            .filter_map(|c| {
                let gap = free.get(c.gap)?;
                Some((c.role, gap.start_bytes, c.size_bytes.min(gap.size_bytes)))
            })
            .collect()
    }

    /// The manual-partitioning bar: every existing partition, gap and planned
    /// new partition on `disk`, left to right in on-disk order.
    pub fn segments(&self, disk: &Disk, table: &PartitionTable) -> Vec<Segment> {
        let total = disk.size_bytes as f32;
        if total <= 0.0 {
            return Vec::new();
        }

        enum Item<'a> {
            Existing { device: &'a str, size: u64 },
            Free { gap: usize, size: u64 },
        }

        let free = table.free_space();
        let mut items: Vec<(u64, Item)> = table
            .ordered_partitions()
            .into_iter()
            .map(|(device, start, size)| (start, Item::Existing { device, size }))
            .collect();
        items.extend(
            free.iter().enumerate().map(|(i, g)| (g.start_bytes, Item::Free { gap: i, size: g.size_bytes })),
        );
        items.sort_by_key(|(start, _)| *start);

        items
            .into_iter()
            .flat_map(|(_, item)| match item {
                Item::Existing { device, size } => {
                    let choice = disk
                        .partitions
                        .iter()
                        .position(|p| p.path == device)
                        .and_then(|i| self.choices.get(i))
                        .copied()
                        .unwrap_or_default();
                    let label = match choice.role {
                        RoleChoice::Delete => format!("{device} — silinecek"),
                        RoleChoice::Unused => device.to_string(),
                        role => format!("{device} — {}", role.label()),
                    };
                    vec![Segment {
                        role_kind: choice.role.index(),
                        is_free: false,
                        fraction: size as f32 / total,
                        label,
                    }]
                }
                Item::Free { gap, size } => {
                    let Some(planned) = self.creates.iter().find(|c| c.gap == gap) else {
                        return vec![Segment {
                            role_kind: RoleChoice::Unused.index(),
                            is_free: true,
                            fraction: size as f32 / total,
                            label: format!("Boş alan — {}", format_bytes(size)),
                        }];
                    };

                    let choice = RoleChoice::from_role(planned.role);
                    let used = planned.size_bytes.min(size);
                    let mut segments = vec![Segment {
                        role_kind: choice.index(),
                        is_free: false,
                        fraction: used as f32 / total,
                        label: format!("Yeni {} — {}", choice.label(), format_bytes(used)),
                    }];
                    if size > used {
                        segments.push(Segment {
                            role_kind: RoleChoice::Unused.index(),
                            is_free: true,
                            fraction: (size - used) as f32 / total,
                            label: format!("Boş alan — {}", format_bytes(size - used)),
                        });
                    }
                    segments
                }
            })
            .collect()
    }
}

/// [`Segment::role_kind`] for the BIOS boot partition. Outside `RoleChoice`'s
/// own range (0..=4, `RoleChoice::ALL`'s indices) since it is never a combo-box
/// option; `DiskBar::role-color` in `pages.slint` gives it its own colour.
const BIOS_BOOT_KIND: i32 = 5;

/// The disk-bar segments for an automatic plan: every partition it will
/// create, in order, coloured the same way a manual assignment would be.
/// Root's `size_bytes: None` ("everything else") is resolved against the
/// disk's total size so its block still renders proportionally.
///
/// Returns nothing for a manual plan — that page renders [`Selection::segments`]
/// instead.
pub fn automatic_segments(plan: &Plan, disk_size_bytes: u64) -> Vec<Segment> {
    let Layout::Automatic(partitions) = &plan.layout else {
        return Vec::new();
    };
    let total = disk_size_bytes as f32;
    if total <= 0.0 {
        return Vec::new();
    }

    let fixed_bytes: u64 = partitions.iter().filter_map(|p| p.size_bytes).sum();

    partitions
        .iter()
        .map(|part| {
            let size = part.size_bytes.unwrap_or_else(|| disk_size_bytes.saturating_sub(fixed_bytes));
            let role_kind = match part.role {
                Role::BiosBoot => BIOS_BOOT_KIND,
                role => RoleChoice::from_role(role).index(),
            };
            Segment {
                role_kind,
                is_free: false,
                fraction: size as f32 / total,
                label: format!("{} — {}", part.role.label(), format_bytes(size)),
            }
        })
        .collect()
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

    #[test]
    fn deletions_lists_only_partitions_marked_for_removal() {
        let target = disk(3);
        let mut selection = Selection::default();
        selection.select_disk(0, &target);
        selection.set_role(0, RoleChoice::Delete);
        selection.set_role(1, RoleChoice::Root);

        assert_eq!(selection.deletions(&target), vec!["/dev/sda1".to_string()]);
        // A deleted partition is never in `assignments` either.
        assert_eq!(selection.assignments(&target).len(), 1);
    }

    #[test]
    fn deleting_and_reassigning_a_role_do_not_interfere() {
        let target = disk(2);
        let mut selection = Selection::default();
        selection.select_disk(0, &target);

        selection.set_role(0, RoleChoice::Delete);
        selection.set_role(1, RoleChoice::Delete);
        let roles: Vec<_> = selection.rows(&[target.clone()]).map(|r| r.role).collect();
        assert_eq!(roles, [RoleChoice::Delete, RoleChoice::Delete], "silme benzersiz bir rol değil");
    }

    fn free(regions: &[(u64, u64)]) -> Vec<FreeSpace> {
        regions.iter().map(|&(start_bytes, size_bytes)| FreeSpace { start_bytes, size_bytes }).collect()
    }

    #[test]
    fn add_create_steals_its_role_from_an_existing_holder() {
        let target = disk(2);
        let mut selection = Selection::default();
        selection.select_disk(0, &target);
        selection.set_role(0, RoleChoice::Root);

        selection.add_create(0, Role::Root, 100 * GIB);

        assert!(selection.rows(&[target]).next().unwrap().role == RoleChoice::Unused, "eski sahibi silinmeli");
        assert_eq!(selection.creates().len(), 1);
    }

    #[test]
    fn create_requests_clamps_to_the_gap_size() {
        let mut selection = Selection::default();
        selection.select_disk(0, &disk(0));
        selection.add_create(0, Role::Root, 999 * GIB);

        let requests = selection.create_requests(&free(&[(MIB, 100 * GIB)]));
        assert_eq!(requests, vec![(Role::Root, MIB, 100 * GIB)]);
    }

    #[test]
    fn create_requests_skips_a_gap_that_no_longer_exists() {
        let mut selection = Selection::default();
        selection.select_disk(0, &disk(0));
        selection.add_create(5, Role::Root, 100 * GIB);

        assert!(selection.create_requests(&free(&[(MIB, 100 * GIB)])).is_empty());
    }

    #[test]
    fn remove_create_undoes_a_planned_partition() {
        let mut selection = Selection::default();
        selection.select_disk(0, &disk(0));
        selection.add_create(0, Role::Root, 100 * GIB);
        selection.remove_create(0);

        assert!(selection.creates().is_empty());
    }

    #[test]
    fn segments_cover_the_whole_disk_and_flag_the_free_gap() {
        let target = disk(1);
        let mut selection = Selection::default();
        selection.select_disk(0, &target);
        selection.set_role(0, RoleChoice::Root);

        // sda1 occupies the first GiB; the rest of the 500 GiB disk is free.
        let table = PartitionTable::for_tests(MIB, target.size_bytes, vec![("/dev/sda1", MIB, 1 << 30)]);
        let segments = selection.segments(&target, &table);

        let total: f32 = segments.iter().map(|s| s.fraction).sum();
        assert!((total - 1.0).abs() < 0.01, "parçalar diskin tamamını kaplamalı: {total}");
        assert!(segments.iter().any(|s| s.is_free), "kalan boş alan bir parça olmalı");
        assert_eq!(segments[0].role_kind, RoleChoice::Root.index());
    }

    #[test]
    fn segments_split_a_gap_between_a_planned_partition_and_leftover_space() {
        let target = disk(0);
        let mut selection = Selection::default();
        selection.select_disk(0, &target);
        selection.add_create(0, Role::Root, 50 * GIB);

        let table = PartitionTable::for_tests(MIB, target.size_bytes, Vec::new());
        let segments = selection.segments(&target, &table);

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].role_kind, RoleChoice::Root.index());
        assert!(!segments[0].is_free);
        assert!(segments[1].is_free, "kullanılmayan kısım boş olarak kalmalı");
    }

    #[test]
    fn automatic_segments_cover_the_whole_disk_with_no_free_gap() {
        let target = disk(0);
        let plan = Plan::automatic(&target, 8 * GIB, true).unwrap();

        let segments = automatic_segments(&plan, target.size_bytes);

        // ESP + swap + root, none of it left over: an automatic plan claims
        // every byte, unlike a manual one.
        assert_eq!(segments.len(), 3);
        assert!(segments.iter().all(|s| !s.is_free));
        assert_eq!(segments[0].role_kind, RoleChoice::Esp.index());
        assert_eq!(segments.last().unwrap().role_kind, RoleChoice::Root.index());

        let total: f32 = segments.iter().map(|s| s.fraction).sum();
        assert!((total - 1.0).abs() < 0.01, "parçalar diskin tamamını kaplamalı: {total}");
    }

    #[test]
    fn automatic_segments_give_bios_boot_its_own_colour() {
        let target = disk(0);
        let plan = Plan::automatic(&target, 8 * GIB, false).unwrap();

        let segments = automatic_segments(&plan, target.size_bytes);

        // BIOS boot is never a `RoleChoice` combo-box option, so it must not
        // collide with one of those indices (which would tint it the same
        // as an unrelated role in the manual editor).
        assert_eq!(segments[0].role_kind, BIOS_BOOT_KIND);
        assert!(RoleChoice::ALL.iter().all(|r| r.index() != BIOS_BOOT_KIND));
    }

    #[test]
    fn automatic_segments_are_empty_for_a_manual_plan() {
        use crate::backend::plan::ManualLayout;

        let plan = Plan { disk: "/dev/sda".into(), layout: Layout::Manual(ManualLayout::default()) };
        assert!(automatic_segments(&plan, 500 * GIB).is_empty());
    }
}
