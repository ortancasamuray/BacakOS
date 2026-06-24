//! Bacak OS — Service Layer
//!
//! Aggregates the non-graphical subsystems of Bacak OS into a single crate:
//!
//! * [`fs`]      — virtual filesystem (native + archive-backed paths).
//! * [`archive`] — ZIP / TAR / TAR.GZ backends behind a uniform listing API.
//! * [`device`] — Audio / Wi-Fi / Bluetooth surfaces with a pluggable provider.
//! * [`network`] — connectivity diagnostics (stub for now).
//!
//! The CLI, compositor, and shell all consume this crate; the compositor never
//! reaches inside `fs`/`archive` directly, and the CLI exercises the same code
//! path the shell uses.

pub mod archive;
pub mod fs;
pub mod device;
pub mod network;

// Convenience re-exports for the most common surface types.
pub use archive::{ArchiveEntry, ArcError};
pub use fs::{DirEntry, FsError, Metadata, VfsPath};
pub use device::{
    AudioSink, AudioSource, AudioState, BluetoothState, BtDevice, BtKind,
    DeviceError, DeviceProvider, MockProvider, SinkKind, WifiNetwork, WifiState,
};
