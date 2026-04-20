//! # Scanner Data Models
//!
//! This module defines core data structures used during filesystem traversal:
//! - [`DirBatchScanResult`]: Encapsulates a chunk of scan results for efficient streaming.
//! - [`DirScanEntry`]: Represents a directory pending traversal in the work queue.
//! - [`ScanStatistics`]: Tracks real-time metrics during scanning (e.g., file count, errors).
//!
//! These types enable **memory-efficient**, **resumable**, and **parallel** scanning of massive
//! directory trees by decoupling metadata collection from processing and serialization.

use std::{path::PathBuf, sync::atomic::AtomicU64};

use serde::{Deserialize, Serialize};

use crate::scanner::metadata::{DirMeta, FileMeta};

/// Result of a batched directory scan operation.
///
/// The scanner processes large directories in chunks (e.g., every 5,000 entries) to:
/// - Limit peak memory usage.
/// - Enable incremental checkpointing.
/// - Support resumable scans after interruption.
///
/// Each batch includes:
/// - `dir`: Metadata of the parent directory.
/// - `files`: List of file metadata entries discovered in this batch.
/// - `partial`: `true` if the scan was interrupted and this batch is incomplete.
/// - `complete`: `true` if this is the final batch for the directory.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct DirBatchScanResult {
    /// Metadata of the scanned directory.
    pub dir: DirMeta,
    /// File entries found in this batch.
    pub files: Vec<FileMeta>,
    /// Indicates whether this batch is partial (scan was interrupted).
    pub partial: bool,
    /// Indicates whether the directory scan is now fully complete.
    pub complete: bool,
}

/// A directory entry queued for traversal.
///
/// Used internally by the scanner's work queue to track pending directories
/// and their current recursion depth.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct DirScanEntry {
    /// Absolute path of the directory to scan.
    pub path: PathBuf,
    /// Current recursion depth (root = 0).
    pub depth: usize,
}

// Create new `DirScanEntry`
impl DirScanEntry {
    pub fn new(path: PathBuf, depth: usize) -> Self {
        Self { path, depth }
    }
}

/// Real-time statistics collected during filesystem scanning.
///
/// All fields are atomic to support safe concurrent updates from multiple worker threads.
/// Note: This struct is **not serializable** as-is due to `AtomicU64`; use [`ScanStatsSnapshot`]
/// for persistence or reporting.
#[derive(Debug)]
pub struct ScanStatistics {
    /// Total logical size of all successfully scanned files (in bytes).
    tot_size: AtomicU64,
    /// Total number of files successfully scanned.
    tot_files: AtomicU64,
    /// Total number of directories successfully scanned.
    tot_dirs: AtomicU64,
    /// Number of files that failed to be stat'd (e.g., permission denied).
    failed_files: AtomicU64,
    /// Number of directories that failed to be opened or stat'd.
    failed_dirs: AtomicU64,
}

impl Default for ScanStatistics {
    fn default() -> Self {
        Self {
            tot_size: AtomicU64::new(0),
            tot_files: AtomicU64::new(0),
            tot_dirs: AtomicU64::new(0),
            failed_files: AtomicU64::new(0),
            failed_dirs: AtomicU64::new(0),
        }
    }
}

impl ScanStatistics {
    /// Returns a snapshot of current statistics as plain integers.
    ///
    /// This method is safe to call concurrently and provides a consistent view
    /// of all counters at approximately the same point in time.
    pub fn snapshot(&self) -> ScanStatsSnapshot {
        ScanStatsSnapshot {
            tot_size: self.tot_size.load(std::sync::atomic::Ordering::Relaxed),
            tot_files: self.tot_files.load(std::sync::atomic::Ordering::Relaxed),
            tot_dirs: self.tot_dirs.load(std::sync::atomic::Ordering::Relaxed),
            failed_files: self.failed_files.load(std::sync::atomic::Ordering::Relaxed),
            failed_dirs: self.failed_dirs.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Atomically increments the total file size counter.
    pub fn add_file_size(&self, size: u64) {
        self.tot_size
            .fetch_add(size, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically increments the successful file count.
    pub fn inc_files(&self) {
        self.tot_files
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically increments the successful directory count.
    pub fn inc_dirs(&self) {
        self.tot_dirs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically increments the file error counter.
    pub fn inc_failed_files(&self) {
        self.failed_files
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Atomically increments the directory error counter.
    pub fn inc_failed_dirs(&self) {
        self.failed_dirs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// A serializable snapshot of scanner statistics.
///
/// Unlike [`ScanStatistics`], this type uses plain integers and can be safely
/// serialized (e.g., to JSON or binary formats) for logging, monitoring, or checkpointing.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct ScanStatsSnapshot {
    /// Total logical size of all successfully scanned files (in bytes).
    pub tot_size: u64,
    /// Total number of files successfully scanned.
    pub tot_files: u64,
    /// Total number of directories successfully scanned.
    pub tot_dirs: u64,
    /// Number of files that failed to be stat'd.
    pub failed_files: u64,
    /// Number of directories that failed to be opened or stat'd.
    pub failed_dirs: u64,
}
