//! Async SMB transport helpers shared by scanner, backup, and post-job flows.
//!
//! This module is organized symmetrically with [`crate::nfs::aio`]:
//! - `connection.rs` — client pool and directory cache
//! - `metrics.rs` — copy performance metrics
//! - `writer.rs` — write operations (mkdir, write, streaming copy, upload)
//! - `path_util.rs` — path normalization and UNC path construction
//! - `hardlink.rs`, `delete.rs`, `mtime.rs` — post-copy phases

pub mod delete;
pub mod hardlink;
pub mod metrics;
pub mod mtime;
pub mod path_util;
pub mod writer;

// Re-export commonly used types for backward compatibility.
pub use connection::{connect_client, new_dir_cache, DirCache, SmbClientPool};
pub use metrics::SmbCopyMetrics;
pub use path_util::{
    close_resource, relative_unc_path, share_relative_path,
    target_relative_path, SMB_MAX_SAFE_READ_CHUNK,
};
pub use writer::{
    copy_relative_file_streaming, ensure_relative_directory, upload_local_dir_to_smb,
    upload_local_file_to_smb, write_relative_file_chunk,
};

// The connection module lives one level up (smb/connection.rs).
use super::connection;
