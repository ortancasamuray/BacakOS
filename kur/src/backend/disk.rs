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
}
