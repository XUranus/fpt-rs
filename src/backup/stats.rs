use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Real-time statistics for backup operations.
///
/// All fields are atomic to support concurrent updates from multiple threads.
/// Note: This struct is **not serializable** as-is due to `AtomicU64`; use [`BackupStatsSnapshot`]
/// for persistence or reporting.
#[derive(Debug)]
pub struct BackupStats {
    /// Total bytes copied to target.
    pub bytes_copied: AtomicU64,
    /// Number of source files successfully opened.
    pub src_opened: AtomicU64,
    /// Number of source files successfully closed.
    pub src_closed: AtomicU64,
    /// Number of target files successfully opened.
    pub dst_opened: AtomicU64,
    /// Number of target files successfully closed.
    pub dst_closed: AtomicU64,
    /// Number of files fully copied.
    pub files_copied: AtomicU64,
    /// Number of files deleted during incremental backup.
    pub files_deleted: AtomicU64,
    /// Number of directories created.
    pub dirs_created: AtomicU64,
    /// Number of directories deleted.
    pub dirs_deleted: AtomicU64,
    /// Number of files that failed during processing.
    pub files_failed: AtomicU64,
    /// Number of directories that failed during processing.
    pub dirs_failed: AtomicU64,
}

impl Default for BackupStats {
    fn default() -> Self {
        Self {
            bytes_copied: AtomicU64::new(0),
            src_opened: AtomicU64::new(0),
            src_closed: AtomicU64::new(0),
            dst_opened: AtomicU64::new(0),
            dst_closed: AtomicU64::new(0),
            files_copied: AtomicU64::new(0),
            files_deleted: AtomicU64::new(0),
            dirs_created: AtomicU64::new(0),
            dirs_deleted: AtomicU64::new(0),
            files_failed: AtomicU64::new(0),
            dirs_failed: AtomicU64::new(0),
        }
    }
}

impl BackupStats {
    /// Returns a snapshot of current statistics as plain integers.
    ///
    /// This method is safe to call concurrently and provides a consistent view
    /// of all counters at approximately the same point in time.
    pub fn snapshot(&self) -> BackupStatsSnapshot {
        BackupStatsSnapshot {
            bytes_copied: self.bytes_copied.load(Ordering::Relaxed),
            src_opened: self.src_opened.load(Ordering::Relaxed),
            src_closed: self.src_closed.load(Ordering::Relaxed),
            dst_opened: self.dst_opened.load(Ordering::Relaxed),
            dst_closed: self.dst_closed.load(Ordering::Relaxed),
            files_copied: self.files_copied.load(Ordering::Relaxed),
            files_deleted: self.files_deleted.load(Ordering::Relaxed),
            dirs_created: self.dirs_created.load(Ordering::Relaxed),
            dirs_deleted: self.dirs_deleted.load(Ordering::Relaxed),
            files_failed: self.files_failed.load(Ordering::Relaxed),
            dirs_failed: self.dirs_failed.load(Ordering::Relaxed),
        }
    }

    /// Atomically increments the total bytes copied counter.
    pub fn add_bytes_copied(&self, bytes: u64) {
        self.bytes_copied.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Atomically increments the source files opened counter.
    pub fn inc_src_opened(&self) {
        self.src_opened.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the source files closed counter.
    pub fn inc_src_closed(&self) {
        self.src_closed.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the target files opened counter.
    pub fn inc_dst_opened(&self) {
        self.dst_opened.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the target files closed counter.
    pub fn inc_dst_closed(&self) {
        self.dst_closed.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the files fully copied counter.
    pub fn inc_files_copied(&self) {
        self.files_copied.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the files deleted counter.
    #[allow(dead_code)]
    pub fn inc_files_deleted(&self) {
        self.files_deleted.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the directories created counter.
    pub fn inc_dirs_created(&self) {
        self.dirs_created.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the directories deleted counter.
    #[allow(dead_code)]
    pub fn inc_dirs_deleted(&self) {
        self.dirs_deleted.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the files failed counter.
    pub fn inc_files_failed(&self) {
        self.files_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Atomically increments the directories failed counter.
    pub fn inc_dirs_failed(&self) {
        self.dirs_failed.fetch_add(1, Ordering::Relaxed);
    }
}

/// A serializable snapshot of backup statistics.
///
/// Unlike `BackupStats`, this type uses plain integers and can be safely
/// serialized (e.g., to JSON or binary formats) for logging, monitoring, or checkpointing.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct BackupStatsSnapshot {
    /// Total bytes copied to target.
    pub bytes_copied: u64,
    /// Number of source files successfully opened.
    pub src_opened: u64,
    /// Number of source files successfully closed.
    pub src_closed: u64,
    /// Number of target files successfully opened.
    pub dst_opened: u64,
    /// Number of target files successfully closed.
    pub dst_closed: u64,
    /// Number of files fully copied.
    pub files_copied: u64,
    /// Number of files deleted during incremental backup.
    pub files_deleted: u64,
    /// Number of directories created.
    pub dirs_created: u64,
    /// Number of directories deleted.
    pub dirs_deleted: u64,
    /// Number of files that failed during processing.
    pub files_failed: u64,
    /// Number of directories that failed during processing.
    pub dirs_failed: u64,
}
