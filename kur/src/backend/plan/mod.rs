// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Partition planning.
//!
//! A [`Plan`] is a complete, inspectable description of what the installer is
//! about to do to a disk. Nothing here touches hardware: a plan can be built,
//! validated, rendered to text and unit-tested without a block device in sight.
//! [`super::install`] is the only module that executes one.
//!
//! The rules a manual layout must satisfy live in [`rules`].

mod rules;

pub use rules::PlanError;

use anyhow::{bail, Result};

use super::disk::{format_bytes, Disk, PartitionTable, GIB, MIB, MIN_DISK_BYTES};

/// EFI System Partition size. 512 MiB is the Debian installer default and
/// leaves room for several kernels' worth of fallback images.
const ESP_BYTES: u64 = 512 * MIB;

/// BIOS boot partition size. 1 MiB is what GRUB's `core.img` needs; the
/// Discoverable Partitions Spec and `sfdisk` both treat this as the standard.
const BIOS_BOOT_BYTES: u64 = MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Esp,
    /// The 1 MiB unformatted partition `grub-install --target=i386-pc` embeds
    /// `core.img` into. Required on BIOS machines with a GPT label, where the
    /// MBR gap that GRUB would otherwise use does not exist.
    BiosBoot,
    Root,
    Swap,
}

impl Role {
    /// GPT partition type GUID, from the Discoverable Partitions Specification.
    fn type_uuid(self) -> &'static str {
        match self {
            Role::Esp => "C12A7328-F81F-11D2-BA4B-00A0C93EC93B",
            Role::BiosBoot => "21686148-6449-6E6F-744E-656564454649",
            Role::Swap => "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F",
            Role::Root => "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Esp => "EFI Sistem Bölümü",
            Role::BiosBoot => "BIOS önyükleme bölümü",
            Role::Root => "Kök dizin (/)",
            Role::Swap => "Takas alanı",
        }
    }

    pub fn filesystem(self) -> &'static str {
        match self {
            Role::Esp => "fat32",
            // GRUB writes raw sectors here; a filesystem would be overwritten.
            Role::BiosBoot => "biçimlendirilmez",
            Role::Root => "ext4",
            Role::Swap => "swap",
        }
    }

    /// False for [`Role::BiosBoot`], the one partition that must stay raw.
    fn needs_filesystem(self) -> bool {
        !matches!(self, Role::BiosBoot)
    }
}

/// A partition to be *created*, in automatic mode.
#[derive(Debug, Clone)]
pub struct NewPartition {
    pub role: Role,
    /// `None` means "all remaining space".
    pub size_bytes: Option<u64>,
}

/// An *existing* partition, assigned a role by the user in manual mode.
#[derive(Debug, Clone)]
pub struct Assignment {
    pub device: String,
    pub role: Role,
    /// When false the existing filesystem is kept. Only ever false for an ESP
    /// that a previous OS already created; [`Layout::validate`] enforces that.
    pub format: bool,
    pub size_bytes: u64,
    pub existing_fstype: Option<String>,
}

/// A brand new partition to append into a free-space gap, in manual mode.
///
/// Unlike [`NewPartition`], this carries an exact placement: `start_bytes`
/// pins it inside a specific gap the user picked, and `number` is the
/// partition number [`Plan::manual`] reserved for it up front — via
/// [`PartitionTable::next_free_numbers`] — so [`Plan::path_for`] can predict
/// its device node before `sfdisk` has created anything.
#[derive(Debug, Clone)]
pub struct ManualCreate {
    pub role: Role,
    pub number: u32,
    pub start_bytes: u64,
    pub size_bytes: u64,
}

/// An existing partition to remove before any new one is created.
#[derive(Debug, Clone)]
pub struct Deletion {
    pub device: String,
    pub number: u32,
}

/// A manual layout: some existing partitions kept (optionally reformatted),
/// some removed, and some new ones appended into the freed or already-free
/// space. Applying it never rewrites the whole table the way an automatic
/// plan does — see [`Plan::needs_partitioning`].
#[derive(Debug, Clone, Default)]
pub struct ManualLayout {
    pub keep: Vec<Assignment>,
    pub delete: Vec<Deletion>,
    pub create: Vec<ManualCreate>,
    /// The disk's logical sector size, needed to render `create` into an
    /// `sfdisk` script. Meaningless (and unused) when `create` is empty.
    sector_bytes: u64,
}

#[cfg(test)]
impl ManualLayout {
    /// A layout that only assigns roles to existing partitions — the shape
    /// every manual plan had before creation/deletion existed. Only built
    /// directly like this in tests; production code always goes through
    /// [`Plan::manual`].
    pub fn keep_only(keep: Vec<Assignment>) -> Self {
        Self { keep, delete: Vec::new(), create: Vec::new(), sector_bytes: 512 }
    }
}

#[derive(Debug, Clone)]
pub enum Layout {
    /// Wipe the disk and create a fresh GPT.
    Automatic(Vec<NewPartition>),
    /// Keep the existing partition table, reshaped by at most a few targeted
    /// deletions and appends.
    Manual(ManualLayout),
}

/// A disk plus the layout to apply to it.
#[derive(Debug, Clone)]
pub struct Plan {
    pub disk: String,
    pub layout: Layout,
}

impl Plan {
    /// The standard automatic layout on a fresh GPT: a firmware-specific boot
    /// partition, optional swap sized to RAM (capped at 8 GiB), and an ext4 root
    /// filling the rest.
    ///
    /// The first partition depends on how the machine booted. UEFI gets a
    /// 512 MiB ESP; BIOS gets a 1 MiB BIOS boot partition, without which
    /// `grub-install` cannot embed itself on a GPT disk. Passing the wrong
    /// `uefi` here yields a layout that installs cleanly but never boots, so the
    /// caller must take it from [`super::disk::is_uefi_boot`].
    ///
    /// Swap is skipped on disks under 40 GiB, where the space is better spent
    /// on the root filesystem — modern kernels cope fine with zram instead.
    pub fn automatic(disk: &Disk, ram_bytes: u64, uefi: bool) -> Result<Self> {
        if !disk.is_installable() {
            bail!(
                "{} kurulum için uygun değil (en az {} gerekli)",
                disk.path,
                format_bytes(MIN_DISK_BYTES)
            );
        }

        let boot = if uefi {
            NewPartition { role: Role::Esp, size_bytes: Some(ESP_BYTES) }
        } else {
            NewPartition { role: Role::BiosBoot, size_bytes: Some(BIOS_BOOT_BYTES) }
        };
        let mut partitions = vec![boot];

        if disk.size_bytes >= 40 * GIB {
            let swap = ram_bytes.clamp(GIB, 8 * GIB);
            partitions.push(NewPartition { role: Role::Swap, size_bytes: Some(swap) });
        }

        // Root last and unsized so sfdisk hands it every remaining sector.
        partitions.push(NewPartition { role: Role::Root, size_bytes: None });

        Ok(Self { disk: disk.path.clone(), layout: Layout::Automatic(partitions) })
    }

    /// Build a manual plan from the user's role assignments plus any
    /// deletions and new partitions, then validate it.
    ///
    /// `table` must be a snapshot of `disk`'s *current* partition table — it
    /// is what tells us `delete`'s partition numbers (for `sfdisk --delete`)
    /// and hands out fresh ones for `create` (for `sfdisk --append`), so a
    /// stale snapshot would produce a plan that deletes or creates the wrong
    /// partition.
    pub fn manual(
        disk: &Disk,
        table: &PartitionTable,
        keep: Vec<Assignment>,
        delete_devices: Vec<String>,
        create_requests: Vec<(Role, u64, u64)>,
        uefi: bool,
    ) -> Result<Self, PlanError> {
        // Refuse anything currently mounted: formatting or deleting a live
        // mount corrupts the running system, and lsblk already told us which
        // those are.
        for assignment in &keep {
            let mounted = disk
                .partitions
                .iter()
                .any(|p| p.path == assignment.device && p.mountpoint.is_some());
            if mounted {
                return Err(PlanError::DeviceMounted(assignment.device.clone()));
            }
        }
        for device in &delete_devices {
            let mounted =
                disk.partitions.iter().any(|p| p.path == *device && p.mountpoint.is_some());
            if mounted {
                return Err(PlanError::DeviceMounted(device.clone()));
            }
        }

        let mut delete = Vec::with_capacity(delete_devices.len());
        for device in delete_devices {
            let number =
                table.number_of(&device).ok_or_else(|| PlanError::UnknownDevice(device.clone()))?;
            delete.push(Deletion { device, number });
        }

        let freed: Vec<u32> = delete.iter().map(|d| d.number).collect();
        let numbers = table.next_free_numbers(&freed, create_requests.len());
        let create = create_requests
            .into_iter()
            .zip(numbers)
            .map(|((role, start_bytes, size_bytes), number)| ManualCreate {
                role,
                number,
                start_bytes,
                size_bytes,
            })
            .collect();

        let layout = Layout::Manual(ManualLayout {
            keep,
            delete,
            create,
            sector_bytes: table.sector_bytes(),
        });
        layout.validate(uefi)?;
        Ok(Self { disk: disk.path.clone(), layout })
    }

    /// True when a partitioning command must run before formatting. An
    /// automatic plan always rewrites the whole table; a manual plan only
    /// does when it deletes or creates something — plain role reassignment
    /// leaves the existing table untouched.
    pub fn needs_partitioning(&self) -> bool {
        match &self.layout {
            Layout::Automatic(_) => true,
            Layout::Manual(m) => !m.delete.is_empty() || !m.create.is_empty(),
        }
    }

    /// Partition numbers for `sfdisk --delete <disk> …` — empty unless this
    /// is a manual plan that removes something.
    pub fn manual_delete_numbers(&self) -> Vec<u32> {
        match &self.layout {
            Layout::Automatic(_) => Vec::new(),
            Layout::Manual(m) => m.delete.iter().map(|d| d.number).collect(),
        }
    }

    /// The `sfdisk --append` script for every partition this plan creates,
    /// targeting each explicit device node so the partition number sfdisk
    /// assigns matches the one already baked into [`Self::path_for`]. `None`
    /// when there is nothing to create.
    pub fn manual_create_script(&self) -> Option<String> {
        let Layout::Manual(m) = &self.layout else { return None };
        if m.create.is_empty() {
            return None;
        }

        let mut script = String::new();
        for c in &m.create {
            let node = self.partition_path(c.number as usize);
            let uuid = c.role.type_uuid();
            script.push_str(&format!(
                "{node} : start={}, size={}, type={uuid}\n",
                c.start_bytes / m.sector_bytes,
                c.size_bytes / m.sector_bytes,
            ));
        }
        Some(script)
    }

    /// True when this plan boots via UEFI, i.e. it has an ESP.
    ///
    /// The bootloader stage reads firmware from *here*, not from
    /// [`super::disk::is_uefi_boot`]: the plan was built for one firmware, and
    /// installing GRUB for the other produces an unbootable system. Deriving it
    /// from the plan's own partitions keeps the two decisions from drifting.
    pub fn is_uefi(&self) -> bool {
        self.path_for(Role::Esp).is_some()
    }

    /// Device node for the n-th partition of an automatic layout (1-based).
    ///
    /// NVMe and mmc devices insert a `p` separator (`/dev/nvme0n1p1`) while SATA
    /// and virtio do not (`/dev/sda1`). The rule is: append `p` when the node
    /// name already ends in a digit.
    pub fn partition_path(&self, index: usize) -> String {
        let ends_with_digit = self.disk.chars().last().is_some_and(|c| c.is_ascii_digit());
        if ends_with_digit {
            format!("{}p{}", self.disk, index)
        } else {
            format!("{}{}", self.disk, index)
        }
    }

    /// The device fulfilling `role`, if the plan has one.
    pub fn path_for(&self, role: Role) -> Option<String> {
        match &self.layout {
            Layout::Automatic(parts) => {
                parts.iter().position(|p| p.role == role).map(|i| self.partition_path(i + 1))
            }
            Layout::Manual(m) => m
                .keep
                .iter()
                .find(|a| a.role == role)
                .map(|a| a.device.clone())
                .or_else(|| {
                    m.create.iter().find(|c| c.role == role).map(|c| self.partition_path(c.number as usize))
                }),
        }
    }

    /// Devices that must be freshly formatted: every partition in automatic
    /// mode; in manual mode, the ticked existing partitions plus every newly
    /// created one, which is always unformatted by definition.
    ///
    /// The BIOS boot partition is never here — GRUB writes raw sectors into it,
    /// and a filesystem would be clobbered on the first `grub-install`.
    pub fn to_format(&self) -> Vec<(String, Role)> {
        match &self.layout {
            Layout::Automatic(parts) => parts
                .iter()
                .enumerate()
                .filter(|(_, p)| p.role.needs_filesystem())
                .map(|(i, p)| (self.partition_path(i + 1), p.role))
                .collect(),
            Layout::Manual(m) => {
                let mut out: Vec<(String, Role)> =
                    m.keep.iter().filter(|a| a.format).map(|a| (a.device.clone(), a.role)).collect();
                out.extend(
                    m.create.iter().map(|c| (self.partition_path(c.number as usize), c.role)),
                );
                out
            }
        }
    }

    /// Render an automatic plan as an `sfdisk` script, or `None` for a manual
    /// plan, which has no table to write.
    ///
    /// `sfdisk` applies the whole script atomically and is what `debian-
    /// installer` itself uses; driving `parted` interactively would be both
    /// slower and harder to verify.
    pub fn to_sfdisk_script(&self) -> Option<String> {
        let Layout::Automatic(partitions) = &self.layout else {
            return None;
        };

        let mut script = String::from("label: gpt\n");
        for part in partitions {
            let uuid = part.role.type_uuid();
            match part.size_bytes {
                Some(bytes) => script.push_str(&format!("size={}KiB, type={uuid}\n", bytes / 1024)),
                None => script.push_str(&format!("type={uuid}\n")),
            }
        }
        Some(script)
    }

    /// Human-readable summary for the confirmation screen.
    pub fn describe(&self) -> String {
        let mut out = format!("Hedef disk: {}\n\n", self.disk);

        match &self.layout {
            Layout::Automatic(partitions) => {
                out.push_str("Düzen: otomatik (disk tamamen silinecek)\n");
                for (i, part) in partitions.iter().enumerate() {
                    let size = part.size_bytes.map_or("kalan alan".into(), format_bytes);
                    out.push_str(&format!(
                        "  {}  {}  —  {size}  ({})\n",
                        self.partition_path(i + 1),
                        part.role.label(),
                        part.role.filesystem(),
                    ));
                }
            }
            Layout::Manual(m) => {
                out.push_str("Düzen: elle (yalnızca seçilen bölümler değişecek)\n");
                for a in &m.keep {
                    let action = if a.format {
                        format!("biçimlendirilecek → {}", a.role.filesystem())
                    } else {
                        "korunacak".to_string()
                    };
                    out.push_str(&format!(
                        "  {}  {}  —  {}  ({action})\n",
                        a.device,
                        a.role.label(),
                        format_bytes(a.size_bytes),
                    ));
                }
                for d in &m.delete {
                    out.push_str(&format!("  {}  silinecek\n", d.device));
                }
                for c in &m.create {
                    out.push_str(&format!(
                        "  {}  {}  —  {}  (yeni bölüm → {})\n",
                        self.partition_path(c.number as usize),
                        c.role.label(),
                        format_bytes(c.size_bytes),
                        c.role.filesystem(),
                    ));
                }
            }
        }
        out
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::backend::disk::PartitionInfo;

    /// A table containing exactly `partitions`, numbered in the order given.
    fn table_for(disk_size: u64, partitions: Vec<(&str, u64, u64)>) -> PartitionTable {
        PartitionTable::for_tests(MIB, disk_size, partitions)
    }

    /// Shared with [`super::rules`]'s tests. Mirrors `crate::wizard::assign`,
    /// which is where production code builds these.
    pub fn assign(
        device: &str,
        size_bytes: u64,
        existing_fstype: Option<&str>,
        role: Role,
        format: bool,
    ) -> Assignment {
        Assignment {
            device: device.into(),
            role,
            format,
            size_bytes,
            existing_fstype: existing_fstype.map(String::from),
        }
    }

    fn disk(path: &str, size: u64) -> Disk {
        Disk {
            path: path.into(),
            model: "Test".into(),
            size_bytes: size,
            removable: false,
            is_live_medium: false,
            partitions: Vec::new(),
        }
    }

    #[test]
    fn nvme_partitions_get_p_separator() {
        let plan = Plan::automatic(&disk("/dev/nvme0n1", 500 * GIB), 8 * GIB, true).unwrap();
        assert_eq!(plan.partition_path(1), "/dev/nvme0n1p1");

        let plan = Plan::automatic(&disk("/dev/sda", 500 * GIB), 8 * GIB, true).unwrap();
        assert_eq!(plan.partition_path(1), "/dev/sda1");
    }

    #[test]
    fn small_disks_skip_swap() {
        let plan = Plan::automatic(&disk("/dev/sda", 30 * GIB), 8 * GIB, true).unwrap();
        assert!(plan.path_for(Role::Swap).is_none());
        assert_eq!(plan.to_format().len(), 2);
    }

    #[test]
    fn swap_is_clamped_to_8gib() {
        let plan = Plan::automatic(&disk("/dev/sda", 500 * GIB), 64 * GIB, true).unwrap();
        let Layout::Automatic(parts) = &plan.layout else { panic!() };
        let swap = parts.iter().find(|p| p.role == Role::Swap).unwrap();
        assert_eq!(swap.size_bytes, Some(8 * GIB));
    }

    #[test]
    fn undersized_disk_is_rejected() {
        assert!(Plan::automatic(&disk("/dev/sda", 8 * GIB), 4 * GIB, true).is_err());
    }

    #[test]
    fn sfdisk_script_puts_unsized_root_last() {
        let plan = Plan::automatic(&disk("/dev/sda", 500 * GIB), 8 * GIB, true).unwrap();
        let script = plan.to_sfdisk_script().unwrap();
        assert!(script.starts_with("label: gpt\n"));
        let lines: Vec<_> = script.lines().skip(1).collect();
        assert!(lines[..lines.len() - 1].iter().all(|l| l.contains("size=")));
        assert!(!lines.last().unwrap().contains("size="));
    }

    #[test]
    fn automatic_plan_partitions_and_formats_everything() {
        let plan = Plan::automatic(&disk("/dev/sda", 500 * GIB), 8 * GIB, true).unwrap();
        assert!(plan.needs_partitioning());
        // ESP + swap + root, all formatted.
        assert_eq!(plan.to_format().len(), 3);
    }

    #[test]
    fn bios_layout_uses_an_unformatted_bios_boot_partition() {
        let plan = Plan::automatic(&disk("/dev/sda", 500 * GIB), 8 * GIB, false).unwrap();

        // No ESP on BIOS; a BIOS boot partition instead.
        assert!(plan.path_for(Role::Esp).is_none());
        assert_eq!(plan.path_for(Role::BiosBoot).as_deref(), Some("/dev/sda1"));

        // The BIOS boot partition must never be handed to mkfs — GRUB owns it.
        let formatted: Vec<Role> = plan.to_format().into_iter().map(|(_, r)| r).collect();
        assert!(!formatted.contains(&Role::BiosBoot));
        assert!(formatted.contains(&Role::Root));

        // It is a 1 MiB sized partition in the sfdisk script, and only root is
        // left unsized.
        let script = plan.to_sfdisk_script().unwrap();
        assert!(script.contains("size=1024KiB"));
    }

    fn valid_manual() -> Vec<Assignment> {
        vec![
            assign("/dev/sda1", 512 * MIB, Some("vfat"), Role::Esp, false),
            assign("/dev/sda2", 100 * GIB, Some("ext4"), Role::Root, true),
        ]
    }

    fn valid_manual_table() -> PartitionTable {
        table_for(500 * GIB, vec![("/dev/sda1", MIB, 512 * MIB), ("/dev/sda2", MIB + 512 * MIB, 100 * GIB)])
    }

    #[test]
    fn manual_plan_keeps_table_and_formats_only_ticked() {
        let table = valid_manual_table();
        let plan =
            Plan::manual(&disk("/dev/sda", 500 * GIB), &table, valid_manual(), Vec::new(), Vec::new(), true)
                .unwrap();
        assert!(!plan.needs_partitioning());
        assert!(plan.to_sfdisk_script().is_none());
        assert!(plan.manual_create_script().is_none());
        // The ESP is preserved, so only root is formatted.
        assert_eq!(plan.to_format(), vec![("/dev/sda2".to_string(), Role::Root)]);
        assert_eq!(plan.path_for(Role::Esp).as_deref(), Some("/dev/sda1"));
    }

    #[test]
    fn mounted_partitions_are_refused() {
        let mut target = disk("/dev/sda", 500 * GIB);
        target.partitions = vec![PartitionInfo {
            path: "/dev/sda2".into(),
            size_bytes: 100 * GIB,
            fstype: Some("ext4".into()),
            label: None,
            mountpoint: Some("/home".into()),
        }];
        assert_eq!(
            Plan::manual(&target, &valid_manual_table(), valid_manual(), Vec::new(), Vec::new(), true)
                .unwrap_err(),
            PlanError::DeviceMounted("/dev/sda2".into())
        );
    }

    #[test]
    fn mounted_deletions_are_refused() {
        let mut target = disk("/dev/sda", 500 * GIB);
        target.partitions = vec![PartitionInfo {
            path: "/dev/sda1".into(),
            size_bytes: 512 * MIB,
            fstype: Some("vfat".into()),
            label: None,
            mountpoint: Some("/boot/efi".into()),
        }];
        let root = vec![assign("/dev/sda2", 100 * GIB, Some("ext4"), Role::Root, true)];
        assert_eq!(
            Plan::manual(&target, &valid_manual_table(), root, vec!["/dev/sda1".into()], Vec::new(), true)
                .unwrap_err(),
            PlanError::DeviceMounted("/dev/sda1".into())
        );
    }

    /// A manual plan that deletes the ESP and creates a fresh, larger one in
    /// its place, alongside a kept root — the scenario `kur`'s manual
    /// partitioning UI exists for.
    #[test]
    fn manual_plan_creates_a_new_partition_in_freed_space() {
        let target = disk("/dev/sda", 500 * GIB);
        let table = valid_manual_table();
        let keep = vec![assign("/dev/sda2", 100 * GIB, Some("ext4"), Role::Root, true)];
        let create = vec![(Role::Esp, MIB, 512 * MIB)];

        let plan = Plan::manual(&target, &table, keep, vec!["/dev/sda1".into()], create, true).unwrap();

        assert!(plan.needs_partitioning());
        assert_eq!(plan.manual_delete_numbers(), vec![1]);
        // The freed number 1 is handed straight back to the new partition.
        assert_eq!(plan.path_for(Role::Esp).as_deref(), Some("/dev/sda1"));

        let script = plan.manual_create_script().unwrap();
        assert!(script.contains("/dev/sda1 :"));
        assert!(script.contains(&format!("start={}", MIB / 512)));
        assert!(script.contains(&format!("size={}", 512 * MIB / 512)));

        // The new ESP is unconditionally formatted; the kept root only if ticked.
        let formatted: Vec<Role> = plan.to_format().into_iter().map(|(_, r)| r).collect();
        assert!(formatted.contains(&Role::Esp));
        assert!(formatted.contains(&Role::Root));
    }

    #[test]
    fn describe_mentions_every_partition() {
        let plan = Plan::automatic(&disk("/dev/sda", 500 * GIB), 8 * GIB, true).unwrap();
        let text = plan.describe();
        for role in [Role::Esp, Role::Swap, Role::Root] {
            assert!(text.contains(role.label()), "{} eksik", role.label());
        }
    }
}
