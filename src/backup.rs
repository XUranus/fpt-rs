use crate::backup::{
    aggregate::AggregateConfig,
    stats::BackupStats,
};
use crate::failure::{FailureLogConfig, FailureRecorder, RetryPolicy};
use crate::frame::control_files::classify_control_file_name;
use log::info;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread,
};

pub(crate) mod copy_block;
pub(crate) mod copy_plan;
pub(crate) mod fcb;
pub(crate) mod stats;

// Re-export snapshot types used in RunningBackup fields
pub(crate) use crate::native::backup::bio::delete::DeleteStatsSnapshot;
pub(crate) use crate::native::backup::bio::hardlink::HardlinkStatsSnapshot;
pub(crate) use crate::native::backup::bio::mtime::MtimeStatsSnapshot;
pub use stats::BackupStatsSnapshot;

// Aggregate backup/restore modules
pub mod aggregate;
mod restore_pipeline;

// Async I/O pipeline (used for remote targets / sources such as NFS and SMB).
#[cfg(any(feature = "nfs", feature = "smb"))]
pub(crate) mod aio;

// Restore uses the generic local AIO helpers even in a no-remote-feature build.
#[cfg(not(any(feature = "nfs", feature = "smb")))]
#[allow(dead_code)]
pub(crate) mod aio {
    #[allow(unused_imports)]
    pub mod copy_block {
        pub use crate::backup::copy_block::*;
    }
    pub mod entry;
    pub mod local_fs;
    pub mod transport;
}

pub struct BackupOption {
    /// Data source location (local path, NFS export, or SMB share).
    source: crate::frame::location::DataLocation,
    /// Data target location (local path, NFS export, or SMB share).
    target: crate::frame::location::DataLocation,

    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    control_file: PathBuf,

    worker_count: usize,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_log: Option<FailureLogConfig>,

    /// Post-copy phases enabled for this task.
    phase_flags: PhaseFlags,

    /// Aggregate backup configuration
    pub aggregate_config: AggregateConfig,

    /// Relative path prepended to D_REPO data on remote targets
    /// (e.g. `COPY_COMMON_FULL_xxx/D_REPO`). Computed at the frame layer.
    target_prefix: Option<String>,

    /// Number of SMB client connections per SMB endpoint.
    #[cfg(feature = "smb")]
    pub smb_connection_count: usize,
    /// Maximum concurrent SMB file copy tasks. 0 means auto.
    #[cfg(feature = "smb")]
    pub smb_copy_task_count: usize,
}

/// Named flags for post-copy phases.
///
/// This avoids passing several adjacent booleans through backup code.
#[derive(Debug, Clone, Copy, Default)]
pub struct PhaseFlags {
    pub hardlink: bool,
    pub delete: bool,
    pub mtime: bool,
}

// each backup task do the data copy following the instruction of one control file
pub struct BackupTask {
    option: BackupOption,
}

pub struct RunningBackup {
    #[allow(dead_code)]
    option: BackupOption,
    stats: Arc<BackupStats>,
    hardlink_stats: Option<HardlinkStatsSnapshot>,
    delete_stats: Option<DeleteStatsSnapshot>,
    mtime_stats: Option<MtimeStatsSnapshot>,
    terminate_handle: thread::JoinHandle<()>,
    terminate_indicator: Arc<AtomicBool>,
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
    /// Create a new backup configuration.
    ///
    /// The `source_dir_base` is the root of the data to back up. The `target_dir_base`
    /// is where the copy will be created. `meta_dir` and `ctrl_dir` hold scan metadata
    /// and control files respectively. `control_file` is the path to the main control
    /// file listing entries to process.
    pub fn new(
        source: crate::frame::location::DataLocation,
        target: crate::frame::location::DataLocation,
        meta_dir: PathBuf,
        ctrl_dir: PathBuf,
        control_file: PathBuf,
    ) -> Self {
        Self {
            worker_count: 8,
            copy_buffer_size: 1024 * 1024,
            retry_policy: RetryPolicy::default(),
            failure_log: None,
            source,
            target,
            meta_dir,
            ctrl_dir,
            control_file,
            phase_flags: PhaseFlags::default(),
            aggregate_config: AggregateConfig::default(),
            target_prefix: None,
            #[cfg(feature = "smb")]
            smb_connection_count: crate::backup::aio::DEFAULT_SMB_POOL_SIZE,
            #[cfg(feature = "smb")]
            smb_copy_task_count: 0,
        }
    }

    /// Enable the hardlink phase
    pub fn enable_hardlink_phase(mut self, enable: bool) -> Self {
        self.phase_flags.hardlink = enable;
        self
    }

    /// Enable the delete phase
    pub fn enable_delete_phase(mut self, enable: bool) -> Self {
        self.phase_flags.delete = enable;
        self
    }

    /// Enable the mtime phase
    pub fn enable_mtime_phase(mut self, enable: bool) -> Self {
        self.phase_flags.mtime = enable;
        self
    }

    /// Set all post-copy phase flags at once.
    pub fn phase_flags(mut self, flags: PhaseFlags) -> Self {
        self.phase_flags = flags;
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

    /// Set maximum per-file copy buffer size in bytes.
    pub fn copy_buffer_size(mut self, size: usize) -> Self {
        self.copy_buffer_size = size.clamp(256 * 1024, 4 * 1024 * 1024);
        self
    }

    /// Set the retry policy for I/O operations (copy, stat, mkdir).
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    /// Set the failure log configuration. When `Some`, failed operations are
    /// recorded to a structured log file for post-mortem analysis.
    pub fn failure_log(mut self, config: Option<FailureLogConfig>) -> Self {
        self.failure_log = config;
        self
    }

    /// Set the relative path prepended to D_REPO on remote targets
    /// (e.g. `COPY_COMMON_FULL_xxx/D_REPO`).
    pub fn target_prefix(mut self, prefix: String) -> Self {
        self.target_prefix = Some(prefix);
        self
    }

    /// Set SMB client connections per SMB endpoint for the async backup path.
    /// Requires the `smb` Cargo feature.
    #[cfg(feature = "smb")]
    pub fn smb_connection_count(mut self, count: usize) -> Self {
        self.smb_connection_count = count.max(1);
        self
    }

    /// Set maximum concurrent SMB file copy tasks. 0 means auto.
    /// Requires the `smb` Cargo feature.
    #[cfg(feature = "smb")]
    pub fn smb_copy_task_count(mut self, count: usize) -> Self {
        self.smb_copy_task_count = count;
        self
    }
}

#[allow(dead_code)]
pub(crate) struct SharedState {
    pub entry_produce_done: AtomicBool,
    pub reader_done: AtomicBool,
    pub writer_done: AtomicBool,
    pub active_reader_io_workers: AtomicU32,
    pub active_writer_io_workers: AtomicU32,
}

impl Default for SharedState {
    fn default() -> Self {
        SharedState {
            entry_produce_done: AtomicBool::new(false),
            reader_done: AtomicBool::new(false),
            writer_done: AtomicBool::new(false),
            active_reader_io_workers: AtomicU32::new(0),
            active_writer_io_workers: AtomicU32::new(0),
        }
    }
}

impl BackupTask {
    /// Start the backup execution.
    ///
    /// Spawns worker threads that read control files and copy data from source to target.
    /// Returns a [`RunningBackup`] handle for monitoring progress and waiting on completion.
    pub fn start(self) -> Result<RunningBackup, BackupError> {
        use crate::frame::location::DataLocation;

        let worker_count = self.option.worker_count;
        let copy_buffer_size = self.option.copy_buffer_size;
        let control_file = self.option.control_file.clone();
        let source_dir_base = self.option.source.base_path();
        let target_dir_base = self.option.target.base_path();
        let meta_dir = self.option.meta_dir.clone();
        let ctrl_dir = self.option.ctrl_dir.clone();
        let phase_flags = self.option.phase_flags;
        let stats = Arc::new(BackupStats::default());
        let shared_state = Arc::new(SharedState::default());
        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let failure_recorder = self
            .option
            .failure_log
            .as_ref()
            .and_then(|cfg| FailureRecorder::create(cfg).ok());
        let retry_policy = self.option.retry_policy;

        #[cfg(feature = "smb")]
        let smb_connection_count = self.option.smb_connection_count;
        #[cfg(feature = "smb")]
        let smb_copy_task_count = self.option.smb_copy_task_count;

        #[allow(unused_variables)]
        let target_prefix = self.option.target_prefix.clone().unwrap_or_default();

        let remote_handle = match (&self.option.source, &self.option.target) {
            (DataLocation::Local(_), DataLocation::Local(_)) => None,

            #[cfg(feature = "nfs")]
            (DataLocation::Local(_), DataLocation::Nfs(nfs_target)) => {
                Some(crate::backup::aio::spawn_local_to_nfs_backup(
                    nfs_target.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_prefix,
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    phase_flags,
                ))
            }

            #[cfg(feature = "nfs")]
            (DataLocation::Nfs(nfs_source), DataLocation::Local(_)) => {
                Some(crate::backup::aio::spawn_nfs_to_local_backup(
                    nfs_source.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_dir_base.clone(),
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    phase_flags,
                ))
            }

            #[cfg(feature = "nfs")]
            (DataLocation::Nfs(nfs_source), DataLocation::Nfs(nfs_target)) => {
                Some(crate::backup::aio::spawn_nfs_to_nfs_backup(
                    nfs_source.clone(),
                    nfs_target.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_prefix,
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    phase_flags,
                ))
            }

            #[cfg(feature = "smb")]
            (DataLocation::Local(_), DataLocation::Smb(smb_target)) => {
                Some(crate::backup::aio::spawn_local_to_smb_backup(
                    smb_target.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_prefix,
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    smb_connection_count,
                    smb_copy_task_count,
                    phase_flags,
                ))
            }

            #[cfg(feature = "smb")]
            (DataLocation::Smb(smb_source), DataLocation::Local(_)) => {
                Some(crate::backup::aio::spawn_smb_to_local_backup(
                    smb_source.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_dir_base.clone(),
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    smb_connection_count,
                    smb_copy_task_count,
                    phase_flags,
                ))
            }

            #[cfg(feature = "smb")]
            (DataLocation::Smb(smb_source), DataLocation::Smb(smb_target)) => {
                Some(crate::backup::aio::spawn_smb_to_smb_backup(
                    smb_source.clone(),
                    smb_target.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_prefix,
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    smb_connection_count,
                    smb_copy_task_count,
                    phase_flags,
                ))
            }

            #[cfg(all(feature = "nfs", feature = "smb"))]
            (DataLocation::Nfs(nfs_source), DataLocation::Smb(smb_target)) => {
                Some(crate::backup::aio::spawn_nfs_to_smb_backup(
                    nfs_source.clone(),
                    smb_target.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_prefix,
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    smb_connection_count,
                    smb_copy_task_count,
                    phase_flags,
                ))
            }

            #[cfg(all(feature = "nfs", feature = "smb"))]
            (DataLocation::Smb(smb_source), DataLocation::Nfs(nfs_target)) => {
                Some(crate::backup::aio::spawn_smb_to_nfs_backup(
                    smb_source.clone(),
                    nfs_target.clone(),
                    control_file.clone(),
                    meta_dir.clone(),
                    ctrl_dir.clone(),
                    source_dir_base.clone(),
                    target_prefix,
                    self.option.aggregate_config,
                    copy_buffer_size,
                    retry_policy,
                    failure_recorder.clone(),
                    Arc::clone(&stats),
                    Arc::clone(&terminate_indicator),
                    smb_connection_count,
                    smb_copy_task_count,
                    phase_flags,
                ))
            }
        };

        if let Some(terminate_handle) = remote_handle {
            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        let terminate_handle = crate::native::backup::bio::spawn_local_backup_pipeline(
            control_file,
            source_dir_base,
            target_dir_base,
            meta_dir,
            ctrl_dir,
            worker_count,
            copy_buffer_size,
            retry_policy,
            failure_recorder,
            self.option.aggregate_config,
            phase_flags,
            Arc::clone(&shared_state),
            Arc::clone(&stats),
            Arc::clone(&terminate_indicator),
        );

        Ok(Self::running_backup(
            self.option,
            stats,
            terminate_handle,
            terminate_indicator,
        ))
    }

    fn running_backup(
        option: BackupOption,
        stats: Arc<BackupStats>,
        terminate_handle: thread::JoinHandle<()>,
        terminate_indicator: Arc<AtomicBool>,
    ) -> RunningBackup {
        RunningBackup {
            option,
            stats,
            hardlink_stats: None,
            delete_stats: None,
            mtime_stats: None,
            terminate_handle,
            terminate_indicator,
        }
    }
}

impl From<BackupOption> for BackupTask {
    fn from(option: BackupOption) -> Self {
        Self { option }
    }
}

impl RunningBackup {
    /// Snapshot the current copy-phase statistics (files, bytes, dirs).
    pub fn stats(&self) -> BackupStatsSnapshot {
        self.stats.snapshot()
    }

    /// Snapshot hardlink-phase statistics, if the hardlink phase was enabled.
    pub fn hardlink_stats(&self) -> Option<&HardlinkStatsSnapshot> {
        self.hardlink_stats.as_ref()
    }

    /// Snapshot delete-phase statistics, if the delete phase was enabled.
    pub fn delete_stats(&self) -> Option<&DeleteStatsSnapshot> {
        self.delete_stats.as_ref()
    }

    /// Snapshot mtime-phase statistics, if the mtime phase was enabled.
    pub fn mtime_stats(&self) -> Option<&MtimeStatsSnapshot> {
        self.mtime_stats.as_ref()
    }

    /// Returns `true` if the backup has finished (success or failure).
    pub fn complete(&self) -> bool {
        self.terminate_indicator.load(Ordering::Relaxed)
    }

    /// Block until the backup completes. Returns `Ok(())` on success.
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
    /// Original source base path recorded in the manifest.
    pub original_source_base: PathBuf,
    /// Target location (restore destination — local, NFS, or SMB).
    pub target: crate::frame::location::DataLocation,
    /// Target directory (restore destination) — derived from target for local paths.
    pub target_dir_base: PathBuf,
    /// Metadata directory containing meta_*.dat files
    pub meta_dir: PathBuf,
    /// Control file directory containing restore control files
    pub ctrl_dir: PathBuf,
    /// Path to the specific control file for this restore task
    pub control_file: PathBuf,
    /// Restore policy for handling existing files
    pub policy: RestorePolicy,
    /// Aggregation settings recorded by the source copy manifest.
    pub aggregate_config: AggregateConfig,
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
        original_source_base: PathBuf,
        target: crate::frame::location::DataLocation,
        meta_dir: PathBuf,
        ctrl_dir: PathBuf,
        control_file: PathBuf,
    ) -> Self {
        Self {
            source_dir_base,
            original_source_base,
            target_dir_base: target.base_path(),
            target,
            meta_dir,
            ctrl_dir,
            control_file,
            policy: RestorePolicy::default(),
            aggregate_config: AggregateConfig::default(),
            worker_count: 8,
            restore_hardlinks: false,
            restore_mtime: true,
        }
    }

    /// Sets the restore policy.
    pub fn policy(mut self, policy: RestorePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn aggregate_config(mut self, config: AggregateConfig) -> Self {
        self.aggregate_config = config;
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
    #[allow(dead_code)]
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
        std::fs::create_dir_all(&self.option.target_dir_base).map_err(RestoreError::IoError)?;

        let stats = Arc::new(Mutex::new(RestoreStats::default()));
        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let terminate_indicator_inner = Arc::clone(&terminate_indicator);

        let option = self.option.clone();
        let stats_inner = Arc::clone(&stats);

        let terminate_handle = thread::spawn(move || {
            info!("Restore operation started with policy: {:?}", option.policy);
            info!(
                "Source: {:?}, Target: {:?}",
                option.source_dir_base, option.target_dir_base
            );

            if let Err(e) = run_restore_task(option, stats_inner) {
                log::error!("Restore operation failed: {e}");
            }

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

fn run_restore_task(
    option: RestoreOption,
    stats: Arc<Mutex<RestoreStats>>,
) -> Result<(), RestoreError> {
    let control_name = option
        .control_file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();

    if classify_control_file_name(&control_name) == Some("hardlink") {
        if option.restore_hardlinks {
            run_restore_hardlink_phase(&option)?;
        }
        return Ok(());
    }

    if classify_control_file_name(&control_name) == Some("delete") {
        run_restore_delete_phase(&option)?;
        return Ok(());
    }

    if classify_control_file_name(&control_name) == Some("mtime") {
        if option.restore_mtime {
            run_restore_mtime_phase(&option)?;
        }
        return Ok(());
    }

    run_restore_copy_phase(&option, stats)
}

fn run_restore_copy_phase(
    option: &RestoreOption,
    stats: Arc<Mutex<RestoreStats>>,
) -> Result<(), RestoreError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("fpt-restore-copy")
        .build()
        .map_err(RestoreError::IoError)?;

    let source = restore_pipeline::LocalRepoRestoreSource::new(
        option.source_dir_base.clone(),
        option.original_source_base.clone(),
        option.aggregate_config.layout,
    )
    .map_err(|e| RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    use crate::frame::location::DataLocation;

    match &option.target {
        #[cfg(feature = "nfs")]
        DataLocation::Nfs(nfs_target) => {
            let nfs_target = nfs_target.clone();
            return rt.block_on(async {
                let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                    .await
                    .map_err(|e| {
                        RestoreError::IoError(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        ))
                    })?;
                let root_fh = pool.root_fh();
                let write_chunk = pool.server_wtmax.max(4096);
                let target = crate::nfs::backup::transport::NfsTarget {
                    pool,
                    dir_cache: crate::nfs::aio::writer::new_dir_handle_cache(),
                    root_fh,
                    write_chunk,
                    buffer_size: crate::backup::aio::transport::DEFAULT_COPY_BUFFER_SIZE,
                };
                restore_pipeline::run_restore_copy_pipeline(
                    option.control_file.clone(),
                    option.meta_dir.clone(),
                    option.original_source_base.clone(),
                    source,
                    target,
                    None,
                    option.policy,
                    stats,
                    "restore-copy-nfs",
                    option.worker_count,
                )
                .await;
                Ok::<(), RestoreError>(())
            });
        }
        #[cfg(feature = "smb")]
        DataLocation::Smb(smb_target) => {
            let smb_target = smb_target.clone();
            return rt.block_on(async {
                let pool =
                    crate::smb::aio::SmbClientPool::connect(&smb_target, option.worker_count.max(1))
                        .await
                        .map_err(|e| {
                            RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e))
                        })?;
                let target = crate::smb::backup::transport::SmbTarget {
                    location: smb_target,
                    pool,
                    dir_cache: crate::smb::aio::new_dir_cache(),
                    buffer_size: crate::backup::aio::transport::DEFAULT_COPY_BUFFER_SIZE,
                };
                restore_pipeline::run_restore_copy_pipeline(
                    option.control_file.clone(),
                    option.meta_dir.clone(),
                    option.original_source_base.clone(),
                    source,
                    target,
                    None,
                    option.policy,
                    stats,
                    "restore-copy-smb",
                    option.worker_count,
                )
                .await;
                Ok::<(), RestoreError>(())
            });
        }
        DataLocation::Local(_) => {}
    }

    let target = crate::backup::aio::transport::LocalTarget {
        base: option.target_dir_base.clone(),
    };
    rt.block_on(async {
        restore_pipeline::run_restore_copy_pipeline(
            option.control_file.clone(),
            option.meta_dir.clone(),
            option.original_source_base.clone(),
            source,
            target,
            Some(option.target_dir_base.clone()),
            option.policy,
            stats,
            "restore-copy-local",
            option.worker_count,
        )
        .await;
    });
    Ok(())
}

fn run_restore_hardlink_phase(option: &RestoreOption) -> Result<(), RestoreError> {
    #[allow(unused_imports)]
    use crate::frame::location::DataLocation;

    #[cfg(feature = "nfs")]
    if let DataLocation::Nfs(nfs_target) = &option.target {
        let nfs_target = nfs_target.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-restore-hardlink-nfs")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async move {
            let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                .await
                .map_err(|e| {
                    RestoreError::IoError(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))
                })?;
            crate::nfs::aio::hardlink::run_nfs_hardlink_phase(
                &option.ctrl_dir,
                &option.original_source_base,
                "",
                Arc::clone(&pool),
                crate::nfs::aio::reader::new_file_handle_cache(),
                crate::nfs::aio::writer::new_dir_handle_cache(),
            )
            .await;
            Ok::<(), RestoreError>(())
        })?;
        return Ok(());
    }

    #[cfg(feature = "smb")]
    if let DataLocation::Smb(smb_target) = &option.target {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-restore-hardlink-smb")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async {
            crate::smb::aio::hardlink::run_smb_hardlink_phase(
                &option.ctrl_dir,
                &option.original_source_base,
                "",
                smb_target,
            )
            .await;
        });
        return Ok(());
    }

    crate::native::backup::bio::hardlink::run_hardlink_phase(
        &option.ctrl_dir,
        &option.meta_dir,
        &option.original_source_base,
        &option.target_dir_base,
        crate::failure::RetryPolicy::default(),
        None,
    )
    .map(|_| ())
    .map_err(RestoreError::IoError)
}

fn run_restore_delete_phase(option: &RestoreOption) -> Result<(), RestoreError> {
    #[allow(unused_imports)]
    use crate::frame::location::DataLocation;

    #[cfg(feature = "nfs")]
    if let DataLocation::Nfs(nfs_target) = &option.target {
        let nfs_target = nfs_target.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-restore-delete-nfs")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async move {
            let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                .await
                .map_err(|e| {
                    RestoreError::IoError(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))
                })?;
            crate::nfs::aio::delete::run_nfs_delete_phase(
                &option.ctrl_dir,
                &option.original_source_base,
                "",
                pool,
                crate::nfs::aio::reader::new_file_handle_cache(),
            )
            .await;
            Ok::<(), RestoreError>(())
        })?;
        return Ok(());
    }

    #[cfg(feature = "smb")]
    if let DataLocation::Smb(smb_target) = &option.target {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-restore-delete-smb")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async {
            crate::smb::aio::delete::run_smb_delete_phase(
                &option.ctrl_dir,
                &option.original_source_base,
                "",
                smb_target,
            )
            .await;
        });
        return Ok(());
    }

    crate::native::backup::bio::delete::run_delete_phase(
        &option.ctrl_dir,
        &option.original_source_base,
        &option.target_dir_base,
        crate::failure::RetryPolicy::default(),
        None,
    )
    .map(|_| ())
    .map_err(RestoreError::IoError)
}

fn run_restore_mtime_phase(option: &RestoreOption) -> Result<(), RestoreError> {
    #[allow(unused_imports)]
    use crate::frame::location::DataLocation;

    #[cfg(feature = "nfs")]
    if let DataLocation::Nfs(nfs_target) = &option.target {
        let nfs_target = nfs_target.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-restore-mtime-nfs")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async move {
            let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                .await
                .map_err(|e| {
                    RestoreError::IoError(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))
                })?;
            crate::nfs::aio::mtime::run_nfs_mtime_phase(
                &option.ctrl_dir,
                &option.original_source_base,
                "",
                pool,
                crate::nfs::aio::reader::new_file_handle_cache(),
            )
            .await;
            Ok::<(), RestoreError>(())
        })?;
        return Ok(());
    }

    #[cfg(feature = "smb")]
    if let DataLocation::Smb(smb_target) = &option.target {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-restore-mtime-smb")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async {
            crate::smb::aio::mtime::run_smb_mtime_phase(
                &option.ctrl_dir,
                &option.original_source_base,
                "",
                smb_target,
            )
            .await;
        });
        return Ok(());
    }

    crate::native::backup::bio::mtime::run_mtime_phase(
        &option.ctrl_dir,
        &option.original_source_base,
        &option.target_dir_base,
        crate::failure::RetryPolicy::default(),
        None,
    )
    .map(|_| ())
    .map_err(RestoreError::IoError)
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
