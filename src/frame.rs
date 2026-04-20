//! Backup/restore framework (`frame`).
//!
//! The `frame` module provides a structured, four-phase pipeline for backup
//! and restore operations that cleanly separates *where data lives* from
//! *how it is transferred*:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │ Phase 1 – Prerequisite                                                  │
//! │   Verify connectivity, create local M_REPO / C_REPO / D_REPO dirs.     │
//! │   (Restore: pre-fetch M_REPO + C_REPO from remote copy if needed.)     │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │ Phase 2 – Scan (backup only)                                            │
//! │   Traverse source (local FS or NFS); write meta_*.dat, copy.txt …      │
//! │   to the LOCAL M_REPO / C_REPO via FileScanner.                        │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │ Phase 3 – Subtasks                                                      │
//! │   For each control file, dispatch to FileBackup / FileRestore.          │
//! │   Local target → LocalFileBackup / LocalFileRestore (BIO).             │
//! │   NFS target   → NfsFileBackup   / NfsFileRestore   (AIO).             │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │ Phase 4 – Post-job                                                      │
//! │   Write manifest.json.  For NFS targets, upload M_REPO + C_REPO.       │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Key invariant: **M_REPO, C_REPO, and logs are always on the local
//! filesystem during job execution.**  Only D_REPO data files may be written
//! to NFS directly (via the AIO pipeline in phase 3).
//!
//! # Trait hierarchy
//!
//! ```text
//! FileScanner      ← LocalFileScanner  |  NfsFileScanner
//! FileBackup       ← LocalFileBackup   |  NfsFileBackup
//! FileRestore      ← LocalFileRestore  |  NfsFileRestore
//! BackupRestoreJob ← FileBackupJob     |  FileRestoreJob
//! ```
//!
//! # Public surface
//!
//! **Traits** (in [`traits`]):
//! - [`traits::FileScanner`] / [`traits::FileBackup`] / [`traits::FileRestore`]
//! - [`traits::BackupRestoreJob`]
//! - [`traits::ScanStats`] / [`traits::TransferStats`] / [`traits::JobResult`]
//!
//! **Implementations**:
//! - [`scanner_impls::LocalFileScanner`] / [`scanner_impls::NfsFileScanner`]
//! - [`backup_impls::LocalFileBackup`] / [`backup_impls::NfsFileBackup`]
//! - [`restore_impls::LocalFileRestore`] / [`restore_impls::NfsFileRestore`]
//!
//! **Job orchestrators**:
//! - [`backup_job::FileBackupJob`] (alias: `BackupJob`) + [`backup_job::BackupJobConfig`]
//! - [`restore_job::FileRestoreJob`] (alias: `RestoreJob`) + [`restore_job::RestoreJobConfig`]
//!
//! **Infrastructure**:
//! - [`DataLocation`] — where the user's data lives (local path or NFS URL).
//! - [`RepoLayout`] / [`TempRepoConfig`] — local staging area description.

pub mod backup_impls;
pub mod backup_job;
pub mod location;
pub mod postjob;
pub mod prereq;
pub mod repo;
pub mod restore_impls;
pub mod restore_job;
pub mod scan;
pub mod scanner_impls;
pub mod subtask;
pub mod traits;

// ── Core traits ──────────────────────────────────────────────────────────────
pub use traits::{
    BackupRestoreJob, FileBackup, FileRestore, FileScanner, JobResult, ScanStats, TransferStats,
};

// ── Scanner implementations ───────────────────────────────────────────────────
#[cfg(feature = "nfs")]
pub use scanner_impls::NfsFileScanner;
pub use scanner_impls::{LocalFileScanner, ScannerConfig};

// ── Backup implementations ────────────────────────────────────────────────────
#[cfg(feature = "nfs")]
pub use backup_impls::NfsFileBackup;
pub use backup_impls::{BackupConfig, LocalFileBackup};

// ── Restore implementations ───────────────────────────────────────────────────
#[cfg(feature = "nfs")]
pub use restore_impls::NfsFileRestore;
pub use restore_impls::{LocalFileRestore, RestoreConfig};

// ── Infrastructure ────────────────────────────────────────────────────────────
pub use location::DataLocation;
pub use repo::{RepoLayout, TempRepoConfig};

// ── Job orchestrators (canonical + legacy aliases) ────────────────────────────
pub use backup_job::{
    // legacy aliases
    BackupJob,
    BackupJobConfig,
    BackupJobError,
    BackupJobResult,
    FileBackupJob,
};
pub use restore_job::{
    FileRestoreJob,
    // legacy aliases
    RestoreJob,
    RestoreJobConfig,
    RestoreJobError,
    RestoreJobResult,
};
