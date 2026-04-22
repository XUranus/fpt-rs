//! Common lifecycle adapters for scan, backup, and restore tasks.
//!
//! The low-level engines keep their existing APIs. This module provides a
//! uniform control surface for orchestration and monitoring code that wants to
//! treat scanner, backup, and restore tasks the same way.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::backup::{BackupTask, RestoreStats, RestoreTask};
use crate::scanner::{RunningScan, ScanStatsSnapshot, Scanner};

use super::traits::{FileScanner, ScanStats};

/// Error returned by common lifecycle adapters.
#[derive(Debug)]
pub enum TaskLifecycleError {
    AlreadyStarted,
    NotStarted,
    StartFailed(String),
    WorkerPanicked,
}

impl fmt::Display for TaskLifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskLifecycleError::AlreadyStarted => write!(f, "task already started"),
            TaskLifecycleError::NotStarted => write!(f, "task has not been started"),
            TaskLifecycleError::StartFailed(e) => write!(f, "task start failed: {e}"),
            TaskLifecycleError::WorkerPanicked => write!(f, "task worker panicked"),
        }
    }
}

impl std::error::Error for TaskLifecycleError {}

/// Common task lifecycle interface.
///
/// `stop()` is currently best-effort. Existing scan/backup/restore engines do
/// not support hard cancellation in all paths, so callers must use
/// `is_complete()` to observe actual termination.
pub trait TaskLifecycle {
    type Stats: Clone + Default + Send + 'static;

    fn start(&mut self) -> Result<(), TaskLifecycleError>;
    fn stop(&mut self) -> Result<(), TaskLifecycleError>;
    fn get_stats(&self) -> Self::Stats;
    fn is_complete(&self) -> bool;
}

/// Lifecycle wrapper for direct local scanner tasks.
pub struct ScannerLifecycleTask {
    scanner: Option<Scanner>,
    running: Option<RunningScan>,
    stopped: bool,
}

impl ScannerLifecycleTask {
    pub fn new(scanner: Scanner) -> Self {
        Self {
            scanner: Some(scanner),
            running: None,
            stopped: false,
        }
    }
}

impl TaskLifecycle for ScannerLifecycleTask {
    type Stats = ScanStatsSnapshot;

    fn start(&mut self) -> Result<(), TaskLifecycleError> {
        if self.running.is_some() {
            return Err(TaskLifecycleError::AlreadyStarted);
        }
        let scanner = self
            .scanner
            .take()
            .ok_or(TaskLifecycleError::AlreadyStarted)?;
        let running = scanner
            .start()
            .map_err(|e| TaskLifecycleError::StartFailed(e.to_string()))?;
        self.running = Some(running);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), TaskLifecycleError> {
        self.stopped = true;
        Ok(())
    }

    fn get_stats(&self) -> Self::Stats {
        self.running
            .as_ref()
            .map(|running| running.stats())
            .unwrap_or_default()
    }

    fn is_complete(&self) -> bool {
        self.running
            .as_ref()
            .map(|running| running.complete())
            .unwrap_or(self.stopped)
    }
}

/// Lifecycle wrapper for frame-level scanner implementations, including local,
/// NFS, and SMB scanners.
pub struct FileScannerLifecycleTask<S>
where
    S: FileScanner + Send + 'static,
{
    scanner: Option<S>,
    stats: Arc<Mutex<ScanStats>>,
    complete: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    stopped: bool,
}

impl<S> FileScannerLifecycleTask<S>
where
    S: FileScanner + Send + 'static,
{
    pub fn new(scanner: S) -> Self {
        Self {
            scanner: Some(scanner),
            stats: Arc::new(Mutex::new(ScanStats::default())),
            complete: Arc::new(AtomicBool::new(false)),
            handle: None,
            stopped: false,
        }
    }
}

impl<S> TaskLifecycle for FileScannerLifecycleTask<S>
where
    S: FileScanner + Send + 'static,
{
    type Stats = ScanStats;

    fn start(&mut self) -> Result<(), TaskLifecycleError> {
        if self.handle.is_some() {
            return Err(TaskLifecycleError::AlreadyStarted);
        }
        let scanner = self
            .scanner
            .take()
            .ok_or(TaskLifecycleError::AlreadyStarted)?;
        let stats = Arc::clone(&self.stats);
        let complete = Arc::clone(&self.complete);
        self.handle = Some(thread::spawn(move || {
            match scanner.scan() {
                Ok(result) => {
                    *stats.lock().unwrap() = result;
                }
                Err(e) => {
                    log::error!("scanner lifecycle task failed: {e}");
                }
            }
            complete.store(true, Ordering::Relaxed);
        }));
        Ok(())
    }

    fn stop(&mut self) -> Result<(), TaskLifecycleError> {
        self.stopped = true;
        Ok(())
    }

    fn get_stats(&self) -> Self::Stats {
        self.stats.lock().unwrap().clone()
    }

    fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Relaxed) || (self.stopped && self.handle.is_none())
    }
}

/// Lifecycle wrapper for backup tasks. Covers local, NFS, SMB, common, and
/// aggregate backup because those are selected inside [`BackupTask`].
pub struct BackupLifecycleTask {
    task: Option<BackupTask>,
    running: Option<crate::backup::RunningBackup>,
    stopped: bool,
}

impl BackupLifecycleTask {
    pub fn new(task: BackupTask) -> Self {
        Self {
            task: Some(task),
            running: None,
            stopped: false,
        }
    }
}

impl TaskLifecycle for BackupLifecycleTask {
    type Stats = crate::backup::BackupStatsSnapshot;

    fn start(&mut self) -> Result<(), TaskLifecycleError> {
        if self.running.is_some() {
            return Err(TaskLifecycleError::AlreadyStarted);
        }
        let task = self.task.take().ok_or(TaskLifecycleError::AlreadyStarted)?;
        let running = task
            .start()
            .map_err(|e| TaskLifecycleError::StartFailed(e.to_string()))?;
        self.running = Some(running);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), TaskLifecycleError> {
        self.stopped = true;
        Ok(())
    }

    fn get_stats(&self) -> Self::Stats {
        self.running
            .as_ref()
            .map(|running| running.stats())
            .unwrap_or_default()
    }

    fn is_complete(&self) -> bool {
        self.running
            .as_ref()
            .map(|running| running.complete())
            .unwrap_or(self.stopped)
    }
}

/// Lifecycle wrapper for restore tasks. Covers local, NFS, SMB, common, and
/// aggregate restore because those are selected inside [`RestoreTask`].
pub struct RestoreLifecycleTask {
    task: Option<RestoreTask>,
    running: Option<crate::backup::RunningRestore>,
    stopped: bool,
}

impl RestoreLifecycleTask {
    pub fn new(task: RestoreTask) -> Self {
        Self {
            task: Some(task),
            running: None,
            stopped: false,
        }
    }
}

impl TaskLifecycle for RestoreLifecycleTask {
    type Stats = RestoreStats;

    fn start(&mut self) -> Result<(), TaskLifecycleError> {
        if self.running.is_some() {
            return Err(TaskLifecycleError::AlreadyStarted);
        }
        let task = self.task.take().ok_or(TaskLifecycleError::AlreadyStarted)?;
        let running = task
            .start()
            .map_err(|e| TaskLifecycleError::StartFailed(e.to_string()))?;
        self.running = Some(running);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), TaskLifecycleError> {
        self.stopped = true;
        Ok(())
    }

    fn get_stats(&self) -> Self::Stats {
        self.running
            .as_ref()
            .map(|running| running.stats())
            .unwrap_or_default()
    }

    fn is_complete(&self) -> bool {
        self.running
            .as_ref()
            .map(|running| running.complete())
            .unwrap_or(self.stopped)
    }
}
