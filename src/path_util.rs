//! # Cross-Platform Path Utilities
//!
//! All metadata and control files store paths as **forward-slash** logical strings.
//! This module provides conversion between native [`PathBuf`] (which uses `\` on
//! Windows) and the canonical logical representation.
//!
//! ## Convention
//!
//! - **Logical paths**: always use `/` as separator, may start with `/` (absolute).
//!   Used in metadata (DirMeta.path, FileMeta.path), control files, and diff output.
//! - **Native paths**: use OS-native separators (`\` on Windows, `/` on Unix).
//!   Used only for actual filesystem operations.

use std::path::{Path, PathBuf};

/// Convert a native filesystem path to a forward-slash logical string.
///
/// On Windows, `D:\foo\bar` becomes `D:/foo/bar`.
/// On Unix, this is a no-op (paths already use `/`).
pub fn to_logical_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Normalize a raw path string to a canonical logical path.
///
/// - Replaces all `\` with `/`
/// - Removes empty segments and `.`
/// - Resolves `..` (pops parent)
/// - Ensures the result starts with `/`
pub fn normalize_logical(raw: &str) -> String {
    let normalized = raw.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            let _ = parts.pop();
            continue;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Join two logical path components with `/`.
///
/// `base` should be a logical path (e.g. `/dir/subdir`).
/// `name` is a single path component (e.g. `file.txt`).
/// Returns a normalized logical path.
pub fn join_logical(base: &str, name: &str) -> String {
    if base == "/" {
        normalize_logical(&format!("/{}", name))
    } else {
        normalize_logical(&format!("{}/{}", base.trim_end_matches('/'), name))
    }
}

/// Convert a forward-slash logical path string back to a native PathBuf.
///
/// On Windows, `/foo/bar` becomes a relative path `foo\bar` (no drive letter).
/// If the logical path starts with a drive letter like `D:/foo`, it becomes `D:\foo`.
pub fn logical_to_native(logical: &str) -> PathBuf {
    let normalized = normalize_logical(logical);
    // Strip leading `/` unless it looks like a drive path (e.g. `/D/` -> `D:\`)
    if normalized.len() > 2 {
        let chars: Vec<char> = normalized.chars().collect();
        if chars[0] == '/' && chars[2] == '/' && chars[1].is_ascii_alphabetic() {
            // Looks like `/D/path/...` -> treat as `D:\path\...`
            let drive_path = format!("{}:{}", chars[1], &normalized[2..]);
            return PathBuf::from(drive_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        }
    }
    // Normal logical path: strip leading `/` and convert separators
    let stripped = normalized.strip_prefix('/').unwrap_or(&normalized);
    PathBuf::from(stripped.replace('/', std::path::MAIN_SEPARATOR_STR))
}

/// Try to strip a native source base path from a logical path string.
///
/// Handles the case where `logical_path` is an absolute native path
/// (e.g. `D:\source\subdir\file.txt` on Windows) and we need to extract
/// the relative part against `source_base` (e.g. `D:\source`).
///
/// Returns the relative logical path (with `/` separators) on success.
pub fn strip_source_base(logical_path: &str, source_base: &Path) -> Option<String> {
    let native = logical_to_native(logical_path);
    if native.starts_with(source_base) {
        let rel = native.strip_prefix(source_base).ok()?;
        let rel_str = to_logical_string(rel);
        let trimmed = rel_str.strip_prefix('/').unwrap_or(&rel_str);
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    }
}

/// Convert a native path to a null-terminated wide string for Win32 APIs.
///
/// On Windows, prepends `\\?\` for absolute paths longer than MAX_PATH to
/// bypass the 260-character limit. On non-Windows, returns `OsStr`-to-`u16`.
#[cfg(windows)]
pub fn to_wide_for_win32(path: &Path) -> Vec<u16> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;

    let s = path.to_string_lossy();
    // If the path is absolute and long, prepend \\?\
    let owned: OsString = if s.len() > 240
        && (s.starts_with('\\') || (s.len() > 3 && s.as_bytes()[1] == b':'))
    {
        OsString::from(format!("\\\\?\\{}", s))
    } else {
        path.as_os_str().to_os_string()
    };

    owned.encode_wide().chain(std::iter::once(0)).collect()
}

/// Check if a logical path string represents a root-level entry (no subdirectories).
///
/// e.g. `"/file.txt"` -> true, `"/dir/file.txt"` -> false.
pub fn is_root_entry(logical_path: &str) -> bool {
    let normalized = normalize_logical(logical_path);
    !normalized[1..].contains('/')
}

/// Map a metadata path (logical or native string) to a target filesystem path
/// by stripping a native source base prefix and joining with a native target base.
///
/// This is the cross-platform replacement for the `make_relative_and_join` function
/// that was copy-pasted across delete.rs, hardlink.rs, and mtime.rs.
///
/// - `source_base`: native filesystem path of the source root (e.g. `D:\source`).
/// - `target_base`: native filesystem path of the target root.
/// - `metadata_path`: path string from a metadata entry (may use `/` or mixed separators).
/// - `logical_paths`: whether entries use logical/virtual roots (NFS/SMB).
pub fn make_relative_and_join(
    source_base: &Path,
    target_base: PathBuf,
    metadata_path: &str,
    logical_paths: bool,
) -> PathBuf {
    // Normalize both paths to forward-slash logical form for comparison.
    // This avoids all platform-specific PathBuf comparison issues.
    let logical_path = normalize_logical(metadata_path);
    let logical_source = normalize_logical(&to_logical_string(source_base));

    // If logical_paths (NFS/SMB), path is relative to virtual root
    if logical_paths {
        let rel = logical_path.strip_prefix('/').unwrap_or(&logical_path);
        return target_base.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    }

    // Try stripping the source prefix from the logical path
    if let Some(rel) = logical_path.strip_prefix(&logical_source) {
        let rel = rel.strip_prefix('/').unwrap_or(rel);
        if !rel.is_empty() {
            return target_base.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        }
    }

    // Fallback: treat as relative path
    let rel = logical_path.strip_prefix('/').unwrap_or(&logical_path);
    target_base.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_logical() {
        assert_eq!(normalize_logical("/d1//d2/"), "/d1/d2");
        assert_eq!(normalize_logical("d1/d2"), "/d1/d2");
        assert_eq!(normalize_logical("/d1/./d2/../f1"), "/d1/f1");
        assert_eq!(normalize_logical("C:\\foo\\bar"), "/C:/foo/bar");
        assert_eq!(normalize_logical("/"), "/");
    }

    #[test]
    fn test_join_logical() {
        assert_eq!(join_logical("/dir", "file.txt"), "/dir/file.txt");
        assert_eq!(join_logical("/", "file.txt"), "/file.txt");
        assert_eq!(join_logical("/dir/", "file.txt"), "/dir/file.txt");
    }

    #[test]
    fn test_to_logical_string() {
        let p = Path::new("foo/bar");
        assert_eq!(to_logical_string(p), "foo/bar");

        #[cfg(windows)]
        {
            let p = Path::new("C:\\foo\\bar");
            assert_eq!(to_logical_string(p), "C:/foo/bar");
        }
    }

    #[test]
    fn test_is_root_entry() {
        assert!(is_root_entry("/file.txt"));
        assert!(is_root_entry("file.txt"));
        assert!(!is_root_entry("/dir/file.txt"));
    }

    #[test]
    fn test_make_relative_and_join_logical() {
        let target = PathBuf::from("/backup/target");
        // Logical paths mode (NFS/SMB): path is relative to virtual root
        let result = make_relative_and_join(
            Path::new("/opt/dataset"),
            target.clone(),
            "/docs/file.txt",
            true,
        );
        assert_eq!(result, PathBuf::from("/backup/target/docs/file.txt"));
    }

    #[test]
    #[cfg(unix)]
    fn test_make_relative_and_join_unix_native() {
        let source = Path::new("/home/user/source");
        let target = PathBuf::from("/backup/target");

        // Path stored as absolute native path
        let result = make_relative_and_join(
            source,
            target.clone(),
            "/home/user/source/docs/file.txt",
            false,
        );
        assert_eq!(result, PathBuf::from("/backup/target/docs/file.txt"));

        // Non-matching path falls back to relative
        let result = make_relative_and_join(
            source,
            target.clone(),
            "/other/path/file.txt",
            false,
        );
        assert_eq!(result, PathBuf::from("/backup/target/other/path/file.txt"));
    }

    #[test]
    #[cfg(windows)]
    fn test_make_relative_and_join_windows_native() {
        let source = Path::new(r"D:\datasets\source");
        let target = PathBuf::from(r"D:\datasets\backup\v2");

        // Logical path with drive letter — should strip source prefix
        let result = make_relative_and_join(
            source,
            target.clone(),
            "/D:/datasets/source/dir/file.txt",
            false,
        );
        assert_eq!(result, PathBuf::from(r"D:\datasets\backup\v2\dir\file.txt"));

        // Logical path relative to source — should join correctly
        let result = make_relative_and_join(
            source,
            target.clone(),
            "/dir/file.txt",
            false,
        );
        assert_eq!(result, PathBuf::from(r"D:\datasets\backup\v2\dir\file.txt"));

        // Exact scenario from the incremental test
        let source2 = Path::new(r"D:\datasets\local\test_xxx\source");
        let target2 = PathBuf::from(r"D:\datasets\local\test_xxx\backup\v2");
        let result = make_relative_and_join(
            source2,
            target2.clone(),
            "/D:/datasets/local/test_xxx/source/dir_0_0/dir_1_0/dir_2_0/file_3_1.dat",
            false,
        );
        assert_eq!(result, PathBuf::from(r"D:\datasets\local\test_xxx\backup\v2\dir_0_0\dir_1_0\dir_2_0\file_3_1.dat"));
    }
}
