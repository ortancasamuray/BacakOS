//! Connector hot-plug ↔ window manager bridge.
//!
//! The DRM/udev backend tells us a connector flipped state (a monitor was
//! plugged in or unplugged) by emitting an opaque `change` event for the
//! DRM card device. This module turns *that* — a flat list of currently
//! connected connectors — into the precise set of [`WindowManager`]
//! mutations needed to keep the WM model in sync:
//!
//! * a brand-new connector becomes a new output via
//!   [`WindowManager::add_output`];
//! * a connector that vanished triggers
//!   [`WindowManager::remove_output`];
//! * a connector whose mode/resolution changed since the last reconcile
//!   re-publishes its bounds via [`WindowManager::set_output_bounds`].
//!
//! The logic lives behind a thin data type ([`OutputRegistry`]) that
//! holds a `connector_handle (u32) → OutputId` map. The udev backend
//! converts its `connector::Handle`s into raw u32s and feeds
//! [`ConnectorSnapshot`]s here; nothing in this module pulls in DRM or
//! Smithay types, so the diff logic is unit-testable without a real
//! display.

use std::collections::{HashMap, HashSet};

use crate::wm::{OutputId, Rect, WindowManager, WmError};

/// One row in a hot-plug snapshot. `handle` is the connector's DRM id
/// (typed as `u32` so this module stays DRM-free); `bounds` is the
/// connector's current mode in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConnectorSnapshot {
    pub handle: u32,
    pub bounds: Rect,
}

/// What [`OutputRegistry::reconcile`] actually did. Useful for tracing
/// hot-plug behaviour — the udev runtime logs it on every reconcile so
/// "did the second monitor really show up?" is answerable from a log.
///
/// Each variant carries both the DRM connector handle and the
/// [`OutputId`] so callers (the render-target bring-up path) can
/// correlate without a reverse lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotplugChange {
    Added { connector: u32, output: OutputId },
    Removed { connector: u32, output: OutputId },
    Resized { connector: u32, output: OutputId },
}

impl HotplugChange {
    pub fn output(&self) -> OutputId {
        match *self {
            HotplugChange::Added { output, .. }
            | HotplugChange::Removed { output, .. }
            | HotplugChange::Resized { output, .. } => output,
        }
    }
    pub fn connector(&self) -> u32 {
        match *self {
            HotplugChange::Added { connector, .. }
            | HotplugChange::Removed { connector, .. }
            | HotplugChange::Resized { connector, .. } => connector,
        }
    }
}

/// `connector::Handle (u32) → OutputId` mapping. The registry is
/// stateful — it remembers which OutputIds it handed out — so a later
/// `reconcile` can tell the difference between "new connector" and "the
/// monitor we already know about, slightly resized".
#[derive(Debug, Default)]
pub struct OutputRegistry {
    by_connector: HashMap<u32, OutputId>,
}

impl OutputRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the registry's notion of "this connector" to an OutputId
    /// that was created outside the registry (typically the primary
    /// output that [`WindowManager::new`] seeded at boot).
    ///
    /// Without this, a later unplug of the primary connector would be a
    /// no-op — the registry wouldn't know which OutputId to remove.
    pub fn adopt(&mut self, connector_handle: u32, output: OutputId) {
        self.by_connector.insert(connector_handle, output);
    }

    /// Whether the registry has a binding for `connector_handle`.
    pub fn knows(&self, connector_handle: u32) -> bool {
        self.by_connector.contains_key(&connector_handle)
    }

    /// The OutputId currently bound to `connector_handle`, if any.
    pub fn output_for(&self, connector_handle: u32) -> Option<OutputId> {
        self.by_connector.get(&connector_handle).copied()
    }

    /// Number of connectors the registry is currently tracking.
    pub fn len(&self) -> usize {
        self.by_connector.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_connector.is_empty()
    }

    /// Apply a fresh connector snapshot to the WM. Returns the list of
    /// changes that took effect, in the order they were applied —
    /// removes first (so a re-plug of the same connector handle into a
    /// fresh slot reads cleanly in logs), then adds, then resizes.
    pub fn reconcile(
        &mut self,
        snapshots: &[ConnectorSnapshot],
        wm: &WindowManager,
    ) -> Vec<HotplugChange> {
        let mut changes = Vec::new();
        let now: HashSet<u32> =
            snapshots.iter().map(|s| s.handle).collect();

        // ----- removes ----------------------------------------------------
        // Collect the handles that are no longer present, then drop them.
        // Sorting keeps the change list deterministic across HashMap
        // iteration order — handy for tests and tracing diffs across runs.
        let mut gone: Vec<u32> = self
            .by_connector
            .keys()
            .copied()
            .filter(|h| !now.contains(h))
            .collect();
        gone.sort_unstable();
        for h in gone {
            let Some(oid) = self.by_connector.remove(&h) else { continue };
            match wm.remove_output(oid) {
                Ok(()) => changes.push(HotplugChange::Removed { connector: h, output: oid }),
                Err(WmError::LastOutput) => {
                    // Headless transition: the user unplugged the only
                    // monitor. The WM refuses to delete the last output
                    // (snap math + struts assume at least one exists);
                    // keep the mapping so the *next* reconcile that
                    // re-introduces this connector is treated as a
                    // bounds update, not a duplicate add.
                    self.by_connector.insert(h, oid);
                }
                Err(_) => {
                    // Other WmError variants here would mean the
                    // OutputId is already gone — fine, we already
                    // dropped the mapping.
                }
            }
        }

        // ----- adds + resizes --------------------------------------------
        // Pre-sort the snapshot so adds happen in a stable order. The
        // hot-plug log otherwise depends on whatever order the DRM
        // driver enumerated the connectors in.
        let mut snapshots: Vec<&ConnectorSnapshot> = snapshots.iter().collect();
        snapshots.sort_by_key(|s| s.handle);

        for snap in snapshots {
            match self.by_connector.get(&snap.handle) {
                Some(&oid) => {
                    // Existing connector — check for a resolution / mode
                    // change. We *don't* attempt to reposition existing
                    // windows; that's the shell's job (and would need
                    // user intent: keep absolute geom vs. re-snap).
                    if let Some(o) = wm.output(oid) {
                        if o.bounds != snap.bounds
                            && wm.set_output_bounds(oid, snap.bounds).is_ok()
                        {
                            changes.push(HotplugChange::Resized {
                                connector: snap.handle,
                                output: oid,
                            });
                        }
                    }
                }
                None => {
                    let oid = wm.add_output(snap.bounds);
                    self.by_connector.insert(snap.handle, oid);
                    changes.push(HotplugChange::Added {
                        connector: snap.handle,
                        output: oid,
                    });
                }
            }
        }

        changes
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wm::{Monitor, Rect, WindowManager};

    fn wm_with_primary() -> (WindowManager, u32, OutputId) {
        let wm = WindowManager::new(Monitor {
            work_area: Rect::new(0.0, 0.0, 1920.0, 1080.0),
        });
        // Pretend connector handle 100 is the primary that boot already wired.
        let primary = wm.primary_output().unwrap();
        (wm, 100, primary)
    }

    #[test]
    fn add_new_connector_becomes_new_output() {
        let (wm, conn_a, primary) = wm_with_primary();
        let mut reg = OutputRegistry::new();
        reg.adopt(conn_a, primary);

        let snaps = vec![
            ConnectorSnapshot { handle: conn_a, bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0) },
            ConnectorSnapshot { handle: 200,    bounds: Rect::new(1920.0, 0.0, 2560.0, 1440.0) },
        ];
        let changes = reg.reconcile(&snaps, &wm);

        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], HotplugChange::Added { connector: 200, .. }));
        assert_eq!(wm.outputs().len(), 2);
        assert!(reg.knows(200));
    }

    #[test]
    fn remove_disconnected_connector() {
        let (wm, conn_a, primary) = wm_with_primary();
        let mut reg = OutputRegistry::new();
        reg.adopt(conn_a, primary);

        // First reconcile adds a secondary.
        let _ = reg.reconcile(
            &[
                ConnectorSnapshot { handle: conn_a, bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0) },
                ConnectorSnapshot { handle: 200,    bounds: Rect::new(1920.0, 0.0, 2560.0, 1440.0) },
            ],
            &wm,
        );
        let secondary_oid = reg.output_for(200).unwrap();
        assert_eq!(wm.outputs().len(), 2);

        // Now the secondary is unplugged.
        let changes = reg.reconcile(
            &[ConnectorSnapshot { handle: conn_a, bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0) }],
            &wm,
        );
        assert_eq!(
            changes,
            vec![HotplugChange::Removed { connector: 200, output: secondary_oid }]
        );
        assert_eq!(wm.outputs().len(), 1);
        assert!(!reg.knows(200));
    }

    #[test]
    fn mode_change_emits_resize() {
        let (wm, conn_a, primary) = wm_with_primary();
        let mut reg = OutputRegistry::new();
        reg.adopt(conn_a, primary);

        // Same connector reports a new bounds (4K → 1080p, say).
        let changes = reg.reconcile(
            &[ConnectorSnapshot { handle: conn_a, bounds: Rect::new(0.0, 0.0, 3840.0, 2160.0) }],
            &wm,
        );

        assert_eq!(
            changes,
            vec![HotplugChange::Resized { connector: conn_a, output: primary }]
        );
        assert_eq!(wm.output(primary).unwrap().bounds, Rect::new(0.0, 0.0, 3840.0, 2160.0));
    }

    #[test]
    fn noop_when_snapshot_matches_current_state() {
        let (wm, conn_a, primary) = wm_with_primary();
        let mut reg = OutputRegistry::new();
        reg.adopt(conn_a, primary);

        let bounds = wm.output(primary).unwrap().bounds;
        let changes = reg.reconcile(
            &[ConnectorSnapshot { handle: conn_a, bounds }],
            &wm,
        );
        assert!(changes.is_empty());
    }

    #[test]
    fn last_output_unplug_keeps_mapping() {
        // Single monitor session: user yanks the only cable. The WM
        // refuses to delete the last output (a headless WM has no
        // useful coordinate system), so the registry must preserve the
        // mapping — otherwise a re-plug would silently duplicate.
        let (wm, conn_a, primary) = wm_with_primary();
        let mut reg = OutputRegistry::new();
        reg.adopt(conn_a, primary);

        let changes = reg.reconcile(&[], &wm);
        assert!(changes.is_empty(), "remove of last output is refused, no change emitted");
        assert!(reg.knows(conn_a), "mapping survives so re-plug is a bounds update");
        assert_eq!(wm.outputs().len(), 1);

        // Re-plug at a new resolution — treated as resize, not double-add.
        let changes = reg.reconcile(
            &[ConnectorSnapshot { handle: conn_a, bounds: Rect::new(0.0, 0.0, 2560.0, 1440.0) }],
            &wm,
        );
        assert_eq!(
            changes,
            vec![HotplugChange::Resized { connector: conn_a, output: primary }]
        );
        assert_eq!(wm.outputs().len(), 1);
    }

    #[test]
    fn add_remove_combo_in_one_reconcile() {
        let (wm, conn_a, primary) = wm_with_primary();
        let mut reg = OutputRegistry::new();
        reg.adopt(conn_a, primary);

        // First, plug in a second monitor.
        let _ = reg.reconcile(
            &[
                ConnectorSnapshot { handle: conn_a, bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0) },
                ConnectorSnapshot { handle: 200,    bounds: Rect::new(1920.0, 0.0, 2560.0, 1440.0) },
            ],
            &wm,
        );
        let secondary_oid = reg.output_for(200).unwrap();

        // Now swap that second monitor for a third one in the same dock
        // (different connector handle), reported in a single reconcile.
        let changes = reg.reconcile(
            &[
                ConnectorSnapshot { handle: conn_a, bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0) },
                ConnectorSnapshot { handle: 300,    bounds: Rect::new(1920.0, 0.0, 1920.0, 1080.0) },
            ],
            &wm,
        );

        assert!(changes
            .iter()
            .any(|c| matches!(c, HotplugChange::Removed { connector: 200, output } if *output == secondary_oid)));
        assert!(changes
            .iter()
            .any(|c| matches!(c, HotplugChange::Added { connector: 300, .. })));
        assert!(!reg.knows(200));
        assert!(reg.knows(300));
        assert_eq!(wm.outputs().len(), 2);
    }
}
