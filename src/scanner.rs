//! # Filesystem Scanner
//!
//! This module provides a high-performance, parallel filesystem scanner for backup systems.
//! It recursively traverses directory trees, collects rich metadata (including xattrs,
//! ACLs, symlinks), and writes results to disk in batches for crash resilience.
//!
//! ## Architecture
//!
//! The scanner uses a **multi-threaded pipeline**:
//! - **Traversal workers**: Process directories from a spillable queue, collect file metadata.
//! - **Metadata writers**: Serialize and persist metadata to disk in background threads.
//! - **Shared queues**: Coordinate work between components with bounded memory usage.
//!
//! Key features:
//! - Resumable scanning via checkpointed batch writes.
//! - Configurable depth limits, hidden file handling, and symlink following.
//! - Real-time statistics tracking (file count, size, errors).
//! - Automatic spilling to disk when memory pressure is high.

use log::{error, info};
use std::{
    fmt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};

use crate::failure::FailureRecorder;
use crate::frame::control_files::{
    classify_control_file_name, find_primary_control_file, primary_control_file_path,
};
use crate::scanner::metadata::HardlinkIndex;

use crate::{
    scanner::{
        engine::bio,
        models::{DirBatchScanResult, DirScanEntry, ScanStatistics},
        options::ControlPathOption,
    },
    utility::{BlockingQueue, SpillQueue},
};

pub(crate) mod engine;
pub mod filter;
pub mod metadata;
pub(crate) mod models;
pub mod options;

pub use filter::ScanPathFilterSet;
pub use models::ScanStatsSnapshot;
pub use options::ScanOption;

/// Main entry point for filesystem scanning.
///
/// Orchestrates the entire scan process: enqueuing root paths, spawning worker threads,
/// and managing lifecycle. Constructed from [`ScanOption`] for full configurability.
pub struct Scanner {
    /// Root paths to scan (enqueued before starting).
    enqueued_paths: Vec<PathBuf>,
    /// Shared context for all worker threads.
    context: ScanWorkerContext,
}

impl From<ScanOption> for Scanner {
    /// Creates a new `Scanner` from scan configuration options.
    fn from(scan_option: ScanOption) -> Self {
        let queue_option = &scan_option.queue_option;
        let failure_recorder = scan_option
            .failure_log
            .as_ref()
            .and_then(|cfg| FailureRecorder::create(cfg).ok());
        let dirent_queue = SpillQueue::new(
            queue_option.temp_dir.clone(),
            queue_option.memory_upper_bound,
            queue_option.memory_lower_bound,
            queue_option.spill_load_batch_size,
        )
        .expect("Failed to create directory entry queue");

        let output_queue = BlockingQueue::new(crate::scanner::options::DEFAULT_SCAN_QUEUE_CAPACITY);

        Self {
            enqueued_paths: Vec::new(),
            context: ScanWorkerContext {
                scan_option: Arc::new(scan_option),
                dirent_queue: Arc::new(dirent_queue),
                output_queue: Arc::new(output_queue),
                stats: Arc::new(ScanStatistics::default()),
                failure_recorder,
            },
        }
    }
}

/// Shared context passed to all scanner worker threads.
///
/// Contains configuration, work queues, and statistics counters.
/// Cloning this struct only clones the `Arc` handles—no deep copying occurs.
#[derive(Clone)]
pub struct ScanWorkerContext {
    /// Immutable scan configuration.
    pub scan_option: Arc<ScanOption>,
    /// Queue of directories pending traversal.
    pub dirent_queue: Arc<SpillQueue<DirScanEntry>>,
    /// Queue of completed scan batches ready for serialization.
    pub output_queue: Arc<BlockingQueue<DirBatchScanResult>>,
    /// Real-time scan statistics (atomically updated).
    pub stats: Arc<ScanStatistics>,
    /// Optional failure recorder shared by scan workers.
    pub failure_recorder: Option<FailureRecorder>,
}

/// A running scan instance.
///
/// Provides access to live statistics and lifecycle management (wait, check completion).
pub struct RunningScan {
    /// Shared statistics counter.
    stats: Arc<ScanStatistics>,
    /// Handle to the termination/coordinator thread.
    terminator_handle: JoinHandle<()>,
    /// Flag indicating whether scanning has completed.
    terminate_indicator: Arc<AtomicBool>,
}

/// Errors that can occur during scanner setup.
#[derive(Debug)]
pub enum ScanError {
    /// No enqueued paths to scan.
    EmptyEnqueue,
    /// Failed to enqueue a path (e.g., invalid path).
    InvalidEnqueue(String),
    /// Invalid scan configuration option.
    InvalidOption(String),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::EmptyEnqueue => write!(f, "No paths enqueued for scanning"),
            ScanError::InvalidEnqueue(msg) => write!(f, "Failed to enqueue path: {}", msg),
            ScanError::InvalidOption(msg) => write!(f, "Invalid scan option: {}", msg),
        }
    }
}

impl std::error::Error for ScanError {}

impl Scanner {
    /// Creates a new scanner from the given options.
    pub fn new(scan_option: ScanOption) -> Self {
        scan_option.into()
    }

    /// Adds a root path to be scanned.
    ///
    /// Paths are enqueued before the scan starts. Returns an error if the path is invalid.
    pub fn enqueue_path(&mut self, path: PathBuf) -> Result<&Self, Box<dyn std::error::Error>> {
        // Validate path exists and is accessible
        if !path.exists() {
            return Err(
                ScanError::InvalidEnqueue(format!("Path does not exist: {:?}", path)).into(),
            );
        }
        self.enqueued_paths.push(path);
        Ok(self)
    }

    /// Starts the scanning process.
    ///
    /// Spawns traversal and writer worker pools, then returns a `RunningScan` handle
    /// for monitoring and control. The actual scanning runs in the background.
    pub fn start(self) -> Result<RunningScan, ScanError> {
        let worker_count = self.context.scan_option.worker_count;
        let writer_count = self.context.scan_option.writer_count;
        let stats = Arc::clone(&self.context.stats);

        if self.enqueued_paths.is_empty() {
            return Err(ScanError::EmptyEnqueue);
        }

        // Enqueue all root paths at depth 0
        for path in &self.enqueued_paths {
            self.context
                .dirent_queue
                .push(DirScanEntry::new(path.clone(), 0))
                .map_err(|e| ScanError::InvalidEnqueue(e.to_string()))?;
        }

        // Create hardlink index if hardlink scanning is enabled
        let scan_hardlinks = self.context.scan_option.meta_option.scan_hardlinks;
        let hardlink_index: Option<Arc<Mutex<HardlinkIndex>>> = if scan_hardlinks {
            Some(Arc::new(Mutex::new(HardlinkIndex::new())))
        } else {
            None
        };

        // Start worker threads
        let traversal_handles = bio::traversal::start_workers(&self.context, worker_count);
        let writer_handles = if self.context.scan_option.stats_only {
            engine::start_stats_consumers(&self.context, writer_count.max(1))
        } else {
            engine::start_meta_writers(&self.context, writer_count, hardlink_index.clone())
        };

        // Spawn termination/coordinator thread
        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let terminate_indicator_cloned = Arc::clone(&terminate_indicator);
        let output_queue = Arc::clone(&self.context.output_queue);
        let scan_option = Arc::clone(&self.context.scan_option);
        let hardlink_index_clone = hardlink_index.clone();

        let terminator_handle = std::thread::spawn(move || {
            // Wait for all traversal workers to finish
            for handle in traversal_handles {
                if let Err(e) = handle.join() {
                    error!("Traversal worker panicked: {:?}", e);
                }
            }

            // Signal end of scan data
            output_queue.close();

            // Wait for all writers to finish
            for handle in writer_handles {
                if let Err(e) = handle.join() {
                    error!("Writer worker panicked: {:?}", e);
                }
            }

            if !scan_option.stats_only {
                // Generate final control files
                if let Err(e) = engine::generate_control_files(&scan_option) {
                    error!("Failed to generate control files: {}", e);
                }

                if let Err(e) = normalize_control_artifacts(&scan_option) {
                    error!("Failed to normalize control artifacts: {}", e);
                }

                // Write hardlink control file if hardlink scanning was enabled
                if scan_hardlinks {
                    if let Some(index) = hardlink_index_clone {
                        if let Ok(idx) = index.lock() {
                            let hardlink_ctrl_path =
                                primary_control_file_path(&scan_option.target_dir.ctrl_dir, "hardlink");
                            if let Err(e) = idx.write_to_file_with_source(
                                &hardlink_ctrl_path,
                                &scan_option.control_path.source_kind,
                                &scan_option.control_path.source_root,
                            ) {
                                error!("Failed to write hardlink control file: {}", e);
                            } else {
                                info!("Hardlink control file written to {:?}", hardlink_ctrl_path);
                                info!(
                                    "Found {} hardlink groups with {} total files",
                                    idx.group_count(),
                                    idx.total_file_count()
                                );
                            }
                        }
                    }
                }

                if let Err(e) = normalize_hardlink_control_file(&scan_option) {
                    error!("Failed to normalize hardlink control file: {}", e);
                }
            }

            terminate_indicator_cloned.store(true, Ordering::Relaxed);
            info!("Scanning completed successfully.");
        });

        Ok(RunningScan {
            terminator_handle,
            stats,
            terminate_indicator,
        })
    }
}

impl RunningScan {
    /// Returns a snapshot of current scan statistics.
    pub fn stats(&self) -> ScanStatsSnapshot {
        self.stats.snapshot()
    }

    /// Returns `true` if the scan has completed.
    pub fn complete(&self) -> bool {
        self.terminate_indicator.load(Ordering::Relaxed)
    }

    /// Blocks until the scan completes (successfully or with errors).
    pub fn wait(self) {
        let _ = self.terminator_handle.join();
    }
}

// ---------------------------------------------------------------------------
// NFS scan integration
// ---------------------------------------------------------------------------

/// Run a full NFS scan and write metadata/control files to disk.
///
/// This is the NFS equivalent of creating a [`Scanner`], calling
/// [`Scanner::enqueue_path`], and then [`Scanner::start`] + waiting.
///
/// # How it works
///
/// 1. A Tokio runtime drives [`crate::nfs::NfsScanner::scan`], which
///    emits [`DirBatchScanResult`] items on a `tokio::sync::mpsc` channel.
/// 2. A bridge task converts each item and pushes it into the shared
///    [`BlockingQueue`] that the metadata writers drain.
/// 3. The standard metadata writers run on OS threads (exactly as in the
///    local-FS scanner), producing `meta_*.dat`, `fcache_*.dat`, and
///    `dcache_*.dat` files.
/// 4. After all items are consumed, the standard control-file generator
///    runs and emits copy control files (and optionally hardlink/delete/mtime control files).
///
/// Returns `(total_files, total_dirs, total_size_bytes)` on success.
#[cfg(feature = "nfs")]
pub async fn run_nfs_scan(
    location: &crate::nfs::NfsLocation,
    scan_option: ScanOption,
) -> Result<(u64, u64, u64, u64, u64), String> {
    use crate::nfs::connection::NfsConnectionPool;
    use crate::nfs::NfsScanner;
    use crate::scanner::engine::aio::{run_aio_scan, NfsScanAdapter};

    let pool = NfsConnectionPool::new(location)
        .await
        .map_err(|e| format!("NFS connect failed: {e}"))?;

    let root_fh = pool.root_fh();
    let root_path = if location.sub_path.is_empty() {
        location.export.clone()
    } else {
        format!(
            "{}/{}",
            location.export.trim_end_matches('/'),
            location.sub_path.trim_start_matches('/')
        )
    };

    let failure_recorder = scan_option
        .failure_log
        .as_ref()
        .and_then(|cfg| FailureRecorder::create(cfg).ok());

    let nfs_scanner = NfsScanner::new(location, scan_option.retry_policy, failure_recorder)
        .await
        .map_err(|e| format!("NFS scanner init failed: {e}"))?;

    let adapter = NfsScanAdapter {
        scanner: nfs_scanner,
        root_fh,
        root_path,
    };

    let result = run_aio_scan(adapter, scan_option).await?;
    Ok((
        result.total_files,
        result.total_dirs,
        result.total_size,
        result.failed_files,
        result.failed_dirs,
    ))
}

/// Run a full SMB scan and write metadata/control files to disk.
///
/// This mirrors [`run_nfs_scan`] but uses the SMB client transport.
#[cfg(feature = "smb")]
pub async fn run_smb_scan(
    location: &crate::smb::SmbLocation,
    scan_option: ScanOption,
) -> Result<(u64, u64, u64, u64, u64), String> {
    use crate::scanner::engine::aio::{run_aio_scan, SmbScanAdapter};
    use crate::smb::scanner::SmbScanner;

    let failure_recorder = scan_option
        .failure_log
        .as_ref()
        .and_then(|cfg| FailureRecorder::create(cfg).ok());

    let smb_scanner = SmbScanner::new(location, scan_option.retry_policy, failure_recorder).await?;

    let adapter = SmbScanAdapter {
        scanner: smb_scanner,
    };

    let result = run_aio_scan(adapter, scan_option).await?;
    Ok((
        result.total_files,
        result.total_dirs,
        result.total_size,
        result.failed_files,
        result.failed_dirs,
    ))
}

pub(crate) fn normalize_control_artifacts(scan_option: &ScanOption) -> Result<(), String> {
    normalize_copy_controls(scan_option)?;
    normalize_delete_control_file(scan_option)?;
    normalize_mtime_control_file(scan_option)?;
    normalize_hardlink_control_file(scan_option)?;
    Ok(())
}

fn normalize_copy_controls(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{
        ControlEntry, ControlFileHeader, ControlFileReader, ControlFileWriter,
    };

    let ctrl_dir = &scan_option.target_dir.ctrl_dir;
    let entries = std::fs::read_dir(ctrl_dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if classify_control_file_name(file_name) != Some("copy") {
            continue;
        }

        let reader = ControlFileReader::open(&path).map_err(|e| e.to_string())?;
        let rewritten: Vec<ControlEntry> = reader
            .map(|result| {
                result.map(|entry| match entry {
                    ControlEntry::Dir(mut dir) => {
                        dir.path = normalize_control_path(&scan_option.control_path, &dir.path);
                        ControlEntry::Dir(dir)
                    }
                    ControlEntry::File(file) => ControlEntry::File(file),
                })
            })
            .collect::<Result<_, _>>()
            .map_err(|e: std::io::Error| e.to_string())?;

        let tmp = path.with_extension("tmp");
        let header = ControlFileHeader {
            source_kind: scan_option.control_path.source_kind.clone(),
            source_root: scan_option.control_path.source_root.clone(),
            ..ControlFileHeader::default()
        };
        let mut writer =
            ControlFileWriter::new_with_header(&tmp, &header).map_err(|e| e.to_string())?;
        for entry in &rewritten {
            match entry {
                ControlEntry::Dir(dir) => writer.write_dir(dir).map_err(|e| e.to_string())?,
                ControlEntry::File(file) => writer.write_file(file).map_err(|e| e.to_string())?,
            }
        }
        writer.finish().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn normalize_delete_control_file(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{DeleteControlFileReader, DeleteControlFileWriter};

    let Some(path) = find_primary_control_file(&scan_option.target_dir.ctrl_dir, "delete") else {
        return Ok(());
    };

    let reader = DeleteControlFileReader::open(&path).map_err(|e| e.to_string())?;
    let entries: Vec<_> = reader
        .collect::<Result<_, _>>()
        .map_err(|e: std::io::Error| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut writer = DeleteControlFileWriter::new_with_source(
        &tmp,
        &scan_option.control_path.source_kind,
        &scan_option.control_path.source_root,
    )
    .map_err(|e| e.to_string())?;
    for entry in entries {
        writer
            .write_entry(&crate::scanner::metadata::DeleteEntry {
                entry_type: entry.entry_type,
                path: normalize_control_path(&scan_option.control_path, &entry.path),
            })
            .map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_mtime_control_file(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{MtimeControlFileReader, MtimeControlFileWriter};

    let Some(path) = find_primary_control_file(&scan_option.target_dir.ctrl_dir, "mtime") else {
        return Ok(());
    };

    let reader = MtimeControlFileReader::open(&path).map_err(|e| e.to_string())?;
    let entries: Vec<_> = reader
        .collect::<Result<_, _>>()
        .map_err(|e: std::io::Error| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut writer = MtimeControlFileWriter::new_with_source(
        &tmp,
        &scan_option.control_path.source_kind,
        &scan_option.control_path.source_root,
    )
    .map_err(|e| e.to_string())?;
    for mut entry in entries {
        entry.path = normalize_control_path(&scan_option.control_path, &entry.path);
        writer.write_dir(&entry).map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_hardlink_control_file(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{
        HardlinkControlFileReader, HardlinkControlFileWriter, HardlinkEntry, HardlinkFileEntry,
    };

    let Some(path) = find_primary_control_file(&scan_option.target_dir.ctrl_dir, "hardlink") else {
        return Ok(());
    };
    let reader = HardlinkControlFileReader::open(&path).map_err(|e| e.to_string())?;
    let entries: Vec<_> = reader
        .collect::<Result<_, _>>()
        .map_err(|e: std::io::Error| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut writer = HardlinkControlFileWriter::new_with_source(
        &tmp,
        &scan_option.control_path.source_kind,
        &scan_option.control_path.source_root,
    )
    .map_err(|e| e.to_string())?;
    for entry in entries {
        match entry {
            HardlinkEntry::Inode(inode) => writer.write_inode(&inode).map_err(|e| e.to_string())?,
            HardlinkEntry::File(file) => writer
                .write_file(&HardlinkFileEntry {
                    path: normalize_control_path(&scan_option.control_path, &file.path),
                    ..file
                })
                .map_err(|e| e.to_string())?,
        }
    }
    writer.finish().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_control_path(cfg: &ControlPathOption, path: &str) -> String {
    let physical = PathBuf::from(path);
    let logical_root = PathBuf::from(&cfg.source_root);
    if !physical.starts_with(&cfg.physical_base) && physical.starts_with(&logical_root) {
        return path.to_string();
    }
    let base = cfg.physical_base.clone();
    let rel = physical
        .strip_prefix(&base)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| physical.clone());

    format!("/{}", rel.to_string_lossy().trim_start_matches('/'))
}
