//! Async backup execution for remote-involved data paths.
//!
//! This module is the remote counterpart to [`crate::backup::bio`]:
//! - Direction-specific **copy** pipelines live under `backup/aio/directions/`.
//! - Direction-specific **orchestrators** (spawn + run) live under
//!   `backup/aio/directions/{local_to_nfs, nfs_to_local, ...}.rs`.
//! - NFS target **post-copy phases** (hardlink/delete/mtime) reuse the RPC
//!   helpers under [`crate::nfs::aio`].
//! - SMB target post-copy phases reuse the helpers under [`crate::smb::aio`].
//! - Local target post-copy phases reuse the existing BIO phase handlers.
//!
//! The public entry points here are direction-level orchestrators so callers
//! do not need to manually stitch together copy and post-copy phases.

#[cfg(feature = "smb")]
pub const DEFAULT_SMB_POOL_SIZE: usize = 2;

pub(crate) mod aggregation;
pub(crate) mod directions;
pub(crate) mod entry;
pub(crate) mod executor;
pub(crate) mod local_fs;
pub(crate) mod path_util;
pub(crate) mod phases;
pub(crate) mod pipeline;
pub(crate) mod transport;

// Re-export spawn functions from per-direction modules for backward compatibility.

#[cfg(feature = "nfs")]
pub use crate::nfs::backup::local_to_nfs::spawn as spawn_local_to_nfs_backup;

#[cfg(feature = "smb")]
pub use crate::smb::backup::local_to_smb::spawn as spawn_local_to_smb_backup;

#[cfg(feature = "smb")]
pub use crate::smb::backup::smb_to_local::spawn as spawn_smb_to_local_backup;

#[cfg(feature = "smb")]
pub use crate::smb::backup::smb_to_smb::spawn as spawn_smb_to_smb_backup;

#[cfg(all(feature = "nfs", feature = "smb"))]
pub use directions::nfs_to_smb::spawn as spawn_nfs_to_smb_backup;

#[cfg(all(feature = "nfs", feature = "smb"))]
pub use directions::smb_to_nfs::spawn as spawn_smb_to_nfs_backup;

#[cfg(feature = "nfs")]
pub use crate::nfs::backup::nfs_to_local::spawn as spawn_nfs_to_local_backup;

#[cfg(feature = "nfs")]
pub use crate::nfs::backup::nfs_to_nfs::spawn as spawn_nfs_to_nfs_backup;
