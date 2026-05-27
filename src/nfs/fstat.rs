//! Conversion from NFSv3 file attributes ([`fattr3`]) to Fpt metadata types.
//!
//! These functions map the NFS server's view of a file/directory onto the same
//! [`FileMeta`] / [`DirMeta`] structures that the local filesystem scanner produces,
//! so that downstream metadata writers and control file generators need no changes.

use nfs3_client::nfs3_types::nfs3::fattr3;

use crate::scanner::metadata::{DirMeta, FileMeta, MetaCommon};

/// Convert NFS file attributes to a [`FileMeta`].
///
/// # Arguments
/// * `attrs` – `fattr3` returned by `getattr` or from a `readdirplus` entry.
/// * `name`  – The base name of the file (not a full path).
/// * `symlink_target` – Pre-resolved symlink target, if this entry is `NF3LNK`.
pub fn nfs_fattr3_to_file_meta(
    attrs: &fattr3,
    name: &str,
    symlink_target: Option<String>,
) -> FileMeta {
    FileMeta {
        common: MetaCommon {
            id: attrs.fileid,
            mode: attrs.mode,
            attr: 0,
            atime: attrs.atime.seconds,
            ctime: attrs.ctime.seconds,
            mtime: attrs.mtime.seconds,
            devno: 0,
            name: name.to_string(),
            security_descriptor: None,
            posix_access_acl: None,
            posix_default_acl: None,
            symlink_target_path: symlink_target,
            xattributes: None,
        },
        size: attrs.size,
        links: u64::from(attrs.nlink),
        sparse_range: None,
    }
}

/// Convert NFS file attributes to a [`DirMeta`].
///
/// # Arguments
/// * `attrs` – `fattr3` returned by `getattr` or from a `readdirplus` entry.
/// * `path`  – Full path of the directory (used in [`DirMeta::path`]).
/// * `name`  – Base name of the directory.
pub fn nfs_fattr3_to_dir_meta(attrs: &fattr3, path: &str, name: &str) -> DirMeta {
    DirMeta {
        common: MetaCommon {
            id: attrs.fileid,
            mode: attrs.mode,
            attr: 0,
            atime: attrs.atime.seconds,
            ctime: attrs.ctime.seconds,
            mtime: attrs.mtime.seconds,
            devno: 0,
            name: name.to_string(),
            security_descriptor: None,
            posix_access_acl: None,
            posix_default_acl: None,
            symlink_target_path: None,
            xattributes: None,
        },
        path: path.to_string(),
    }
}
