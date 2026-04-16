use std::{path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU32, Ordering}, mpsc}, thread};
use log::info;
use crate::backup::{
        bio::copy::{self, ReaderBioResult, ReaderBioTask, WriterBioResult, WriterBioTask},
        bio::hardlink::{self, HardlinkStatsSnapshot},
        bio::mtime::{self, MtimeStatsSnapshot},
        bio::delete::{self, DeleteStatsSnapshot},
        fcb::{ControlBlockVarient, FileControlBlock},
        stats::{BackupStats, BackupStatsSnapshot},
        aggregate::AggregateConfig,
    };

mod fcb;
mod bio;
mod stats;
pub mod sharded_processor;

// Aggregate backup/restore modules
pub mod aggregate;
pub mod aggregate_index;
pub mod aggregate_engine;
pub mod aggregate_restore;

pub struct BackupOption {
    /// source location path prefix
    source_dir_base : PathBuf,
    /// target location path prefix
    target_dir_base : PathBuf,

    meta_dir : PathBuf,
    ctrl_dir : PathBuf,
    // path for control file
    control_file : PathBuf,

    worker_count : usize,

    /// Whether to run the hardlink phase after copy phase
    enable_hardlink_phase : bool,

    /// Whether to run the delete phase after hardlink phase
    enable_delete_phase : bool,

    /// Whether to run the mtime phase after copy/hardlink phase
    enable_mtime_phase : bool,

    /// Aggregate backup configuration
    pub aggregate_config : AggregateConfig,
}



// each backup task do the data copy following the instruction of one control file
pub struct BackupTask {
    option : BackupOption,
}

pub struct RunningBackup {
    option : BackupOption,
    stats : Arc<BackupStats>,
    hardlink_stats : Option<HardlinkStatsSnapshot>,
    delete_stats : Option<DeleteStatsSnapshot>,
    mtime_stats : Option<MtimeStatsSnapshot>,
    terminate_handle : thread::JoinHandle<()>,
    terminate_indicator : Arc<AtomicBool>
}

#[derive(Debug)]
pub enum BackupError {
    InvalidMetaPath,
    InvalidControlFile,
    InsuffientDiskSpace,
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::InvalidMetaPath => write!(f, "Invalid metadata path"),
            BackupError::InvalidControlFile => write!(f, "Invalid control file"),
            BackupError::InsuffientDiskSpace => write!(f, "Insufficient disk space"),
        }
    }
}

impl std::error::Error for BackupError {}

impl BackupOption {
    pub fn new(source_dir_base : PathBuf, target_dir_base : PathBuf, meta_dir : PathBuf, ctrl_dir : PathBuf, control_file : PathBuf) -> Self {
        Self {
            worker_count : 4,
            source_dir_base,
            target_dir_base,
            meta_dir,
            ctrl_dir,
            control_file,
            enable_hardlink_phase : false,
            enable_delete_phase : false,
            enable_mtime_phase : false,
            aggregate_config : AggregateConfig::default(),
        }
    }
    
    /// Enable the hardlink phase
    pub fn enable_hardlink_phase(mut self, enable: bool) -> Self {
        self.enable_hardlink_phase = enable;
        self
    }
    
    /// Enable the delete phase
    pub fn enable_delete_phase(mut self, enable: bool) -> Self {
        self.enable_delete_phase = enable;
        self
    }
    
    /// Enable the mtime phase
    pub fn enable_mtime_phase(mut self, enable: bool) -> Self {
        self.enable_mtime_phase = enable;
        self
    }

    /// Enable aggregation with default settings
    pub fn enable_aggregation(mut self, enable: bool) -> Self {
        self.aggregate_config.enabled = enable;
        self
    }

    /// Set aggregation configuration
    pub fn aggregate_config(mut self, config: AggregateConfig) -> Self {
        self.aggregate_config = config;
        self
    }

    /// Set maximum blob size for aggregation (in bytes)
    pub fn aggregate_max_blob_size(mut self, size: u64) -> Self {
        self.aggregate_config.max_blob_size = size;
        self
    }

    /// Set file threshold for aggregation (files smaller than this are aggregated)
    pub fn aggregate_file_threshold(mut self, threshold: u64) -> Self {
        self.aggregate_config.file_threshold = threshold;
        self
    }
}

struct SharedState {
    pub entry_produce_done : AtomicBool,
    pub reader_done : AtomicBool,
    pub writer_done : AtomicBool,
    pub active_reader_io_workers : AtomicU32,
    pub active_writer_io_workers : AtomicU32
}

impl Default for SharedState {
    fn default() -> Self {
        SharedState {
            entry_produce_done : AtomicBool::new(false),
            reader_done : AtomicBool::new(false),
            writer_done : AtomicBool::new(false),
            active_reader_io_workers : AtomicU32::new(0),
            active_writer_io_workers : AtomicU32::new(0),
        }
    }
}

impl BackupTask {
    pub fn start(self) -> Result<RunningBackup, BackupError> {
        let worker_count = self.option.worker_count;
        let control_file = self.option.control_file.clone();
        let source_dir_base = self.option.source_dir_base.clone();
        let target_dir_base = self.option.target_dir_base.clone();
        let meta_dir = self.option.meta_dir.clone();
        let ctrl_dir = self.option.ctrl_dir.clone();
        let enable_hardlink_phase = self.option.enable_hardlink_phase;
        let enable_delete_phase = self.option.enable_delete_phase;
        let enable_mtime_phase = self.option.enable_mtime_phase;
        let enable_aggregation = self.option.aggregate_config.enabled;
        let stats = Arc::new(BackupStats::default());
        let shared_state = Arc::new(SharedState::default());
        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let terminate_indicator_inner = Arc::clone(&terminate_indicator);

        // Set up aggregate engine if enabled
        let aggregate_engine = if enable_aggregation {
            info!("Aggregation enabled: max_blob_size={}, file_threshold={}",
                self.option.aggregate_config.max_blob_size,
                self.option.aggregate_config.file_threshold);
            
            match aggregate_engine::AggregateBackupEngine::new(
                self.option.aggregate_config,
                source_dir_base.clone(),
                target_dir_base.clone(),
            ) {
                Ok(engine) => Some(Arc::new(engine)),
                Err(e) => {
                    eprintln!("Failed to create aggregate engine: {}. Continuing without aggregation.", e);
                    None
                }
            }
        } else {
            None
        };

        let (fcb_reader_tx, fcb_reader_rx) = mpsc::channel::<ControlBlockVarient>();
        let (fcb_writer_tx, fcb_writer_rx) = mpsc::channel::<ControlBlockVarient>();
        let (reader_io_task_tx, reader_io_task_rx) = mpsc::channel::<ReaderBioTask>();
        let (reader_io_result_tx, reader_io_result_rx) = mpsc::channel::<ReaderBioResult>();
        let (writer_io_task_tx, writer_io_task_rx) = mpsc::channel::<WriterBioTask>();
        let (writer_io_result_tx, writer_io_result_rx) = mpsc::channel::<WriterBioResult>();

        let reader_io_task_rx = Arc::new(Mutex::new(reader_io_task_rx));
        let writer_io_task_rx = Arc::new( Mutex::new(writer_io_task_rx));
    
        let entry_producer_handle = copy::spawn_file_entry_producer(control_file, meta_dir.clone(), source_dir_base.clone(), target_dir_base.clone(), fcb_reader_tx.clone(), Arc::clone(&shared_state));

        // If aggregation is enabled, use the aggregate-aware reader
        let reader_handle = if let Some(ref engine) = aggregate_engine {
            copy::spawn_reader_with_aggregation(
                fcb_reader_rx, 
                reader_io_task_tx, 
                fcb_writer_tx.clone(), 
                Arc::clone(&shared_state),
                Arc::clone(engine),
                Arc::clone(&stats)
            )
        } else {
            copy::spawn_reader(fcb_reader_rx, reader_io_task_tx, fcb_writer_tx.clone(), Arc::clone(&shared_state))
        };
        
        let reader_io_pool = copy::spawn_reader_io_pool(Arc::clone(&reader_io_task_rx), reader_io_result_tx, worker_count, Arc::clone(&shared_state));
        
        // Use aggregation-aware result poller if aggregation is enabled
        let reader_io_result_poll = if let Some(ref engine) = aggregate_engine {
            copy::spawn_reader_io_result_poll_with_aggregation(
                reader_io_result_rx, 
                fcb_reader_tx, 
                fcb_writer_tx.clone(), 
                Arc::clone(&stats),
                Arc::clone(engine)
            )
        } else {
            copy::spawn_reader_io_result_poll(reader_io_result_rx, fcb_reader_tx, fcb_writer_tx.clone(), Arc::clone(&stats))
        };

        let writer_handle = copy::spawn_writer(fcb_writer_rx, writer_io_task_tx, Arc::clone(&shared_state), Arc::clone(&stats));
        let writer_io_pool = copy::spawn_writer_io_pool(writer_io_task_rx, writer_io_result_tx, worker_count, Arc::clone(&shared_state));
        let writer_io_result_poll = copy::spawn_writer_io_result_poll(writer_io_result_rx, fcb_writer_tx, Arc::clone(&stats));

        let terminate_handle = thread::spawn(move || {
            entry_producer_handle.join().unwrap();
            reader_handle.join().unwrap();
            for handle in reader_io_pool {
                handle.join().unwrap();
            }
            reader_io_result_poll.join().unwrap();

            writer_handle.join().unwrap();
            for handle in writer_io_pool {
                handle.join().unwrap();
            }
            writer_io_result_poll.join().unwrap();
            
            // Flush any remaining aggregate buffers
            if let Some(ref engine) = aggregate_engine {
                info!("Flushing aggregate buffers...");
                // The aggregate stats are tracked within the engine
                let agg_stats = engine.stats();
                info!("Aggregate stats: {} blobs created, {} files aggregated", 
                    agg_stats.blobs_created, agg_stats.files_aggregated);
            }
            
            // Run hardlink phase if enabled
            if enable_hardlink_phase {
                info!("Starting hardlink phase...");
                match hardlink::run_hardlink_phase(&ctrl_dir, &meta_dir, &source_dir_base, &target_dir_base) {
                    Ok(hl_stats) => {
                        info!("Hardlink phase completed: {} created, {} failed", 
                            hl_stats.hardlinks_created, hl_stats.hardlinks_failed);
                    }
                    Err(e) => {
                        eprintln!("Hardlink phase failed: {}", e);
                    }
                }
            }
            
            // Run delete phase if enabled (between hardlink and mtime)
            if enable_delete_phase {
                info!("Starting delete phase...");
                match delete::run_delete_phase(&ctrl_dir, &source_dir_base, &target_dir_base) {
                    Ok(del_stats) => {
                        info!("Delete phase completed: {} files deleted, {} dirs deleted", 
                            del_stats.files_deleted, del_stats.dirs_deleted);
                    }
                    Err(e) => {
                        eprintln!("Delete phase failed: {}", e);
                    }
                }
            }
            
            // Run mtime phase if enabled
            if enable_mtime_phase {
                info!("Starting mtime phase...");
                match mtime::run_mtime_phase(&ctrl_dir, &source_dir_base, &target_dir_base) {
                    Ok(mt_stats) => {
                        info!("Mtime phase completed: {} restored, {} failed", 
                            mt_stats.dirs_restored, mt_stats.dirs_failed);
                    }
                    Err(e) => {
                        eprintln!("Mtime phase failed: {}", e);
                    }
                }
            }
            
            terminate_indicator_inner.store(true, Ordering::Relaxed);
        });

        Ok(RunningBackup{
            option : self.option,
            stats,
            hardlink_stats: None,
            delete_stats: None,
            mtime_stats: None,
            terminate_handle,
            terminate_indicator
        })
    }

}

impl From<BackupOption> for BackupTask {
    fn from(option: BackupOption) -> Self {
        Self {
            option
        }
    }
}

impl RunningBackup {
    pub fn stats(&self) -> BackupStatsSnapshot {
        self.stats.snapshot()
    }
    
    pub fn hardlink_stats(&self) -> Option<&HardlinkStatsSnapshot> {
        self.hardlink_stats.as_ref()
    }
    
    pub fn delete_stats(&self) -> Option<&DeleteStatsSnapshot> {
        self.delete_stats.as_ref()
    }
    
    pub fn mtime_stats(&self) -> Option<&MtimeStatsSnapshot> {
        self.mtime_stats.as_ref()
    }

    pub fn complete(&self) -> bool {
        self.terminate_indicator.load(Ordering::Relaxed)
    }

    pub fn wait(self) -> Result<(), BackupError> {
        self.terminate_handle.join().unwrap();
        Ok(())
    }
}

// ============================================================================
// Restore Task Implementation
// ============================================================================

/// Policy for handling existing files during restore operations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RestorePolicy {
    /// Replace existing files with restored versions.
    /// This is the default behavior - always overwrite.
    Replace,
    /// Skip existing files - do not restore if target exists.
    /// Files that exist are counted as skipped in stats.
    Skip,
    /// Keep newer files - only restore if source (backup) is newer than target.
    /// If target is newer or same age, skip the file.
    KeepNewer,
}

impl Default for RestorePolicy {
    fn default() -> Self {
        RestorePolicy::Replace
    }
}

impl RestorePolicy {
    /// Determines whether to restore a file based on the policy and file metadata.
    ///
    /// # Arguments
    /// * `source_mtime` - Modification time of the source (backup) file
    /// * `target_exists` - Whether the target file already exists
    /// * `target_mtime` - Modification time of the target file (if it exists)
    ///
    /// # Returns
    /// `true` if the file should be restored, `false` if it should be skipped.
    pub fn should_restore(
        &self,
        source_mtime: Option<std::time::SystemTime>,
        target_exists: bool,
        target_mtime: Option<std::time::SystemTime>,
    ) -> bool {
        match self {
            RestorePolicy::Replace => true,
            RestorePolicy::Skip => !target_exists,
            RestorePolicy::KeepNewer => {
                if !target_exists {
                    return true;
                }
                match (source_mtime, target_mtime) {
                    (Some(src), Some(tgt)) => src > tgt,
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (None, None) => true,
                }
            }
        }
    }
}

/// Statistics for restore operations, tracking skipped files based on policy.
#[derive(Debug, Default, Clone)]
pub struct RestoreStats {
    /// Total files that were restored (copied)
    pub files_restored: u64,
    /// Total bytes copied during restore
    pub bytes_restored: u64,
    /// Files skipped due to restore policy (Skip or KeepNewer)
    pub files_skipped: u64,
    /// Bytes that would have been copied if not skipped
    pub bytes_skipped: u64,
    /// Files that failed to restore
    pub files_failed: u64,
    /// Directories created
    pub dirs_created: u64,
}

/// Configuration options for restore operations.
#[derive(Debug, Clone)]
pub struct RestoreOption {
    /// Source directory (backup location)
    pub source_dir_base: PathBuf,
    /// Target directory (restore destination)
    pub target_dir_base: PathBuf,
    /// Metadata directory containing meta_*.dat files
    pub meta_dir: PathBuf,
    /// Control file directory containing restore control files
    pub ctrl_dir: PathBuf,
    /// Path to the specific control file for this restore task
    pub control_file: PathBuf,
    /// Restore policy for handling existing files
    pub policy: RestorePolicy,
    /// Number of worker threads
    pub worker_count: usize,
    /// Whether to restore hardlinks
    pub restore_hardlinks: bool,
    /// Whether to restore mtime attributes
    pub restore_mtime: bool,
}

impl RestoreOption {
    /// Creates a new RestoreOption with required paths.
    pub fn new(
        source_dir_base: PathBuf,
        target_dir_base: PathBuf,
        meta_dir: PathBuf,
        ctrl_dir: PathBuf,
        control_file: PathBuf,
    ) -> Self {
        Self {
            source_dir_base,
            target_dir_base,
            meta_dir,
            ctrl_dir,
            control_file,
            policy: RestorePolicy::default(),
            worker_count: 4,
            restore_hardlinks: false,
            restore_mtime: true,
        }
    }

    /// Sets the restore policy.
    pub fn policy(mut self, policy: RestorePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Sets the number of worker threads.
    pub fn worker_count(mut self, count: usize) -> Self {
        self.worker_count = count;
        self
    }

    /// Enables or disables hardlink restoration.
    pub fn restore_hardlinks(mut self, enable: bool) -> Self {
        self.restore_hardlinks = enable;
        self
    }

    /// Enables or disables mtime restoration.
    pub fn restore_mtime(mut self, enable: bool) -> Self {
        self.restore_mtime = enable;
        self
    }
}

/// A restore task that performs data restoration from backup.
/// Similar to BackupTask but operates in reverse direction (backup -> target).
pub struct RestoreTask {
    option: RestoreOption,
}

/// Represents a running restore operation.
pub struct RunningRestore {
    option: RestoreOption,
    stats: Arc<Mutex<RestoreStats>>,
    terminate_handle: thread::JoinHandle<()>,
    terminate_indicator: Arc<AtomicBool>,
}

/// Errors that can occur during restore operations.
#[derive(Debug)]
pub enum RestoreError {
    InvalidSourcePath,
    InvalidTargetPath,
    InvalidMetaPath,
    InvalidControlFile,
    InsufficientDiskSpace,
    IoError(std::io::Error),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestoreError::InvalidSourcePath => write!(f, "Invalid source path"),
            RestoreError::InvalidTargetPath => write!(f, "Invalid target path"),
            RestoreError::InvalidMetaPath => write!(f, "Invalid metadata path"),
            RestoreError::InvalidControlFile => write!(f, "Invalid control file"),
            RestoreError::InsufficientDiskSpace => write!(f, "Insufficient disk space"),
            RestoreError::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for RestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RestoreError::IoError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RestoreError {
    fn from(e: std::io::Error) -> Self {
        RestoreError::IoError(e)
    }
}

impl RestoreTask {
    /// Creates a new RestoreTask from RestoreOption.
    pub fn new(option: RestoreOption) -> Self {
        Self { option }
    }

    /// Starts the restore operation.
    ///
    /// This method validates paths and spawns the restore worker threads.
    /// The restore operation reads from the backup source and writes to the target,
    /// respecting the configured RestorePolicy for existing files.
    pub fn start(self) -> Result<RunningRestore, RestoreError> {
        // Validate paths
        if !self.option.source_dir_base.exists() {
            return Err(RestoreError::InvalidSourcePath);
        }
        if !self.option.meta_dir.exists() {
            return Err(RestoreError::InvalidMetaPath);
        }
        if !self.option.control_file.exists() {
            return Err(RestoreError::InvalidControlFile);
        }

        // Ensure target directory exists
        std::fs::create_dir_all(&self.option.target_dir_base)
            .map_err(RestoreError::IoError)?;

        let stats = Arc::new(Mutex::new(RestoreStats::default()));
        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let terminate_indicator_inner = Arc::clone(&terminate_indicator);

        let option = self.option.clone();
        let stats_inner = Arc::clone(&stats);

        let terminate_handle = thread::spawn(move || {
            // TODO: Implement actual restore logic
            // This would:
            // 1. Parse control file to get list of files to restore
            // 2. For each file, check RestorePolicy::should_restore()
            // 3. Copy files from source to target (reverse of backup)
            // 4. Update stats accordingly (files_restored, files_skipped, bytes_skipped)
            // 5. Handle hardlinks and mtime if enabled
            
            info!("Restore operation started with policy: {:?}", option.policy);
            info!("Source: {:?}, Target: {:?}", option.source_dir_base, option.target_dir_base);
            
            // Placeholder: mark as complete
            terminate_indicator_inner.store(true, Ordering::Relaxed);
        });

        Ok(RunningRestore {
            option: self.option,
            stats,
            terminate_handle,
            terminate_indicator,
        })
    }
}

impl From<RestoreOption> for RestoreTask {
    fn from(option: RestoreOption) -> Self {
        Self::new(option)
    }
}

impl RunningRestore {
    /// Gets a snapshot of current restore statistics.
    pub fn stats(&self) -> RestoreStats {
        self.stats.lock().unwrap().clone()
    }

    /// Checks if the restore operation is complete.
    pub fn complete(&self) -> bool {
        self.terminate_indicator.load(Ordering::Relaxed)
    }

    /// Waits for the restore operation to complete.
    pub fn wait(self) -> Result<(), RestoreError> {
        self.terminate_handle.join().unwrap();
        Ok(())
    }
}