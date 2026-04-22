//! Concrete [`FileRestore`] implementations.
//!
//! | Type | Target | Pipeline |
//! |------|--------|----------|
//! | [`LocalFileRestore`] | Local filesystem path | Blocking BIO threads, `std::fs` |
//! | [`NfsFileRestore`]   | NFSv3 export          | Tokio AIO tasks, `nfs3_client` WRITE RPCs |
//!
//! Both types read D_REPO data from the local staging directory and write
//! to the target.  Only the **write destination** differs.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::{RestoreOption, RestorePolicy, RestoreTask};
use crate::frame::traits::{FileRestore, TransferStats};

// ---------------------------------------------------------------------------
// RestoreConfig — shared configuration for one restore subtask
// ---------------------------------------------------------------------------

/// Configuration for a single restore subtask execution.
#[derive(Debug, Clone)]
pub struct RestoreConfig {
    /// Local D_REPO staging directory (source of data for restore).
    pub d_repo_dir: PathBuf,
    /// Original source base path recorded in the backup manifest.
    pub source_base_dir: PathBuf,
    /// Local restore target directory (used by local target; ignored by NFS target).
    pub local_target_dir: PathBuf,
    /// Local M_REPO/meta directory (always BIO).
    pub meta_dir: PathBuf,
    /// Local C_REPO/ctrl directory (always BIO).
    pub ctrl_dir: PathBuf,
    /// Control file that describes what to restore.
    pub control_file: PathBuf,
    /// Conflict resolution policy.
    pub policy: RestorePolicy,
    /// Aggregation layout/settings from the backup manifest.
    pub aggregate_config: AggregateConfig,
}

impl RestoreConfig {
    pub fn new(
        d_repo_dir: impl Into<PathBuf>,
        source_base_dir: impl Into<PathBuf>,
        local_target_dir: impl Into<PathBuf>,
        meta_dir: impl Into<PathBuf>,
        ctrl_dir: impl Into<PathBuf>,
        control_file: impl Into<PathBuf>,
    ) -> Self {
        Self {
            d_repo_dir: d_repo_dir.into(),
            source_base_dir: source_base_dir.into(),
            local_target_dir: local_target_dir.into(),
            meta_dir: meta_dir.into(),
            ctrl_dir: ctrl_dir.into(),
            control_file: control_file.into(),
            policy: RestorePolicy::Replace,
            aggregate_config: AggregateConfig::default(),
        }
    }

    pub fn policy(mut self, p: RestorePolicy) -> Self {
        self.policy = p;
        self
    }

    pub fn aggregate_config(mut self, cfg: AggregateConfig) -> Self {
        self.aggregate_config = cfg;
        self
    }
}

// ---------------------------------------------------------------------------
// RestoreTaskError — shared error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum RestoreTaskError {
    Engine(String),
    PartialFailure { files_failed: u64 },
}

impl fmt::Display for RestoreTaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestoreTaskError::Engine(s) => write!(f, "restore engine error: {s}"),
            RestoreTaskError::PartialFailure { files_failed } => {
                write!(f, "{files_failed} file(s) failed to restore")
            }
        }
    }
}

impl std::error::Error for RestoreTaskError {}

// ---------------------------------------------------------------------------
// LocalFileRestore
// ---------------------------------------------------------------------------

/// Restores data to a **local filesystem** target using the BIO pipeline.
pub struct LocalFileRestore {
    pub config: RestoreConfig,
}

impl LocalFileRestore {
    pub fn new(config: RestoreConfig) -> Self {
        Self { config }
    }
}

impl FileRestore for LocalFileRestore {
    type Error = RestoreTaskError;

    fn run(&self) -> Result<TransferStats, RestoreTaskError> {
        let cfg = &self.config;
        let option = RestoreOption::new(
            cfg.d_repo_dir.clone(),
            cfg.source_base_dir.clone(),
            cfg.local_target_dir.clone(),
            cfg.meta_dir.clone(),
            cfg.ctrl_dir.clone(),
            cfg.control_file.clone(),
        )
        .policy(cfg.policy)
        .aggregate_config(cfg.aggregate_config);

        run_restore_task(option)
    }
}

// ---------------------------------------------------------------------------
// NfsFileRestore
// ---------------------------------------------------------------------------

#[cfg(feature = "nfs")]
pub use nfs_impl::NfsFileRestore;

#[cfg(feature = "nfs")]
mod nfs_impl {
    use super::*;
    use crate::nfs::NfsLocation;

    /// Restores data to an **NFS server** target using the AIO pipeline.
    ///
    /// D_REPO data is read locally and written to the NFS server via
    /// `nfs3_client` WRITE RPCs.
    pub struct NfsFileRestore {
        pub config: RestoreConfig,
        pub nfs_target: NfsLocation,
    }

    impl NfsFileRestore {
        pub fn new(config: RestoreConfig, nfs_target: NfsLocation) -> Self {
            Self { config, nfs_target }
        }
    }

    impl FileRestore for NfsFileRestore {
        type Error = RestoreTaskError;

        fn run(&self) -> Result<TransferStats, RestoreTaskError> {
            let cfg = &self.config;
            let option = RestoreOption::new(
                cfg.d_repo_dir.clone(),
                cfg.source_base_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .policy(cfg.policy)
            .aggregate_config(cfg.aggregate_config)
            .nfs_target(self.nfs_target.clone());

            run_restore_task(option)
        }
    }
}

#[cfg(feature = "smb")]
pub use smb_impl::SmbFileRestore;

#[cfg(feature = "smb")]
mod smb_impl {
    use super::*;
    use crate::smb::SmbLocation;

    pub struct SmbFileRestore {
        pub config: RestoreConfig,
        pub smb_target: SmbLocation,
    }

    impl SmbFileRestore {
        pub fn new(config: RestoreConfig, smb_target: SmbLocation) -> Self {
            Self { config, smb_target }
        }
    }

    impl FileRestore for SmbFileRestore {
        type Error = RestoreTaskError;

        fn run(&self) -> Result<TransferStats, RestoreTaskError> {
            let cfg = &self.config;
            let option = RestoreOption::new(
                cfg.d_repo_dir.clone(),
                cfg.source_base_dir.clone(),
                cfg.local_target_dir.clone(),
                cfg.meta_dir.clone(),
                cfg.ctrl_dir.clone(),
                cfg.control_file.clone(),
            )
            .policy(cfg.policy)
            .aggregate_config(cfg.aggregate_config)
            .smb_target(self.smb_target.clone());

            run_restore_task(option)
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helper
// ---------------------------------------------------------------------------

/// Start a `RestoreTask`, poll until complete, return `TransferStats`.
fn run_restore_task(option: RestoreOption) -> Result<TransferStats, RestoreTaskError> {
    let task = RestoreTask::new(option);
    let running = task
        .start()
        .map_err(|e| RestoreTaskError::Engine(e.to_string()))?;

    loop {
        if running.complete() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let snap = running.stats();
    running
        .wait()
        .map_err(|e| RestoreTaskError::Engine(e.to_string()))?;

    if snap.files_failed > 0 {
        return Err(RestoreTaskError::PartialFailure {
            files_failed: snap.files_failed,
        });
    }

    Ok(TransferStats {
        files_transferred: snap.files_restored,
        bytes_transferred: snap.bytes_restored,
        dirs_created: snap.dirs_created,
        files_failed: snap.files_failed,
        dirs_failed: 0,
    })
}
