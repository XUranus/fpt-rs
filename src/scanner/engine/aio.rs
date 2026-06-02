//! Async (AIO) scan orchestration for remote transports (NFS, SMB).
//!
//! This module provides the shared scaffolding for running an async directory
//! scanner against a remote transport. The protocol-specific scanner
//! ([`crate::nfs::NfsScanner`], [`crate::smb::SmbScanner`]) is passed in as
//! a generic `AsyncDirScanner` implementation.
//!
//! This is the async counterpart to [`super::bio`] which handles
//! local filesystem scanning via blocking OS threads.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::failure::FailureRecorder;
use crate::scanner::engine::{self, start_meta_writers, start_stats_consumers};
use crate::scanner::models::{DirBatchScanResult, ScanStatistics};
use crate::scanner::options::ScanOption;
use crate::scanner::{BlockingQueue, ScanWorkerContext};
use crate::utility::SpillQueue;

/// A trait abstracting over protocol-specific async directory scanners.
#[allow(dead_code)]
///
/// Both [`crate::nfs::NfsScanner`] and [`crate::smb::SmbScanner`] implement
/// this trait (the NFS impl is defined inline below due to signature differences).
pub trait AsyncDirScanner: Send + 'static {
    /// The error type returned by the scan.
    type Error: std::fmt::Display + Send + 'static;

    /// Run the scan, pushing [`DirBatchScanResult`] items into `tx`.
    fn scan(
        self,
        scan_option: Arc<ScanOption>,
        tx: tokio::sync::mpsc::Sender<DirBatchScanResult>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send>>;
}

/// Result of a completed async scan.
#[allow(dead_code)]
pub struct AioScanResult {
    pub total_files: u64,
    pub total_dirs: u64,
    pub total_size: u64,
    pub failed_files: u64,
    pub failed_dirs: u64,
}

/// Run a full async scan: spawn scanner, bridge to BlockingQueue, write metadata.
#[allow(dead_code)]
///
/// This function handles the shared scaffolding that was previously duplicated
/// between `run_nfs_scan` and `run_smb_scan`:
/// 1. Create output queue and statistics
/// 2. Start metadata writer threads
/// 3. Spawn the async scanner task
/// 4. Bridge results from tokio mpsc → BlockingQueue
/// 5. Wait for scanner completion
/// 6. Close queue, join writers, generate control files
pub async fn run_aio_scan<S>(
    scanner: S,
    scan_option: ScanOption,
) -> Result<AioScanResult, String>
where
    S: AsyncDirScanner,
{
    let output_queue = Arc::new(BlockingQueue::<DirBatchScanResult>::new(
        crate::scanner::options::DEFAULT_SCAN_QUEUE_CAPACITY,
    ));
    let stats = Arc::new(ScanStatistics::default());
    let failure_recorder = scan_option
        .failure_log
        .as_ref()
        .and_then(|cfg| FailureRecorder::create(cfg).ok());

    let writer_count = scan_option.writer_count;
    let scan_opt_arc = Arc::new(scan_option);

    // Build a minimal ScanWorkerContext so we can reuse start_meta_writers.
    let context = ScanWorkerContext {
        scan_option: Arc::clone(&scan_opt_arc),
        dirent_queue: Arc::new(
            SpillQueue::new(
                scan_opt_arc.queue_option.temp_dir.clone(),
                scan_opt_arc.queue_option.memory_upper_bound,
                scan_opt_arc.queue_option.memory_lower_bound,
                scan_opt_arc.queue_option.spill_load_batch_size,
            )
            .map_err(|e| format!("queue init failed: {e}"))?,
        ),
        output_queue: Arc::clone(&output_queue),
        stats: Arc::clone(&stats),
        failure_recorder: failure_recorder.clone(),
    };

    // Start metadata writers (they drain output_queue synchronously).
    let writer_handles = if scan_opt_arc.stats_only {
        start_stats_consumers(&context, writer_count.max(1))
    } else {
        start_meta_writers(&context, writer_count, None)
    };

    // Create a tokio mpsc channel and spawn the scanner.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DirBatchScanResult>(256);
    let scan_opt_for_task = Arc::clone(&scan_opt_arc);
    let scan_handle = tokio::spawn(async move { scanner.scan(scan_opt_for_task, tx).await });

    // Bridge: forward DirBatchScanResult items from tokio mpsc → BlockingQueue.
    let oq = Arc::clone(&output_queue);
    let bridge_stats = Arc::clone(&stats);
    while let Some(batch) = rx.recv().await {
        let file_count = batch.files.len();
        let batch_size: u64 = batch.files.iter().map(|f| f.size).sum();
        oq.push(batch);
        for _ in 0..file_count {
            bridge_stats.inc_files();
        }
        bridge_stats.add_file_size(batch_size);
        bridge_stats.inc_dirs();
    }

    // Wait for the scan task to complete.
    match scan_handle.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("scan failed: {e}")),
        Err(e) => return Err(format!("scan task panicked: {e:?}")),
    }

    // Signal the writers that no more items are coming.
    output_queue.close();

    // Wait for metadata writers to finish.
    for h in writer_handles {
        let _ = h.join();
    }

    if !scan_opt_arc.stats_only {
        engine::generate_control_files(&scan_opt_arc)
            .map_err(|e| format!("generate_control_files failed: {e}"))?;
        crate::scanner::normalize_control_artifacts(&scan_opt_arc)
            .map_err(|e| format!("normalize control artifacts failed: {e}"))?;
    }

    let snap = stats.snapshot();
    Ok(AioScanResult {
        total_files: snap.tot_files,
        total_dirs: snap.tot_dirs,
        total_size: snap.tot_size,
        failed_files: snap.failed_files,
        failed_dirs: snap.failed_dirs,
    })
}

// ---------------------------------------------------------------------------
// AsyncDirScanner implementations for protocol-specific scanners
// ---------------------------------------------------------------------------

/// Wrapper for [`crate::nfs::NfsScanner`] that adapts its extra parameters
/// (`root_fh`, `root_path`) into the [`AsyncDirScanner`] trait.
#[cfg(feature = "nfs")]
pub(crate) struct NfsScanAdapter {
    pub scanner: crate::nfs::NfsScanner,
    pub root_fh: nfs3_client::nfs3_types::nfs3::nfs_fh3,
    pub root_path: String,
}

#[cfg(feature = "nfs")]
impl AsyncDirScanner for NfsScanAdapter {
    type Error = crate::nfs::NfsError;

    fn scan(
        self,
        scan_option: Arc<ScanOption>,
        tx: tokio::sync::mpsc::Sender<DirBatchScanResult>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send>> {
        Box::pin(async move {
            self.scanner
                .scan(self.root_fh, self.root_path, &scan_option, tx)
                .await
        })
    }
}

/// Wrapper for [`crate::smb::SmbScanner`] implementing [`AsyncDirScanner`].
#[cfg(feature = "smb")]
pub(crate) struct SmbScanAdapter {
    pub scanner: crate::smb::scanner::SmbScanner,
}

#[cfg(feature = "smb")]
impl AsyncDirScanner for SmbScanAdapter {
    type Error = String;

    fn scan(
        self,
        scan_option: Arc<ScanOption>,
        tx: tokio::sync::mpsc::Sender<DirBatchScanResult>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send>> {
        Box::pin(async move { self.scanner.scan(&scan_option, tx).await })
    }
}
