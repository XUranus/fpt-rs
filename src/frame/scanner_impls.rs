//! Concrete `FileScanner` implementations.
//!
//! | Type | Source | Pipeline |
//! |------|--------|----------|
//! | `LocalFileScanner` | Local filesystem path | Blocking threads, `std::fs` |
//! | `NfsFileScanner`   | NFSv3 export          | Tokio tasks, `nfs3_client` READDIRPLUS |
//! | `SmbFileScanner`   | SMB share             | Tokio tasks, `smb-rs` async client |
//!
//! Both types write metadata and control files to the **local** M_REPO /
//! C_REPO directories supplied in `ScannerConfig`.

use std::fmt;
use std::path::PathBuf;

use crate::failure::{FailureLogConfig, RetryPolicy};
use crate::frame::traits::{FileScanner, ScanStats};
use crate::scanner::filter::ScanPathFilterSet;
use crate::scanner::options::ScanOption;

// ---------------------------------------------------------------------------
// ScannerConfig — shared configuration
// ---------------------------------------------------------------------------

/// Configuration shared by both scanner implementations.
#[derive(Debug, Clone)]
pub struct ScannerConfig {
    /// Output directory for control files (C_REPO/ctrl).
    pub ctrl_dir: PathBuf,
    /// Output directory for metadata files (M_REPO/meta).
    pub meta_dir: PathBuf,
    /// Number of traversal worker threads (local) / concurrent RPC tasks (NFS).
    pub worker_count: usize,
    /// Number of metadata writer threads.
    pub writer_count: usize,
    /// Previous metadata directory for incremental scanning.
    pub prev_meta_dir: Option<PathBuf>,
    /// When true, only collect scan statistics and skip on-disk outputs.
    pub stats_only: bool,
    /// Whether the backup job stores small files in aggregate blobs.
    pub enable_aggregation: bool,
    /// Maximum aggregate blob size in bytes.
    pub max_aggregate_blob_size: u64,
    /// Files smaller than this threshold are aggregate candidates.
    pub aggregate_file_threshold: u64,
    /// Optional failure log file for the scan.
    pub failure_log: Option<FailureLogConfig>,
    /// Retry policy for scan operations.
    pub retry_policy: RetryPolicy,
    /// Optional compiled path filters.
    pub path_filters: Option<ScanPathFilterSet>,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            ctrl_dir: PathBuf::from("/tmp/fpt/ctrl"),
            meta_dir: PathBuf::from("/tmp/fpt/meta"),
            worker_count: 4,
            writer_count: 1,
            prev_meta_dir: None,
            stats_only: false,
            enable_aggregation: false,
            max_aggregate_blob_size: crate::scanner::options::DEFAULT_MAX_AGGREGATE_BLOB_SIZE,
            aggregate_file_threshold: crate::scanner::options::DEFAULT_AGGREGATE_FILE_THRESHOLD,
            failure_log: None,
            retry_policy: RetryPolicy::default(),
            path_filters: None,
        }
    }
}

impl ScannerConfig {
    pub fn new(ctrl_dir: impl Into<PathBuf>, meta_dir: impl Into<PathBuf>) -> Self {
        Self {
            ctrl_dir: ctrl_dir.into(),
            meta_dir: meta_dir.into(),
            ..Default::default()
        }
    }

    pub fn worker_count(mut self, n: usize) -> Self {
        self.worker_count = n;
        self
    }
    pub fn writer_count(mut self, n: usize) -> Self {
        self.writer_count = n;
        self
    }
    pub fn prev_meta_dir(mut self, p: Option<PathBuf>) -> Self {
        self.prev_meta_dir = p;
        self
    }
    pub fn stats_only(mut self, enabled: bool) -> Self {
        self.stats_only = enabled;
        self
    }
    pub fn enable_aggregation(mut self, enabled: bool) -> Self {
        self.enable_aggregation = enabled;
        self
    }
    pub fn max_aggregate_blob_size(mut self, size: u64) -> Self {
        self.max_aggregate_blob_size = size;
        self
    }
    pub fn aggregate_file_threshold(mut self, threshold: u64) -> Self {
        self.aggregate_file_threshold = threshold;
        self
    }
    pub fn failure_log(mut self, config: Option<FailureLogConfig>) -> Self {
        self.failure_log = config;
        self
    }
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }
    pub fn path_filters(mut self, filters: Option<ScanPathFilterSet>) -> Self {
        self.path_filters = filters;
        self
    }

    /// Build a [`ScanOption`] from this config.
    pub(crate) fn to_scan_option(
        &self,
        control_base: PathBuf,
        source_root: impl Into<String>,
        source_kind: impl Into<String>,
    ) -> ScanOption {
        let mut opt = ScanOption::new(self.ctrl_dir.clone(), self.meta_dir.clone())
            .worker_count(self.worker_count)
            .writer_count(self.writer_count)
            .control_path(control_base, source_root, source_kind)
            .stats_only(self.stats_only)
            .enable_aggregation(self.enable_aggregation)
            .max_aggregate_blob_size(self.max_aggregate_blob_size)
            .aggregate_file_threshold(self.aggregate_file_threshold)
            .failure_log(self.failure_log.clone())
            .retry_policy(self.retry_policy)
            .path_filters(self.path_filters.clone());
        if let Some(ref prev) = self.prev_meta_dir {
            opt = opt.prev_meta_dir(Some(prev.clone()));
        }
        opt
    }
}

// ---------------------------------------------------------------------------
// LocalScanError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LocalScanError {
    Enqueue(String),
    Start(crate::localfs::ScanError),
}

impl fmt::Display for LocalScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalScanError::Enqueue(s) => write!(f, "enqueue error: {s}"),
            LocalScanError::Start(e) => write!(f, "scanner start error: {e}"),
        }
    }
}

impl std::error::Error for LocalScanError {}

impl From<crate::localfs::ScanError> for LocalScanError {
    fn from(e: crate::localfs::ScanError) -> Self {
        LocalScanError::Start(e)
    }
}

// ---------------------------------------------------------------------------
// LocalFileScanner
// ---------------------------------------------------------------------------

/// Scans a local filesystem path using blocking threads and `std::fs`.
///
/// Writes metadata and control files to the local M_REPO / C_REPO dirs
/// specified in [`ScannerConfig`].
pub struct LocalFileScanner {
    /// Root path to scan.
    pub source: PathBuf,
    /// Scanner configuration (output dirs, thread counts, incremental base).
    pub config: ScannerConfig,
}

impl LocalFileScanner {
    pub fn new(source: impl Into<PathBuf>, config: ScannerConfig) -> Self {
        Self {
            source: source.into(),
            config,
        }
    }
}

impl FileScanner for LocalFileScanner {
    type Error = LocalScanError;

    fn scan(&self) -> Result<ScanStats, LocalScanError> {
        let scan_option = self
            .config
            .to_scan_option(self.source.clone(), "/", "local");
        let mut scanner = crate::localfs::Scanner::new(scan_option);
        scanner
            .enqueue_path(self.source.clone())
            .map_err(|e| LocalScanError::Enqueue(e.to_string()))?;

        let running = scanner.start()?;

        loop {
            if running.complete() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let snap = running.stats();
        running.wait();

        Ok(ScanStats {
            total_files: snap.tot_files,
            total_dirs: snap.tot_dirs,
            total_size_bytes: snap.tot_size,
            failed_files: snap.failed_files,
            failed_dirs: snap.failed_dirs,
        })
    }
}

// ---------------------------------------------------------------------------
// NfsFileScanner
// ---------------------------------------------------------------------------

#[cfg(feature = "nfs")]
pub use nfs_impl::{NfsFileScanner, NfsScanError};
#[cfg(feature = "smb")]
pub use smb_impl::{SmbFileScanner, SmbScanError};

#[cfg(feature = "nfs")]
mod nfs_impl {
    use super::*;
    use crate::nfs::NfsLocation;

    /// Error type for NFS scanning.
    #[derive(Debug)]
    pub enum NfsScanError {
        Scan(String),
        Runtime(std::io::Error),
    }

    impl fmt::Display for NfsScanError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                NfsScanError::Scan(s) => write!(f, "NFS scan error: {s}"),
                NfsScanError::Runtime(e) => write!(f, "runtime error: {e}"),
            }
        }
    }

    impl std::error::Error for NfsScanError {}

    /// Scans an NFSv3 export using async Tokio tasks and `nfs3_client` READDIRPLUS RPCs.
    ///
    /// Bridges the async scan output into the synchronous metadata writer
    /// pipeline so M_REPO / C_REPO are always written via BIO to local dirs.
    pub struct NfsFileScanner {
        /// Source NFS location to scan.
        pub source: NfsLocation,
        /// Scanner configuration (output dirs, thread counts, incremental base).
        pub config: ScannerConfig,
    }

    impl NfsFileScanner {
        pub fn new(source: NfsLocation, config: ScannerConfig) -> Self {
            Self { source, config }
        }
    }

    impl FileScanner for NfsFileScanner {
        type Error = NfsScanError;

        fn scan(&self) -> Result<ScanStats, NfsScanError> {
            let base_path = if self.source.sub_path.is_empty() {
                PathBuf::from(&self.source.export)
            } else {
                PathBuf::from(&self.source.export)
                    .join(self.source.sub_path.trim_start_matches('/'))
            };
            let control_base = base_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| base_path.clone());
            let logical_root = base_path
                .file_name()
                .map(|n| format!("/{}", n.to_string_lossy()))
                .unwrap_or_else(|| "/".to_string());
            let scan_option = self
                .config
                .to_scan_option(control_base, logical_root, "nfs");

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("fpt-nfs-scan")
                .build()
                .map_err(NfsScanError::Runtime)?;

            let (tot_files, tot_dirs, tot_size, failed_files, failed_dirs) = rt
                .block_on(crate::nfs::scanner::run_nfs_scan(&self.source, scan_option))
                .map_err(NfsScanError::Scan)?;

            Ok(ScanStats {
                total_files: tot_files,
                total_dirs: tot_dirs,
                total_size_bytes: tot_size,
                failed_files,
                failed_dirs,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// SmbFileScanner
// ---------------------------------------------------------------------------

#[cfg(feature = "smb")]
mod smb_impl {
    use super::*;
    use crate::smb::SmbLocation;

    #[derive(Debug)]
    pub enum SmbScanError {
        Scan(String),
        Runtime(std::io::Error),
    }

    impl fmt::Display for SmbScanError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                SmbScanError::Scan(s) => write!(f, "SMB scan error: {s}"),
                SmbScanError::Runtime(e) => write!(f, "runtime error: {e}"),
            }
        }
    }

    impl std::error::Error for SmbScanError {}

    /// Validates access to an SMB source and reserves the scanner slot for the
    /// upcoming SMB traversal implementation.
    pub struct SmbFileScanner {
        pub source: SmbLocation,
        pub config: ScannerConfig,
    }

    impl SmbFileScanner {
        pub fn new(source: SmbLocation, config: ScannerConfig) -> Self {
            Self { source, config }
        }
    }

    impl FileScanner for SmbFileScanner {
        type Error = SmbScanError;

        fn scan(&self) -> Result<ScanStats, SmbScanError> {
            let base_path = self.source.synthetic_root();
            let control_base = base_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| base_path.clone());
            let logical_root = base_path
                .file_name()
                .map(|n| format!("/{}", n.to_string_lossy()))
                .unwrap_or_else(|| "/".to_string());
            let scan_option = self
                .config
                .to_scan_option(control_base, logical_root, "smb");
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(self.config.worker_count.max(1))
                .thread_name("fpt-smb-scan")
                .build()
                .map_err(SmbScanError::Runtime)?;

            let source = self.source.clone();
            let (tot_files, tot_dirs, tot_size, failed_files, failed_dirs) = rt
                .block_on(crate::smb::scanner::run_smb_scan(&source, scan_option))
                .map_err(SmbScanError::Scan)?;

            Ok(ScanStats {
                total_files: tot_files,
                total_dirs: tot_dirs,
                total_size_bytes: tot_size,
                failed_files,
                failed_dirs,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::filter::ScanPathFilterSet;

    #[test]
    fn scanner_config_preserves_path_filters() {
        let filters = ScanPathFilterSet::compile(
            vec!["/dir/*/keep".to_string()],
            vec!["/dir/*.txt".to_string()],
            vec!["/dir/tmp".to_string()],
            vec!["/dir/skip.txt".to_string()],
        )
        .unwrap();

        let option = ScannerConfig::new("/tmp/ctrl", "/tmp/meta")
            .path_filters(filters)
            .to_scan_option("/source".into(), "/", "local");

        let filters = option
            .meta_option
            .path_filters
            .as_ref()
            .expect("path filters should propagate to ScanOption");
        assert!(filters.should_descend_dir("/dir/a/keep"));
        assert!(filters.should_emit_file("/dir/a.txt"));
        assert!(!filters.should_descend_dir("/dir/tmp"));
        assert!(!filters.should_emit_file("/dir/skip.txt"));
    }
}
