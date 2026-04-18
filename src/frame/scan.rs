//! Scanning phase of the job framework.
//!
//! [`ScanJob`] is a thin orchestration wrapper that selects the right
//! [`FileScanner`] implementation at runtime based on the [`DataLocation`]
//! of the source, then delegates to it.
//!
//! ## Output
//!
//! Both the local and NFS scanners write metadata files (`meta_*.dat`) and
//! control files (`copy.txt`, `hardlink.txt`, …) to the **local** M_REPO and
//! C_REPO directories inside the [`RepoLayout`].  The rest of the pipeline
//! (subtasks, post-job) can therefore always use standard BIO I/O.
//!
//! ## Statistics
//!
//! [`ScanStats`] (re-exported from [`crate::frame::traits`]) is returned by
//! [`ScanJob::run`].

use std::path::PathBuf;

use crate::frame::location::DataLocation;
use crate::frame::repo::RepoLayout;
use crate::frame::scanner_impls::{LocalFileScanner, ScannerConfig};
use crate::frame::traits::FileScanner;
pub use crate::frame::traits::ScanStats;

// ---------------------------------------------------------------------------
// ScanError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ScanError {
    /// Local filesystem scanner error.
    LocalScan(crate::frame::scanner_impls::LocalScanError),
    /// NFS scan failure (feature-gated).
    #[cfg(feature = "nfs")]
    NfsScan(crate::frame::scanner_impls::NfsScanError),
    /// Generic I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::LocalScan(e)   => write!(f, "local scan: {e}"),
            #[cfg(feature = "nfs")]
            ScanError::NfsScan(e)     => write!(f, "NFS scan: {e}"),
            ScanError::Io(e)          => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ScanError {}

impl From<crate::frame::scanner_impls::LocalScanError> for ScanError {
    fn from(e: crate::frame::scanner_impls::LocalScanError) -> Self { ScanError::LocalScan(e) }
}

#[cfg(feature = "nfs")]
impl From<crate::frame::scanner_impls::NfsScanError> for ScanError {
    fn from(e: crate::frame::scanner_impls::NfsScanError) -> Self { ScanError::NfsScan(e) }
}

impl From<crate::scanner::ScanError> for ScanError {
    fn from(e: crate::scanner::ScanError) -> Self {
        ScanError::LocalScan(crate::frame::scanner_impls::LocalScanError::Start(e))
    }
}

// ---------------------------------------------------------------------------
// ScanConfig
// ---------------------------------------------------------------------------

/// Configuration forwarded to the scanner implementation.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Number of directory traversal worker threads (local scan).
    pub worker_count: usize,
    /// Number of metadata writer threads.
    pub writer_count: usize,
    /// Previous metadata directory for incremental scanning.
    pub prev_meta_dir: Option<PathBuf>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            worker_count:  4,
            writer_count:  1,
            prev_meta_dir: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ScanJob
// ---------------------------------------------------------------------------

/// Orchestrates the scanning phase by delegating to the appropriate
/// [`FileScanner`] implementation for the given [`DataLocation`].
pub struct ScanJob<'a> {
    pub source: &'a DataLocation,
    pub repo:   &'a RepoLayout,
    pub config: ScanConfig,
}

impl<'a> ScanJob<'a> {
    pub fn new(source: &'a DataLocation, repo: &'a RepoLayout, config: ScanConfig) -> Self {
        Self { source, repo, config }
    }

    /// Build a [`ScannerConfig`] pointing at the local repo's ctrl/meta dirs.
    fn scanner_config(&self) -> ScannerConfig {
        ScannerConfig::new(self.repo.ctrl_dir.clone(), self.repo.meta_dir.clone())
            .worker_count(self.config.worker_count)
            .writer_count(self.config.writer_count)
            .prev_meta_dir(self.config.prev_meta_dir.clone())
    }

    /// Run the scan via a [`LocalFileScanner`] (blocking).
    pub fn run_local(&self) -> Result<ScanStats, ScanError> {
        let source_path = self.source
            .local_path()
            .expect("run_local called on non-local source")
            .clone();

        let scanner = LocalFileScanner::new(source_path, self.scanner_config());
        Ok(scanner.scan()?)
    }

    /// Run the scan via an [`NfsFileScanner`] (blocking; creates Tokio runtime internally).
    #[cfg(feature = "nfs")]
    pub fn run_nfs(&self) -> Result<ScanStats, ScanError> {
        use crate::frame::scanner_impls::NfsFileScanner;

        let nfs_loc = self.source
            .nfs_location()
            .expect("run_nfs called on non-NFS source")
            .clone();

        let scanner = NfsFileScanner::new(nfs_loc, self.scanner_config());
        Ok(scanner.scan()?)
    }

    /// Run the scan, automatically choosing the implementation based on [`DataLocation`].
    pub fn run(&self) -> Result<ScanStats, ScanError> {
        match self.source {
            DataLocation::Local(_) => self.run_local(),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(_)   => self.run_nfs(),
        }
    }
}
