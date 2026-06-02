//! SMB path utilities and resource helpers.

use std::path::{Path, PathBuf};

use crate::smb::SmbLocation;

pub(crate) const SMB_DEFAULT_WRITE_CHUNK: usize = 256 * 1024;
pub(crate) const SMB_DEFAULT_READ_CHUNK: usize = 1024 * 1024;
pub(crate) const SMB_MAX_SAFE_WRITE_CHUNK: usize = 256 * 1024;
pub const SMB_MAX_SAFE_READ_CHUNK: usize = 2 * 1024 * 1024;

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

/// Normalize a path for SMB use: convert backslashes, trim leading/trailing slashes.
pub fn normalize_relative_path(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

pub fn target_relative_path(source_dir_base: &Path, target_prefix: &str, path: &str) -> String {
    let rel = relative_path_buf(source_dir_base, Path::new(path));
    let prefixed = if target_prefix.is_empty() {
        rel
    } else {
        Path::new(target_prefix).join(rel)
    };
    normalize_relative_path(&prefixed.to_string_lossy())
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

pub(crate) fn join_relative(base: &str, child: &str) -> String {
    let child = normalize_relative_path(child);
    if base.is_empty() {
        child
    } else if child.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{child}")
    }
}

fn relative_path_buf(source_dir_base: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(source_dir_base)
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| {
            if path.is_absolute() {
                let logical_root_name = source_dir_base.file_name().and_then(|n| n.to_str());
                let first_segment = path
                    .strip_prefix("/")
                    .ok()
                    .and_then(|p| p.iter().next())
                    .and_then(|s| s.to_str());
                if logical_root_name.is_some() && logical_root_name == first_segment {
                    return path
                        .strip_prefix("/")
                        .map(|r| r.to_path_buf())
                        .unwrap_or_else(|_| path.to_path_buf());
                }
            }
            path.file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| path.to_path_buf())
        })
}

/// Close an SMB resource (file, directory, or pipe) regardless of its type.
pub async fn close_resource(resource: smb_client::Resource) -> Result<(), String> {
    match resource {
        smb_client::Resource::File(file) => file.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Directory(dir) => dir.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Pipe(pipe) => pipe.close().await.map_err(|e| e.to_string()),
    }
}
