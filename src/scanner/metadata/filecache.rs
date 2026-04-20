//! # Filesystem Metadata Index Entries
//!
//! This module defines compact, fixed-size index entries used to accelerate
//! incremental backup and diff operations by enabling efficient comparison
//! of filesystem states across backup versions.
//!
//! Two primary index types are provided:
//! - [`FileCacheEntry`]: Represents a file, indexed by its unique ID (inode/index).
//! - [`DirCacheEntry`]: Represents a directory, including metadata and a pointer
//!   to the range of files it contains in the file index.
//!
//! Both entries store:
//! - A **unique ID** (inode on Unix/Linux, file index on Windows).
//! - A **32-bit hash** of the full serialized metadata (`FileMeta`/`DirMeta`), used
//!   to detect modifications.
//! - A [`MetaEntryLocator`] pointing to the full metadata in the metadata repository.
//!
//! The entries are stored in **sorted order by ID** in dense, sequential binary files
//! (`fcache_*` for files, `dcache_*` for directories). This layout enables:
//! - **Fast binary search** for lookups.
//! - **Efficient diffing** between backup versions by comparing hashes.
//! - **Range-based file enumeration** for a given directory (via `files_count`,
//!   `fcache_fid`, and `fcache_offset` in `DirCacheEntry`).

use bincode;
use sha2::{Digest, Sha256};

use super::{DirMeta, FileMeta, MetaEntryLocator};

/// A trait for types with a known compile-time size.
///
/// Used to solve the serialization padding size mismatch issue,
/// and document the expected on-disk size of index entries.
pub trait FixedSize {
    /// The size of the type in bytes.
    const SIZE: usize;
}

/// An index entry for a file, used in file cache (`fcache`) files.
///
/// Stored in `fcache_*` files sorted by `id`. Enables O(log n) lookup and efficient
/// diffing via the `hash` field.
#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileCacheEntry {
    /// Unique identifier: inode (Unix) or file index (Windows).
    pub id: u64,
    /// 32-bit hash of the serialized `FileMeta` (first 4 bytes of SHA-256).
    pub hash: u32,
    /// Locator for the full `FileMeta` in the metadata repository.
    pub meta_loc: MetaEntryLocator,
}

/// An index entry for a directory, used in directory cache (`dcache`) files.
///
/// Stored in `dcache_*` files sorted by `id`. In addition to metadata, it
/// provides a **pointer to the contiguous block of `FileCacheEntry` records**
/// that belong to this directory in the file cache.
#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DirCacheEntry {
    /// Unique identifier: inode (Unix) or file index (Windows).
    pub id: u64,
    /// 32-bit hash of the serialized `DirMeta` (first 4 bytes of SHA-256).
    pub hash: u32,
    /// Locator for the full `DirMeta` in the metadata repository.
    pub meta_loc: MetaEntryLocator,
    /// Number of files directly contained in this directory.
    pub files_count: u32,
    /// ID of the `fcache` file containing the first `FileCacheEntry` for this directory.
    pub fcache_fid: u32,
    /// Byte offset of the first `FileCacheEntry` for this directory within `fcache_<fcache_fid>.dat`.
    pub fcache_offset: u32,
}

// Ensure the declared sizes match the actual struct sizes.
// const _: () = assert!(std::mem::size_of::<FileCacheEntry>() == 20);
// const _: () = assert!(std::mem::size_of::<DirCacheEntry>() == 32);

impl FixedSize for FileCacheEntry {
    const SIZE: usize = 20;
}

impl FixedSize for DirCacheEntry {
    const SIZE: usize = 32;
}

impl From<FileMeta> for FileCacheEntry {
    /// Converts a `FileMeta` into a `FileCacheEntry`.
    ///
    /// Computes a 32-bit hash of the serialized metadata and extracts the ID.
    /// The `meta_loc` field is initialized to `(0, 0)` and must be set later
    /// once the metadata is written to the repository.
    fn from(fmeta: FileMeta) -> Self {
        let bytes = bincode::serialize(&fmeta)
            .expect("Failed to serialize FileMeta (should be serializable)");
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let result = hasher.finalize();
        let hash = u32::from_le_bytes(result[..4].try_into().unwrap());

        Self {
            id: fmeta.common.id,
            hash,
            meta_loc: (0, 0),
        }
    }
}

impl From<DirMeta> for DirCacheEntry {
    /// Converts a `DirMeta` into a `DirCacheEntry`.
    ///
    /// Computes a 32-bit hash of the serialized metadata and extracts the ID.
    /// The `meta_loc`, `fcache_fid`, and `fcache_offset` fields are initialized
    /// to zero and must be updated during index finalization.
    fn from(dmeta: DirMeta) -> Self {
        let bytes = bincode::serialize(&dmeta)
            .expect("Failed to serialize DirMeta (should be serializable)");
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let result = hasher.finalize();
        let hash = u32::from_le_bytes(result[..4].try_into().unwrap());

        Self {
            id: dmeta.common.id,
            hash,
            meta_loc: (0, 0),
            files_count: 0,
            fcache_fid: 0,
            fcache_offset: 0,
        }
    }
}
