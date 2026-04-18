//! Concrete [`FileScanner`] implementations.
//!
//! | Type | Source | Pipeline |
//! |------|--------|----------|
//! | [`LocalFileScanner`] | Local filesystem path | Blocking threads, `std::fs` |
//! | [`NfsFileScanner`]   | NFSv3 export          | Tokio tasks, `nfs3_client` READDIRPLUS |
//!
//! Both types write metadata and control files to the **local** M_REPO /
//! C_REPO directories supplied in [`ScannerConfig`].

use std::path::PathBuf;
use std::fmt;

use crate::frame::traits::{FileScanner, ScanStats};
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
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            ctrl_dir:      PathBuf::from("/tmp/bifrost/ctrl"),
            meta_dir:      PathBuf::from("/tmp/bifrost/meta"),
            worker_count:  4,
            writer_count:  1,
            prev_meta_dir: None,
            stats_only: false,
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

    pub fn worker_count(mut self, n: usize) -> Self { self.worker_count = n; self }
    pub fn writer_count(mut self, n: usize) -> Self { self.writer_count = n; self }
    pub fn prev_meta_dir(mut self, p: Option<PathBuf>) -> Self { self.prev_meta_dir = p; self }
    pub fn stats_only(mut self, enabled: bool) -> Self { self.stats_only = enabled; self }

    /// Build a [`ScanOption`] from this config.
    pub(crate) fn to_scan_option(&self) -> ScanOption {
        let mut opt = ScanOption::new(self.ctrl_dir.clone(), self.meta_dir.clone())
            .worker_count(self.worker_count)
            .writer_count(self.writer_count)
            .stats_only(self.stats_only);
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
    Start(crate::scanner::ScanError),
}

impl fmt::Display for LocalScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalScanError::Enqueue(s) => write!(f, "enqueue error: {s}"),
            LocalScanError::Start(e)   => write!(f, "scanner start error: {e}"),
        }
    }
}

impl std::error::Error for LocalScanError {}

impl From<crate::scanner::ScanError> for LocalScanError {
    fn from(e: crate::scanner::ScanError) -> Self { LocalScanError::Start(e) }
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
        Self { source: source.into(), config }
    }
}

impl FileScanner for LocalFileScanner {
    type Error = LocalScanError;

    fn scan(&self) -> Result<ScanStats, LocalScanError> {
        use crate::scanner::Scanner;

        let scan_option = self.config.to_scan_option();
        let mut scanner = Scanner::new(scan_option);
        scanner.enqueue_path(self.source.clone())
            .map_err(|e| LocalScanError::Enqueue(e.to_string()))?;

        let running = scanner.start()?;

        loop {
            if running.complete() { break; }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let snap = running.stats();
        running.wait();

        Ok(ScanStats {
            total_files:      snap.tot_files,
            total_dirs:       snap.tot_dirs,
            total_size_bytes: snap.tot_size,
        })
    }
}

// ---------------------------------------------------------------------------
// NfsFileScanner
// ---------------------------------------------------------------------------

#[cfg(feature = "nfs")]
pub use nfs_impl::{NfsFileScanner, NfsScanError};

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
                NfsScanError::Scan(s)    => write!(f, "NFS scan error: {s}"),
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
            let scan_option = self.config.to_scan_option();

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("bifrost-nfs-scan")
                .build()
                .map_err(NfsScanError::Runtime)?;

            let (tot_files, tot_dirs, tot_size) = rt.block_on(
                crate::scanner::run_nfs_scan(&self.source, scan_option)
            ).map_err(NfsScanError::Scan)?;

            Ok(ScanStats {
                total_files:      tot_files,
                total_dirs:       tot_dirs,
                total_size_bytes: tot_size,
            })
        }
    }
}
