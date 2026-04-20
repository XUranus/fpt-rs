// src/scanner/fstat.rs

// Enable required features for nix
#[cfg(all(unix, not(target_os = "windows")))]
use nix::sys::stat::{SFlag, FileStat};
#[cfg(all(unix, target_os = "linux"))]
use xattr;
#[cfg(all(unix, target_os = "linux"))]
use base64::Engine as _;

use std::path::{Path, PathBuf};

use super::super::scanner::metadata::{DirMeta, FileMeta, MetaCommon};


#[cfg(unix)]
fn stat_common(path: &Path, is_dir: bool) -> std::io::Result<MetaCommon> {
    use nix::sys::stat::lstat;
    
    // Use lstat to not follow symlinks (we want to stat the symlink itself)
    let p = path
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid UTF-8 path"))?;

    let metadata: FileStat = lstat(p)
        .map_err(|e| std::io::Error::from(e))?; // nix::Error → std::io::Error

    let name = path
        .file_name()
        .map_or_else(|| String::from(""), |s| s.to_string_lossy().into_owned());

    let mut common = MetaCommon {
        id: metadata.st_ino,
        mode: metadata.st_mode,
        attr: 0,
        atime: metadata.st_atime as u32,
        ctime: metadata.st_ctime as u32,
        mtime: metadata.st_mtime as u32,
        devno: metadata.st_dev,
        name,
        security_descriptor: None,
        posix_access_acl: None,
        posix_default_acl: None,
        symlink_target_path: None,
        xattributes: None,
    };

    // Check if it's a symlink
    let file_type = metadata.st_mode & SFlag::S_IFMT.bits();
    if file_type == SFlag::S_IFLNK.bits() {
        if let Ok(target) = std::fs::read_link(path) {
            common.symlink_target_path = Some(target.to_string_lossy().into_owned());
        }
    }

    // Linux: extended attributes and ACL
    #[cfg(target_os = "linux")]
    {
        if let Ok(xattrs_str) = get_xattrs_as_string(path) {
            if !xattrs_str.is_empty() {
                common.xattributes = Some(xattrs_str);
            }
        }
        
        // Get ACL (access and default for directories)
        if let Ok((access_acl, default_acl)) = get_acl_text(path, is_dir) {
            if !access_acl.is_empty() {
                common.posix_access_acl = Some(access_acl);
            }
            if !default_acl.is_empty() {
                common.posix_default_acl = Some(default_acl);
            }
        }
    }

    Ok(common)
}

#[cfg(all(unix, target_os = "linux"))]
fn get_xattrs_as_string(path: &Path) -> std::io::Result<String> {
    let mut pairs = Vec::new();

    // xattr::list returns Vec<OsString>
    if let Ok(names) = xattr::list(path) {
        for name_os in names {
            // Convert OsString to string lossily (xattr names are usually ASCII)
            let name_str = name_os.to_string_lossy().into_owned();

            // Get value as Vec<u8>
            if let Ok(Some(value)) = xattr::get(path, name_os) {
                // Encode value in base64 (safe for binary data)
                let b64_value = base64::engine::general_purpose::STANDARD.encode(&value);
                pairs.push(format!("{}={}", name_str, b64_value));
            }
        }
    }

    Ok(pairs.join("\n"))
}

/// Get ACL text representation for a file or directory
/// Returns (access_acl, default_acl) where default_acl is only for directories
#[cfg(all(unix, target_os = "linux"))]
fn get_acl_text(path: &Path, is_dir: bool) -> std::io::Result<(String, String)> {
    use exacl::getfacl;
    
    let mut access_acl = String::new();
    let mut default_acl = String::new();
    
    match getfacl(path, None) {
        Ok(acl_entries) => {
            for entry in acl_entries {
                let entry_str = entry.to_string();
                // Check if it's a default ACL (starts with "default:")
                if entry_str.starts_with("default:") {
                    if is_dir {
                        if !default_acl.is_empty() {
                            default_acl.push('\n');
                        }
                        default_acl.push_str(&entry_str);
                    }
                } else {
                    if !access_acl.is_empty() {
                        access_acl.push('\n');
                    }
                    access_acl.push_str(&entry_str);
                }
            }
        }
        Err(_) => {
            // ACL not supported or not available
        }
    }
    
    Ok((access_acl, default_acl))
}


#[cfg(windows)]
fn stat_common(path: &Path, is_dir: bool) -> std::io::Result<MetaCommon> {
    use windows::{
        Win32::Foundation::*,
        Win32::Storage::FileSystem::*,
        Win32::Security::*,
    };
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let hfile = CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        );

        if hfile == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }

        let mut basic_info = FILE_BASIC_INFO::default();
        let mut internal_info = FILE_INTERNAL_INFO::default();

        let ok1 = GetFileInformationByHandleEx(hfile, FileBasicInfo, &mut basic_info as *mut _ as _, std::mem::size_of::<FILE_BASIC_INFO>() as u32);
        let ok2 = GetFileInformationByHandleEx(hfile, FileInternalInfo, &mut internal_info as *mut _ as _, std::mem::size_of::<FILE_INTERNAL_INFO>() as u32);

        if !(ok1.as_bool() && ok2.as_bool()) {
            CloseHandle(hfile);
            return Err(std::io::Error::last_os_error());
        }

        let mut common = MetaCommon {
            id: internal_info.IndexNumber,
            mode: 0,
            attr: basic_info.FileAttributes,
            atime: filetime_to_u32(basic_info.LastAccessTime),
            ctime: filetime_to_u32(basic_info.CreationTime),
            mtime: filetime_to_u32(basic_info.LastWriteTime),
            devno: 0,
            name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            security_descriptor: None,
            posix_access_acl: None,
            posix_default_acl: None,
            symlink_target_path: None,
            xattributes: None,
        };

        // Security descriptor (SDDL)
        if let Ok(sd) = get_security_descriptor_sddl(hfile) {
            common.security_descriptor = Some(sd);
        }

        // Reparse point (symlink/junction)
        if (basic_info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0) != 0 {
            if let Ok(target) = std::fs::read_link(path) {
                common.symlink_target_path = Some(target.to_string_lossy().into_owned());
            }
        }

        CloseHandle(hfile);
        Ok(common)
    }
}

#[cfg(windows)]
fn filetime_to_u32(ft: windows::Win32::Foundation::FILETIME) -> u32 {
    const WINDOWS_TICK: u64 = 10_000_000;
    const SEC_TO_UNIX_EPOCH: u64 = 11_644_473_600;
    let ticks = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
    let seconds = ticks / WINDOWS_TICK - SEC_TO_UNIX_EPOCH;
    seconds as u32
}

#[cfg(windows)]
fn get_security_descriptor_sddl(hfile: HANDLE) -> std::io::Result<String> {
    use windows::Win32::Security::*;
    use windows::Win32::Foundation::*;

    unsafe {
        let mut size = 0u32;
        GetKernelObjectSecurity(hfile, OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, None, 0, &mut size);
        let mut sd = vec![0u8; size as usize];
        if !GetKernelObjectSecurity(
            hfile,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(sd.as_mut_ptr() as *mut _),
            size,
            &mut size,
        ).as_bool() {
            return Err(std::io::Error::last_os_error());
        }

        let mut sddl = PWSTR::null();
        if !ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd.as_ptr() as *const _,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut sddl,
            None,
        ).as_bool() {
            return Err(std::io::Error::last_os_error());
        }

        let result = if !sddl.is_null() {
            let len = (0..).take_while(|&i| *sddl.0.add(i) != 0).count();
            let slice = std::slice::from_raw_parts(sddl.0, len);
            String::from_utf16_lossy(slice)
        } else {
            String::new()
        };

        LocalFree(HLOCAL(sddl.0 as _));
        Ok(result)
    }
}

pub fn stat_dir(path: &PathBuf) -> std::io::Result<DirMeta> {
    let common = stat_common(path, true)?;
    Ok(DirMeta {
        common,
        path : path.to_string_lossy().into_owned()
    })
}

/// Detect sparse file holes by analyzing file extents
/// Returns a vector of (offset, length) tuples representing holes
#[cfg(all(unix, target_os = "linux"))]
fn detect_sparse_ranges(path: &Path) -> Option<Vec<(u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    
    // Check if file is actually sparse by comparing size vs blocks used
    let metadata = std::fs::metadata(path).ok()?;
    let size = metadata.len();
    let blocks = metadata.blocks() as u64;
    let block_size = 512u64; // Standard block size for st_blocks
    
    let apparent_size = size;
    let actual_size = blocks * block_size;
    
    // If apparent size is significantly larger than actual size, it's sparse
    if apparent_size <= actual_size || apparent_size == 0 {
        return None;
    }
    
    // Try to use FIEMAP ioctl to get exact hole locations
    // For now, we return a simple representation: one hole at the end
    // A full implementation would use ioctl(FIEMAP) to get precise extents
    let hole_start = actual_size;
    let hole_len = apparent_size - actual_size;
    
    if hole_len > 0 {
        Some(vec![(hole_start, hole_len)])
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_sparse_ranges(_path: &Path) -> Option<Vec<(u64, u64)>> {
    None
}

pub fn stat_file(path: &PathBuf) -> std::io::Result<FileMeta> {
    let common = stat_common(path, false)?;

    let size = if path.is_file() {
        let metadata = path.metadata()?;
        metadata.len()
    } else {
        0
    };

    let links = if path.is_file() {
        let metadata = path.metadata()?;
        if cfg!(unix) {
            use std::os::unix::fs::MetadataExt;
            metadata.nlink() as u64
        } else {
            1 // TODO::Windows doesn't expose link count easily without syscalls
        }
    } else {
        1
    };
    
    // Detect sparse file ranges
    let sparse_range = detect_sparse_ranges(path);

    Ok(FileMeta {
        common,
        size,
        links,
        sparse_range,
    })
}
