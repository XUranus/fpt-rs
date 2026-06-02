pub(crate) mod fstat;
mod fwrite_meta;

pub mod bio;

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
use crate::frame::control_files::primary_control_file_path;
use crate::scanner::metadata::HardlinkIndex;
use crate::scanner::models::{DirScanEntry, ScanStatistics};
use crate::scanner::{engine, normalize_control_artifacts, normalize_hardlink_control_file};
use crate::utility::{BlockingQueue, SpillQueue};

pub use crate::scanner::ScanWorkerContext;

/// Main entry point for local filesystem scanning.
///
/// Orchestrates the entire scan process: enqueuing root paths, spawning worker threads,
/// and managing lifecycle. Constructed from [`crate::scanner::ScanOption`] for full configurability.
pub struct Scanner {
    enqueued_paths: Vec<PathBuf>,
    context: ScanWorkerContext,
}

impl From<crate::scanner::ScanOption> for Scanner {
    fn from(scan_option: crate::scanner::ScanOption) -> Self {
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

/// A running scan instance.
pub struct RunningScan {
    stats: Arc<ScanStatistics>,
    terminator_handle: JoinHandle<()>,
    terminate_indicator: Arc<AtomicBool>,
}

/// Errors that can occur during scanner setup.
#[derive(Debug)]
pub enum ScanError {
    EmptyEnqueue,
    InvalidEnqueue(String),
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
    pub fn new(scan_option: crate::scanner::ScanOption) -> Self {
        scan_option.into()
    }

    pub fn enqueue_path(&mut self, path: PathBuf) -> Result<&Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Err(
                ScanError::InvalidEnqueue(format!("Path does not exist: {:?}", path)).into(),
            );
        }
        self.enqueued_paths.push(path);
        Ok(self)
    }

    pub fn start(self) -> Result<RunningScan, ScanError> {
        let worker_count = self.context.scan_option.worker_count;
        let writer_count = self.context.scan_option.writer_count;
        let stats = Arc::clone(&self.context.stats);

        if self.enqueued_paths.is_empty() {
            return Err(ScanError::EmptyEnqueue);
        }

        for path in &self.enqueued_paths {
            self.context
                .dirent_queue
                .push(DirScanEntry::new(path.clone(), 0))
                .map_err(|e| ScanError::InvalidEnqueue(e.to_string()))?;
        }

        let scan_hardlinks = self.context.scan_option.meta_option.scan_hardlinks;
        let hardlink_index: Option<Arc<Mutex<HardlinkIndex>>> = if scan_hardlinks {
            Some(Arc::new(Mutex::new(HardlinkIndex::new())))
        } else {
            None
        };

        let traversal_handles = bio::start_workers(&self.context, worker_count);
        let writer_handles = if self.context.scan_option.stats_only {
            engine::start_stats_consumers(&self.context, writer_count.max(1))
        } else {
            engine::start_meta_writers(&self.context, writer_count, hardlink_index.clone())
        };

        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let terminate_indicator_cloned = Arc::clone(&terminate_indicator);
        let output_queue = Arc::clone(&self.context.output_queue);
        let scan_option = Arc::clone(&self.context.scan_option);
        let hardlink_index_clone = hardlink_index.clone();

        let terminator_handle = std::thread::spawn(move || {
            for handle in traversal_handles {
                if let Err(e) = handle.join() {
                    error!("Traversal worker panicked: {:?}", e);
                }
            }

            output_queue.close();

            for handle in writer_handles {
                if let Err(e) = handle.join() {
                    error!("Writer worker panicked: {:?}", e);
                }
            }

            if !scan_option.stats_only {
                if let Err(e) = engine::generate_control_files(&scan_option) {
                    error!("Failed to generate control files: {}", e);
                }

                if let Err(e) = normalize_control_artifacts(&scan_option) {
                    error!("Failed to normalize control artifacts: {}", e);
                }

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
    pub fn stats(&self) -> crate::scanner::ScanStatsSnapshot {
        self.stats.snapshot()
    }

    pub fn complete(&self) -> bool {
        self.terminate_indicator.load(Ordering::Relaxed)
    }

    pub fn wait(self) {
        let _ = self.terminator_handle.join();
    }
}
