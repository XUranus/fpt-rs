use std::{path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU32, Ordering}}, thread};
use log::info;
use crate::backup::{
        bio::hardlink::HardlinkStatsSnapshot,
        bio::mtime::MtimeStatsSnapshot,
        bio::delete::DeleteStatsSnapshot,
        stats::{BackupStats, BackupStatsSnapshot},
        aggregate::AggregateConfig,
    };

pub(crate) mod fcb;
mod bio;
mod stats;
pub mod sharded_processor;

// Aggregate backup/restore modules
pub mod aggregate;
pub mod aggregate_index;
pub mod aggregate_engine;
pub mod aggregate_restore;
mod restore_pipeline;

// Async I/O pipeline (used for remote targets / sources such as NFS and SMB)
#[cfg(any(feature = "nfs", feature = "smb"))]
pub mod aio;

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

    /// NFS target location.  When `Some`, the AIO pipeline writes to NFS
    /// instead of the local filesystem.  Requires the `nfs` feature.
    #[cfg(feature = "nfs")]
    pub nfs_target: Option<crate::nfs::NfsLocation>,

    /// NFS source location.  When `Some`, the AIO pipeline reads from NFS
    /// instead of the local filesystem.  Requires the `nfs` feature.
    #[cfg(feature = "nfs")]
    pub nfs_source: Option<crate::nfs::NfsLocation>,

    /// Relative path within the NFS target where D_REPO data should be written.
    /// e.g. `COPY_COMMON_FULL_xxx/D_REPO`. The NFS pool's root_fh points to the
    /// target's configured sub_path; this prefix is prepended to each dst_path
    /// to place files under the correct copy structure.
    #[cfg(feature = "nfs")]
    pub nfs_target_d_repo_path: Option<String>,

    /// SMB target location. When `Some`, the async pipeline writes to SMB
    /// instead of the local filesystem. Requires the `smb` feature.
    #[cfg(feature = "smb")]
    pub smb_target: Option<crate::smb::SmbLocation>,

    /// SMB source location. When `Some`, the async pipeline reads from SMB
    /// instead of the local filesystem. Requires the `smb` feature.
    #[cfg(feature = "smb")]
    pub smb_source: Option<crate::smb::SmbLocation>,

    /// Relative path within the SMB target where D_REPO data should be written.
    /// e.g. `COPY_COMMON_FULL_xxx/D_REPO`.
    #[cfg(feature = "smb")]
    pub smb_target_d_repo_path: Option<String>,
}



// each backup task do the data copy following the instruction of one control file
pub struct BackupTask {
    option : BackupOption,
}

pub struct RunningBackup {
    #[allow(dead_code)]
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
            #[cfg(feature = "nfs")]
            nfs_target: None,
            #[cfg(feature = "nfs")]
            nfs_source: None,
            #[cfg(feature = "nfs")]
            nfs_target_d_repo_path: None,
            #[cfg(feature = "smb")]
            smb_target: None,
            #[cfg(feature = "smb")]
            smb_source: None,
            #[cfg(feature = "smb")]
            smb_target_d_repo_path: None,
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

    /// Set an NFS target location.  When set, the AIO pipeline is used and
    /// files are written to the NFS server instead of the local filesystem.
    /// Requires the `nfs` Cargo feature.
    #[cfg(feature = "nfs")]
    pub fn nfs_target(mut self, loc: crate::nfs::NfsLocation) -> Self {
        self.nfs_target = Some(loc);
        self
    }

    /// Set an NFS source location.  When set, the AIO pipeline reads files
    /// from the NFS server instead of the local filesystem.
    /// Requires the `nfs` Cargo feature.
    #[cfg(feature = "nfs")]
    pub fn nfs_source(mut self, loc: crate::nfs::NfsLocation) -> Self {
        self.nfs_source = Some(loc);
        self
    }

    /// Set the relative path within the NFS target where D_REPO data should
    /// be written (e.g. `COPY_COMMON_FULL_xxx/D_REPO`).
    /// Requires the `nfs` Cargo feature.
    #[cfg(feature = "nfs")]
    pub fn nfs_target_d_repo_path(mut self, path: String) -> Self {
        self.nfs_target_d_repo_path = Some(path);
        self
    }

    /// Set an SMB target location. When set, the async pipeline writes files
    /// to the SMB share instead of the local filesystem.
    /// Requires the `smb` Cargo feature.
    #[cfg(feature = "smb")]
    pub fn smb_target(mut self, loc: crate::smb::SmbLocation) -> Self {
        self.smb_target = Some(loc);
        self
    }

    /// Set an SMB source location. When set, the async pipeline reads files
    /// from the SMB share instead of the local filesystem.
    /// Requires the `smb` Cargo feature.
    #[cfg(feature = "smb")]
    pub fn smb_source(mut self, loc: crate::smb::SmbLocation) -> Self {
        self.smb_source = Some(loc);
        self
    }

    /// Set the relative path within the SMB target where D_REPO data should
    /// be written (e.g. `COPY_COMMON_FULL_xxx/D_REPO`).
    /// Requires the `smb` Cargo feature.
    #[cfg(feature = "smb")]
    pub fn smb_target_d_repo_path(mut self, path: String) -> Self {
        self.smb_target_d_repo_path = Some(path);
        self
    }
}

pub(crate) struct SharedState {
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
        let stats = Arc::new(BackupStats::default());
        let shared_state = Arc::new(SharedState::default());
        let terminate_indicator = Arc::new(AtomicBool::new(false));

        // Capture the NFS target location (if any) before moving `self.option`.
        #[cfg(feature = "nfs")]
        let nfs_target = self.option.nfs_target.clone();
        #[cfg(feature = "nfs")]
        let nfs_source = self.option.nfs_source.clone();
        #[cfg(feature = "nfs")]
        let nfs_target_d_repo_path = self.option.nfs_target_d_repo_path.clone();
        #[cfg(feature = "smb")]
        let smb_target = self.option.smb_target.clone();
        #[cfg(feature = "smb")]
        let smb_source = self.option.smb_source.clone();
        #[cfg(feature = "smb")]
        let smb_target_d_repo_path = self.option.smb_target_d_repo_path.clone();

        // When both NFS source AND NFS target are configured, run the
        // dual-pool NFS→NFS AIO pipeline.
        #[cfg(feature = "nfs")]
        if let (Some(ref src_loc), Some(ref tgt_loc)) = (&nfs_source, &nfs_target) {
            let terminate_handle = crate::backup::aio::spawn_nfs_to_nfs_backup(
                src_loc.clone(),
                tgt_loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                nfs_target_d_repo_path.clone().unwrap_or_default(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        #[cfg(all(feature = "nfs", feature = "smb"))]
        if let (Some(ref src_loc), Some(ref tgt_loc)) = (&nfs_source, &smb_target) {
            let terminate_handle = crate::backup::aio::spawn_nfs_to_smb_backup(
                src_loc.clone(),
                tgt_loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                smb_target_d_repo_path.clone().unwrap_or_default(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        #[cfg(all(feature = "nfs", feature = "smb"))]
        if let (Some(ref src_loc), Some(ref tgt_loc)) = (&smb_source, &nfs_target) {
            let terminate_handle = crate::backup::aio::spawn_smb_to_nfs_backup(
                src_loc.clone(),
                tgt_loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                nfs_target_d_repo_path.clone().unwrap_or_default(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        // When an NFS target is configured (local source → NFS target),
        // run the entire pipeline on the AIO (async) path and skip the BIO pipeline.
        #[cfg(feature = "nfs")]
        if let Some(ref loc) = nfs_target {
            let terminate_handle = crate::backup::aio::spawn_local_to_nfs_backup(
                loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                nfs_target_d_repo_path.clone().unwrap_or_default(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        // When an NFS source is configured (NFS→local backup), use the
        // nfs_to_local AIO pipeline for the copy phase.
        #[cfg(feature = "nfs")]
        if let Some(ref loc) = nfs_source {
            let terminate_handle = crate::backup::aio::spawn_nfs_to_local_backup(
                loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                target_dir_base.clone(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        #[cfg(feature = "smb")]
        if let (Some(ref src_loc), Some(ref tgt_loc)) = (&smb_source, &smb_target) {
            let terminate_handle = crate::backup::aio::spawn_smb_to_smb_backup(
                src_loc.clone(),
                tgt_loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                smb_target_d_repo_path.clone().unwrap_or_default(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        #[cfg(feature = "smb")]
        if let Some(ref loc) = smb_target {
            let terminate_handle = crate::backup::aio::spawn_local_to_smb_backup(
                loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                smb_target_d_repo_path.clone().unwrap_or_default(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        #[cfg(feature = "smb")]
        if let Some(ref loc) = smb_source {
            let terminate_handle = crate::backup::aio::spawn_smb_to_local_backup(
                loc.clone(),
                control_file.clone(),
                meta_dir.clone(),
                ctrl_dir.clone(),
                source_dir_base.clone(),
                target_dir_base.clone(),
                self.option.aggregate_config,
                Arc::clone(&stats),
                Arc::clone(&terminate_indicator),
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            );

            return Ok(Self::running_backup(
                self.option,
                stats,
                terminate_handle,
                terminate_indicator,
            ));
        }

        let terminate_handle = bio::spawn_local_backup_pipeline(
            control_file,
            source_dir_base,
            target_dir_base,
            meta_dir,
            ctrl_dir,
            worker_count,
            self.option.aggregate_config,
            enable_hardlink_phase,
            enable_delete_phase,
            enable_mtime_phase,
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
    /// Original source base path recorded in the manifest.
    pub original_source_base: PathBuf,
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
    /// NFS target location.  When `Some`, the AIO pipeline writes restored
    /// files to the NFS server instead of the local filesystem.
    /// Requires the `nfs` Cargo feature.
    #[cfg(feature = "nfs")]
    pub nfs_target: Option<crate::nfs::NfsLocation>,
    /// SMB target location. When `Some`, restored files are written to SMB.
    #[cfg(feature = "smb")]
    pub smb_target: Option<crate::smb::SmbLocation>,
}

impl RestoreOption {
    /// Creates a new RestoreOption with required paths.
    pub fn new(
        source_dir_base: PathBuf,
        original_source_base: PathBuf,
        target_dir_base: PathBuf,
        meta_dir: PathBuf,
        ctrl_dir: PathBuf,
        control_file: PathBuf,
    ) -> Self {
        Self {
            source_dir_base,
            original_source_base,
            target_dir_base,
            meta_dir,
            ctrl_dir,
            control_file,
            policy: RestorePolicy::default(),
            worker_count: 4,
            restore_hardlinks: false,
            restore_mtime: true,
            #[cfg(feature = "nfs")]
            nfs_target: None,
            #[cfg(feature = "smb")]
            smb_target: None,
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

    /// Set an NFS target location.  When set, the AIO pipeline is used and
    /// restored files are written to the NFS server instead of the local
    /// filesystem.  Requires the `nfs` Cargo feature.
    #[cfg(feature = "nfs")]
    pub fn nfs_target(mut self, loc: crate::nfs::NfsLocation) -> Self {
        self.nfs_target = Some(loc);
        self
    }

    /// Set an SMB target location for restore.
    #[cfg(feature = "smb")]
    pub fn smb_target(mut self, loc: crate::smb::SmbLocation) -> Self {
        self.smb_target = Some(loc);
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
        std::fs::create_dir_all(&self.option.target_dir_base)
            .map_err(RestoreError::IoError)?;

        let stats = Arc::new(Mutex::new(RestoreStats::default()));
        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let terminate_indicator_inner = Arc::clone(&terminate_indicator);

        let option = self.option.clone();
        let stats_inner = Arc::clone(&stats);

        let terminate_handle = thread::spawn(move || {
            info!("Restore operation started with policy: {:?}", option.policy);
            info!("Source: {:?}, Target: {:?}", option.source_dir_base, option.target_dir_base);

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

    if control_name == "hardlink.txt" {
        if option.restore_hardlinks {
            run_restore_hardlink_phase(&option)?;
        }
        return Ok(());
    }

    if control_name == "delete.txt" {
        run_restore_delete_phase(&option)?;
        return Ok(());
    }

    if control_name == "mtime.txt" {
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
        .thread_name("bifrost-restore-copy")
        .build()
        .map_err(RestoreError::IoError)?;

    let source = restore_pipeline::LocalRepoRestoreSource::new(
        option.source_dir_base.clone(),
        option.original_source_base.clone(),
    )
        .map_err(|e| RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    #[cfg(feature = "nfs")]
    if let Some(nfs_target) = &option.nfs_target {
        let nfs_target = nfs_target.clone();
        return rt.block_on(async {
            let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                .await
                .map_err(|e| RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
            let root_fh = pool.root_fh();
            let write_chunk = pool.server_wtmax.max(4096);
            let target = crate::backup::aio::transport::NfsTarget {
                pool,
                dir_cache: crate::nfs::aio::writer::new_dir_handle_cache(),
                root_fh,
                write_chunk,
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
    if let Some(smb_target) = &option.smb_target {
        let smb_target = smb_target.clone();
        return rt.block_on(async {
            let pool = crate::smb::aio::SmbClientPool::connect(&smb_target, option.worker_count.max(1))
                .await
                .map_err(|e| RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
            let target = crate::backup::aio::transport::SmbTarget {
                location: smb_target,
                pool,
                dir_cache: crate::smb::aio::new_dir_cache(),
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
    #[cfg(feature = "nfs")]
    if let Some(nfs_target) = &option.nfs_target {
        let nfs_target = nfs_target.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-restore-hardlink-nfs")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async move {
            let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                .await
                .map_err(|e| RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
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
    if let Some(smb_target) = &option.smb_target {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-restore-hardlink-smb")
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

    crate::backup::bio::hardlink::run_hardlink_phase(
        &option.ctrl_dir,
        &option.meta_dir,
        &option.original_source_base,
        &option.target_dir_base,
    )
    .map(|_| ())
    .map_err(RestoreError::IoError)
}

fn run_restore_delete_phase(option: &RestoreOption) -> Result<(), RestoreError> {
    #[cfg(feature = "nfs")]
    if let Some(nfs_target) = &option.nfs_target {
        let nfs_target = nfs_target.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-restore-delete-nfs")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async move {
            let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                .await
                .map_err(|e| RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
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
    if let Some(smb_target) = &option.smb_target {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-restore-delete-smb")
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

    crate::backup::bio::delete::run_delete_phase(
        &option.ctrl_dir,
        &option.original_source_base,
        &option.target_dir_base,
    )
    .map(|_| ())
    .map_err(RestoreError::IoError)
}

fn run_restore_mtime_phase(option: &RestoreOption) -> Result<(), RestoreError> {
    #[cfg(feature = "nfs")]
    if let Some(nfs_target) = &option.nfs_target {
        let nfs_target = nfs_target.clone();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-restore-mtime-nfs")
            .build()
            .map_err(RestoreError::IoError)?;
        rt.block_on(async move {
            let pool = crate::nfs::connection::NfsConnectionPool::new(&nfs_target)
                .await
                .map_err(|e| RestoreError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
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
    if let Some(smb_target) = &option.smb_target {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-restore-mtime-smb")
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

    crate::backup::bio::mtime::run_mtime_phase(
        &option.ctrl_dir,
        &option.original_source_base,
        &option.target_dir_base,
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
