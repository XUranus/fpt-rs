//! # File Metadata Models
//!
//! This module defines shared data structures used to represent file system metadata
//! during both the **scanning** and **backup** phases of the backup system.
//!
//! These models capture essential attributes of files and directories in a
//! cross-platform manner (supporting both Unix-like systems and Windows),
//! including:
//! - Basic metadata (timestamps, permissions, device/inode info)
//! - Security and access control (ACLs, Windows security descriptors)
//! - Special file properties (symlinks, extended attributes, sparseness)
//! - Batched scan results for efficient I/O and serialization
//!
//! All structures are `serde`-serializable to enable persistence to disk
//! (e.g., for checkpointing or incremental backup catalogs) and are designed
//! to be cloned efficiently where needed in multi-threaded pipelines.

use serde;

/// Common metadata shared by both files and directories.
///
/// This structure abstracts platform-specific identifiers and attributes into
/// a unified cross-platform representation:
/// - On **Unix/Linux**: `id` is the inode number, `mode` is the `mode_t` bits,
///   and ACLs/xattrs follow POSIX conventions.
/// - On **Windows**: `id` is the file index (from `FILE_ID_INFO`), `attr` holds
///   `FILE_ATTRIBUTE_*` flags, and security is represented via SDDL strings.
///
/// Timestamps are stored in **seconds since Unix epoch** (UTC) for simplicity
/// and compatibility, though sub-second precision is not preserved.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Default)]
pub struct MetaCommon {
    /// Unique identifier: inode (Unix) or file index (Windows).
    pub id: u64,
    /// File type and permission bits (`mode_t` on Unix).
    pub mode: u32,
    /// File attributes (`DWORD` flags like `FILE_ATTRIBUTE_*` on Windows).
    pub attr: u32,
    /// Last access time in seconds since Unix epoch.
    pub atime: u32,
    /// Creation time in seconds since Unix epoch.
    /// Note: On Unix, this typically maps to `stat.st_ctime` (status change time),
    /// not true creation time.
    pub ctime: u32,
    /// Last modification time in seconds since Unix epoch.
    pub mtime: u32,
    /// Device number (major/minor combined); useful for detecting mount boundaries.
    pub devno: u64,

    /// Base name of the file or directory (without parent path).
    pub name: String,
    /// Windows-only: Security descriptor in SDDL string format.
    /// `None` on non-Windows platforms.
    pub security_descriptor: Option<String>,
    /// POSIX access ACL in text form (e.g., from `acl_to_text`).
    /// `None` if not present or on non-POSIX systems.
    pub posix_access_acl: Option<String>,
    /// POSIX default ACL (for directories) in text form.
    /// `None` if not present, not a directory, or on non-POSIX systems.
    pub posix_default_acl: Option<String>,
    /// Target path if this entry is a symbolic link.
    /// `None` for regular files/directories.
    pub symlink_target_path: Option<String>,
    /// Extended attributes (xattrs) serialized as a string (format TBD).
    /// Typically base64-encoded or custom delimited format.
    pub xattributes: Option<String>,
}

/// Metadata specific to a regular file.
///
/// Extends [`MetaCommon`] with file-size, link count, and sparseness information.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Default)]
pub struct FileMeta {
    /// Shared common metadata.
    pub common: MetaCommon,
    /// Logical file size in bytes (as reported by `stat.st_size`).
    pub size: u64,
    /// Number of hard links to this inode.
    pub links: u64,
    /// List of sparse regions as `(offset, length)` tuples.
    /// Each region represents a hole (unallocated range) in the file.
    /// `None` if sparseness was not checked or the file is not sparse.
    pub sparse_range: Option<Vec<(u64, u64)>>,
}

/// Metadata specific to a directory.
///
/// Extends [`MetaCommon`] with the full path (for context during backup).
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Default)]
pub struct DirMeta {
    /// Shared common metadata.
    pub common: MetaCommon,
    /// Full absolute path of the directory (used during recovery or validation).
    pub path: String,
}
