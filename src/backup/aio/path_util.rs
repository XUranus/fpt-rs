//! Shared path helpers for remote-transport post-copy phases.

use std::path::{Path, PathBuf};

/// Compute the target-relative path for a file in a remote backup target.
///
/// Given a local `base` directory (the scan root), a `target_prefix` (e.g.
/// `COPY_COMMON_FULL_xxx/D_REPO`), and an absolute `path`, returns the
/// path as it should appear on the remote target.
///
/// For example:
/// - base = `/opt/dataset`, target_prefix = `COPY_xxx/D_REPO`
/// - path  = `/opt/dataset/subdir/file.txt`
/// - result = `COPY_xxx/D_REPO/subdir/file.txt`
#[allow(dead_code)]
pub fn target_relative_path(base: &Path, target_prefix: &str, path: &str) -> String {
    let rel = Path::new(path)
        .strip_prefix(base)
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| {
            let p = Path::new(path);
            let logical_root_name = base.file_name().and_then(|n| n.to_str());
            let first_segment = p
                .strip_prefix("/")
                .ok()
                .and_then(|p| p.iter().next())
                .and_then(|s| s.to_str());
            if logical_root_name.is_some() && logical_root_name == first_segment {
                p.strip_prefix("/")
                    .map(|r| r.to_path_buf())
                    .unwrap_or_else(|_| PathBuf::from(path))
            } else {
                p.file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(path))
            }
        });
    let prefixed = if target_prefix.is_empty() {
        rel
    } else {
        Path::new(target_prefix).join(rel)
    };
    prefixed.to_string_lossy().into_owned()
}
