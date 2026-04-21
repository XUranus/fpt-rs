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

use crate::{
    backup::{
        aggregate_engine::{fcb_to_pending_file, AggregateBackupEngine, AggregateBackupState},
        fcb::{
            ControlBlockVarient, DirControlBlock, FileControlBlock, SourceHandleState,
            TargetHandleState, MAX_FILE_BUFFER_SIZE,
        },
        stats::BackupStats,
        SharedState,
    },
    scanner::metadata::{ControlEntry, ControlFileReader, MetaRepoReader},
};
use log::{debug, error, info, warn};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Mutex;
use std::sync::{atomic::Ordering, mpsc, Arc};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::mpsc::RecvTimeoutError,
    time::Duration,
};

/// A blocking I/O task for the source (reader) side.
#[derive(Debug)]
pub enum ReaderBioTask {
    /// Open the source file for reading.
    OpenSource(FileControlBlock),
    /// Read data from the source file into the buffer.
    ReadSource(FileControlBlock),
    /// Close the source file handle.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    InsufficientSpace(io::Error),
    /// Any other I/O error.
    #[allow(dead_code)]
    Unknown(io::Error),
}

pub fn spawn_file_entry_producer(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    target_dir_base: PathBuf,
    fcb_producer_tx: mpsc::SyncSender<ControlBlockVarient>,
    shared_state: Arc<SharedState>,
) -> std::thread::JoinHandle<()> {
    let meta_repo_reader = MetaRepoReader::new(meta_dir).unwrap();
    std::thread::spawn(move || {
        let control_reader = ControlFileReader::open(control_file).unwrap();
        let logical_source_root = PathBuf::from(control_reader.header().source_root.clone());
        let mut dirpath = PathBuf::new();

        for entry in control_reader {
            let entry = entry.unwrap();
            let item = match entry {
                ControlEntry::Dir(dentry) => {
                    let dmeta = meta_repo_reader
                        .get_dmeta((dentry.meta_fid, dentry.meta_offset))
                        .unwrap();
                    let mut dcb = DirControlBlock::from(dmeta);
                    dcb.src_path = resolve_local_source_path(
                        &source_dir_base,
                        &logical_source_root,
                        &dentry.path,
                    );
                    dcb.dst_path = logical_target_path(target_dir_base.clone(), &dentry.path);
                    dirpath = dentry.path.into();
                    ControlBlockVarient::DirControlBlock(dcb)
                }
                ControlEntry::File(fentry) => {
                    let fmeta = meta_repo_reader
                        .get_fmeta((fentry.meta_fid, fentry.meta_offset))
                        .unwrap();
                    let mut fcb: FileControlBlock = FileControlBlock::from(fmeta);
                    fcb.src_path = resolve_local_source_path(
                        &source_dir_base,
                        &logical_source_root,
                        &dirpath.to_string_lossy(),
                    )
                    .join(fentry.name.clone());
                    let relative_dir =
                        logical_target_path(target_dir_base.clone(), &dirpath.to_string_lossy());
                    fcb.dst_path = relative_dir.join(fentry.name.clone());
                    ControlBlockVarient::FileControlBlock(fcb)
                }
            };
            fcb_producer_tx.send(item).unwrap();
        }
        shared_state
            .entry_produce_done
            .store(true, Ordering::Relaxed);
        info!("file entry producer thread end.");
    })
}

fn resolve_local_source_path(
    source_root: &Path,
    logical_source_root: &Path,
    control_path: &str,
) -> PathBuf {
    let control_path = PathBuf::from(control_path);
    if control_path.starts_with(source_root) {
        return control_path;
    }
    let rel = control_path
        .strip_prefix(logical_source_root)
        .or_else(|_| control_path.strip_prefix("/"))
        .map(|p| p.to_path_buf())
        .unwrap_or(control_path);
    source_root.join(rel)
}

fn logical_target_path(target_root: PathBuf, control_path: &str) -> PathBuf {
    target_root.join(
        PathBuf::from(control_path)
            .strip_prefix("/")
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| PathBuf::from(control_path)),
    )
}

// === Reader Control Thread ===

/// Spawns a reader control thread that routes `FileControlBlock`s to I/O tasks.
///
/// The thread continuously receives FCBs, inspects their source state, and
/// enqueues the next required I/O operation.
pub fn spawn_reader(
    reader_rx: mpsc::Receiver<ControlBlockVarient>,
    reader_io_pool_tx: mpsc::SyncSender<ReaderBioTask>,
    writer_tx: mpsc::SyncSender<ControlBlockVarient>,
    shared_state: Arc<SharedState>,
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
                            // Check if this is a symlink - symlinks don't need content copying
                            if fcb.meta.common.symlink_target_path.is_some() {
                                // Forward directly to writer for symlink creation
                                let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                            } else {
                                match fcb.src_state {
                                    SourceHandleState::Inited => {
                                        let _ =
                                            reader_io_pool_tx.send(ReaderBioTask::OpenSource(fcb));
                                    }
                                    SourceHandleState::Opened => {
                                        let _ =
                                            reader_io_pool_tx.send(ReaderBioTask::ReadSource(fcb));
                                    }
                                    // Read/PartialRead/Closed states are handled by writer or completion
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if shared_state.entry_produce_done.load(Ordering::Relaxed)
                        && shared_state
                            .active_reader_io_workers
                            .load(Ordering::Relaxed)
                            == 0
                    {
                        shared_state.reader_done.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
        info!("reader thread end.");
    })
}

/// Spawns a reader control thread with aggregation support.
/// Small files are routed to the aggregate engine instead of normal backup.
pub fn spawn_reader_with_aggregation(
    reader_rx: mpsc::Receiver<ControlBlockVarient>,
    reader_io_pool_tx: mpsc::SyncSender<ReaderBioTask>,
    writer_tx: mpsc::SyncSender<ControlBlockVarient>,
    shared_state: Arc<SharedState>,
    aggregate_engine: Arc<AggregateBackupEngine>,
    stats: Arc<BackupStats>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Create aggregate state for buffering files
        let agg_state = Arc::new(AggregateBackupState::new(aggregate_engine));

        loop {
            let result = reader_rx.recv_timeout(Duration::from_millis(100));
            match result {
                Ok(item) => {
                    match item {
                        ControlBlockVarient::DirControlBlock(dcb) => {
                            // Forward directory entries to writer for creation
                            let _ = writer_tx.send(ControlBlockVarient::DirControlBlock(dcb));
                        }
                        ControlBlockVarient::FileControlBlock(mut fcb) => {
                            // Check if this is a symlink - symlinks don't need content copying
                            if fcb.meta.common.symlink_target_path.is_some() {
                                // Forward directly to writer for symlink creation
                                let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                            } else {
                                // Check if file should be aggregated
                                let should_agg = agg_state.engine.should_aggregate(fcb.meta.size);

                                if should_agg && fcb.src_state == SourceHandleState::Read {
                                    // File is small and already read - aggregate it
                                    let _file_size = fcb.meta.size;

                                    // BUG FIX: Explicitly close the source file handle immediately after
                                    // reading to avoid "Too many open files (os error 24)" error.
                                    //
                                    // Background: When read_source() reads a file, it takes the file
                                    // handle from fcb.src_handle using take(), reads the data, and the
                                    // local file variable should be dropped at the end of read_source().
                                    // However, under high concurrency with many small files being
                                    // aggregated, file handles can accumulate faster than they are
                                    // released, causing the process to hit the system file descriptor
                                    // limit (default 1024 on many systems).
                                    //
                                    // This explicit close ensures the file descriptor is released
                                    // immediately before we continue processing, preventing resource
                                    // exhaustion. See docs/bugfix-file-handle-leak.md for details.
                                    if fcb.src_handle.is_some() {
                                        drop(fcb.src_handle.take());
                                    }

                                    let pending = fcb_to_pending_file(&fcb);
                                    let relative_path =
                                        fcb.dst_path.to_string_lossy().replace('\\', "/");

                                    if let Some((bucket_key, files)) =
                                        agg_state.add_file(&relative_path, pending)
                                    {
                                        let file_count = files.len() as u64;
                                        let bytes_in_blob: u64 =
                                            files.iter().map(|f| f.data.len() as u64).sum();

                                        match agg_state.engine.create_blob(&bucket_key, files) {
                                            Ok(blob_meta) => {
                                                info!(
                                                    "Created blob {} for bucket {} with {} files",
                                                    blob_meta.blob_path,
                                                    bucket_key,
                                                    blob_meta.file_count
                                                );
                                                stats
                                                    .files_copied
                                                    .fetch_add(file_count, Ordering::Relaxed);
                                                stats
                                                    .bytes_copied
                                                    .fetch_add(bytes_in_blob, Ordering::Relaxed);
                                            }
                                            Err(e) => {
                                                error!(
                                                    "Failed to create blob for bucket {}: {}",
                                                    bucket_key, e
                                                );
                                                stats
                                                    .files_failed
                                                    .fetch_add(file_count, Ordering::Relaxed);
                                            }
                                        }
                                    }
                                    // Note: We do NOT count files when added to buffer.
                                    // Files are only counted when successfully written to a blob.
                                    // This prevents double-counting.
                                } else if should_agg {
                                    // File should be aggregated but not yet read - send to reader
                                    match fcb.src_state {
                                        SourceHandleState::Inited => {
                                            let _ = reader_io_pool_tx
                                                .send(ReaderBioTask::OpenSource(fcb));
                                        }
                                        SourceHandleState::Opened => {
                                            let _ = reader_io_pool_tx
                                                .send(ReaderBioTask::ReadSource(fcb));
                                        }
                                        _ => {}
                                    }
                                } else {
                                    // Large file - normal backup pipeline
                                    match fcb.src_state {
                                        SourceHandleState::Inited => {
                                            let _ = reader_io_pool_tx
                                                .send(ReaderBioTask::OpenSource(fcb));
                                        }
                                        SourceHandleState::Opened => {
                                            let _ = reader_io_pool_tx
                                                .send(ReaderBioTask::ReadSource(fcb));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if shared_state.entry_produce_done.load(Ordering::Relaxed)
                        && shared_state
                            .active_reader_io_workers
                            .load(Ordering::Relaxed)
                            == 0
                    {
                        // Flush remaining aggregate buffers before exiting
                        let remaining = agg_state.flush_all();
                        for (bucket_key, files) in remaining {
                            if !files.is_empty() {
                                let file_count = files.len() as u64;
                                let bytes_in_blob: u64 =
                                    files.iter().map(|f| f.data.len() as u64).sum();

                                match agg_state.engine.create_blob(&bucket_key, files) {
                                    Ok(blob_meta) => {
                                        info!(
                                            "Created final blob {} for bucket {} with {} files",
                                            blob_meta.blob_path, bucket_key, blob_meta.file_count
                                        );
                                        stats.files_copied.fetch_add(file_count, Ordering::Relaxed);
                                        stats
                                            .bytes_copied
                                            .fetch_add(bytes_in_blob, Ordering::Relaxed);
                                    }
                                    Err(e) => {
                                        error!(
                                            "Failed to create final blob for bucket {}: {}",
                                            bucket_key, e
                                        );
                                        stats.files_failed.fetch_add(file_count, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                        let _ = agg_state.engine.flush_all_indexes();
                        shared_state.reader_done.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
        info!("reader with aggregation thread end.");
    })
}

// === Reader I/O Result Poller ===

/// Spawns a thread that processes reader I/O results and routes FCBs onward.
///
/// Completed read operations are sent to the writer queue; errors are logged
/// and counted in statistics.
pub fn spawn_reader_io_result_poll(
    result_rx: mpsc::Receiver<ReaderBioResult>,
    reader_tx: mpsc::SyncSender<ControlBlockVarient>,
    writer_tx: mpsc::SyncSender<ControlBlockVarient>,
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

/// Spawns a thread that processes reader I/O results with aggregation support.
///
/// Completed read operations for small files are routed back to the aggregation
/// reader for blob creation. Large files and other operations go to the writer.
pub fn spawn_reader_io_result_poll_with_aggregation(
    result_rx: mpsc::Receiver<ReaderBioResult>,
    reader_tx: mpsc::SyncSender<ControlBlockVarient>,
    writer_tx: mpsc::SyncSender<ControlBlockVarient>,
    stats: Arc<BackupStats>,
    aggregate_engine: Arc<AggregateBackupEngine>,
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
                    // Check if this file should be aggregated
                    if aggregate_engine.should_aggregate(fcb.meta.size) {
                        // Route back to reader for aggregation
                        let _ = reader_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                    } else {
                        // Large file - send to writer for normal backup
                        let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                    }
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
        info!("reader io_pool polling thread (with aggregation) end.");
    })
}

// === Reader I/O Thread Pool ===

/// Spawns a pool of threads to execute reader-side blocking I/O tasks.
///
/// ⚠️ **Note**: Using `Arc<Mutex<mpsc::Receiver>>` serializes task retrieval.
/// For higher throughput, consider lock-free channels (e.g., `crossbeam`).
pub fn spawn_reader_io_pool(
    task_rx: Arc<Mutex<mpsc::Receiver<ReaderBioTask>>>,
    result_tx: mpsc::SyncSender<ReaderBioResult>,
    num_threads: usize,
    shared_state: Arc<SharedState>,
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
                        shared_state
                            .active_reader_io_workers
                            .fetch_add(1, Ordering::Relaxed);
                        let result = match task {
                            ReaderBioTask::OpenSource(fcb) => open_source(fcb),
                            ReaderBioTask::ReadSource(fcb) => read_source(fcb),
                            ReaderBioTask::CloseSource(fcb) => close_source(fcb),
                        };
                        let _ = result_tx.send(result);
                        shared_state
                            .active_reader_io_workers
                            .fetch_sub(1, Ordering::Relaxed);
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
    writer_io_pool_tx: mpsc::SyncSender<WriterBioTask>,
    shared_state: Arc<SharedState>,
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
                            debug!("Creating directory: {:?}", dcb.dst_path);
                            if let Err(e) = std::fs::create_dir_all(&dcb.dst_path) {
                                error!(
                                    "Failed to create target directory {:?}: {}",
                                    dcb.dst_path, e
                                );
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
                }
                Err(RecvTimeoutError::Timeout) => {
                    if shared_state.reader_done.load(Ordering::Relaxed)
                        && shared_state
                            .active_writer_io_workers
                            .load(Ordering::Relaxed)
                            == 0
                    {
                        shared_state.writer_done.store(true, Ordering::Relaxed);
                        break;
                    }
                }
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
    writer_tx: mpsc::SyncSender<ControlBlockVarient>,
    stats: Arc<BackupStats>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(result) = result_rx.recv() {
            match result {
                WriterBioResult::OpenTarget(Ok(fcb)) => {
                    stats.dst_opened.fetch_add(1, Ordering::Relaxed);
                    // Check if this is a symlink that was already written (symlinks are created in open_target)
                    if fcb.dst_state == TargetHandleState::Written {
                        stats.files_copied.fetch_add(1, Ordering::Relaxed);
                        stats
                            .bytes_copied
                            .fetch_add(fcb.meta.size, Ordering::Relaxed);
                    } else {
                        let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                    }
                }
                WriterBioResult::OpenTarget(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                WriterBioResult::WriteTarget(Ok(mut fcb)) => {
                    // Check if write is complete
                    if fcb.dst_offset >= fcb.meta.size {
                        fcb.dst_state = TargetHandleState::Written;
                        fcb.buffer.clear();
                        fcb.buffer.shrink_to(0);
                        stats.files_copied.fetch_add(1, Ordering::Relaxed);
                        stats
                            .bytes_copied
                            .fetch_add(fcb.meta.size, Ordering::Relaxed);
                    } else {
                        let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                    }
                }
                WriterBioResult::WriteTarget(Err(_)) => {
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
                }
                WriterBioResult::CloseTarget(Ok(_fcb)) => {
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
    shared_state: Arc<SharedState>,
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
                        shared_state
                            .active_writer_io_workers
                            .fetch_add(1, Ordering::Relaxed);
                        let result = match task {
                            WriterBioTask::OpenTarget(fcb) => open_target(fcb),
                            WriterBioTask::WriteTarget(fcb) => write_target(fcb),
                            WriterBioTask::CloseTarget(fcb) => close_target(fcb),
                        };
                        shared_state
                            .active_writer_io_workers
                            .fetch_sub(1, Ordering::Relaxed);
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

/// Check if a path is a block device
#[cfg(unix)]
fn is_block_device(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.file_type().is_block_device(),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_block_device(_path: &Path) -> bool {
    // Block devices are a Unix concept; always return false on non-Unix systems
    false
}

fn open_source(mut fcb: FileControlBlock) -> ReaderBioResult {
    // Check if the source is a block device before opening
    if is_block_device(&fcb.src_path) {
        warn!("Skipping block device: {:?}", fcb.src_path);
        // Return an error to skip this file
        let e = io::Error::new(io::ErrorKind::Other, "Block device skipped");
        return ReaderBioResult::OpenSource(Err(BioError::Unknown(e)));
    }

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
    let mut file = fcb
        .src_handle
        .take()
        .expect("Source handle missing in ReadSource");
    let offset = fcb.src_offset;

    if let Err(e) = file.seek(SeekFrom::Start(offset as u64)) {
        error!(
            "Failed to seek in source file {:?} at {}: {}",
            fcb.src_path, offset, e
        );
        return ReaderBioResult::ReadSource(Err(BioError::Unknown(e)));
    }

    // Allocate only for the active chunk. This keeps queued FCBs cheap and
    // prevents large file sets from pre-reserving payload buffers in memory.
    let remaining = fcb.meta.size.saturating_sub(offset) as usize;
    let chunk_len = remaining.min(MAX_FILE_BUFFER_SIZE);
    if fcb.buffer.len() != chunk_len {
        fcb.buffer.resize(chunk_len, 0);
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
    // Check if this is a symlink
    if let Some(ref symlink_target) = fcb.meta.common.symlink_target_path {
        // Create parent directories if needed
        if let Some(parent) = fcb.dst_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                error!("Failed to create target directory {:?}: {}", parent, e);
                return WriterBioResult::OpenTarget(Err(BioError::Unknown(e)));
            }
        }

        // Create the symlink
        if let Err(e) = create_symlink(&fcb.src_path, &fcb.dst_path, symlink_target) {
            error!(
                "Failed to create symlink {:?} -> {}: {}",
                fcb.dst_path, symlink_target, e
            );
            return WriterBioResult::OpenTarget(Err(BioError::Unknown(e)));
        }

        // Restore ACLs and xattrs for symlinks too
        #[cfg(target_os = "linux")]
        {
            restore_xattrs(&fcb.dst_path, &fcb.meta.common.xattributes);
            restore_acl(
                &fcb.dst_path,
                &fcb.meta.common.posix_access_acl,
                &fcb.meta.common.posix_default_acl,
            );
        }

        // Mark as written (symlinks don't need content copying)
        fcb.dst_state = TargetHandleState::Written;
        return WriterBioResult::OpenTarget(Ok(fcb));
    }

    // Create parent directories if needed
    if let Some(parent) = fcb.dst_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            error!("Failed to create target directory {:?}: {}", parent, e);
            return WriterBioResult::OpenTarget(Err(BioError::Unknown(e)));
        }
    }

    debug!(
        "Copying file: {:?} -> {:?} ({} bytes)",
        fcb.src_path, fcb.dst_path, fcb.meta.size
    );
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
    let mut file = fcb
        .dst_handle
        .take()
        .expect("Target handle missing in WriteTarget");
    let offset = fcb.dst_offset;
    let buffer_len = fcb.buffer.len();

    if let Err(e) = file.seek(SeekFrom::Start(offset as u64)) {
        error!(
            "Failed to seek in target file {:?} at {}: {}",
            fcb.dst_path, offset, e
        );
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

/// Restore extended attributes to the target file
#[cfg(target_os = "linux")]
fn restore_xattrs(path: &PathBuf, xattrs: &Option<String>) {
    use base64::Engine as _;

    if let Some(xattr_str) = xattrs {
        for line in xattr_str.lines() {
            if let Some((name, b64_value)) = line.split_once('=') {
                if let Ok(value) = base64::engine::general_purpose::STANDARD.decode(b64_value) {
                    if let Err(e) = xattr::set(path, name, &value) {
                        error!("Failed to set xattr {} on {:?}: {}", name, path, e);
                    }
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_xattrs(_path: &PathBuf, _xattrs: &Option<String>) {}

/// Restore ACL to the target file
#[cfg(target_os = "linux")]
fn restore_acl(path: &PathBuf, access_acl: &Option<String>, default_acl: &Option<String>) {
    use exacl::{setfacl, AclEntry};

    let mut acl_entries = Vec::new();

    // Parse and add access ACL entries
    if let Some(acl_str) = access_acl {
        for line in acl_str.lines() {
            if let Ok(entry) = line.parse::<AclEntry>() {
                acl_entries.push(entry);
            }
        }
    }

    // Parse and add default ACL entries (for directories)
    if let Some(acl_str) = default_acl {
        for line in acl_str.lines() {
            if let Ok(entry) = line.parse::<AclEntry>() {
                acl_entries.push(entry);
            }
        }
    }

    if !acl_entries.is_empty() {
        if let Err(e) = setfacl(&[path.as_path()], &acl_entries, None) {
            error!("Failed to set ACL on {:?}: {}", path, e);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_acl(_path: &PathBuf, _access_acl: &Option<String>, _default_acl: &Option<String>) {}

/// Create a symlink at the target path
fn create_symlink(_src_path: &PathBuf, dst_path: &PathBuf, target: &str) -> io::Result<()> {
    // Remove existing file/symlink if exists
    if dst_path.exists() {
        std::fs::remove_file(dst_path)?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, dst_path)
    }
    #[cfg(windows)]
    {
        // On Windows, determine if it's a file or directory symlink
        let src_target = src_path.parent().unwrap_or(Path::new("")).join(target);
        if src_target.is_dir() {
            std::os::windows::fs::symlink_dir(target, dst_path)
        } else {
            std::os::windows::fs::symlink_file(target, dst_path)
        }
    }
}

fn close_target(mut fcb: FileControlBlock) -> WriterBioResult {
    drop(fcb.dst_handle.take()); // Close file if open

    // Restore metadata (ACLs, xattrs) after file is closed
    #[cfg(target_os = "linux")]
    {
        restore_xattrs(&fcb.dst_path, &fcb.meta.common.xattributes);
        restore_acl(
            &fcb.dst_path,
            &fcb.meta.common.posix_access_acl,
            &fcb.meta.common.posix_default_acl,
        );
    }

    fcb.dst_state = TargetHandleState::Closed;
    WriterBioResult::CloseTarget(Ok(fcb))
}
