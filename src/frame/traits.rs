//! Core traits for the backup/restore framework.
//!
//! These traits define the uniform interface that lets the job orchestrators
//! (`FileBackupJob`, `FileRestoreJob`) work identically regardless of whether
//! the underlying data lives on a local filesystem or an NFS server.
//!
//! ## Trait hierarchy
//!
//! ```text
//! FileScanner          ← LocalFileScanner  |  NfsFileScanner
//! FileBackup           ← LocalFileBackup   |  NfsFileBackup
//! FileRestore          ← LocalFileRestore  |  NfsFileRestore
//! BackupRestoreJob     ← FileBackupJob     |  FileRestoreJob
//! ```
//!
//! ## Design principles
//!
//! * **M_REPO, C_REPO, and logs are always local.** Only the data path
//!   (D_REPO) differs between local and NFS implementations.
//! * Traits carry only the behaviour contract; configuration is passed at
//!   construction time through dedicated `*Config` structs.
//! * All trait methods are synchronous from the caller's perspective.  NFS
//!   implementations spin up their own Tokio runtime internally.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ScanStats — shared result type
// ---------------------------------------------------------------------------

/// Statistics produced by a completed scan operation.
#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub total_files: u64,
    pub total_dirs: u64,
    pub total_size_bytes: u64,
}

impl std::fmt::Display for ScanStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let size = self.total_size_bytes;
        if size == 0 {
            write!(f, "{} files, {} dirs", self.total_files, self.total_dirs)
        } else if size < 1024 {
            write!(
                f,
                "{} files, {} dirs, {} bytes",
                self.total_files, self.total_dirs, size
            )
        } else {
            write!(
                f,
                "{} files, {} dirs, {:.2} MB ({} bytes)",
                self.total_files,
                self.total_dirs,
                size as f64 / (1024.0 * 1024.0),
                size,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// TransferStats — shared result type for backup / restore
// ---------------------------------------------------------------------------

/// Statistics produced by a single backup or restore execution.
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    pub files_transferred: u64,
    pub bytes_transferred: u64,
    pub dirs_created: u64,
    pub files_failed: u64,
}

impl std::fmt::Display for TransferStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} files ({:.2} MB), {} dirs, {} failed",
            self.files_transferred,
            self.bytes_transferred as f64 / (1024.0 * 1024.0),
            self.dirs_created,
            self.files_failed,
        )
    }
}

// ---------------------------------------------------------------------------
// JobResult — returned by BackupRestoreJob::run
// ---------------------------------------------------------------------------

/// Final result of a complete job run (all phases).
#[derive(Debug)]
pub struct JobResult {
    /// UUID assigned to this backup copy.
    pub copy_uuid: String,
    /// Absolute path to the copy root on the local filesystem.
    pub copy_root: PathBuf,
    /// Number of subtasks that finished without errors.
    pub subtasks_ok: usize,
    /// Number of subtasks that encountered errors.
    pub subtasks_failed: usize,
    /// Aggregate files transferred across all subtasks.
    pub total_files: u64,
    /// Aggregate directories observed or created across the job.
    pub total_dirs: u64,
    /// Aggregate bytes transferred across all subtasks.
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// FileScanner trait
// ---------------------------------------------------------------------------

/// Uniform interface for filesystem scanning (local or NFS).
///
/// Implementors traverse a source, write metadata files (`meta_*.dat`) and
/// control files (`copy.txt`, `hardlink.txt`, …) into the **local** M_REPO
/// and C_REPO directories, then return aggregate statistics.
///
/// ## Implementations
/// * [`super::scanner_impls::LocalFileScanner`] — local FS via `std::fs`.
/// * [`super::scanner_impls::NfsFileScanner`] — NFSv3 via `nfs3_client` RPCs.
pub trait FileScanner {
    type Error: std::error::Error + Send + 'static;

    /// Execute the scan and return statistics.
    ///
    /// This call **blocks** until the scan is complete. NFS implementations
    /// manage their own async runtime internally.
    fn scan(&self) -> Result<ScanStats, Self::Error>;
}

// ---------------------------------------------------------------------------
// FileBackup trait
// ---------------------------------------------------------------------------

/// Uniform interface for executing one backup subtask (one control file).
///
/// Reads source data via BIO (local) or NFS READ RPCs (NFS source — future),
/// and writes data to the target via BIO (local target) or NFS WRITE RPCs
/// (NFS target).  Metadata reads/writes always use BIO to the local repo.
///
/// ## Implementations
/// * [`super::backup_impls::LocalFileBackup`] — BIO pipeline.
/// * [`super::backup_impls::NfsFileBackup`] — AIO pipeline (NFS target).
pub trait FileBackup {
    type Error: std::error::Error + Send + 'static;

    /// Execute the backup subtask and return statistics.
    fn run(&self) -> Result<TransferStats, Self::Error>;
}

// ---------------------------------------------------------------------------
// FileRestore trait
// ---------------------------------------------------------------------------

/// Uniform interface for executing one restore subtask (one control file).
///
/// Reads data from the local D_REPO staging dir and writes to the restore
/// target via BIO (local) or NFS WRITE RPCs (NFS target).
///
/// ## Implementations
/// * [`super::restore_impls::LocalFileRestore`] — BIO pipeline.
/// * [`super::restore_impls::NfsFileRestore`] — AIO pipeline (NFS target).
pub trait FileRestore {
    type Error: std::error::Error + Send + 'static;

    /// Execute the restore subtask and return statistics.
    fn run(&self) -> Result<TransferStats, Self::Error>;
}

// ---------------------------------------------------------------------------
// BackupRestoreJob trait
// ---------------------------------------------------------------------------

/// Uniform lifecycle interface for a complete backup or restore job.
///
/// A job drives the four-phase pipeline (prerequisite → scan → subtasks →
/// post-job) in a single blocking call, returning a summary result.
///
/// ## Implementations
/// * [`super::backup_job::FileBackupJob`] — full backup pipeline.
/// * [`super::restore_job::FileRestoreJob`] — full restore pipeline.
pub trait BackupRestoreJob {
    type Error: std::error::Error + Send + 'static;

    /// Run the complete job pipeline and return a summary result.
    ///
    /// This is a blocking call.  Implementations are free to create internal
    /// thread pools or async runtimes as needed.
    fn run(self) -> Result<JobResult, Self::Error>;
}
