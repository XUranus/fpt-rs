//! Subtask execution phase of the job framework.
//!
//! Each subtask corresponds to one control file (`copy.txt`, `copy_N.txt`, …).
//! [`run_backup_subtask`] and [`run_restore_subtask`] select the right
//! [`FileBackup`] / [`FileRestore`] implementation at runtime, delegate to it,
//! and return unified [`SubtaskStats`].
//!
//! ## Pipeline selection
//!
//! | Target | Implementation |
//! |--------|----------------|
//! | Local FS | [`LocalFileBackup`] / [`LocalFileRestore`] (BIO, blocking threads) |
//! | NFS      | [`NfsFileBackup`] / [`NfsFileRestore`] (AIO, Tokio + `nfs3_client`) |
//!
//! In both cases M_REPO and C_REPO are always read/written locally via BIO.

use std::path::PathBuf;

use crate::backup::aggregate::AggregateConfig;
use crate::frame::backup_impls::{BackupConfig, LocalFileBackup};
use crate::frame::location::DataLocation;
use crate::frame::repo::RepoLayout;
use crate::frame::restore_impls::{LocalFileRestore, RestoreConfig};
use crate::frame::traits::{FileBackup, FileRestore, TransferStats};
// ---------------------------------------------------------------------------
// SubtaskConfig
// ---------------------------------------------------------------------------

/// Everything needed to execute one backup or restore subtask.
#[derive(Debug, Clone)]
pub struct SubtaskConfig {
    /// UUID that identifies this subtask.
    pub subtask_uuid: String,
    /// Path to the control file this subtask should process.
    pub control_file: PathBuf,
    /// Local source path (backup only; used when source is local).
    pub source_dir: PathBuf,
    /// Aggregation settings.
    pub aggregate_config: AggregateConfig,
    /// Whether to run the hardlink phase.
    pub enable_hardlink: bool,
    /// Whether to run the delete phase.
    pub enable_delete: bool,
    /// Whether to run the mtime phase.
    pub enable_mtime: bool,
    /// Data source for backup (local or NFS).
    pub backup_source: DataLocation,
    /// Data target for backup (local or NFS).
    pub backup_target: DataLocation,
    /// Data target for restore (local or NFS).
    pub restore_target: DataLocation,
    /// Original source base path recorded in the backup manifest.
    pub restore_source_base: PathBuf,
}

// ---------------------------------------------------------------------------
// SubtaskError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum SubtaskError {
    /// The backup/restore engine reported a hard failure.
    Engine(String),
    /// Files failed to copy (non-zero `files_failed` counter).
    PartialFailure { files_failed: u64 },
}

impl std::fmt::Display for SubtaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubtaskError::Engine(s) =>
                write!(f, "engine error: {s}"),
            SubtaskError::PartialFailure { files_failed } =>
                write!(f, "{files_failed} file(s) failed to copy"),
        }
    }
}

impl std::error::Error for SubtaskError {}

// ---------------------------------------------------------------------------
// SubtaskStats
// ---------------------------------------------------------------------------

/// Statistics returned from a completed subtask.
pub type SubtaskStats = TransferStats;

// ---------------------------------------------------------------------------
// run_backup_subtask
// ---------------------------------------------------------------------------

/// Execute a single backup subtask using the appropriate [`FileBackup`] impl.
///
/// Dispatch table:
/// | Source  | Target  | Implementation                  |
/// |---------|---------|---------------------------------|
/// | Local   | Local   | [`LocalFileBackup`] (BIO) |
/// | Local   | NFS     | [`NfsFileBackup`] (AIO, local read → NFS write) |
/// | Local   | SMB     | [`SmbFileBackup`] (AIO, local read → SMB write) |
/// | NFS     | Local   | [`NfsSourceFileBackup`] (AIO, NFS read → local write) |
/// | NFS     | NFS     | [`NfsSourceTargetFileBackup`] (AIO, direct NFS→NFS copy) |
pub fn run_backup_subtask(
    config: &SubtaskConfig,
    repo:   &RepoLayout,
) -> Result<SubtaskStats, SubtaskError> {
    let backup_cfg = BackupConfig::new(
        config.source_dir.clone(),
        repo.d_repo.clone(),
        repo.meta_dir.clone(),
        repo.ctrl_dir.clone(),
        config.control_file.clone(),
    )
    .aggregate_config(config.aggregate_config.clone())
    .enable_hardlink(config.enable_hardlink)
    .enable_delete(config.enable_delete)
    .enable_mtime(config.enable_mtime);

    match (&config.backup_source, &config.backup_target) {
        (DataLocation::Local(_), DataLocation::Local(_)) => {
            LocalFileBackup::new(backup_cfg)
                .run()
                .map_err(map_backup_err)
        }
        #[cfg(feature = "nfs")]
        (DataLocation::Local(_), DataLocation::Nfs(nfs_target)) => {
            use crate::frame::backup_impls::NfsFileBackup;
            NfsFileBackup::new(backup_cfg, nfs_target.clone())
                .run()
                .map_err(map_backup_err)
        }
        #[cfg(feature = "nfs")]
        (DataLocation::Nfs(nfs_source), DataLocation::Local(_)) => {
            use crate::frame::backup_impls::NfsSourceFileBackup;
            NfsSourceFileBackup::new(backup_cfg, nfs_source.clone())
                .run()
                .map_err(map_backup_err)
        }
        #[cfg(feature = "nfs")]
        (DataLocation::Nfs(nfs_source), DataLocation::Nfs(nfs_target)) => {
            // NFS→NFS: direct copy via dual-pool AIO pipeline.
            use crate::frame::backup_impls::NfsSourceTargetFileBackup;
            NfsSourceTargetFileBackup::new(backup_cfg, nfs_source.clone(), nfs_target.clone())
                .run()
                .map_err(map_backup_err)
        }
        #[cfg(feature = "smb")]
        (DataLocation::Local(_), DataLocation::Smb(smb_target)) => {
            use crate::frame::backup_impls::SmbFileBackup;
            SmbFileBackup::new(backup_cfg, smb_target.clone())
                .run()
                .map_err(map_backup_err)
        }
        #[cfg(feature = "smb")]
        (DataLocation::Smb(smb_source), DataLocation::Local(_)) => {
            use crate::frame::backup_impls::SmbSourceFileBackup;
            SmbSourceFileBackup::new(backup_cfg, smb_source.clone())
                .run()
                .map_err(map_backup_err)
        }
        #[cfg(feature = "smb")]
        (DataLocation::Smb(smb_source), DataLocation::Smb(smb_target)) => {
            use crate::frame::backup_impls::SmbSourceTargetFileBackup;
            SmbSourceTargetFileBackup::new(backup_cfg, smb_source.clone(), smb_target.clone())
                .run()
                .map_err(map_backup_err)
        }
        #[cfg(feature = "smb")]
        _ if config.backup_source.is_smb() || config.backup_target.is_smb() => {
            Err(SubtaskError::Engine(
                "this SMB backup direction is not implemented yet".to_string()
            ))
        }
        _ => Err(SubtaskError::Engine(
            "this backup direction is not compiled in".to_string()
        )),
    }
}

// ---------------------------------------------------------------------------
// run_restore_subtask
// ---------------------------------------------------------------------------

/// Execute a single restore subtask using the appropriate [`FileRestore`] impl.
pub fn run_restore_subtask(
    config:              &SubtaskConfig,
    repo:                &RepoLayout,
    local_restore_target: &PathBuf,
) -> Result<SubtaskStats, SubtaskError> {
    let restore_cfg = RestoreConfig::new(
        repo.d_repo.clone(),
        config.restore_source_base.clone(),
        local_restore_target.clone(),
        repo.meta_dir.clone(),
        repo.ctrl_dir.clone(),
        config.control_file.clone(),
    );

    match &config.restore_target {
        DataLocation::Local(_) => {
            LocalFileRestore::new(restore_cfg)
                .run()
                .map_err(map_restore_err)
        }
        #[cfg(feature = "nfs")]
        DataLocation::Nfs(nfs_loc) => {
            use crate::frame::restore_impls::NfsFileRestore;
            NfsFileRestore::new(restore_cfg, nfs_loc.clone())
                .run()
                .map_err(map_restore_err)
        }
        #[cfg(feature = "smb")]
        DataLocation::Smb(smb_loc) => {
            use crate::frame::restore_impls::SmbFileRestore;
            SmbFileRestore::new(restore_cfg, smb_loc.clone())
                .run()
                .map_err(map_restore_err)
        }
    }
}

// ---------------------------------------------------------------------------
// Control file discovery helpers
// ---------------------------------------------------------------------------

/// Collect backup control files (copy.txt and shards) from `ctrl_dir`.
pub fn find_backup_control_files(ctrl_dir: &PathBuf) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let copy_file = ctrl_dir.join("copy.txt");
    if copy_file.exists() { files.push(copy_file); }

    if let Ok(entries) = std::fs::read_dir(ctrl_dir) {
        let mut shards: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with("copy_") && s.ends_with(".txt")
            })
            .map(|e| e.path())
            .collect();
        shards.sort();
        files.extend(shards);
    }
    files
}

/// Collect all restore control files (copy, hardlink, delete, mtime) from `ctrl_dir`.
pub fn find_restore_control_files(ctrl_dir: &PathBuf) -> Vec<(PathBuf, &'static str)> {
    let phases = [
        ("copy.txt",     "copy"),
        ("hardlink.txt", "hardlink"),
        ("delete.txt",   "delete"),
        ("mtime.txt",    "mtime"),
    ];
    let mut files = Vec::new();
    for (name, tag) in &phases {
        let p = ctrl_dir.join(name);
        if p.exists() { files.push((p, *tag)); }
    }
    if let Ok(entries) = std::fs::read_dir(ctrl_dir) {
        let mut shards: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with("copy_") && s.ends_with(".txt")
            })
            .map(|e| e.path())
            .collect();
        shards.sort();
        for p in shards { files.push((p, "copy")); }
    }
    files
}

// ---------------------------------------------------------------------------
// Error mapping helpers
// ---------------------------------------------------------------------------

fn map_backup_err(e: crate::frame::backup_impls::BackupTaskError) -> SubtaskError {
    use crate::frame::backup_impls::BackupTaskError;
    match e {
        BackupTaskError::Engine(s)            => SubtaskError::Engine(s),
        BackupTaskError::PartialFailure { files_failed } =>
            SubtaskError::PartialFailure { files_failed },
    }
}

fn map_restore_err(e: crate::frame::restore_impls::RestoreTaskError) -> SubtaskError {
    use crate::frame::restore_impls::RestoreTaskError;
    match e {
        RestoreTaskError::Engine(s)            => SubtaskError::Engine(s),
        RestoreTaskError::PartialFailure { files_failed } =>
            SubtaskError::PartialFailure { files_failed },
    }
}
