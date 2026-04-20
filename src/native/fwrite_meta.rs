use std::path::Path;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Default)]
#[allow(dead_code)]
pub struct FileMeta {
    pub path: String,
    #[cfg(unix)]
    pub mode: Option<u32>,
    #[cfg(unix)]
    pub uid: Option<u32>,
    #[cfg(unix)]
    pub gid: Option<u32>,

    pub atime: Option<i64>, // seconds since Unix epoch
    pub mtime: Option<i64>,
    pub ctime: Option<i64>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub btime: Option<i64>, // birth time

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub xattrs: Option<std::collections::HashMap<String, Vec<u8>>>,

    #[cfg(target_os = "linux")]
    pub acl: Option<String>, // textual representation

    #[cfg(windows)]
    pub security_descriptor: Option<Vec<u8>>, // binary SD
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
fn get_xattrs(path: &Path) -> Option<std::collections::HashMap<String, Vec<u8>>> {
    match xattr::list(path) {
        Ok(attrs) => {
            let mut map = std::collections::HashMap::new();
            for attr in attrs {
                let attr_str = attr.to_string_lossy().into_owned();
                if let Ok(Some(value)) = xattr::get(path, &attr) {
                    map.insert(attr_str, value);
                }
            }
            Some(map)
        }
        Err(_) => None,
    }
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
fn get_acl_text(path: &Path) -> Option<String> {
    use exacl::getfacl;
    match getfacl(path, None) {
        Ok(acl_entries) => {
            // Convert ACL entries to a textual representation
            let text = acl_entries.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        Err(_) => None,
    }
}

#[cfg(unix)]
#[allow(dead_code)]
fn fill_unix_meta(meta: &mut FileMeta, path: &Path, md: &std::fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    meta.mode = Some(md.mode());
    meta.uid = Some(md.uid());
    meta.gid = Some(md.gid());

    // Timestamps
    use std::time::UNIX_EPOCH;
    meta.atime = Some(
        md.accessed()
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    );
    meta.mtime = Some(
        md.modified()
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    );
    // ctime is not directly available in std::fs::Metadata on all platforms
    // Using modified time as fallback
    meta.ctime = meta.mtime;

    // Birth time (Linux: requires statx; fallback not always available)
    #[cfg(target_os = "linux")]
    {
        // Use statx if available (glibc 2.28+, kernel 4.11+)
        // For simplicity, we skip birth time on Linux unless using advanced syscalls.
        // Many Rust programs omit btime on Linux due to portability.
        meta.btime = None;
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        meta.btime = Some(md.created().map(|t| {
            t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
        }).unwrap_or(0));
    }

    // Extended attributes
    meta.xattrs = get_xattrs(path);

    // ACLs (Linux only)
    #[cfg(target_os = "linux")]
    {
        meta.acl = get_acl_text(path);
    }
}

#[cfg(windows)]
fn fill_windows_meta(meta: &mut FileMeta, path: &Path, md: &std::fs::Metadata) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::fileapi::*;
    use winapi::um::winbase::*;
    use winapi::um::securitybase::*;
    use winapi::um::accctrl::*;
    use winapi::um::aclapi::*;
    use winapi::shared::minwindef::*;
    use winapi::shared::ntdef::NULL;

    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let handle = CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            NULL as *mut _,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS, // required for directories
            NULL,
        );

        if handle == INVALID_HANDLE_VALUE {
            return;
        }

        // Get timestamps from Metadata (already available)
        use std::time::UNIX_EPOCH;
        meta.atime = Some(
            md.accessed()
                .unwrap_or(UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        );
        meta.mtime = Some(
            md.modified()
                .unwrap_or(UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        );
        meta.ctime = Some(
            md.created()
                .unwrap_or(UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        );

        // Security Descriptor
        let mut sd_size: DWORD = 0;
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            NULL as *mut _,
            NULL as *mut _,
            NULL as *mut _,
            NULL as *mut _,
            &mut sd_size as *mut _,
        );

        let mut buffer = vec![0u8; sd_size as usize];
        let psd = buffer.as_mut_ptr() as *mut BYTE;
        if GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            NULL as *mut _,
            NULL as *mut _,
            NULL as *mut _,
            NULL as *mut _,
            &mut psd as *mut _,
        ) == 0 {
            meta.security_descriptor = Some(buffer);
        }

        CloseHandle(handle);
    }
}

#[allow(dead_code)]
pub fn read_file_meta<P: AsRef<Path>>(path: P) -> std::io::Result<FileMeta> {
    let path = path.as_ref();
    let md = std::fs::metadata(path)?;

    let mut meta = FileMeta {
        path: path.to_string_lossy().into_owned(),
        ..Default::default()
    };

    #[cfg(unix)]
    fill_unix_meta(&mut meta, path, &md);

    #[cfg(windows)]
    fill_windows_meta(&mut meta, path, &md);

    Ok(meta)
}
