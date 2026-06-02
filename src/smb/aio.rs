//! Async SMB transport helpers shared by scanner, backup, and post-job flows.
//!
//! This module is organized symmetrically with [`crate::nfs::aio`]:
//! - `connection.rs` — client pool and directory cache
//! - `metrics.rs` — copy performance metrics
//! - `writer.rs` — write operations (mkdir, write, streaming copy, upload)
//! - `hardlink.rs`, `delete.rs`, `mtime.rs` — post-copy phases
//!
//! Generic path utilities live in [`crate::utility::path_util`].

use crate::path_util;
use crate::smb::SmbLocation;

pub mod delete;
pub mod hardlink;
pub mod metrics;
pub mod mtime;
pub mod writer;

// Re-export commonly used types for backward compatibility.
pub use connection::{connect_client, new_dir_cache, DirCache, SmbClientPool};
pub use metrics::SmbCopyMetrics;
pub use path_util::{join_relative, normalize_relative_path, target_relative_path};
pub use writer::{
    copy_relative_file_streaming, ensure_relative_directory, upload_local_dir_to_smb,
    upload_local_file_to_smb, write_relative_file_chunk,
};

pub(crate) const SMB_DEFAULT_WRITE_CHUNK: usize = 256 * 1024;
pub(crate) const SMB_DEFAULT_READ_CHUNK: usize = 1024 * 1024;
pub(crate) const SMB_MAX_SAFE_WRITE_CHUNK: usize = 256 * 1024;
pub const SMB_MAX_SAFE_READ_CHUNK: usize = 2 * 1024 * 1024;

// The connection module lives one level up (smb/connection.rs).
use super::connection;

/// Build a UNC path for a file relative to the share location.
pub fn relative_unc_path(
    location: &SmbLocation,
    relative_path: &str,
) -> Result<smb_client::UncPath, String> {
    let relative_path = normalize_relative_path(relative_path);
    let root = location.root_unc_path()?;
    if relative_path.is_empty() {
        Ok(root)
    } else {
        Ok(root.with_add_path(&relative_path))
    }
}

pub fn share_relative_path(location: &SmbLocation, relative_path: &str) -> String {
    let relative_path = normalize_relative_path(relative_path);
    if location.sub_path.is_empty() {
        relative_path.replace('/', "\\")
    } else if relative_path.is_empty() {
        location.sub_path.replace('/', "\\")
    } else {
        format!(
            "{}\\{}",
            location.sub_path.replace('/', "\\"),
            relative_path.replace('/', "\\")
        )
    }
}

// Re-export from parent module for backward compatibility.
pub use crate::smb::close_resource;
