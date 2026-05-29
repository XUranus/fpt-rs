//! Conversion helpers from SMB directory/file information to Fpt metadata.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::UNIX_EPOCH;

use smb_client::binrw_util::prelude::FileTime;

use crate::scanner::metadata::{DirMeta, FileMeta, MetaCommon};

#[derive(Debug, Clone)]
pub struct SmbDirSeed {
    pub id: u64,
    pub attr: u32,
    pub ctime: u32,
    pub atime: u32,
    pub mtime: u32,
    pub name: String,
}

pub fn smb_all_info_to_dir_meta(
    info: &smb_client::FileAllInformation,
    path: &str,
    devno: u64,
) -> DirMeta {
    let name = file_name_from_path(path);
    DirMeta {
        common: MetaCommon {
            id: info.internal.index_number,
            mode: synthesize_mode(
                info.basic.file_attributes.directory(),
                info.basic.file_attributes.readonly(),
            ),
            attr: file_attributes_to_u32(info.basic.file_attributes),
            atime: filetime_to_unix_seconds(info.basic.last_access_time),
            ctime: filetime_to_unix_seconds(info.basic.change_time),
            mtime: filetime_to_unix_seconds(info.basic.last_write_time),
            devno,
            name,
            security_descriptor: None,
            posix_access_acl: None,
            posix_default_acl: None,
            symlink_target_path: None,
            xattributes: None,
        },
        path: path.to_string(),
    }
}

pub fn smb_dir_seed_from_entry(entry: &smb_client::FileIdBothDirectoryInformation) -> SmbDirSeed {
    SmbDirSeed {
        id: entry.file_id,
        attr: file_attributes_to_u32(entry.file_attributes),
        ctime: filetime_to_unix_seconds(entry.change_time),
        atime: filetime_to_unix_seconds(entry.last_access_time),
        mtime: filetime_to_unix_seconds(entry.last_write_time),
        name: entry.file_name.to_string(),
    }
}

pub fn smb_seed_to_dir_meta(seed: &SmbDirSeed, path: &str, devno: u64) -> DirMeta {
    DirMeta {
        common: MetaCommon {
            id: seed.id,
            mode: synthesize_mode(true, seed.attr & 0x1 != 0),
            attr: seed.attr,
            atime: seed.atime,
            ctime: seed.ctime,
            mtime: seed.mtime,
            devno,
            name: seed.name.clone(),
            security_descriptor: None,
            posix_access_acl: None,
            posix_default_acl: None,
            symlink_target_path: None,
            xattributes: None,
        },
        path: path.to_string(),
    }
}

pub fn smb_dir_info_to_file_meta(
    entry: &smb_client::FileIdBothDirectoryInformation,
    devno: u64,
    symlink_target: Option<String>,
    links: u64,
) -> FileMeta {
    FileMeta {
        common: MetaCommon {
            id: entry.file_id,
            mode: synthesize_mode(false, entry.file_attributes.readonly()),
            attr: file_attributes_to_u32(entry.file_attributes),
            atime: filetime_to_unix_seconds(entry.last_access_time),
            ctime: filetime_to_unix_seconds(entry.change_time),
            mtime: filetime_to_unix_seconds(entry.last_write_time),
            devno,
            name: entry.file_name.to_string(),
            security_descriptor: None,
            posix_access_acl: None,
            posix_default_acl: None,
            symlink_target_path: symlink_target,
            xattributes: None,
        },
        size: entry.end_of_file,
        links,
        sparse_range: None,
    }
}

pub fn share_devno(host: &str, share: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    host.hash(&mut hasher);
    share.hash(&mut hasher);
    hasher.finish()
}

fn file_name_from_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn file_attributes_to_u32(attrs: smb_client::FileAttributes) -> u32 {
    u32::from_le_bytes(attrs.into_bytes())
}

fn filetime_to_unix_seconds(ft: FileTime) -> u32 {
    let system_time = std::time::SystemTime::from(ft);
    match system_time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().min(u32::MAX as u64) as u32,
        Err(_) => 0,
    }
}

fn synthesize_mode(is_dir: bool, readonly: bool) -> u32 {
    let perms: u32 = if readonly { 0o555 } else { 0o755 };
    if is_dir {
        libc::S_IFDIR as u32 | perms
    } else {
        let file_perms: u32 = if readonly { 0o444 } else { 0o644 };
        libc::S_IFREG as u32 | file_perms
    }
}
