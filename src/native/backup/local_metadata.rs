use std::io;
use std::path::Path;

use log::error;

use crate::scanner::metadata::MetaCommon;

pub(crate) fn restore_common_metadata(path: &Path, meta: &MetaCommon) {
    restore_xattrs(path, &meta.xattributes);
    restore_acl(path, &meta.posix_access_acl, &meta.posix_default_acl);
    #[cfg(windows)]
    {
        restore_windows_attrs(path, meta.attr);
        restore_windows_sd(path, &meta.security_descriptor);
    }
}

/// Mark a file as sparse on Windows (call before writing content).
/// Returns true if successfully marked.
#[cfg(windows)]
pub(crate) fn mark_file_sparse(path: &Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::fs::OpenOptionsExt;

    const FSCTL_SET_SPARSE: u32 = 0x000900C4;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;

    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
    else {
        return false;
    };

    unsafe {
        use windows::Win32::Foundation::HANDLE;

        let handle = HANDLE(file.as_raw_handle() as _);
        let mut bytes_returned: u32 = 0;
        // Call DeviceIoControl via windows-targets
        extern "system" {
            fn DeviceIoControl(
                hdevice: windows::Win32::Foundation::HANDLE,
                dwiocontrolcode: u32,
                lpinbuffer: *const core::ffi::c_void,
                ninbuffersize: u32,
                lpoutbuffer: *mut core::ffi::c_void,
                noutbuffersize: u32,
                lpbytesreturned: *mut u32,
                lpoverlapped: *mut core::ffi::c_void,
            ) -> windows::Win32::Foundation::BOOL;
        }

        let result = DeviceIoControl(
            handle,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        );
        result.as_bool()
    }
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

/// Restore Windows security descriptor from an SDDL string.
///
/// Converts the SDDL to a binary SD, extracts DACL/owner/group,
/// and applies them via `SetNamedSecurityInfoW`.
#[cfg(windows)]
fn restore_windows_sd(path: &Path, sddl: &Option<String>) {
    let Some(sddl_str) = sddl else { return };
    if sddl_str.is_empty() {
        return;
    }

    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::*;
    use windows::Win32::Security::*;
    use windows::Win32::Security::Authorization::*;

    unsafe {
        // Convert SDDL string to binary security descriptor
        let sddl_w: Vec<u16> = sddl_str
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut sd_ptr = PSECURITY_DESCRIPTOR::default();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            windows::core::PCWSTR(sddl_w.as_ptr()),
            SDDL_REVISION_1,
            &mut sd_ptr,
            None,
        )
        .is_err()
        {
            error!("Failed to convert SDDL for {:?}: {}", path, sddl_str);
            return;
        }

        // Extract owner, group, DACL from the SD
        let mut owner_sid: PSID = PSID::default();
        let mut owner_defaulted = BOOL::default();
        let has_owner = GetSecurityDescriptorOwner(sd_ptr, &mut owner_sid, &mut owner_defaulted).is_ok();

        let mut group_sid: PSID = PSID::default();
        let mut group_defaulted = BOOL::default();
        let has_group = GetSecurityDescriptorGroup(sd_ptr, &mut group_sid, &mut group_defaulted).is_ok();

        let mut dacl_present = BOOL::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut dacl_defaulted = BOOL::default();
        let _ = GetSecurityDescriptorDacl(sd_ptr, &mut dacl_present, &mut dacl, &mut dacl_defaulted);

        // Determine what to set
        let mut se_info = OBJECT_SECURITY_INFORMATION(0);
        let p_owner = if has_owner {
            se_info |= OWNER_SECURITY_INFORMATION;
            PSID(owner_sid.0)
        } else {
            PSID::default()
        };
        let p_group = if has_group {
            se_info |= GROUP_SECURITY_INFORMATION;
            PSID(group_sid.0)
        } else {
            PSID::default()
        };
        let p_dacl = if dacl_present.as_bool() && !dacl.is_null() {
            se_info |= DACL_SECURITY_INFORMATION;
            Some(dacl as *const ACL)
        } else {
            None
        };

        if se_info.0 == 0 {
            let _ = LocalFree(HLOCAL(sd_ptr.0 as _));
            return;
        }

        // Apply via SetNamedSecurityInfoW
        let path_w: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let result = SetNamedSecurityInfoW(
            windows::core::PCWSTR(path_w.as_ptr()),
            SE_FILE_OBJECT,
            se_info,
            p_owner,
            p_group,
            p_dacl,
            None,
        );

        if result != ERROR_SUCCESS {
            error!(
                "Failed to set security info on {:?}: Win32 error {}",
                path, result.0
            );
        }

        let _ = LocalFree(HLOCAL(sd_ptr.0 as _));
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
