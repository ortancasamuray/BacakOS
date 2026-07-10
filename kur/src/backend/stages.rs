// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! The individual installation stages, in the order [`super::install::STAGES`]
//! runs them. Each is a plain `fn` so the stage table can be a `const`.
//!
//! Every stage is expected to be *restartable from a wiped disk*: on failure the
//! engine unmounts and stops, and the user starts over. None of them try to be
//! idempotent against a half-built target.

use anyhow::{Context, Result};

use super::cmd::{self, Cancel};
use super::install::{Config, Reporter, MIRROR, SUITE, TARGET};
use super::plan::{Plan, Role};
use super::timezone;

/// Packages `debootstrap` does not install but the later stages depend on.
///
/// `locales` provides `locale-gen`, which [`configure`] runs; `sudo` makes the
/// `sudo` group [`account`] adds the user to actually mean something. Both are
/// Priority: standard, so the base system omits them. Installing them here
/// rather than later keeps the chroot's first `apt-get` run for the kernel.
const EXTRA_BASE_PACKAGES: &str = "locales,sudo,tzdata,ca-certificates";

/// Where a live image stages the locally-built BacakOS packages.
const BACAKOS_DEB_DIR: &str = "/usr/share/bacakos/debs";

pub fn partition(config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    if !config.plan.needs_partitioning() {
        // Manual layout: the user's existing partition table stays untouched.
        on(1.0, "Elle bölümleme: bölüm tablosu korunuyor.");
        return Ok(());
    }

    let script = config
        .plan
        .to_sfdisk_script()
        .context("otomatik plan bir sfdisk betiği üretmedi")?;
    on(0.0, &format!("sfdisk {} <<EOF\n{script}EOF", config.plan.disk));

    // `--wipe always` clears stale filesystem signatures that would otherwise
    // make blkid report two filesystems on one partition.
    cmd::run_with_stdin("sfdisk", &["--wipe", "always", &config.plan.disk], &script, cancel)?;

    // Give udev time to create the new /dev/…p1 nodes before mkfs looks for them.
    cmd::run("udevadm", &["settle"], cancel)?;
    on(1.0, "");
    Ok(())
}

pub fn format(config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    let targets = config.plan.to_format();
    let total = targets.len() as f32;

    for (index, (device, role)) in targets.iter().enumerate() {
        match role {
            Role::Esp => cmd::run("mkfs.vfat", &["-F", "32", "-n", "ESP", device], cancel)?,
            Role::Root => cmd::run("mkfs.ext4", &["-F", "-L", "bacakos", device], cancel)?,
            Role::Swap => cmd::run("mkswap", &["-L", "swap", device], cancel)?,
            // `to_format` never yields this; GRUB owns its raw sectors.
            Role::BiosBoot => continue,
        }
        on((index as f32 + 1.0) / total, &format!("{device}: {} hazır", role.filesystem()));
    }
    Ok(())
}

pub fn mount(config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    let root = config.plan.path_for(Role::Root).context("kök bölüm planda yok")?;

    mkdir_p(TARGET)?;
    cmd::run("mount", &[&root, TARGET], cancel)?;

    // A BIOS install has no ESP, and `path_for` returns None for it.
    if let Some(esp) = config.plan.path_for(Role::Esp) {
        let boot_efi = format!("{TARGET}/boot/efi");
        mkdir_p(&boot_efi)?;
        cmd::run("mount", &[&esp, &boot_efi], cancel)?;
    }

    on(1.0, "");
    Ok(())
}

/// The long one. `debootstrap` prints a line per package it unpacks, so we count
/// them against the known package total to synthesise a fraction.
pub fn debootstrap(_config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    // `debootstrap --print-debs` would give an exact count but costs a full
    // dependency solve up front. The base system is ~320 packages and varies
    // little between Debian point releases; a stale estimate only skews the
    // bar's speed, never its endpoint (we force 1.0 below).
    const ESTIMATED_PACKAGES: f32 = 320.0;
    let mut unpacked = 0.0_f32;

    let include = format!("--include={EXTRA_BASE_PACKAGES}");
    cmd::run_streaming(
        "debootstrap",
        &[
            "--arch=amd64",
            "--components=main,contrib,non-free-firmware",
            &include,
            SUITE,
            TARGET,
            MIRROR,
        ],
        cancel,
        |line| {
            // debootstrap's progress lines look like "I: Unpacking libc6..."
            if line.contains("Unpacking ") || line.contains("Extracting ") {
                unpacked += 1.0;
            }
            on((unpacked / ESTIMATED_PACKAGES).min(0.99), line);
        },
    )?;

    on(1.0, "");
    Ok(())
}

pub fn configure(config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    // Bind the kernel filesystems so chroot'd maintainer scripts work.
    //
    // `--rbind`, not `--bind`: /dev and /sys have submounts (`/dev/pts`,
    // `/sys/firmware/efi/efivars`) that a plain bind leaves behind. Without
    // efivars visible in the chroot, `grub-install` on UEFI cannot write the
    // NVRAM boot entry and the machine will not find its bootloader. `--rbind`
    // pulls the whole subtree in; `--make-rslave` stops the installer's mount
    // events from propagating back onto the live system's mounts.
    for (source, target) in [("/dev", "dev"), ("/proc", "proc"), ("/sys", "sys")] {
        let dest = format!("{TARGET}/{target}");
        mkdir_p(&dest)?;
        cmd::run("mount", &["--rbind", source, &dest], cancel)?;
        cmd::run("mount", &["--make-rslave", &dest], cancel)?;
    }
    on(0.2, "");

    let hostname = &config.account.hostname;
    write_file(&format!("{TARGET}/etc/hostname"), &format!("{hostname}\n"))?;
    write_file(
        &format!("{TARGET}/etc/hosts"),
        &format!("127.0.0.1\tlocalhost\n127.0.1.1\t{hostname}\n::1\tlocalhost ip6-localhost ip6-loopback\n"),
    )?;
    write_file(&format!("{TARGET}/etc/fstab"), &render_fstab(&config.plan)?)?;
    on(0.4, "");

    // Locale: enable the chosen line, then regenerate. `locales` came in via
    // debootstrap's --include, so `locale-gen` exists.
    write_file(&format!("{TARGET}/etc/locale.gen"), &format!("{} UTF-8\n", config.locale))?;
    write_file(&format!("{TARGET}/etc/default/locale"), &format!("LANG={}\n", config.locale))?;
    cmd::chroot(TARGET, "locale-gen", &[], cancel)?;
    on(0.7, "");

    // /etc/timezone and /etc/localtime must agree, or tzdata's maintainer
    // script silently reverts the zone on the next upgrade.
    for (path, contents) in timezone::files_for(&config.timezone) {
        write_file(&format!("{TARGET}{path}"), &contents)?;
    }
    cmd::chroot(
        TARGET,
        "ln",
        &["-sf", &timezone::localtime_target(&config.timezone), "/etc/localtime"],
        cancel,
    )?;
    on(0.9, "");

    write_file(
        &format!("{TARGET}/etc/default/keyboard"),
        &format!(
            "XKBMODEL=\"pc105\"\nXKBLAYOUT=\"{}\"\nXKBVARIANT=\"\"\nXKBOPTIONS=\"\"\n",
            config.keymap
        ),
    )?;
    on(1.0, "");
    Ok(())
}

pub fn account(config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    let account = &config.account;

    cmd::chroot(
        TARGET,
        "useradd",
        &[
            "--create-home",
            "--shell",
            "/bin/bash",
            "--groups",
            "sudo,audio,video,plugdev",
            &account.username,
        ],
        cancel,
    )?;
    on(0.4, "");

    // Passwords go over stdin, never argv. See `cmd::run_with_stdin`.
    cmd::run_with_stdin(
        "chroot",
        &[TARGET, "chpasswd"],
        &format!("{}:{}\n", account.username, account.password),
        cancel,
    )?;
    on(0.7, "");

    if account.root_same_password {
        cmd::run_with_stdin(
            "chroot",
            &[TARGET, "chpasswd"],
            &format!("root:{}\n", account.password),
            cancel,
        )?;
    } else {
        // Ubuntu model: no root password, sudo only.
        cmd::chroot(TARGET, "passwd", &["--lock", "root"], cancel)?;
    }
    on(1.0, "");
    Ok(())
}

pub fn bootloader(config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    // Firmware comes from the plan, not a fresh probe: the partition layout was
    // built for one firmware, and GRUB must match it. See `Plan::is_uefi`.
    let uefi = config.plan.is_uefi();

    let packages: &[&str] = if uefi {
        &["linux-image-amd64", "grub-efi-amd64", "grub-efi-amd64-signed", "shim-signed"]
    } else {
        &["linux-image-amd64", "grub-pc"]
    };

    // debootstrap populates sources.list but leaves no apt cache behind.
    cmd::chroot(TARGET, "apt-get", &["update"], cancel)?;
    on(0.05, "");

    let mut argv = vec![TARGET, "apt-get", "install", "-y", "--no-install-recommends"];
    argv.extend_from_slice(packages);

    // The kernel plus GRUB pulls in ~120 packages including firmware. As in
    // debootstrap, the count only shapes the bar's speed, never its endpoint.
    let mut seen = 0.0_f32;
    cmd::run_streaming("chroot", &argv, cancel, |line| {
        if line.starts_with("Unpacking ") || line.starts_with("Setting up ") {
            seen += 1.0;
        }
        on((seen / 120.0).clamp(0.05, 0.85), line);
    })?;

    if uefi {
        let mut args =
            vec!["--target=x86_64-efi", "--efi-directory=/boot/efi", "--bootloader-id=BacakOS"];

        // `--removable` also installs to the firmware-agnostic fallback path
        // (\EFI\BOOT\BOOTX64.EFI) and skips the NVRAM boot entry. Real installs
        // want the NVRAM entry, so this is gated to the VM test, where the guest
        // firmware starts with an empty NVRAM and can only find the fallback —
        // and where writing NVRAM would pollute the *host's* boot menu.
        if std::env::var_os("KUR_GRUB_REMOVABLE").is_some_and(|v| v == "1") {
            args.push("--removable");
        }

        cmd::chroot(TARGET, "grub-install", &args, cancel)?;
    } else {
        cmd::chroot(TARGET, "grub-install", &["--target=i386-pc", &config.plan.disk], cancel)?;
    }
    on(0.95, "");

    cmd::chroot(TARGET, "update-grub", &[], cancel)?;
    on(1.0, "");
    Ok(())
}

/// Install the locally-built BacakOS packages (compositor, display manager,
/// altay, bacak-belge) staged on the live image by `build-debs.sh`.
///
/// Skipped with a log line when the directory is absent: `kur` must still be
/// able to produce a plain Debian system when run from a stock live image.
pub fn bacakos(_config: &Config, cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    let debs = collect_debs(BACAKOS_DEB_DIR);
    if debs.is_empty() {
        on(1.0, &format!("{BACAKOS_DEB_DIR} boş — BacakOS paketleri atlanıyor."));
        return Ok(());
    }

    let staging = format!("{TARGET}/var/cache/bacakos");
    mkdir_p(&staging)?;

    for deb in &debs {
        let name = deb.file_name().and_then(|n| n.to_str()).context("geçersiz paket adı")?;
        if cmd::is_dry_run() {
            log::info!("[dry-run] cp {} {staging}/{name}", deb.display());
            continue;
        }
        std::fs::copy(deb, format!("{staging}/{name}"))
            .with_context(|| format!("{name} kopyalanamadı"))?;
    }
    on(0.3, &format!("{} paket hedefe kopyalandı", debs.len()));

    // `apt-get install ./*.deb` resolves each package's dependencies from the
    // mirror; `dpkg -i` would leave them unsatisfied. The shell glob is
    // expanded by apt itself, not by us, so pass the directory pattern.
    let pattern = "/var/cache/bacakos/*.deb";
    let mut seen = 0.0_f32;
    cmd::run_streaming(
        "chroot",
        &[TARGET, "sh", "-c", &format!("apt-get install -y {pattern}")],
        cancel,
        |line| {
            if line.starts_with("Unpacking ") || line.starts_with("Setting up ") {
                seen += 1.0;
            }
            on((0.3 + seen / 100.0).min(0.95), line);
        },
    )?;

    on(1.0, "");
    Ok(())
}

pub fn cleanup(_config: &Config, _cancel: &Cancel, on: &mut Reporter) -> Result<()> {
    unmount_all();
    on(1.0, "");
    Ok(())
}

// ---- Helpers ----------------------------------------------------------------

/// `.deb` files in `dir`, sorted for a deterministic install order. Returns
/// empty when the directory does not exist.
fn collect_debs(dir: &str) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut debs: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "deb"))
        .collect();
    debs.sort();
    debs
}

/// Unmount everything under [`TARGET`], deepest path first.
///
/// Best-effort and infallible by design: it runs on the error path too, where a
/// second error would only mask the first.
pub fn unmount_all() {
    let cancel = Cancel::new();

    // Flush the page cache to the disk *first*. `grub-install` writes its
    // runtime modules (`/boot/grub/i386-pc/*.mod`) as almost the last thing the
    // install does; a lazy unmount detaches the tree without waiting for those
    // dirty pages, and if the medium is then disconnected (USB pulled, VM image
    // released) the modules never reach it — GRUB drops to a rescue prompt on
    // the next boot. `sync` closes that window.
    let _ = cmd::run("sync", &[], &cancel);

    let mounts = [
        format!("{TARGET}/dev"),
        format!("{TARGET}/proc"),
        format!("{TARGET}/sys"),
        format!("{TARGET}/boot/efi"),
        TARGET.to_string(),
    ];
    for mount in mounts {
        // Try a real recursive unmount first, which forces a final flush.
        // `-R` recurses into the submounts `--rbind` brought in (/dev/pts,
        // /sys/firmware/efi/efivars). Only if that fails — a chroot process
        // still holds the mount busy — fall back to a lazy detach.
        if cmd::run("umount", &["-R", &mount], &cancel).is_err() {
            if let Err(error) = cmd::run("umount", &["-R", "-l", &mount], &cancel) {
                log::debug!("umount {mount} atlandı: {error}");
            }
        }
    }

    // A lazy fallback above returns before its flush completes; sync once more
    // so the caller can safely disconnect the medium the moment we return.
    let _ = cmd::run("sync", &[], &cancel);
}

/// Build `/etc/fstab` from partition UUIDs.
///
/// UUIDs, not device nodes: `/dev/sda2` becomes `/dev/sdb2` the moment the user
/// adds a second drive, and the machine stops booting.
fn render_fstab(plan: &Plan) -> Result<String> {
    let mut fstab = String::from("# /etc/fstab — kur tarafından oluşturuldu\n");

    let root = plan.path_for(Role::Root).context("kök bölüm planda yok")?;
    fstab.push_str(&format!("UUID={}\t/\text4\terrors=remount-ro\t0\t1\n", uuid_of(&root)?));

    if let Some(esp) = plan.path_for(Role::Esp) {
        fstab.push_str(&format!("UUID={}\t/boot/efi\tvfat\tumask=0077\t0\t1\n", uuid_of(&esp)?));
    }
    if let Some(swap) = plan.path_for(Role::Swap) {
        fstab.push_str(&format!("UUID={}\tnone\tswap\tsw\t0\t0\n", uuid_of(&swap)?));
    }
    Ok(fstab)
}

/// A dry run never partitions, so the partitions `blkid` is asked about do not
/// exist. Synthesise a stable placeholder instead: it only ever reaches
/// [`render_fstab`], whose output is discarded by [`write_file`].
fn uuid_of(device: &str) -> Result<String> {
    if cmd::is_dry_run() {
        return Ok(format!("DRY-RUN-{}", device.trim_start_matches("/dev/")));
    }
    let uuid = cmd::capture("blkid", &["-s", "UUID", "-o", "value", device])
        .with_context(|| format!("{device} için UUID okunamadı"))?;
    Ok(uuid.trim().to_string())
}

/// `mkdir -p`, skipped in a dry run.
///
/// Without the guard a dry run would try to create `/mnt/kur-target` — which
/// needs root, and which a developer testing the wizard never asked for.
fn mkdir_p(path: &str) -> Result<()> {
    if cmd::is_dry_run() {
        log::info!("[dry-run] mkdir -p {path}");
        return Ok(());
    }
    std::fs::create_dir_all(path).with_context(|| format!("{path} oluşturulamadı"))
}

fn write_file(path: &str, contents: &str) -> Result<()> {
    if cmd::is_dry_run() {
        log::info!("[dry-run] write {path} ({} bayt)", contents.len());
        return Ok(());
    }
    log::debug!("write: {path}");
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, contents).with_context(|| format!("{path} yazılamadı"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_deb_dir_yields_no_packages() {
        assert!(collect_debs("/nonexistent/bacakos/debs").is_empty());
    }

    #[test]
    fn only_deb_files_are_collected_and_sorted() {
        let dir = std::env::temp_dir().join("kur-deb-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.deb", "a.deb", "notes.txt"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }

        let debs = collect_debs(dir.to_str().unwrap());
        let names: Vec<_> =
            debs.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["a.deb", "b.deb"]);

        std::fs::remove_dir_all(&dir).ok();
    }
}
