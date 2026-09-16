// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Block-device discovery.
//!
//! Enumeration goes through `lsblk -J`, whose JSON output is a stable, tested
//! interface — far safer than scraping `fdisk -l`. Turning a disk into a
//! partition layout is [`super::plan`]'s job; this module only reports what
//! exists.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::cmd;

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;

/// Smallest disk we will install onto. Debian's base system plus a desktop
/// needs roughly 12 GiB; below this the install fails halfway through, which is
/// far worse than refusing up front.
pub const MIN_DISK_BYTES: u64 = 20 * GIB;

/// An existing partition on a disk. Only meaningful in manual mode, where the
/// user assigns roles to partitions instead of wiping the whole device.
#[derive(Debug, Clone)]
pub struct PartitionInfo {
    pub path: String,
    pub size_bytes: u64,
    /// Existing filesystem, e.g. `ext4`, `vfat`. `None` on an unformatted one.
    pub fstype: Option<String>,
    pub label: Option<String>,
    pub mountpoint: Option<String>,
}

impl PartitionInfo {
    pub fn size_text(&self) -> String {
        format_bytes(self.size_bytes)
    }

    /// A one-line description for the manual partitioning list.
    pub fn describe(&self) -> String {
        let fs = self.fstype.as_deref().unwrap_or("biçimlendirilmemiş");
        match &self.label {
            Some(label) => format!("{} — {} · {fs} · «{label}»", self.path, self.size_text()),
            None => format!("{} — {} · {fs}", self.path, self.size_text()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Disk {
    /// Kernel device node, e.g. `/dev/nvme0n1`.
    pub path: String,
    pub model: String,
    pub size_bytes: u64,
    pub removable: bool,
    /// True when one of this disk's partitions is mounted at `/` or
    /// `/run/live/medium`. Installing onto it would destroy the running system.
    pub is_live_medium: bool,
    pub partitions: Vec<PartitionInfo>,
}

impl Disk {
    /// Whether this disk is a legal install target.
    pub fn is_installable(&self) -> bool {
        !self.is_live_medium && self.size_bytes >= MIN_DISK_BYTES
    }

    pub fn size_text(&self) -> String {
        format_bytes(self.size_bytes)
    }
}

// ---- lsblk JSON -------------------------------------------------------------

#[derive(Deserialize)]
struct LsblkOutput {
    blockdevices: Vec<LsblkNode>,
}

#[derive(Deserialize)]
struct LsblkNode {
    path: String,
    #[serde(default)]
    model: Option<String>,
    /// `-b` makes this a plain byte count rather than "931.5G".
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    rm: Option<bool>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    fstype: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    mountpoint: Option<String>,
    #[serde(default)]
    children: Vec<LsblkNode>,
}

impl LsblkNode {
    /// True if this node or any descendant is mounted somewhere that indicates
    /// the live system is running from it.
    fn hosts_live_system(&self) -> bool {
        let mounted_at_live = self
            .mountpoint
            .as_deref()
            .is_some_and(|m| m == "/" || m.starts_with("/run/live"));

        mounted_at_live || self.children.iter().any(LsblkNode::hosts_live_system)
    }

    /// Flatten this node's descendants into partitions, following LVM/LUKS
    /// nesting so an encrypted volume's children are not silently dropped.
    fn collect_partitions(&self, out: &mut Vec<PartitionInfo>) {
        for child in &self.children {
            if child.kind == "part" {
                out.push(PartitionInfo {
                    path: child.path.clone(),
                    size_bytes: child.size.unwrap_or(0),
                    fstype: child.fstype.clone(),
                    label: child.label.clone(),
                    mountpoint: child.mountpoint.clone(),
                });
            }
            child.collect_partitions(out);
        }
    }
}

/// Enumerate whole disks (not partitions, not loop/ram devices).
pub fn list_disks() -> Result<Vec<Disk>> {
    // NAME is not used, but it must be requested: lsblk nests partitions under
    // `children` only when the NAME column is present, and emits a flat list
    // otherwise. Without it every disk looks unmounted — including the one the
    // live system is running from. `-b` makes SIZE a byte count, not "931.5G".
    let json = cmd::capture(
        "lsblk",
        &["-J", "-b", "-o", "NAME,PATH,MODEL,SIZE,RM,TYPE,FSTYPE,LABEL,MOUNTPOINT"],
    )
    .context("lsblk çalıştırılamadı — util-linux kurulu mu?")?;

    parse_lsblk(&json)
}

/// Split out from [`list_disks`] so the nesting behaviour above is testable
/// against a captured fixture rather than the developer's own hardware.
fn parse_lsblk(json: &str) -> Result<Vec<Disk>> {
    let parsed: LsblkOutput = serde_json::from_str(json).context("lsblk çıktısı ayrıştırılamadı")?;

    Ok(parsed
        .blockdevices
        .into_iter()
        // `type == "disk"` filters out loop devices (the squashfs of the live
        // image), device-mapper nodes and CD-ROMs in one go.
        .filter(|node| node.kind == "disk")
        .map(|node| {
            let mut partitions = Vec::new();
            node.collect_partitions(&mut partitions);

            Disk {
                is_live_medium: node.hosts_live_system(),
                partitions,
                path: node.path,
                model: node
                    .model
                    .map(|m| m.trim().to_string())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| "Bilinmeyen aygıt".into()),
                size_bytes: node.size.unwrap_or(0),
                removable: node.rm.unwrap_or(false),
            }
        })
        .collect())
}

/// Total physical RAM, used to size the swap partition.
///
/// Returns 0 when `/proc/meminfo` is unreadable; `Plan::automatic` clamps that
/// up to its 1 GiB floor, so a failure here degrades rather than breaks.
pub fn total_ram_bytes() -> u64 {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().strip_suffix(" kB"))
        .and_then(|kb| kb.trim().parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

/// True when the machine booted via UEFI rather than legacy BIOS.
///
/// The kernel only creates `/sys/firmware/efi` in the former case. This decides
/// whether an ESP is required and which GRUB flavour is installed.
pub fn is_uefi_boot() -> bool {
    std::path::Path::new("/sys/firmware/efi").exists()
}

/// Force everything currently mounted or swapped-on under `disk`'s existing
/// partitions to let go, right before an automatic plan wipes it.
///
/// The wizard reads the disk once, at selection time, and the automatic
/// install may only start minutes later — long enough for the desktop's own
/// automount (gvfs/udisks2) to have silently mounted a partition that was
/// free when the user picked the disk (an old NTFS partition is exactly the
/// kind of thing autofs mounts on sight). `sfdisk` then refuses outright:
/// "This disk is currently in use — repartitioning is probably a bad idea."
/// Best-effort and infallible by design, matching [`super::stages::unmount_all`]:
/// a disk with nothing mounted has nothing to release, which is not an error.
pub fn release(disk_path: &str, cancel: &cmd::Cancel) {
    let Ok(json) = cmd::capture("lsblk", &["-J", "-o", "PATH,MOUNTPOINT,FSTYPE", disk_path]) else {
        return;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&json) else {
        return;
    };
    if let Some(devices) = root["blockdevices"].as_array() {
        release_tree(devices, cancel);
    }
}

fn release_tree(devices: &[serde_json::Value], cancel: &cmd::Cancel) {
    for device in devices {
        if let Some(mountpoint) = device["mountpoint"].as_str() {
            let _ = cmd::run("umount", &["-R", mountpoint], cancel);
        }
        if device["fstype"].as_str() == Some("swap") {
            if let Some(path) = device["path"].as_str() {
                let _ = cmd::run("swapoff", &[path], cancel);
            }
        }
        if let Some(children) = device["children"].as_array() {
            release_tree(children, cancel);
        }
    }
}

/// A gap between (or around) existing partitions, big enough to be worth
/// offering the user in manual mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeSpace {
    pub start_bytes: u64,
    pub size_bytes: u64,
}

impl FreeSpace {
    pub fn size_text(&self) -> String {
        format_bytes(self.size_bytes)
    }
}

/// Gaps smaller than this are alignment slack, not usable free space.
const MIN_FREE_REGION_BYTES: u64 = MIB;

/// A disk's partition table as `sfdisk` sees it: exact sector offsets, which
/// `lsblk` never reports. Only [`super::plan`]'s manual layout needs this — it
/// is the one place that must know *where* a partition sits, to compute free
/// space and to pick partition numbers for new ones.
#[derive(Debug, Clone)]
pub struct PartitionTable {
    sector_bytes: u64,
    first_usable_bytes: u64,
    last_usable_bytes: u64,
    partitions: Vec<TablePartition>,
}

#[derive(Debug, Clone)]
struct TablePartition {
    node: String,
    number: u32,
    start_bytes: u64,
    size_bytes: u64,
}

#[derive(Deserialize)]
struct SfdiskOutput {
    partitiontable: Option<SfdiskTable>,
}

#[derive(Deserialize)]
struct SfdiskTable {
    #[serde(default = "default_sector")]
    sectorsize: u64,
    /// GPT-only: `sfdisk -J` on an MBR ("dos") label omits both fields
    /// entirely rather than emitting a zero, so these must default to
    /// `None`, not `0` — defaulting to `0` would collapse `last_usable_bytes`
    /// to a single sector and hide the disk's free space from manual mode.
    firstlba: Option<u64>,
    lastlba: Option<u64>,
    #[serde(default)]
    partitions: Vec<SfdiskPartition>,
}

fn default_sector() -> u64 {
    512
}

#[derive(Deserialize)]
struct SfdiskPartition {
    node: String,
    start: u64,
    size: u64,
}

impl PartitionTable {
    /// Read `disk_path`'s live partition table via `sfdisk -J`.
    ///
    /// A disk with no partition table yet (a fresh drive) makes `sfdisk` exit
    /// non-zero rather than emit empty JSON, so that case is treated as an
    /// empty table spanning the whole disk instead of an error — it is the
    /// normal starting point for manual partitioning, not a failure.
    pub fn read(disk_path: &str, disk_size_bytes: u64) -> Result<Self> {
        let empty = || Self::empty(disk_size_bytes);

        let Ok(json) = cmd::capture("sfdisk", &["-J", disk_path]) else {
            return Ok(empty());
        };
        Self::parse(&json, disk_size_bytes)
    }

    /// Split out from [`Self::read`] so it can be tested against a captured
    /// `sfdisk -J` transcript instead of a real block device.
    fn parse(json: &str, disk_size_bytes: u64) -> Result<Self> {
        let parsed: SfdiskOutput = serde_json::from_str(json).context("sfdisk çıktısı ayrıştırılamadı")?;
        let Some(table) = parsed.partitiontable else {
            return Ok(Self::empty(disk_size_bytes));
        };

        let sector = table.sectorsize.max(1);
        let partitions = table
            .partitions
            .into_iter()
            .enumerate()
            .map(|(i, p)| TablePartition {
                node: p.node,
                number: i as u32 + 1,
                start_bytes: p.start * sector,
                size_bytes: p.size * sector,
            })
            .collect();

        Ok(Self {
            sector_bytes: sector,
            // MBR ("dos") labels report neither field (see `SfdiskTable`);
            // fall back to the same margin/whole-disk bounds `Self::empty`
            // uses for a disk with no partition table at all.
            first_usable_bytes: table.firstlba.map_or(MIB, |lba| lba * sector),
            last_usable_bytes: table
                .lastlba
                .map_or(disk_size_bytes, |lba| (lba + 1).saturating_mul(sector))
                .min(disk_size_bytes),
            partitions,
        })
    }

    /// A disk with no partition table at all: everything past a 1 MiB
    /// alignment margin — the same margin `sfdisk` itself reserves for the
    /// GPT header — counts as free.
    pub(crate) fn empty(disk_size_bytes: u64) -> Self {
        Self {
            sector_bytes: 512,
            first_usable_bytes: MIB,
            last_usable_bytes: disk_size_bytes,
            partitions: Vec::new(),
        }
    }

    pub fn sector_bytes(&self) -> u64 {
        self.sector_bytes
    }

    /// The partition number `sfdisk --delete`/`--append` would need for
    /// `device`, i.e. its 1-based position in the table.
    pub fn number_of(&self, device: &str) -> Option<u32> {
        self.partitions.iter().find(|p| p.node == device).map(|p| p.number)
    }

    /// The lowest partition numbers not currently in use, one per requested
    /// slot — the same numbers `sfdisk --append` would hand out on its own,
    /// computed up front so a [`super::plan::Plan`] can predict device paths
    /// for partitions it hasn't created yet. `freed` additionally releases
    /// numbers belonging to partitions that will be deleted first, since a
    /// deletion frees its number before any append runs.
    pub fn next_free_numbers(&self, freed: &[u32], count: usize) -> Vec<u32> {
        let mut used: std::collections::HashSet<u32> = self.partitions.iter().map(|p| p.number).collect();
        for number in freed {
            used.remove(number);
        }
        (1..).filter(|n| !used.contains(n)).take(count).collect()
    }

    /// Existing partitions in on-disk order — device, start and size — for
    /// rendering the manual-partitioning bar, which needs real positions that
    /// `lsblk` (the source for [`PartitionInfo`]) does not report.
    pub fn ordered_partitions(&self) -> Vec<(&str, u64, u64)> {
        let mut sorted: Vec<&TablePartition> = self.partitions.iter().collect();
        sorted.sort_by_key(|p| p.start_bytes);
        sorted.into_iter().map(|p| (p.node.as_str(), p.start_bytes, p.size_bytes)).collect()
    }

    /// Gaps of at least [`MIN_FREE_REGION_BYTES`] between, before or after the
    /// existing partitions, in disk order.
    pub fn free_space(&self) -> Vec<FreeSpace> {
        let mut sorted: Vec<&TablePartition> = self.partitions.iter().collect();
        sorted.sort_by_key(|p| p.start_bytes);

        let mut regions = Vec::new();
        let mut cursor = self.first_usable_bytes;
        for part in sorted {
            if part.start_bytes > cursor {
                push_region(&mut regions, cursor, part.start_bytes - cursor);
            }
            cursor = cursor.max(part.start_bytes + part.size_bytes);
        }
        if self.last_usable_bytes > cursor {
            push_region(&mut regions, cursor, self.last_usable_bytes - cursor);
        }
        regions
    }
}

fn push_region(regions: &mut Vec<FreeSpace>, start_bytes: u64, size_bytes: u64) {
    if size_bytes >= MIN_FREE_REGION_BYTES {
        regions.push(FreeSpace { start_bytes, size_bytes });
    }
}

#[cfg(test)]
impl PartitionTable {
    /// Build a table directly from `(device, start_bytes, size_bytes)`
    /// triples, bypassing `sfdisk -J` JSON. Partition numbers are assigned in
    /// the order given, 1-based — for [`super::plan`]'s tests, which need a
    /// table with existing partitions at known numbers.
    pub(crate) fn for_tests(
        first_usable_bytes: u64,
        last_usable_bytes: u64,
        partitions: Vec<(&str, u64, u64)>,
    ) -> Self {
        Self {
            sector_bytes: 512,
            first_usable_bytes,
            last_usable_bytes,
            partitions: partitions
                .into_iter()
                .enumerate()
                .map(|(i, (node, start_bytes, size_bytes))| TablePartition {
                    node: node.to_string(),
                    number: i as u32 + 1,
                    start_bytes,
                    size_bytes,
                })
                .collect(),
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from `lsblk -J -b -o NAME,PATH,MODEL,SIZE,RM,TYPE,FSTYPE,LABEL,MOUNTPOINT`
    /// (util-linux 2.41): one empty card reader, one NVMe holding the root fs.
    const LSBLK_FIXTURE: &str = r#"{
       "blockdevices": [
          {"name":"sda","path":"/dev/sda","model":"STORAGE DEVICE","size":0,"rm":true,"type":"disk","fstype":null,"label":null,"mountpoint":null},
          {"name":"nvme0n1","path":"/dev/nvme0n1","model":"UMIS RPIRJ256VME2MWD","size":256060514304,"rm":false,"type":"disk","fstype":null,"label":null,"mountpoint":null,
           "children":[
              {"name":"nvme0n1p1","path":"/dev/nvme0n1p1","model":null,"size":1023410176,"rm":false,"type":"part","fstype":"vfat","label":"ESP","mountpoint":"/boot/efi"},
              {"name":"nvme0n1p2","path":"/dev/nvme0n1p2","model":null,"size":248664555520,"rm":false,"type":"part","fstype":"ext4","label":null,"mountpoint":"/"}
           ]}
       ]
    }"#;

    #[test]
    fn parses_real_lsblk_output() {
        let disks = parse_lsblk(LSBLK_FIXTURE).unwrap();
        // Partitions are `type: part` and must not appear as install targets.
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].path, "/dev/sda");
        assert_eq!(disks[1].model, "UMIS RPIRJ256VME2MWD");
        assert!(disks[0].removable);
    }

    /// The bug this guards against: without the NAME column lsblk emits a flat
    /// list, `children` is empty, the root mountpoint is never seen, and the
    /// installer offers to wipe the running system.
    #[test]
    fn disk_holding_root_is_flagged_as_live_medium() {
        let disks = parse_lsblk(LSBLK_FIXTURE).unwrap();
        let nvme = disks.iter().find(|d| d.path == "/dev/nvme0n1").unwrap();
        assert!(nvme.is_live_medium, "kök dosya sistemini taşıyan disk korunmalı");
        assert!(!nvme.is_installable());
    }

    #[test]
    fn partitions_are_collected_with_their_filesystems() {
        let disks = parse_lsblk(LSBLK_FIXTURE).unwrap();
        let nvme = disks.iter().find(|d| d.path == "/dev/nvme0n1").unwrap();
        assert_eq!(nvme.partitions.len(), 2);
        assert_eq!(nvme.partitions[0].fstype.as_deref(), Some("vfat"));
        assert_eq!(nvme.partitions[0].label.as_deref(), Some("ESP"));
        assert_eq!(nvme.partitions[1].mountpoint.as_deref(), Some("/"));
        assert!(nvme.partitions[1].describe().contains("ext4"));
    }

    /// LUKS and LVM nest a second level under the partition. Those inner nodes
    /// are not `type: part`, but the partition above them still must be listed.
    #[test]
    fn nested_containers_do_not_hide_their_partition() {
        let json = r#"{"blockdevices":[
            {"name":"vda","path":"/dev/vda","size":107374182400,"type":"disk","fstype":null,"label":null,"mountpoint":null,
             "children":[
               {"name":"vda1","path":"/dev/vda1","size":107374182400,"type":"part","fstype":"crypto_LUKS","label":null,"mountpoint":null,
                "children":[
                  {"name":"root","path":"/dev/mapper/root","size":107000000000,"type":"crypt","fstype":"ext4","label":null,"mountpoint":"/"}
                ]}
             ]}]}"#;
        let disks = parse_lsblk(json).unwrap();
        assert_eq!(disks[0].partitions.len(), 1);
        assert_eq!(disks[0].partitions[0].path, "/dev/vda1");
        // The `/` mount lives on the crypt node, two levels down.
        assert!(disks[0].is_live_medium);
    }

    #[test]
    fn empty_card_reader_is_not_installable() {
        let disks = parse_lsblk(LSBLK_FIXTURE).unwrap();
        let sda = disks.iter().find(|d| d.path == "/dev/sda").unwrap();
        // 0 bytes: a reader with no card in it.
        assert!(!sda.is_installable());
    }

    #[test]
    fn missing_model_gets_a_placeholder() {
        let json = r#"{"blockdevices":[
            {"name":"vda","path":"/dev/vda","model":null,"size":107374182400,"rm":false,"type":"disk","fstype":null,"label":null,"mountpoint":null}]}"#;
        let disks = parse_lsblk(json).unwrap();
        assert_eq!(disks[0].model, "Bilinmeyen aygıt");
        assert!(disks[0].is_installable());
    }

    /// Captured via `sfdisk -J` against a 500 MiB GPT image with a 100 MiB ESP
    /// followed by a 48 MiB partition and free space to the end of the disk.
    const SFDISK_FIXTURE: &str = r#"{
       "partitiontable": {
          "label": "gpt",
          "device": "/dev/sda",
          "unit": "sectors",
          "firstlba": 2048,
          "lastlba": 1023966,
          "sectorsize": 512,
          "partitions": [
             {"node": "/dev/sda1", "start": 2048, "size": 204800, "type": "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"},
             {"node": "/dev/sda2", "start": 206848, "size": 100000, "type": "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709"}
          ]
       }
    }"#;

    #[test]
    fn sfdisk_table_reports_partition_numbers() {
        let table = PartitionTable::parse(SFDISK_FIXTURE, 524288000).unwrap();
        assert_eq!(table.number_of("/dev/sda1"), Some(1));
        assert_eq!(table.number_of("/dev/sda2"), Some(2));
        assert_eq!(table.number_of("/dev/sda9"), None);
    }

    #[test]
    fn sfdisk_table_finds_the_trailing_gap() {
        let table = PartitionTable::parse(SFDISK_FIXTURE, 524288000).unwrap();
        let free = table.free_space();
        assert_eq!(free.len(), 1, "iki bölüm birbirine bitişik, sadece sondaki boşluk kalmalı");
        let last_usable = (1023966 + 1) * 512; // firstlba/lastlba come from the fixture
        assert_eq!(free[0].start_bytes, 206848 * 512 + 100000 * 512);
        assert_eq!(free[0].size_bytes, last_usable - free[0].start_bytes);
    }

    #[test]
    fn sfdisk_table_finds_a_gap_between_partitions() {
        // Same fixture, but the second partition starts 50 MiB after the first
        // ends, leaving a gap `free_space` must report.
        let json = SFDISK_FIXTURE.replace("\"start\": 206848", "\"start\": 309248");
        let table = PartitionTable::parse(&json, 524288000).unwrap();
        let free = table.free_space();
        assert_eq!(free.len(), 2, "aradaki boşluk ve sondaki boşluk");
        assert_eq!(free[0].start_bytes, 204800 * 512 + 2048 * 512);
        assert_eq!(free[0].size_bytes, (309248 - 206848) * 512);
    }

    #[test]
    fn sfdisk_table_skips_slack_smaller_than_one_mib() {
        // A 1000-sector gap (~512 KiB) after sda1 ends (at sector 206848) is
        // alignment slack, not usable space.
        let json = SFDISK_FIXTURE.replace("\"start\": 206848", "\"start\": 207848");
        let last_usable_sector = 207848 + 100000;
        let table = PartitionTable::parse(&json, last_usable_sector * 512).unwrap();
        assert!(table.free_space().is_empty());
    }

    /// Captured shape of `sfdisk -J` against a real MBR ("dos") disk: unlike
    /// GPT, it reports neither `firstlba` nor `lastlba` at all. A used Windows
    /// disk with one NTFS partition and the rest of the drive free is exactly
    /// the case a manual-mode user hits first.
    #[test]
    fn mbr_table_without_firstlba_lastlba_still_finds_the_trailing_gap() {
        let json = r#"{
           "partitiontable": {
              "label": "dos",
              "device": "/dev/sdb",
              "unit": "sectors",
              "sectorsize": 512,
              "partitions": [
                 {"node": "/dev/sdb1", "start": 2048, "size": 204800, "type": "7"}
              ]
           }
        }"#;
        let disk_size = 524288000_u64;
        let table = PartitionTable::parse(json, disk_size).unwrap();
        let free = table.free_space();

        assert_eq!(free.len(), 1, "NTFS bölümünden sonraki tüm alan boş görünmeli");
        assert_eq!(free[0].start_bytes, (2048 + 204800) * 512);
        assert_eq!(free[0].size_bytes, disk_size - free[0].start_bytes);
    }

    #[test]
    fn missing_partition_table_is_treated_as_one_big_free_region() {
        let table = PartitionTable::parse(r#"{"partitiontable": null}"#, 100 * GIB).unwrap();
        let free = table.free_space();
        assert_eq!(free.len(), 1);
        assert_eq!(free[0].start_bytes, MIB);
        assert_eq!(free[0].size_bytes, 100 * GIB - MIB);
    }

    #[test]
    fn next_free_numbers_fills_gaps_before_extending() {
        let table = PartitionTable::parse(SFDISK_FIXTURE, 524288000).unwrap();
        // sda1 and sda2 exist; the next two free slots are 3 and 4.
        assert_eq!(table.next_free_numbers(&[], 2), vec![3, 4]);
    }

    #[test]
    fn next_free_numbers_reuses_a_freed_number() {
        let table = PartitionTable::parse(SFDISK_FIXTURE, 524288000).unwrap();
        // sda2 (number 2) is about to be deleted, so it should be handed back
        // out before sda3.
        assert_eq!(table.next_free_numbers(&[2], 2), vec![2, 3]);
    }
}
