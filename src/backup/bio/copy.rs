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

use crate::{backup::{SharedState, fcb::{ControlBlockVarient, DirControlBlock, FileControlBlock, SourceHandleState, TargetHandleState}, stats::BackupStats}, scanner::metadata::{ControlEntry, ControlFileReader, DirMeta, MetaRepoReader}};
use std::{fs::File, path::PathBuf, sync::mpsc::RecvTimeoutError, time::Duration};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Mutex;
use std::sync::{
    atomic::{Ordering},
    mpsc,
    Arc,
};
use bincode::de;
use chrono::format::Item;
use log::{debug, info, warn, error};
use sha2::digest::typenum::Or;

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
pub enum BioError {
    /// Insufficient disk space on the target device.
    InsufficientSpace(io::Error),
    /// Any other I/O error.
    Unknown(io::Error),
}

/// Make path relative to base_dir and then join with target_base
/// e.g., base_dir=/tmp/bifrost_test/source, path=/tmp/bifrost_test/source/subdir
///       -> target_base/subdir
fn make_relative_and_join(
    base_dir: &PathBuf,
    target_base: PathBuf,
    path: String,
) -> PathBuf {
    let path_buf = PathBuf::from(&path);
    
    // Try to strip the base_dir prefix from path
    let relative_path = if path_buf.starts_with(base_dir) {
        path_buf.strip_prefix(base_dir)
            .map(|p| p.to_path_buf())
            .unwrap_or(path_buf)
    } else if path_buf.is_absolute() {
        // If path doesn't start with base_dir but is absolute,
        // just use the last component as fallback
        path_buf.file_name()
            .map(|name| PathBuf::from(name))
            .unwrap_or_else(|| path_buf)
    } else {
        path_buf
    };
    
    target_base.join(relative_path)
}


pub fn spawn_file_entry_producer(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base : PathBuf,
    target_dir_base : PathBuf,
    fcb_producer_tx: mpsc::Sender<ControlBlockVarient>,
    shared_state : Arc<SharedState>) -> std::thread::JoinHandle<()>
{
    let meta_repo_reader = MetaRepoReader::new(meta_dir).unwrap();
    std::thread::spawn(move || {
        let control_reader = ControlFileReader::open(control_file).unwrap();
        let mut dirpath = PathBuf::new();

        for entry in control_reader {
            let entry = entry.unwrap();
            let item = match entry {
                ControlEntry::Dir(dentry) => {
                    let dmeta = meta_repo_reader.get_dmeta((dentry.meta_fid, dentry.meta_offset)).unwrap();
                    let mut dcb = DirControlBlock::from(dmeta);
                    // Source path uses the absolute path from control file
                    dcb.src_path = PathBuf::from(dentry.path.clone());
                    // Target path: make relative to source base and join with target base
                    dcb.dst_path = make_relative_and_join(&source_dir_base, target_dir_base.clone(), dentry.path.clone());
                    dirpath = dentry.path.into();
                    ControlBlockVarient::DirControlBlock(dcb)
                },
                ControlEntry::File(fentry) => {
                    let fmeta = meta_repo_reader.get_fmeta((fentry.meta_fid, fentry.meta_offset)).unwrap();
                    let mut fcb: FileControlBlock = FileControlBlock::from(fmeta);
                    // Source path uses absolute path from dirpath + filename
                    fcb.src_path = PathBuf::from(&dirpath).join(fentry.name.clone());
                    // Target path: make dirpath relative to source base and join with target base + filename
                    let relative_dir = make_relative_and_join(&source_dir_base, target_dir_base.clone(), dirpath.to_string_lossy().to_string());
                    fcb.dst_path = relative_dir.join(fentry.name.clone());
                    ControlBlockVarient::FileControlBlock(fcb)
                }
            };
            fcb_producer_tx.send(item).unwrap();
        }
        shared_state.entry_produce_done.store(true, Ordering::Relaxed);
        info!("file entry producer thread end.");
    })
}



// === Reader Control Thread ===

/// Spawns a reader control thread that routes `FileControlBlock`s to I/O tasks.
///
/// The thread continuously receives FCBs, inspects their source state, and
/// enqueues the next required I/O operation.
pub fn spawn_reader(
    reader_rx: mpsc::Receiver<ControlBlockVarient>,
    reader_io_pool_tx: mpsc::Sender<ReaderBioTask>,
    writer_tx: mpsc::Sender<ControlBlockVarient>,
    shared_state : Arc<SharedState>
) -> std::thread::JoinHandle<()> {

    std::thread::spawn(move || {
        loop {
            let result = reader_rx.recv_timeout(Duration::from_millis(100));
            match result {
                Ok(item) => {
                    match item {
                        ControlBlockVarient::DirControlBlock(dcb) => {
                            // Forward directory entries to writer for creation
                            let _ = writer_tx.send(ControlBlockVarient::DirControlBlock(dcb));
                        }
                        ControlBlockVarient::FileControlBlock(fcb) => {
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
                    }
                },
                Err(RecvTimeoutError::Timeout) => {
                    if shared_state.entry_produce_done.load(Ordering::Relaxed)
                        && shared_state.active_reader_io_workers.load(Ordering::Relaxed) == 0 {
                        shared_state.reader_done.store(true, Ordering::Relaxed);
                        break;
                    }
                },
                Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
        info!("reader thread end.");
    })
}

// === Reader I/O Result Poller ===

/// Spawns a thread that processes reader I/O results and routes FCBs onward.
///
/// Completed read operations are sent to the writer queue; errors are logged
/// and counted in statistics.
pub fn spawn_reader_io_result_poll(
    result_rx: mpsc::Receiver<ReaderBioResult>,
    reader_tx: mpsc::Sender<ControlBlockVarient>,
    writer_tx: mpsc::Sender<ControlBlockVarient>,
    stats: Arc<BackupStats>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(result) = result_rx.recv() {
            match result {
                ReaderBioResult::OpenSource(Ok(fcb)) => {
                    stats.src_opened.fetch_add(1, Ordering::Relaxed);
                    let _ = reader_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                }
                ReaderBioResult::OpenSource(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                ReaderBioResult::ReadSource(Ok(fcb)) => {
                    let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                }
                ReaderBioResult::ReadSource(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                ReaderBioResult::CloseSource(Ok(fcb)) => {
                    stats.src_closed.fetch_add(1, Ordering::Relaxed);
                    let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                }
                ReaderBioResult::CloseSource(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        info!("reader io_pool polling thread end.");
    })
}

// === Reader I/O Thread Pool ===

/// Spawns a pool of threads to execute reader-side blocking I/O tasks.
///
/// ⚠️ **Note**: Using `Arc<Mutex<mpsc::Receiver>>` serializes task retrieval.
/// For higher throughput, consider lock-free channels (e.g., `crossbeam`).
pub fn spawn_reader_io_pool(
    task_rx: Arc<Mutex<mpsc::Receiver<ReaderBioTask>>>,
    result_tx: mpsc::Sender<ReaderBioResult>,
    num_threads: usize,
    shared_state : Arc<SharedState>
) -> Vec<std::thread::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(num_threads);
    for i in 0..num_threads {
        let task_rx = Arc::clone(&task_rx);
        let result_tx = result_tx.clone();
        let shared_state = Arc::clone(&shared_state);
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
                        shared_state.active_reader_io_workers.fetch_add(1, Ordering::Relaxed);
                        let result = match task {
                            ReaderBioTask::OpenSource(fcb) => open_source(fcb),
                            ReaderBioTask::ReadSource(fcb) => read_source(fcb),
                            ReaderBioTask::CloseSource(fcb) => close_source(fcb),
                        };
                        let _ = result_tx.send(result);
                        shared_state.active_reader_io_workers.fetch_sub(1, Ordering::Relaxed);
                    }
                    Err(_) => break, // Channel closed
                }
            }
            info!("reader io_pool worker[{}] thread end.", i);
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
    writer_rx: mpsc::Receiver<ControlBlockVarient>,
    writer_io_pool_tx: mpsc::Sender<WriterBioTask>,
    shared_state : Arc<SharedState>,
    stats: Arc<BackupStats>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            let result = writer_rx.recv_timeout(Duration::from_millis(100));
            match result {
                Ok(item) => {
                    match item {
                        ControlBlockVarient::DirControlBlock(dcb) => {
                            // Create the directory explicitly
                            if let Err(e) = std::fs::create_dir_all(&dcb.dst_path) {
                                error!("Failed to create target directory {:?}: {}", dcb.dst_path, e);
                                stats.dirs_failed.fetch_add(1, Ordering::Relaxed);
                            } else {
                                stats.dirs_created.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        ControlBlockVarient::FileControlBlock(fcb) => {
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
                    }
                },
                Err(RecvTimeoutError::Timeout) => {
                    if shared_state.reader_done.load(Ordering::Relaxed)
                        && shared_state.active_writer_io_workers.load(Ordering::Relaxed) == 0 {
                        shared_state.writer_done.store(true, Ordering::Relaxed);
                        break;
                    }
                },
                Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
        info!("writer thread end.");
    })
}

// === Writer I/O Result Poller ===

/// Spawns a thread that processes writer I/O results and routes FCBs onward.
///
/// After a successful write, if the file is complete, it may be finalized.
/// Errors are logged and counted in statistics.
pub fn spawn_writer_io_result_poll(
    result_rx: mpsc::Receiver<WriterBioResult>,
    writer_tx: mpsc::Sender<ControlBlockVarient>,
    stats: Arc<BackupStats>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(result) = result_rx.recv() {
            match result {
                WriterBioResult::OpenTarget(Ok(fcb)) => {
                    stats.dst_opened.fetch_add(1, Ordering::Relaxed);
                    let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
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
                    let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
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
        info!("writer io_pool polling thread end.");
    })
}

// === Writer I/O Thread Pool ===

/// Spawns a pool of threads to execute writer-side blocking I/O tasks.
pub fn spawn_writer_io_pool(
    task_rx: Arc<Mutex<mpsc::Receiver<WriterBioTask>>>,
    result_tx: mpsc::Sender<WriterBioResult>,
    num_threads: usize,
    shared_state : Arc<SharedState>
) -> Vec<std::thread::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(num_threads);
    for i in 0..num_threads {
        let task_rx = Arc::clone(&task_rx);
        let result_tx = result_tx.clone();
        let shared_state = Arc::clone(&shared_state);
        let handle = std::thread::spawn(move || {
            debug!("Writer BIO worker {} started", i);
            loop {
                let task = {
                    let rx = task_rx.lock().unwrap();
                    rx.recv()
                };

                match task {
                    Ok(task) => {
                        shared_state.active_writer_io_workers.fetch_add(1, Ordering::Relaxed);
                        let result = match task {
                            WriterBioTask::OpenTarget(fcb) => open_target(fcb),
                            WriterBioTask::WriteTarget(fcb) => write_target(fcb),
                            WriterBioTask::CloseTarget(fcb) => close_target(fcb),
                        };
                        shared_state.active_writer_io_workers.fetch_sub(1, Ordering::Relaxed);
                        let _ = result_tx.send(result);
                    }
                    Err(_) => break,
                }
            }
            info!("writer io_pool worker[{}] thread end.", i);
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

    debug!("open dst {:?}", fcb.dst_path);
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