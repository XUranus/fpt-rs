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

use std::{
    fmt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};
use log::{debug, info, warn, error};

use crate::scanner::metadata::HardlinkIndex;

use crate::{
    scanner::{
        engine::bio,
        models::{DirBatchScanResult, DirScanEntry, ScanStatistics, ScanStatsSnapshot},
        options::ScanOption,
    },
    utility::{BlockingQueue, SpillQueue},
};

mod engine;
mod models;
pub mod metadata;
pub mod options;

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
        let dirent_queue = SpillQueue::new(
            queue_option.temp_dir.clone(),
            queue_option.memory_upper_bound,
            queue_option.memory_lower_bound,
            queue_option.spill_load_batch_size,
        )
        .expect("Failed to create directory entry queue");

        let output_queue = BlockingQueue::new(1000);

        Self {
            enqueued_paths: Vec::new(),
            context: ScanWorkerContext {
                scan_option: Arc::new(scan_option),
                dirent_queue: Arc::new(dirent_queue),
                output_queue: Arc::new(output_queue),
                stats: Arc::new(ScanStatistics::default()),
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
            return Err(ScanError::InvalidEnqueue(format!(
                "Path does not exist: {:?}",
                path
            ))
            .into());
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
        let writer_handles = engine::start_meta_writers(&self.context, writer_count, hardlink_index.clone());

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

            // Generate final control files
            if let Err(e) = engine::generate_control_files(&scan_option.target_dir) {
                error!("Failed to generate control files: {}", e);
            }

            // Write hardlink control file if hardlink scanning was enabled
            if scan_hardlinks {
                if let Some(index) = hardlink_index_clone {
                    if let Ok(idx) = index.lock() {
                        let hardlink_ctrl_path = scan_option.target_dir.ctrl_dir.join("hardlink.txt");
                        if let Err(e) = idx.write_to_file(&hardlink_ctrl_path) {
                            error!("Failed to write hardlink control file: {}", e);
                        } else {
                            info!("Hardlink control file written to {:?}", hardlink_ctrl_path);
                            info!("Found {} hardlink groups with {} total files", 
                                idx.group_count(), idx.total_file_count());
                        }
                    }
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

    // TODO: Implement pause/resume/abort functionality
    // pub fn pause(&self) { }
    // pub fn resume(&self) { }
    // pub fn abort(&self) { }
}