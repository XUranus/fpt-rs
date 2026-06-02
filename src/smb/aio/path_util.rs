//! SMB-specific path utilities and resource helpers.
//!
//! Generic path utilities (normalize_relative_path, join_relative, target_relative_path)
//! have been moved to [`crate::path_util`].

use crate::path_util;
use crate::smb::SmbLocation;

pub(crate) const SMB_DEFAULT_WRITE_CHUNK: usize = 256 * 1024;
pub(crate) const SMB_DEFAULT_READ_CHUNK: usize = 1024 * 1024;
pub(crate) const SMB_MAX_SAFE_WRITE_CHUNK: usize = 256 * 1024;
pub const SMB_MAX_SAFE_READ_CHUNK: usize = 2 * 1024 * 1024;

// Re-export generic functions for backward compatibility.
pub use path_util::{join_relative, normalize_relative_path, target_relative_path};

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

/// Close an SMB resource (file, directory, or pipe) regardless of its type.
pub async fn close_resource(resource: smb_client::Resource) -> Result<(), String> {
    match resource {
        smb_client::Resource::File(file) => file.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Directory(dir) => dir.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Pipe(pipe) => pipe.close().await.map_err(|e| e.to_string()),
    }
}
