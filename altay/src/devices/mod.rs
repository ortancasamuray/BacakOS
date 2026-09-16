// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Removable & external device discovery and hotplug.
//!
//! Phase 1 enumerates mounted removable/external volumes from the same places
//! the sandbox trusts (`/media/$USER`, `/run/media/$USER`, `/mnt`). Live
//! hotplug notifications and mount/unmount actions via udisks2 over D-Bus
//! (zbus) arrive in a later phase; the [`Device`] model and [`watch`] hook are
//! defined now so the UI can be wired up.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::security::{AllowedRoot, RootKind, Sandbox};

// ---- UDisks2 D-Bus proxies (typed; avoids manual zvariant parsing) ----------

#[zbus::proxy(
    interface = "org.freedesktop.UDisks2.Filesystem",
    default_service = "org.freedesktop.UDisks2"
)]
trait Filesystem {
    fn mount(&self, options: HashMap<&str, zbus::zvariant::Value<'_>>) -> zbus::Result<String>;
    fn unmount(&self, options: HashMap<&str, zbus::zvariant::Value<'_>>) -> zbus::Result<()>;
    #[zbus(property)]
    fn mount_points(&self) -> zbus::Result<Vec<Vec<u8>>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UDisks2.Block",
    default_service = "org.freedesktop.UDisks2"
)]
trait Block {
    #[zbus(property)]
    fn id_label(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn hint_system(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn read_only(&self) -> zbus::Result<bool>;
}

/// A udisks2-managed filesystem volume (mounted or not).
#[derive(Debug, Clone)]
pub struct Volume {
    pub object_path: String,
    pub label: String,
    pub mount_point: Option<PathBuf>,
    pub read_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("could not reach udisks2 on the system bus: {0}")]
    Bus(String),
    #[error("mount failed: {0}")]
    Mount(String),
    #[error("unmount failed: {0}")]
    Unmount(String),
}

/// Run a future to completion on a fresh current-thread runtime (mount/unmount
/// are occasional, user-initiated actions, so a per-call runtime is fine).
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

/// Enumerate removable/external filesystem volumes known to udisks2. System
/// partitions (HintSystem) are skipped so the OS disk is never exposed.
pub fn list_volumes() -> Vec<Volume> {
    block_on(async { enumerate().await }).unwrap_or_default()
}

async fn enumerate() -> zbus::Result<Vec<Volume>> {
    use futures_util::future::join_all;
    let conn = zbus::Connection::system().await?;
    let om = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination("org.freedesktop.UDisks2")?
        .path("/org/freedesktop/UDisks2")?
        .build()
        .await?;
    let objects = om.get_managed_objects().await?;

    let tasks = objects.into_iter().filter_map(|(path, ifaces)| {
        let has_fs = ifaces.keys().any(|k| k.as_str() == "org.freedesktop.UDisks2.Filesystem");
        if !has_fs {
            return None;
        }
        let conn = conn.clone();
        Some(async move {
            let block = BlockProxy::builder(&conn).path(path.clone()).ok()?.build().await.ok()?;
            // Never surface OS/system partitions.
            if block.hint_system().await.unwrap_or(true) {
                return None;
            }
            let fs = FilesystemProxy::builder(&conn).path(path.clone()).ok()?.build().await.ok()?;
            let mount_point = fs
                .mount_points()
                .await
                .unwrap_or_default()
                .first()
                .map(|b| bytes_to_path(b));
            let raw_label = block.id_label().await.unwrap_or_default();
            let label = if raw_label.is_empty() { object_basename(path.as_str()) } else { raw_label };
            Some(Volume {
                object_path: path.as_str().to_string(),
                label,
                mount_point,
                read_only: block.read_only().await.unwrap_or(false),
            })
        })
    });

    Ok(join_all(tasks).await.into_iter().flatten().collect())
}

/// Mount a volume by its udisks2 object path; returns the new mount point.
/// udisks2 routes authorization through PolicyKit for the active session user.
pub fn mount(object_path: &str) -> Result<PathBuf, DeviceError> {
    block_on(async {
        let conn = zbus::Connection::system().await.map_err(|e| DeviceError::Bus(e.to_string()))?;
        let fs = FilesystemProxy::builder(&conn)
            .path(object_path.to_string())
            .map_err(|e| DeviceError::Bus(e.to_string()))?
            .build()
            .await
            .map_err(|e| DeviceError::Bus(e.to_string()))?;
        let mp = fs.mount(HashMap::new()).await.map_err(|e| DeviceError::Mount(e.to_string()))?;
        Ok(PathBuf::from(mp))
    })
}

/// Unmount a volume by its udisks2 object path.
pub fn unmount(object_path: &str) -> Result<(), DeviceError> {
    block_on(async {
        let conn = zbus::Connection::system().await.map_err(|e| DeviceError::Bus(e.to_string()))?;
        let fs = FilesystemProxy::builder(&conn)
            .path(object_path.to_string())
            .map_err(|e| DeviceError::Bus(e.to_string()))?
            .build()
            .await
            .map_err(|e| DeviceError::Bus(e.to_string()))?;
        fs.unmount(HashMap::new()).await.map_err(|e| DeviceError::Unmount(e.to_string()))
    })
}

/// Convert a NUL-terminated udisks2 path byte array into a `PathBuf`.
fn bytes_to_path(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    PathBuf::from(std::ffi::OsStr::from_bytes(&b[..end]))
}

fn object_basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// A mounted, user-accessible storage device.
#[derive(Debug, Clone)]
pub struct Device {
    pub label: String,
    pub mount_point: PathBuf,
    pub kind: DeviceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    UsbDrive,
    SdCard,
    ExternalDisk,
    Network,
}

/// Enumerate currently-mounted removable/network roots from the sandbox.
pub fn list(sandbox: &Sandbox) -> Vec<Device> {
    sandbox
        .roots()
        .iter()
        .filter(|r| !r.is_home())
        .map(device_from_root)
        .collect()
}

fn device_from_root(root: &AllowedRoot) -> Device {
    let kind = match root.kind() {
        RootKind::Network => DeviceKind::Network,
        _ => guess_kind(root.label()),
    };
    Device {
        label: root.label().to_string(),
        mount_point: root.path().to_path_buf(),
        kind,
    }
}

fn guess_kind(label: &str) -> DeviceKind {
    let l = label.to_lowercase();
    if l.contains("sd") || l.contains("card") {
        DeviceKind::SdCard
    } else if l.contains("usb") {
        DeviceKind::UsbDrive
    } else {
        DeviceKind::ExternalDisk
    }
}

/// Subscribe to udisks2 hotplug events on the system bus. `on_change` is
/// invoked (from a background thread) whenever a block device / filesystem is
/// added or removed — e.g. a USB stick is plugged in or ejected. If the system
/// bus or udisks2 is unavailable, this logs once and the thread exits cleanly;
/// the rest of the app keeps working with the mounts present at startup.
pub fn monitor<F: Fn() + Send + 'static>(on_change: F) {
    std::thread::Builder::new()
        .name("udisks2-monitor".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    log::warn!("device monitor: failed to start runtime: {e}");
                    return;
                }
            };
            if let Err(e) = rt.block_on(run_monitor(&on_change)) {
                log::info!("device hotplug monitor unavailable (udisks2/D-Bus): {e}");
            }
        })
        .ok();
}

/// Watch the UDisks2 object manager for interface add/remove signals.
async fn run_monitor<F: Fn()>(on_change: &F) -> zbus::Result<()> {
    use futures_util::StreamExt;

    let conn = zbus::Connection::system().await?;
    let object_manager = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination("org.freedesktop.UDisks2")?
        .path("/org/freedesktop/UDisks2")?
        .build()
        .await?;

    let mut added = object_manager.receive_interfaces_added().await?;
    let mut removed = object_manager.receive_interfaces_removed().await?;
    log::info!("udisks2 device hotplug monitor active");

    loop {
        tokio::select! {
            sig = added.next() => match sig {
                Some(_) => on_change(),
                None => break,
            },
            sig = removed.next() => match sig {
                Some(_) => on_change(),
                None => break,
            },
        }
    }
    Ok(())
}
