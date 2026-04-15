//! # Scanner Configuration
//!
//! This module defines the `ScanOption` struct, which encapsulates all configurable
//! parameters for the filesystem scanning engine used in the backup system.
//!
//! The scanner supports:
//! - Parallel traversal with configurable thread counts.
//! - Selective metadata collection (ACLs, xattrs, hard links, etc.).
//! - Depth-limited recursion and symlink handling.
//! - Spillable in-memory queues with disk fallback for handling massive directory trees.
//! - Separation of output paths for control files and metadata storage.
//!
//! Configuration is built using a **builder pattern** for ergonomic and readable setup.

use std::path::PathBuf;

/// Configuration options for the filesystem scanner.
#[derive(Debug, Clone)]
pub struct ScanOption {
    /* Basic configuration */
    /// Maximum depth of directory traversal.
    ///
    /// - `None`: Unlimited depth (default).
    /// - `Some(0)`: Only scan the specified root directory (no subdirectories).
    /// - `Some(n)`: Traverse up to `n` levels deep.
    pub max_depth: Option<usize>,

    /// Number of worker threads used for parallel filesystem traversal.
    ///
    /// Default: 4.
    pub worker_count: usize,

    /// Number of writer threads used for serializing metadata to disk.
    ///
    /// Default: 4.
    pub writer_count: usize,

    /// Output directories for generated artifacts.
    pub target_dir: TargetDirOption,

    /// Metadata collection preferences.
    pub meta_option: MetaScanOption,

    /// Configuration for the spillable work queue.
    pub queue_option: QueueOption,
}

/// Output directory configuration.
#[derive(Debug, Clone)]
pub struct TargetDirOption {
    /// Directory path where **control files** (e.g., diff lists) are stored.
    pub ctrl_dir: PathBuf,

    /// Directory path where **metadata files** (serialized `FileMeta`/`DirMeta`) are stored.
    pub meta_dir: PathBuf,
}

/// Metadata scanning preferences.
#[derive(Debug, Clone)]
pub struct MetaScanOption {
    /// Whether to collect POSIX Access Control Lists (ACLs).
    ///
    /// Disabled by default for performance and portability.
    pub scan_acl: bool,

    /// Whether to collect extended attributes (xattrs).
    ///
    /// Disabled by default; may require elevated privileges on some systems.
    pub scan_xattrs: bool,

    /// Whether to resolve and record hard link information (e.g., link count, inode reuse).
    ///
    /// Disabled by default; useful for accurate deduplication.
    pub scan_hardlinks: bool,

    /// Whether to include hidden files and directories (those starting with `.` on Unix).
    ///
    /// Disabled by default.
    pub scan_hidden: bool,

    /// Whether to follow symbolic links during traversal.
    ///
    /// **Warning**: Enabling this may cause infinite loops in cyclic directory structures.
    /// Disabled by default for safety.
    pub follow_symlinks: bool,
}

/// Configuration for the spillable in-memory work queue.
///
/// When the number of pending entries exceeds `memory_upper_bound`, excess items are
/// written to temporary files in `temp_dir`. When usage drops below `memory_lower_bound`,
/// batches of items are reloaded from disk to maintain throughput.
#[derive(Debug, Clone)]
pub struct QueueOption {
    /// Directory path for temporary spill files.
    pub temp_dir: PathBuf,

    /// Upper memory threshold (in number of items) before spilling to disk.
    pub memory_upper_bound: usize,

    /// Lower memory threshold (in number of items) that triggers reloading from disk.
    pub memory_lower_bound: usize,

    /// Number of items to load from disk in one batch when memory is below the lower bound.
    pub spill_load_batch_size: usize,
}

impl Default for ScanOption {
    fn default() -> Self {
        Self {
            max_depth: None, // unlimited depth
            worker_count: 4,
            writer_count: 4,
            meta_option: MetaScanOption {
                scan_acl: false,
                scan_xattrs: false,
                scan_hardlinks: false,
                scan_hidden: false,
                follow_symlinks: false, // safe default
            },
            target_dir: TargetDirOption {
                ctrl_dir: PathBuf::from("/tmp/bifrost/ctrl"),
                meta_dir: PathBuf::from("/tmp/bifrost/meta"),
            },
            queue_option: QueueOption {
                temp_dir: PathBuf::from("/tmp/bifrost/cache"),
                memory_upper_bound: 100_000,
                memory_lower_bound: 50_000,
                spill_load_batch_size: 20_000,
            },
        }
    }
}

impl ScanOption {
    /// Creates a new `ScanOption` with custom control and metadata output directories.
    pub fn new(ctrl_dir: PathBuf, meta_dir: PathBuf) -> Self {
        let mut opts = Self::default();
        opts.target_dir.ctrl_dir = ctrl_dir;
        opts.target_dir.meta_dir = meta_dir;
        opts
    }

    /// Sets the control file output directory.
    pub fn ctrl_dir(mut self, dir: PathBuf) -> Self {
        self.target_dir.ctrl_dir = dir;
        self
    }

    /// Sets the metadata file output directory.
    pub fn meta_dir(mut self, dir: PathBuf) -> Self {
        self.target_dir.meta_dir = dir;
        self
    }

    /// Configures whether symbolic links should be followed during traversal.
    pub fn follow_symlinks(mut self, follow: bool) -> Self {
        self.meta_option.follow_symlinks = follow;
        self
    }

    /// Sets the maximum traversal depth.
    pub fn max_depth(mut self, depth: Option<usize>) -> Self {
        self.max_depth = depth;
        self
    }

    /// Configures whether hidden files and directories should be included.
    pub fn scan_hidden(mut self, scan: bool) -> Self {
        self.meta_option.scan_hidden = scan;
        self
    }

    /// Sets the number of worker threads for filesystem traversal.
    pub fn worker_count(mut self, count: usize) -> Self {
        self.worker_count = count;
        self
    }

    /// Sets the number of writer threads for metadata serialization.
    pub fn writer_count(mut self, count: usize) -> Self {
        self.writer_count = count;
        self
    }

    /// Sets the temporary directory for spillable queue files.
    pub fn temp_dir(mut self, dir: PathBuf) -> Self {
        self.queue_option.temp_dir = dir;
        self
    }

    /// Configures whether ACLs should be scanned.
    pub fn scan_acl(mut self, scan: bool) -> Self {
        self.meta_option.scan_acl = scan;
        self
    }

    /// Configures whether extended attributes should be scanned.
    pub fn scan_xattrs(mut self, scan: bool) -> Self {
        self.meta_option.scan_xattrs = scan;
        self
    }
}