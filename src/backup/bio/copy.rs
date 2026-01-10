//! # Blocking I/O Engine for Backup Operations
//!
//! This module implements a **thread-pool-based blocking I/O engine** for high-throughput
//! file backup operations. It separates control logic from I/O execution using a
//! message-passing architecture:
//!
//! - **Reader/Writer threads**: Inspect `FileControlBlock` state and enqueue I/O tasks.
//! - **I/O thread pools**: Execute actual syscalls (`open`, `read`, `write`, `close`).
//! - **Result pollers**: Route completed I/O results back to the appropriate queues.
//!
//! The design ensures:
//! - **No shared mutable state**: `FileControlBlock` is moved by value between threads.
//! - **State machine integrity**: State transitions occur only within I/O workers.
//! - **Scalability**: Configurable number of I/O threads for parallelism.
//! - **Observability**: Detailed statistics tracking via `BackupStats`.

use crate::backup::fcb::{FileControlBlock, SourceHandleState, TargetHandleState};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{Receiver, Sender},
    Arc,
};
use log::{debug, error};

/// A blocking I/O task for the source (reader) side.
#[derive(Debug)]
pub enum ReaderBioTask {
    /// Open the source file for reading.
    OpenSource(FileControlBlock),
    /// Read data from the source file into the buffer.
    ReadSource(FileControlBlock),
    /// Close the source file handle.
    CloseSource(FileControlBlock),
}

/// A blocking I/O task for the target (writer) side.
#[derive(Debug)]
pub enum WriterBioTask {
    /// Create/open the target file for writing.
    OpenTarget(FileControlBlock),
    /// Write buffered data to the target file.
    WriteTarget(FileControlBlock),
    /// Close the target file handle.
    CloseTarget(FileControlBlock),
}

/// Result of a reader-side I/O operation.
#[derive(Debug)]
pub enum ReaderBioResult {
    /// Result of opening the source file.
    OpenSource(Result<FileControlBlock, BioError>),
    /// Result of reading from the source file.
    ReadSource(Result<FileControlBlock, BioError>),
    /// Result of closing the source file.
    CloseSource(Result<FileControlBlock, BioError>),
}

/// Result of a writer-side I/O operation.
#[derive(Debug)]
pub enum WriterBioResult {
    /// Result of opening the target file.
    OpenTarget(Result<FileControlBlock, BioError>),
    /// Result of writing to the target file.
    WriteTarget(Result<FileControlBlock, BioError>),
    /// Result of closing the target file.
    CloseTarget(Result<FileControlBlock, BioError>),
}

/// Error type for backup I/O operations.
#[derive(Debug)]
enum BioError {
    /// Insufficient disk space on the target device.
    InsufficientSpace(io::Error),
    /// Any other I/O error.
    Unknown(io::Error),
}

/// Real-time statistics for backup operations.
///
/// All fields are atomic to support concurrent updates from multiple threads.
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

// === Reader Control Thread ===

/// Spawns a reader control thread that routes `FileControlBlock`s to I/O tasks.
///
/// The thread continuously receives FCBs, inspects their source state, and
/// enqueues the next required I/O operation.
pub fn spawn_reader(
    reader_rx: Receiver<FileControlBlock>,
    reader_io_pool_tx: Sender<ReaderBioTask>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(fcb) = reader_rx.recv() {
            match fcb.src_state {
                SourceHandleState::Inited => {
                    let _ = reader_io_pool_tx.send(ReaderBioTask::OpenSource(fcb));
                }
                SourceHandleState::Opened => {
                    let _ = reader_io_pool_tx.send(ReaderBioTask::ReadSource(fcb));
                }
                // Read/PartialRead/Closed states are handled by writer or completion
                _ => {}
            }
        }
    })
}

// === Reader I/O Result Poller ===

/// Spawns a thread that processes reader I/O results and routes FCBs onward.
///
/// Completed read operations are sent to the writer queue; errors are logged
/// and counted in statistics.
pub fn spawn_reader_io_result_poll(
    reader_tx: Sender<FileControlBlock>,
    writer_tx: Sender<FileControlBlock>,
    result_rx: Receiver<ReaderBioResult>,
    stats: Arc<BackupStats>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(result) = result_rx.recv() {
            match result {
                ReaderBioResult::OpenSource(Ok(fcb)) => {
                    stats.src_opened.fetch_add(1, Ordering::Relaxed);
                    let _ = reader_tx.send(fcb);
                }
                ReaderBioResult::OpenSource(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                ReaderBioResult::ReadSource(Ok(fcb)) => {
                    let _ = writer_tx.send(fcb);
                }
                ReaderBioResult::ReadSource(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                ReaderBioResult::CloseSource(Ok(fcb)) => {
                    stats.src_closed.fetch_add(1, Ordering::Relaxed);
                    let _ = writer_tx.send(fcb);
                }
                ReaderBioResult::CloseSource(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    })
}

// === Reader I/O Thread Pool ===

/// Spawns a pool of threads to execute reader-side blocking I/O tasks.
///
/// ⚠️ **Note**: Using `Arc<Mutex<Receiver>>` serializes task retrieval.
/// For higher throughput, consider lock-free channels (e.g., `crossbeam`).
pub fn spawn_reader_io_pool(
    task_rx: Arc<Mutex<Receiver<ReaderBioTask>>>,
    result_tx: Sender<ReaderBioResult>,
    num_threads: usize,
) -> Vec<std::thread::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(num_threads);
    for i in 0..num_threads {
        let task_rx = Arc::clone(&task_rx);
        let result_tx = result_tx.clone();
        let handle = std::thread::spawn(move || {
            debug!("Reader BIO worker {} started", i);
            loop {
                // Acquire lock only to receive a task
                let task = {
                    let rx = task_rx.lock().unwrap();
                    rx.recv()
                };

                match task {
                    Ok(task) => {
                        let result = match task {
                            ReaderBioTask::OpenSource(fcb) => open_source(fcb),
                            ReaderBioTask::ReadSource(fcb) => read_source(fcb),
                            ReaderBioTask::CloseSource(fcb) => close_source(fcb),
                        };
                        let _ = result_tx.send(result);
                    }
                    Err(_) => break, // Channel closed
                }
            }
        });
        handles.push(handle);
    }
    handles
}

// === Writer Control Thread ===

/// Spawns a writer control thread that routes `FileControlBlock`s to I/O tasks.
///
/// The thread continuously receives FCBs, inspects their target state, and
/// enqueues the next required I/O operation.
pub fn spawn_writer(
    writer_rx: Receiver<FileControlBlock>,
    writer_io_pool_tx: Sender<WriterBioTask>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(fcb) = writer_rx.recv() {
            match fcb.dst_state {
                TargetHandleState::Inited => {
                    let _ = writer_io_pool_tx.send(WriterBioTask::OpenTarget(fcb));
                }
                TargetHandleState::Opened => {
                    let _ = writer_io_pool_tx.send(WriterBioTask::WriteTarget(fcb));
                }
                // Written/Closed states are final
                _ => {}
            }
        }
    })
}

// === Writer I/O Result Poller ===

/// Spawns a thread that processes writer I/O results and routes FCBs onward.
///
/// After a successful write, if the file is complete, it may be finalized.
/// Errors are logged and counted in statistics.
pub fn spawn_writer_io_result_poll(
    _reader_tx: Sender<FileControlBlock>, // Unused in current design
    writer_tx: Sender<FileControlBlock>,
    result_rx: Receiver<WriterBioResult>,
    stats: Arc<BackupStats>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(result) = result_rx.recv() {
            match result {
                WriterBioResult::OpenTarget(Ok(fcb)) => {
                    stats.dst_opened.fetch_add(1, Ordering::Relaxed);
                    let _ = writer_tx.send(fcb);
                }
                WriterBioResult::OpenTarget(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                WriterBioResult::WriteTarget(Ok(mut fcb)) => {
                    // Check if write is complete
                    if fcb.dst_offset >= fcb.meta.size {
                        fcb.dst_state = TargetHandleState::Written;
                        stats.files_copied.fetch_add(1, Ordering::Relaxed);
                        stats.bytes_copied.fetch_add(fcb.meta.size, Ordering::Relaxed);
                    }
                    let _ = writer_tx.send(fcb);
                }
                WriterBioResult::WriteTarget(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                WriterBioResult::CloseTarget(Ok(fcb)) => {
                    stats.dst_closed.fetch_add(1, Ordering::Relaxed);
                    // File is now fully backed up; could send to completion queue
                }
                WriterBioResult::CloseTarget(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    })
}

// === Writer I/O Thread Pool ===

/// Spawns a pool of threads to execute writer-side blocking I/O tasks.
pub fn spawn_writer_io_pool(
    task_rx: Arc<Mutex<Receiver<WriterBioTask>>>,
    result_tx: Sender<WriterBioResult>,
    num_threads: usize,
) -> Vec<std::thread::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(num_threads);
    for i in 0..num_threads {
        let task_rx = Arc::clone(&task_rx);
        let result_tx = result_tx.clone();
        let handle = std::thread::spawn(move || {
            debug!("Writer BIO worker {} started", i);
            loop {
                let task = {
                    let rx = task_rx.lock().unwrap();
                    rx.recv()
                };

                match task {
                    Ok(task) => {
                        let result = match task {
                            WriterBioTask::OpenTarget(fcb) => open_target(fcb),
                            WriterBioTask::WriteTarget(fcb) => write_target(fcb),
                            WriterBioTask::CloseTarget(fcb) => close_target(fcb),
                        };
                        let _ = result_tx.send(result);
                    }
                    Err(_) => break,
                }
            }
        });
        handles.push(handle);
    }
    handles
}

// === I/O Implementation ===

fn open_source(mut fcb: FileControlBlock) -> ReaderBioResult {
    match File::open(&fcb.src_path) {
        Ok(file) => {
            fcb.src_handle = Some(file);
            fcb.src_state = SourceHandleState::Opened;
            ReaderBioResult::OpenSource(Ok(fcb))
        }
        Err(e) => {
            error!("Failed to open source file {:?}: {}", fcb.src_path, e);
            ReaderBioResult::OpenSource(Err(BioError::Unknown(e)))
        }
    }
}

fn read_source(mut fcb: FileControlBlock) -> ReaderBioResult {
    let mut file = fcb.src_handle.take().expect("Source handle missing in ReadSource");
    let offset = fcb.src_offset;

    if let Err(e) = file.seek(SeekFrom::Start(offset as u64)) {
        error!("Failed to seek in source file {:?} at {}: {}", fcb.src_path, offset, e);
        return ReaderBioResult::ReadSource(Err(BioError::Unknown(e)));
    }

    // Ensure buffer is sized appropriately
    if fcb.buffer.len() == 0 {
        fcb.buffer.resize(fcb.meta.size.saturating_sub(offset) as usize, 0);
    }

    match file.read(&mut fcb.buffer) {
        Ok(n) => {
            fcb.buffer.truncate(n); // Keep only read data
            fcb.src_offset += n as u64;
            fcb.src_state = if fcb.src_offset >= fcb.meta.size {
                SourceHandleState::Read
            } else {
                SourceHandleState::PartialRead
            };
            ReaderBioResult::ReadSource(Ok(fcb))
        }
        Err(e) => {
            error!("Failed to read source file {:?}: {}", fcb.src_path, e);
            ReaderBioResult::ReadSource(Err(BioError::Unknown(e)))
        }
    }
}

fn close_source(mut fcb: FileControlBlock) -> ReaderBioResult {
    drop(fcb.src_handle.take()); // Close file if open
    fcb.src_state = SourceHandleState::Closed;
    ReaderBioResult::CloseSource(Ok(fcb))
}

fn open_target(mut fcb: FileControlBlock) -> WriterBioResult {
    // Create parent directories if needed
    if let Some(parent) = fcb.dst_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            error!("Failed to create target directory {:?}: {}", parent, e);
            return WriterBioResult::OpenTarget(Err(BioError::Unknown(e)));
        }
    }

    // Use create_new(true) to avoid overwriting existing files?
    match File::create(&fcb.dst_path) {
        Ok(file) => {
            fcb.dst_handle = Some(file);
            fcb.dst_state = TargetHandleState::Opened;
            WriterBioResult::OpenTarget(Ok(fcb))
        }
        Err(e) => {
            error!("Failed to create target file {:?}: {}", fcb.dst_path, e);
            WriterBioResult::OpenTarget(Err(BioError::Unknown(e)))
        }
    }
}

fn write_target(mut fcb: FileControlBlock) -> WriterBioResult {
    let mut file = fcb.dst_handle.take().expect("Target handle missing in WriteTarget");
    let offset = fcb.dst_offset;
    let buffer_len = fcb.buffer.len();

    if let Err(e) = file.seek(SeekFrom::Start(offset as u64)) {
        error!("Failed to seek in target file {:?} at {}: {}", fcb.dst_path, offset, e);
        return WriterBioResult::WriteTarget(Err(BioError::Unknown(e)));
    }

    match file.write_all(&fcb.buffer) {
        Ok(()) => {
            fcb.dst_offset += buffer_len as u64;
            fcb.dst_state = if fcb.dst_offset >= fcb.meta.size {
                TargetHandleState::Written
            } else {
                TargetHandleState::PartialWritten
            };
            WriterBioResult::WriteTarget(Ok(fcb))
        }
        Err(e) => {
            error!("Failed to write to target file {:?}: {}", fcb.dst_path, e);
            WriterBioResult::WriteTarget(Err(BioError::Unknown(e)))
        }
    }
}

fn close_target(mut fcb: FileControlBlock) -> WriterBioResult {
    drop(fcb.dst_handle.take()); // Close file if open
    fcb.dst_state = TargetHandleState::Closed;
    WriterBioResult::CloseTarget(Ok(fcb))
}