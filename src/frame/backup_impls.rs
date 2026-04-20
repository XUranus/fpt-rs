//! Concrete [`FileBackup`] implementations.
//!
//! | Type | Target | Pipeline |
//! |------|--------|----------|
//! | [`LocalFileBackup`] | Local filesystem path | Blocking BIO threads, `std::fs` |
//! | [`NfsFileBackup`]   | NFSv3 export          | Tokio AIO tasks, `nfs3_client` WRITE RPCs |
//! | [`SmbFileBackup`]   | SMB share             | Tokio AIO tasks, `smb-rs` async client |
//!
//! Both types read source data and metadata from local paths.  Only the
//! **data write destination** differs.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::backup::{BackupOption, BackupTask};
use crate::backup::aggregate::AggregateConfig;
use crate::frame::traits::{FileBackup, TransferStats};

// ---------------------------------------------------------------------------
// BackupConfig — shared configuration for one subtask
// ---------------------------------------------------------------------------

/// Configuration for a single backup subtask execution.
///
/// Both [`LocalFileBackup`] and [`NfsFileBackup`] accept this struct.
#[derive(Debug, Clone)]
pub struct BackupConfig {
    /// Local source root directory.
    pub source_dir: PathBuf,
    /// Local D_REPO staging directory (used by local target; ignored by NFS target).
    pub local_target_dir: PathBuf,
    /// Local M_REPO/meta directory (always BIO).
    pub meta_dir: PathBuf,
    /// Local C_REPO/ctrl directory (always BIO).
    pub ctrl_dir: PathBuf,
    /// Control file that describes what to copy.
    pub control_file: PathBuf,
    /// Aggregation settings.
    pub aggregate_config: AggregateConfig,
    /// Override relative target prefix used for remote D_REPO writes.
    pub remote_target_prefix: Option<String>,
    /// Whether to run the hardlink phase.
    pub enable_hardlink: bool,
    /// Whether to run the delete phase.
    pub enable_delete: bool,
    /// Whether to run the mtime phase.
    pub enable_mtime: bool,
}

impl BackupConfig {
    pub fn new(
        source_dir:       impl Into<PathBuf>,
        local_target_dir: impl Into<PathBuf>,
        meta_dir:         impl Into<PathBuf>,
        ctrl_dir:         impl Into<PathBuf>,
        control_file:     impl Into<PathBuf>,
    ) -> Self {
        Self {
            source_dir:       source_dir.into(),
            local_target_dir: local_target_dir.into(),
            meta_dir:         meta_dir.into(),
            ctrl_dir:         ctrl_dir.into(),
            control_file:     control_file.into(),
            aggregate_config: AggregateConfig::default(),
            remote_target_prefix: None,
            enable_hardlink:  false,
            enable_delete:    false,
            enable_mtime:     false,
        }
    }

    pub fn aggregate_config(mut self, cfg: AggregateConfig) -> Self {
        self.aggregate_config = cfg; self
    }
    pub fn remote_target_prefix(mut self, prefix: Option<String>) -> Self {
        self.remote_target_prefix = prefix; self
    }
    pub fn enable_hardlink(mut self, v: bool) -> Self { self.enable_hardlink = v; self }
    pub fn enable_delete(mut self, v: bool) -> Self   { self.enable_delete = v; self }
    pub fn enable_mtime(mut self, v: bool) -> Self    { self.enable_mtime = v; self }
}

// ---------------------------------------------------------------------------
// BackupTaskError — shared error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum BackupTaskError {
    Engine(String),
    PartialFailure { files_failed: u64 },
}

impl fmt::Display for BackupTaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackupTaskError::Engine(s) =>
                write!(f, "backup engine error: {s}"),
            BackupTaskError::PartialFailure { files_failed } =>
                write!(f, "{files_failed} file(s) failed to back up"),
        }
    }
}

impl std::error::Error for BackupTaskError {}

// ---------------------------------------------------------------------------
// LocalFileBackup
// ---------------------------------------------------------------------------

/// Backs up data to a **local filesystem** target using the BIO pipeline.
///
/// Uses blocking OS threads and `std::fs` for all I/O.
pub struct LocalFileBackup {
    pub config: BackupConfig,
}

impl LocalFileBackup {
    pub fn new(config: BackupConfig) -> Self { Self { config } }
}

impl FileBackup for LocalFileBackup {
    type Error = BackupTaskError;

    fn run(&self) -> Result<TransferStats, BackupTaskError> {
        let cfg = &self.config;
        let option = BackupOption::new(
            cfg.source_dir.clone(),
            cfg.local_target_dir.clone(),
            cfg.meta_dir.clone(),
            cfg.ctrl_dir.clone(),
            cfg.control_file.clone(),
        )
        .enable_hardlink_phase(cfg.enable_hardlink)
        .enable_delete_phase(cfg.enable_delete)
        .enable_mtime_phase(cfg.enable_mtime)
        .aggregate_config(cfg.aggregate_config.clone());

        run_backup_task(option)
    }
}

// ---------------------------------------------------------------------------
// NfsFileBackup
// ---------------------------------------------------------------------------

#[cfg(feature = "nfs")]
pub use nfs_impl::NfsFileBackup;
#[cfg(feature = "nfs")]
pub use nfs_impl::NfsSourceFileBackup;
#[cfg(feature = "nfs")]
pub use nfs_impl::NfsSourceTargetFileBackup;
#[cfg(feature = "smb")]
pub use smb_impl::SmbFileBackup;
#[cfg(feature = "smb")]
pub use smb_impl::SmbSourceFileBackup;
#[cfg(feature = "smb")]
pub use smb_impl::SmbSourceTargetFileBackup;
#[cfg(all(feature = "nfs", feature = "smb"))]
pub use mixed_impl::{NfsSourceSmbTargetFileBackup, SmbSourceNfsTargetFileBackup};

#[cfg(feature = "nfs")]
mod nfs_impl {
    use super::*;
    use crate::nfs::NfsLocation;

    /// Backs up data to an **NFS server** target using the AIO pipeline.
    ///
    /// Metadata (M_REPO, C_REPO) is always written locally via BIO.
    /// Data files are sent directly to the NFS server via `nfs3_client` WRITE RPCs.
    pub struct NfsFileBackup {
        pub config:     BackupConfig,
        pub nfs_target: NfsLocation,
    }

    impl NfsFileBackup {
        pub fn new(config: BackupConfig, nfs_target: NfsLocation) -> Self {
            Self { config, nfs_target }
        }
    }

    impl FileBackup for NfsFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .nfs_target(self.nfs_target.clone())
            .nfs_target_d_repo_path(cfg.remote_target_prefix.clone().unwrap_or_else(|| extract_repo_relative_path(&cfg.local_target_dir)));

            run_backup_task(option)
        }
    }

    /// Backs up data from an **NFS server** source to a **local** target.
    ///
    /// Data is read from the NFS server via `nfs3_client` READ RPCs and
    /// written to the local filesystem.  M_REPO and C_REPO are always local.
    pub struct NfsSourceFileBackup {
        pub config:     BackupConfig,
        pub nfs_source: NfsLocation,
    }

    impl NfsSourceFileBackup {
        pub fn new(config: BackupConfig, nfs_source: NfsLocation) -> Self {
            Self { config, nfs_source }
        }
    }

    impl FileBackup for NfsSourceFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .nfs_source(self.nfs_source.clone());

            run_backup_task(option)
        }
    }

    /// Backs up data from an **NFS server** source to an **NFS server** target.
    ///
    /// Data is read from the NFS source and written directly to the NFS target
    /// via dual AIO pipelines (no local staging for D_REPO).  M_REPO and C_REPO
    /// are always local.
    pub struct NfsSourceTargetFileBackup {
        pub config:     BackupConfig,
        pub nfs_source: NfsLocation,
        pub nfs_target: NfsLocation,
    }

    impl NfsSourceTargetFileBackup {
        pub fn new(config: BackupConfig, nfs_source: NfsLocation, nfs_target: NfsLocation) -> Self {
            Self { config, nfs_source, nfs_target }
        }
    }

    impl FileBackup for NfsSourceTargetFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .nfs_source(self.nfs_source.clone())
            .nfs_target(self.nfs_target.clone())
            .nfs_target_d_repo_path(cfg.remote_target_prefix.clone().unwrap_or_else(|| extract_repo_relative_path(&cfg.local_target_dir)));

            run_backup_task(option)
        }
    }
}

#[cfg(all(feature = "nfs", feature = "smb"))]
mod mixed_impl {
    use super::*;
    use crate::nfs::NfsLocation;
    use crate::smb::SmbLocation;

    pub struct NfsSourceSmbTargetFileBackup {
        pub config: BackupConfig,
        pub nfs_source: NfsLocation,
        pub smb_target: SmbLocation,
    }

    impl NfsSourceSmbTargetFileBackup {
        pub fn new(config: BackupConfig, nfs_source: NfsLocation, smb_target: SmbLocation) -> Self {
            Self { config, nfs_source, smb_target }
        }
    }

    impl FileBackup for NfsSourceSmbTargetFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .nfs_source(self.nfs_source.clone())
            .smb_target(self.smb_target.clone())
            .smb_target_d_repo_path(cfg.remote_target_prefix.clone().unwrap_or_else(|| extract_repo_relative_path(&cfg.local_target_dir)));

            run_backup_task(option)
        }
    }

    pub struct SmbSourceNfsTargetFileBackup {
        pub config: BackupConfig,
        pub smb_source: SmbLocation,
        pub nfs_target: NfsLocation,
    }

    impl SmbSourceNfsTargetFileBackup {
        pub fn new(config: BackupConfig, smb_source: SmbLocation, nfs_target: NfsLocation) -> Self {
            Self { config, smb_source, nfs_target }
        }
    }

    impl FileBackup for SmbSourceNfsTargetFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .smb_source(self.smb_source.clone())
            .nfs_target(self.nfs_target.clone())
            .nfs_target_d_repo_path(cfg.remote_target_prefix.clone().unwrap_or_else(|| extract_repo_relative_path(&cfg.local_target_dir)));

            run_backup_task(option)
        }
    }
}

#[cfg(feature = "smb")]
mod smb_impl {
    use super::*;
    use crate::smb::SmbLocation;

    /// Backs up data to an SMB share using the AIO pipeline.
    ///
    /// Metadata (M_REPO, C_REPO) is staged locally and uploaded in post-job.
    /// D_REPO data is written directly to the SMB share during the subtask.
    pub struct SmbFileBackup {
        pub config: BackupConfig,
        pub smb_target: SmbLocation,
    }

    impl SmbFileBackup {
        pub fn new(config: BackupConfig, smb_target: SmbLocation) -> Self {
            Self { config, smb_target }
        }
    }

    impl FileBackup for SmbFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .smb_target(self.smb_target.clone())
            .smb_target_d_repo_path(cfg.remote_target_prefix.clone().unwrap_or_else(|| extract_repo_relative_path(&cfg.local_target_dir)));

            run_backup_task(option)
        }
    }

    /// Backs up data from an SMB share source to a local target.
    pub struct SmbSourceFileBackup {
        pub config: BackupConfig,
        pub smb_source: SmbLocation,
    }

    impl SmbSourceFileBackup {
        pub fn new(config: BackupConfig, smb_source: SmbLocation) -> Self {
            Self { config, smb_source }
        }
    }

    impl FileBackup for SmbSourceFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .smb_source(self.smb_source.clone());

            run_backup_task(option)
        }
    }

    /// Backs up data from an SMB share source to an SMB share target.
    pub struct SmbSourceTargetFileBackup {
        pub config: BackupConfig,
        pub smb_source: SmbLocation,
        pub smb_target: SmbLocation,
    }

    impl SmbSourceTargetFileBackup {
        pub fn new(config: BackupConfig, smb_source: SmbLocation, smb_target: SmbLocation) -> Self {
            Self { config, smb_source, smb_target }
        }
    }

    impl FileBackup for SmbSourceTargetFileBackup {
        type Error = BackupTaskError;

        fn run(&self) -> Result<TransferStats, BackupTaskError> {
            let cfg = &self.config;
            let option = BackupOption::new(
                cfg.source_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .enable_hardlink_phase(cfg.enable_hardlink)
            .enable_delete_phase(cfg.enable_delete)
            .enable_mtime_phase(cfg.enable_mtime)
            .aggregate_config(cfg.aggregate_config.clone())
            .smb_source(self.smb_source.clone())
            .smb_target(self.smb_target.clone())
            .smb_target_d_repo_path(cfg.remote_target_prefix.clone().unwrap_or_else(|| extract_repo_relative_path(&cfg.local_target_dir)));

            run_backup_task(option)
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helper
// ---------------------------------------------------------------------------

/// Extract the D_REPO relative path from a local target directory.
///
/// Given a path like `/tmp/work/COPY_COMMON_FULL_xxx/D_REPO`, returns
/// `COPY_COMMON_FULL_xxx/D_REPO`.
fn extract_repo_relative_path(local_target_dir: &PathBuf) -> String {
    // Walk up from the path to find the COPY_.../D_REPO portion.
    // The structure is: <base>/COPY_{format}_{type}_{uuid}/D_REPO
    // We want: COPY_{format}_{type}_{uuid}/D_REPO
    let mut components: Vec<&str> = Vec::new();
    let mut current: &std::path::Path = local_target_dir;

    // Collect at most 2 components (D_REPO and COPY_...)
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name().and_then(|n| n.to_str()) {
            components.push(name);
            if components.len() >= 2 {
                break;
            }
        }
        current = parent;
    }

    // Reverse to get COPY_.../D_REPO order
    components.reverse();
    components.join("/")
}

/// Start a `BackupTask`, poll until complete, return `TransferStats`.
fn run_backup_task(option: BackupOption) -> Result<TransferStats, BackupTaskError> {
    let task = BackupTask::from(option);
    let running = task.start()
        .map_err(|e| BackupTaskError::Engine(e.to_string()))?;

    loop {
        if running.complete() { break; }
        std::thread::sleep(Duration::from_millis(200));
    }

    let snap = running.stats();
    running.wait()
        .map_err(|e| BackupTaskError::Engine(e.to_string()))?;

    if snap.files_failed > 0 {
        return Err(BackupTaskError::PartialFailure { files_failed: snap.files_failed });
    }

    Ok(TransferStats {
        files_transferred: snap.files_copied,
        bytes_transferred: snap.bytes_copied,
        dirs_created:      snap.dirs_created,
        files_failed:      snap.files_failed,
    })
}
