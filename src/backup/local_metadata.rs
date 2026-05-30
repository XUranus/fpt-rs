use std::io;
use std::path::Path;

use log::error;

use crate::scanner::metadata::MetaCommon;

pub(crate) fn restore_common_metadata(path: &Path, meta: &MetaCommon) {
    restore_xattrs(path, &meta.xattributes);
    restore_acl(path, &meta.posix_access_acl, &meta.posix_default_acl);
    #[cfg(windows)]
    restore_windows_attrs(path, meta.attr);
}

#[cfg(target_os = "linux")]
fn restore_xattrs(path: &Path, xattrs: &Option<String>) {
    use base64::Engine as _;

    if let Some(xattr_str) = xattrs {
        for line in xattr_str.lines() {
            if let Some((name, b64_value)) = line.split_once('=') {
                if let Ok(value) = base64::engine::general_purpose::STANDARD.decode(b64_value) {
                    if let Err(e) = xattr::set(path, name, &value) {
                        error!("Failed to set xattr {} on {:?}: {}", name, path, e);
                    }
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_xattrs(_path: &Path, _xattrs: &Option<String>) {}

#[cfg(target_os = "linux")]
fn restore_acl(path: &Path, access_acl: &Option<String>, default_acl: &Option<String>) {
    use exacl::{setfacl, AclEntry};

    let mut acl_entries = Vec::new();

    if let Some(acl_str) = access_acl {
        for line in acl_str.lines() {
            if let Ok(entry) = line.parse::<AclEntry>() {
                acl_entries.push(entry);
            }
        }
    }

    if let Some(acl_str) = default_acl {
        for line in acl_str.lines() {
            if let Ok(entry) = line.parse::<AclEntry>() {
                acl_entries.push(entry);
            }
        }
    }

    if !acl_entries.is_empty() {
        if let Err(e) = setfacl(&[path], &acl_entries, None) {
            error!("Failed to set ACL on {:?}: {}", path, e);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_acl(_path: &Path, _access_acl: &Option<String>, _default_acl: &Option<String>) {}

#[cfg(windows)]
fn restore_windows_attrs(path: &Path, attr: u32) {
    if attr == 0 {
        return;
    }
    unsafe {
        use windows::Win32::Storage::FileSystem::{
            SetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES,
        };
        use windows::core::PCWSTR;
        let wide = crate::path_util::to_wide_for_win32(path);
        // Retry a few times — the file handle may not be fully released yet
        for _ in 0..3 {
            if SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(attr)).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        error!("Failed to set file attributes on {:?} (attr=0x{:x})", path, attr);
    }
}

pub(crate) fn create_symlink(dst_path: &Path, target: &str) -> io::Result<()> {
    if dst_path.exists() {
        std::fs::remove_file(dst_path)?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dst_path)
    }
    #[cfg(windows)]
    {
        // On Windows, directory symlinks require symlink_dir, not symlink_file.
        // Try symlink_dir first if the target looks like a directory path.
        let is_dir_target = target.ends_with('/')
            || target.ends_with('\\')
            || Path::new(target).is_dir();
        if is_dir_target {
            std::os::windows::fs::symlink_dir(target, dst_path)
                .or_else(|_| std::os::windows::fs::symlink_file(target, dst_path))
        } else {
            std::os::windows::fs::symlink_file(target, dst_path)
        }
    }
}
