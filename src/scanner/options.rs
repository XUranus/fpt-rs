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

    /// Configuration for sharded control files.
    pub shard_option: ShardOption,

    /// SMB query-directory buffer size in bytes.
    ///
    /// Larger values can reduce the number of `QUERY_DIRECTORY` round-trips
    /// when scanning large directories over SMB. The SMB transport will cap
    /// this to the negotiated transact size.
    pub smb_query_buffer_size: u32,

    /// When true, only collect scan statistics and skip on-disk outputs.
    pub stats_only: bool,
}

/// Output directory configuration.
#[derive(Debug, Clone)]
pub struct TargetDirOption {
    /// Directory path where **control files** (e.g., diff lists) are stored.
    pub ctrl_dir: PathBuf,

    /// Directory path where **metadata files** (serialized `FileMeta`/`DirMeta`) are stored.
    pub meta_dir: PathBuf,
    
    /// Optional: Previous metadata directory for incremental backup.
    /// If provided, the scanner will generate incremental control files
    /// (copy.txt with only new/modified entries, delete.txt for deleted files).
    pub prev_meta_dir: Option<PathBuf>,
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

    /// List of entry names to skip during scanning (e.g., "node_modules", ".git").
    ///
    /// Empty by default.
    pub skip_entries: Vec<String>,

    /// Whether to skip block devices during scanning.
    ///
    /// Enabled by default for safety.
    pub skip_block_devices: bool,

    /// Whether to enable aggregate backup mode.
    /// When enabled, small files are combined into larger blob files.
    /// Disabled by default.
    pub enable_aggregation: bool,

    /// Maximum size of aggregate blob files in bytes (default: 64MB).
    pub max_aggregate_blob_size: u64,

    /// Files smaller than this threshold are aggregated (default: 1MB).
    pub aggregate_file_threshold: u64,
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

/// Configuration for sharded control files.
///
/// Enables splitting control files into multiple shards for parallel processing
/// and handling extremely large filesets (100+ billion files).
#[derive(Debug, Clone)]
pub struct ShardOption {
    /// Whether to enable sharded control files.
    pub enabled: bool,

    /// Number of shards to create.
    pub num_shards: usize,

    /// Maximum entries per shard for copy phase.
    pub max_entries_copy: usize,

    /// Maximum entries per shard for other phases (delete, hardlink, mtime).
    pub max_entries_other: usize,

    /// Maximum shard file size in bytes for copy phase.
    pub max_size: u64,
}

impl Default for ShardOption {
    fn default() -> Self {
        Self {
            enabled: false,
            num_shards: 16,
            max_entries_copy: 1_000_000,
            max_entries_other: 5_000_000,
            max_size: 100 * 1024 * 1024, // 100MB
        }
    }
}

impl Default for MetaScanOption {
    fn default() -> Self {
        Self {
            scan_acl: false,
            scan_xattrs: false,
            scan_hardlinks: false,
            scan_hidden: false,
            follow_symlinks: false, // safe default
            skip_entries: Vec::new(),
            skip_block_devices: true, // safe default
            enable_aggregation: false,
            max_aggregate_blob_size: 64 * 1024 * 1024, // 64MB
            aggregate_file_threshold: 1024 * 1024,     // 1MB
        }
    }
}

impl Default for ScanOption {
    fn default() -> Self {
        Self {
            max_depth: None, // unlimited depth
            worker_count: 4,
            writer_count: 4,
            meta_option: MetaScanOption::default(),
            target_dir: TargetDirOption {
                ctrl_dir: PathBuf::from("/tmp/bifrost/ctrl"),
                meta_dir: PathBuf::from("/tmp/bifrost/meta"),
                prev_meta_dir: None,
            },
            queue_option: QueueOption {
                temp_dir: PathBuf::from("/tmp/bifrost/cache"),
                memory_upper_bound: 100_000,
                memory_lower_bound: 50_000,
                spill_load_batch_size: 20_000,
            },
            shard_option: ShardOption::default(),
            smb_query_buffer_size: 8 * 1024 * 1024,
            stats_only: false,
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

    /// Configures whether hardlinks should be scanned and tracked.
    pub fn scan_hardlinks(mut self, scan: bool) -> Self {
        self.meta_option.scan_hardlinks = scan;
        self
    }
    
    /// Sets the previous metadata directory for incremental backup.
    pub fn prev_meta_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.target_dir.prev_meta_dir = dir;
        self
    }

    /// Enables sharded control files.
    pub fn enable_sharding(mut self, enabled: bool) -> Self {
        self.shard_option.enabled = enabled;
        self
    }

    /// Sets the number of shards.
    pub fn shard_num(mut self, num: usize) -> Self {
        self.shard_option.num_shards = num;
        self
    }

    /// Sets the maximum entries per shard for copy phase.
    pub fn shard_max_entries_copy(mut self, max: usize) -> Self {
        self.shard_option.max_entries_copy = max;
        self
    }

    /// Sets the maximum entries per shard for other phases.
    pub fn shard_max_entries_other(mut self, max: usize) -> Self {
        self.shard_option.max_entries_other = max;
        self
    }

    /// Sets the maximum shard file size in bytes.
    pub fn shard_max_size(mut self, size: u64) -> Self {
        self.shard_option.max_size = size;
        self
    }

    /// Sets the SMB query-directory buffer size in bytes.
    pub fn smb_query_buffer_size(mut self, size: u32) -> Self {
        self.smb_query_buffer_size = size;
        self
    }

    /// Sets the list of entry names to skip during scanning.
    pub fn skip_entries(mut self, entries: Vec<String>) -> Self {
        self.meta_option.skip_entries = entries;
        self
    }

    /// Adds a single entry name to skip during scanning.
    pub fn skip_entry(mut self, entry: &str) -> Self {
        self.meta_option.skip_entries.push(entry.to_string());
        self
    }

    /// Configures whether to skip block devices during scanning.
    pub fn skip_block_devices(mut self, skip: bool) -> Self {
        self.meta_option.skip_block_devices = skip;
        self
    }

    /// Configures whether to enable aggregate backup mode.
    pub fn enable_aggregation(mut self, enable: bool) -> Self {
        self.meta_option.enable_aggregation = enable;
        self
    }

    /// Sets the maximum aggregate blob size in bytes.
    pub fn max_aggregate_blob_size(mut self, size: u64) -> Self {
        self.meta_option.max_aggregate_blob_size = size;
        self
    }

    /// Sets the file threshold for aggregation (files smaller than this are aggregated).
    pub fn aggregate_file_threshold(mut self, threshold: u64) -> Self {
        self.meta_option.aggregate_file_threshold = threshold;
        self
    }

    /// Enables stats-only scanning.
    pub fn stats_only(mut self, enabled: bool) -> Self {
        self.stats_only = enabled;
        self
    }
}
