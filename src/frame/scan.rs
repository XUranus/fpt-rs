//! Scanning phase of the job framework.
//!
//! [`ScanJob`] is a thin orchestration wrapper that selects the right
//! [`FileScanner`] implementation at runtime based on the [`DataLocation`]
//! of the source, then delegates to it.
//!
//! ## Output
//!
//! Both the local and NFS scanners write metadata files (`meta_*.dat`) and
//! control files (`*_*.control.bin`) to the **local** M_REPO and
//! C_REPO directories inside the [`RepoLayout`].  The rest of the pipeline
//! (subtasks, post-job) can therefore always use standard BIO I/O.
//!
//! ## Statistics
//!
//! [`ScanStats`] (re-exported from [`crate::frame::traits`]) is returned by
//! [`ScanJob::run`].

use std::path::PathBuf;

use crate::failure::{FailureLogConfig, RetryPolicy};
use crate::frame::location::DataLocation;
use crate::frame::repo::RepoLayout;
use crate::frame::scanner_impls::{LocalFileScanner, ScannerConfig};
use crate::frame::traits::FileScanner;
pub use crate::frame::traits::ScanStats;
use crate::scanner::filter::ScanPathFilterSet;

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
    /// SMB scan failure (feature-gated).
    #[cfg(feature = "smb")]
    SmbScan(crate::frame::scanner_impls::SmbScanError),
    /// Transport exists in the location layer but is not wired yet.
    Unsupported(String),
    /// Generic I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::LocalScan(e) => write!(f, "local scan: {e}"),
            #[cfg(feature = "nfs")]
            ScanError::NfsScan(e) => write!(f, "NFS scan: {e}"),
            #[cfg(feature = "smb")]
            ScanError::SmbScan(e) => write!(f, "SMB scan: {e}"),
            ScanError::Unsupported(s) => write!(f, "unsupported: {s}"),
            ScanError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ScanError {}

impl From<crate::frame::scanner_impls::LocalScanError> for ScanError {
    fn from(e: crate::frame::scanner_impls::LocalScanError) -> Self {
        ScanError::LocalScan(e)
    }
}

#[cfg(feature = "nfs")]
impl From<crate::frame::scanner_impls::NfsScanError> for ScanError {
    fn from(e: crate::frame::scanner_impls::NfsScanError) -> Self {
        ScanError::NfsScan(e)
    }
}

#[cfg(feature = "smb")]
impl From<crate::frame::scanner_impls::SmbScanError> for ScanError {
    fn from(e: crate::frame::scanner_impls::SmbScanError) -> Self {
        ScanError::SmbScan(e)
    }
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
    /// Whether the current backup job uses aggregate storage.
    pub enable_aggregation: bool,
    /// Maximum aggregate blob size in bytes.
    pub max_aggregate_blob_size: u64,
    /// Files smaller than this threshold are aggregate candidates.
    pub aggregate_file_threshold: u64,
    /// Optional scan failure log file.
    pub failure_log: Option<FailureLogConfig>,
    /// Retry policy for scan operations.
    pub retry_policy: RetryPolicy,
    /// Optional include/exclude filters used by the scanner.
    pub path_filters: Option<ScanPathFilterSet>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            worker_count: 4,
            writer_count: 1,
            prev_meta_dir: None,
            enable_aggregation: false,
            max_aggregate_blob_size: crate::scanner::options::DEFAULT_MAX_AGGREGATE_BLOB_SIZE,
            aggregate_file_threshold: crate::scanner::options::DEFAULT_AGGREGATE_FILE_THRESHOLD,
            failure_log: None,
            retry_policy: RetryPolicy::default(),
            path_filters: None,
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
    pub repo: &'a RepoLayout,
    pub config: ScanConfig,
}

impl<'a> ScanJob<'a> {
    pub fn new(source: &'a DataLocation, repo: &'a RepoLayout, config: ScanConfig) -> Self {
        Self {
            source,
            repo,
            config,
        }
    }

    /// Build a [`ScannerConfig`] pointing at the local repo's ctrl/meta dirs.
    fn scanner_config(&self) -> ScannerConfig {
        ScannerConfig::new(self.repo.ctrl_dir.clone(), self.repo.meta_dir.clone())
            .worker_count(self.config.worker_count)
            .writer_count(self.config.writer_count)
            .prev_meta_dir(self.config.prev_meta_dir.clone())
            .enable_aggregation(self.config.enable_aggregation)
            .max_aggregate_blob_size(self.config.max_aggregate_blob_size)
            .aggregate_file_threshold(self.config.aggregate_file_threshold)
            .failure_log(self.config.failure_log.clone())
            .retry_policy(self.config.retry_policy)
            .path_filters(self.config.path_filters.clone())
    }

    /// Run the scan via a [`LocalFileScanner`] (blocking).
    pub fn run_local(&self) -> Result<ScanStats, ScanError> {
        let source_path = self
            .source
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

        let nfs_loc = self
            .source
            .nfs_location()
            .expect("run_nfs called on non-NFS source")
            .clone();

        let scanner = NfsFileScanner::new(nfs_loc, self.scanner_config());
        Ok(scanner.scan()?)
    }

    /// Run the scan via an [`SmbFileScanner`].
    #[cfg(feature = "smb")]
    pub fn run_smb(&self) -> Result<ScanStats, ScanError> {
        use crate::frame::scanner_impls::SmbFileScanner;

        let smb_loc = self
            .source
            .smb_location()
            .expect("run_smb called on non-SMB source")
            .clone();

        let scanner = SmbFileScanner::new(smb_loc, self.scanner_config());
        Ok(scanner.scan()?)
    }

    /// Run the scan, automatically choosing the implementation based on [`DataLocation`].
    pub fn run(&self) -> Result<ScanStats, ScanError> {
        match self.source {
            DataLocation::Local(_) => self.run_local(),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(_) => self.run_nfs(),
            #[cfg(feature = "smb")]
            DataLocation::Smb(_) => self.run_smb(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::repo::RepoLayout;
    use crate::scanner::filter::ScanPathFilterSet;

    #[test]
    fn scan_job_forwards_filter_config_to_scanner_config() {
        let source = DataLocation::local("/dataset/source");
        let repo = RepoLayout::from_existing("/tmp/copy".into());
        let filters =
            ScanPathFilterSet::compile(vec![], vec!["/source/*.txt".to_string()], vec![], vec![])
                .unwrap();
        let job = ScanJob::new(
            &source,
            &repo,
            ScanConfig {
                path_filters: filters,
                ..ScanConfig::default()
            },
        );

        let scanner_config = job.scanner_config();
        let option = scanner_config.to_scan_option("/dataset/source".into(), "/", "local");
        let filters = option
            .meta_option
            .path_filters
            .as_ref()
            .expect("ScanJob should forward path filters");

        assert!(filters.should_emit_file("/source/a.txt"));
        assert!(!filters.should_emit_file("/source/a.log"));
    }
}
