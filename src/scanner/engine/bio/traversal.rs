//! # Directory Scanning Workers
//!
//! This module implements the core logic for **parallel filesystem traversal** using a
//! producer-consumer pattern with shared work queues.
//!
//! - A pool of worker threads processes directories from a shared `SpillQueue<DirScanEntry>`.
//! - Each worker scans a directory, collects file metadata, and pushes results to an output queue.
//! - Subdirectories are enqueued for further processing, enabling recursive traversal.
//! - The system supports configurable filtering (e.g., hidden files) and robust error handling.
//!
//! The design ensures **high throughput**, **bounded memory usage** (via spillable queues),
//! and **graceful shutdown** when all work is complete.

use log::{debug, error, warn};
use std::fs;
use std::io;
use std::{
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    thread, time,
};

use crate::{
    failure::{FailureItemType, FailureRecord},
    native::fstat,
    scanner::{
        filter::logical_path_from_physical,
        models::{DirBatchScanResult, DirScanEntry},
        ScanWorkerContext,
    },
};

fn retry_scan_io<T, F>(context: &ScanWorkerContext, mut op: F) -> io::Result<(T, u32)>
where
    F: FnMut() -> io::Result<T>,
{
    let policy = context.scan_option.retry_policy;
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        match op() {
            Ok(v) => return Ok((v, attempts)),
            Err(e) if policy.should_retry(attempts) => {
                thread::sleep(policy.delay_for_attempt(attempts));
                let _ = &e;
            }
            Err(e) => return Err(e),
        }
    }
}

fn record_scan_failure(
    context: &ScanWorkerContext,
    operation: &str,
    item_type: FailureItemType,
    path: &std::path::Path,
    detail: impl Into<String>,
    attempts: u32,
) {
    if let Some(recorder) = &context.failure_recorder {
        recorder.record(FailureRecord::from_detail(
            "scan",
            operation,
            item_type,
            path.to_string_lossy(),
            detail.into(),
            attempts,
        ));
    }
}

/// Processes a single directory entry: reads its contents, collects file metadata,
/// and enqueues subdirectories for further scanning.
///
/// This function is called by worker threads and performs the following:
/// 1. Stat the directory to capture its metadata.
/// 2. Iterate over its entries.
/// 3. Skip hidden files if `scan_hidden` is disabled.
/// 4. Enqueue subdirectories into the shared directory queue.
/// 5. Stat regular files and collect their metadata.
/// 6. Push the batched result to the output queue.
///
/// Errors during file statting are logged but do not halt the scan.
fn process_dir_entry(dir_entry: DirScanEntry, context: &ScanWorkerContext) {
    let stats = &context.stats;
    let dirent_queue = &context.dirent_queue;
    let output_queue = &context.output_queue;
    let scan_option = &context.scan_option;

    let mut dir_result = DirBatchScanResult::default();
    let depth = dir_entry.depth;
    let path_filters = scan_option.meta_option.path_filters.as_ref();
    let current_logical_path = path_filters
        .map(|_| logical_path_from_physical(&scan_option.control_path, &dir_entry.path));

    if let (Some(filters), Some(logical_path)) = (path_filters, current_logical_path.as_deref()) {
        if !filters.should_descend_dir(logical_path) {
            debug!("Skipping filtered directory subtree: {:?}", dir_entry.path);
            return;
        }
    }

    // Stat the directory itself
    match retry_scan_io(context, || fstat::stat_dir(&dir_entry.path)) {
        Ok((dir_meta, _)) => dir_result.dir = dir_meta,
        Err(e) => {
            error!("Failed to stat directory {:?}: {}", dir_entry.path, e);
            stats.inc_failed_dirs();
            record_scan_failure(
                context,
                "stat_dir",
                FailureItemType::Directory,
                &dir_entry.path,
                e.to_string(),
                context.scan_option.retry_policy.max_retries + 1,
            );
            return; // Skip scanning contents if we can't even stat the dir
        }
    }

    // Read and process directory entries
    match retry_scan_io(context, || fs::read_dir(&dir_entry.path)) {
        Ok((entries, _)) => {
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        error!(
                            "Failed to read directory entry in {:?}: {}",
                            dir_entry.path, e
                        );
                        stats.inc_failed_files(); // Treat as file error (conservative)
                        record_scan_failure(
                            context,
                            "read_dir_entry",
                            FailureItemType::Unknown,
                            &dir_entry.path,
                            e.to_string(),
                            1,
                        );
                        continue;
                    }
                };

                let path = entry.path();
                let file_type = match retry_scan_io(context, || entry.file_type()) {
                    Ok((ft, _)) => ft,
                    Err(e) => {
                        error!("Failed to determine file type for {:?}: {}", path, e);
                        stats.inc_failed_files();
                        record_scan_failure(
                            context,
                            "file_type",
                            FailureItemType::Unknown,
                            &path,
                            e.to_string(),
                            context.scan_option.retry_policy.max_retries + 1,
                        );
                        continue;
                    }
                };

                // Get entry name for filtering
                let entry_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Skip "." and ".." entries (current and parent directory)
                if entry_name == "." || entry_name == ".." {
                    debug!("Skipping special directory entry: {:?}", path);
                    continue;
                }

                // Apply hidden file filter (Unix-style: leading dot)
                let is_hidden = entry_name.starts_with('.');

                if !scan_option.meta_option.scan_hidden && is_hidden {
                    debug!("Skipping hidden entry: {:?}", path);
                    continue;
                }

                // Apply configured entry name filter (e.g., skip "node_modules", ".git")
                if scan_option.meta_option.skip_entries.contains(&entry_name) {
                    debug!("Skipping configured entry: {:?}", path);
                    continue;
                }

                let logical_path = path_filters
                    .map(|_| logical_path_from_physical(&scan_option.control_path, &path));

                if file_type.is_symlink() {
                    // Handle symlinks - always record them as files, but only follow if configured
                    debug!("Processing symlink: {:?}", path);
                    match retry_scan_io(context, || fstat::stat_file(&path)) {
                        Ok((file_meta, _)) => {
                            if let (Some(filters), Some(ref lp)) =
                                (path_filters, logical_path.as_ref())
                            {
                                if !filters.should_emit_file(lp) {
                                    continue;
                                }
                            }
                            let file_size = file_meta.size;
                            dir_result.files.push(file_meta);
                            stats.add_file_size(file_size);
                            stats.inc_files();

                            // Only follow symlink if it's a directory and follow_symlinks is enabled
                            if scan_option.meta_option.follow_symlinks {
                                if let Ok(target_meta) = std::fs::metadata(&path) {
                                    if target_meta.is_dir() {
                                        if let (Some(filters), Some(ref lp)) =
                                            (path_filters, logical_path.as_ref())
                                        {
                                            if !filters.should_descend_dir(lp) {
                                                continue;
                                            }
                                        }
                                        debug!("Following symlink to directory: {:?}", path);
                                        if let Err(e) =
                                            dirent_queue.push(DirScanEntry::new(path, depth + 1))
                                        {
                                            error!("Failed to push directory to queue: {:?}", e);
                                            stats.inc_failed_dirs();
                                        } else {
                                            stats.inc_dirs();
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            // Broken symlinks will fail here, but we should still record them
                            error!("Failed to stat symlink {:?}: {} (may be broken)", path, e);
                            // Try to get basic info for broken symlinks
                            match fstat::stat_file(&path) {
                                Ok(file_meta) => {
                                    dir_result.files.push(file_meta);
                                    stats.inc_files();
                                }
                                Err(_) => {
                                    stats.inc_failed_files();
                                }
                            }
                            record_scan_failure(
                                context,
                                "stat_symlink",
                                FailureItemType::Symlink,
                                &path,
                                e.to_string(),
                                context.scan_option.retry_policy.max_retries + 1,
                            );
                        }
                    }
                } else if file_type.is_dir() {
                    if let (Some(filters), Some(ref lp)) = (path_filters, logical_path.as_ref()) {
                        if !filters.should_descend_dir(lp) {
                            debug!("Skipping filtered subdirectory: {:?}", path);
                            continue;
                        }
                    }
                    // Enqueue subdirectory for recursive scanning
                    debug!("Enqueuing subdirectory: {:?}", path);
                    if let Err(e) = dirent_queue.push(DirScanEntry::new(path, depth + 1)) {
                        error!("Failed to push directory to queue: {:?}", e);
                        stats.inc_failed_dirs();
                        record_scan_failure(
                            context,
                            "enqueue_dir",
                            FailureItemType::Directory,
                            &dir_entry.path,
                            e.to_string(),
                            1,
                        );
                    } else {
                        stats.inc_dirs();
                    }
                } else if file_type.is_file() {
                    if let (Some(filters), Some(ref lp)) = (path_filters, logical_path.as_ref()) {
                        if !filters.should_emit_file(lp) {
                            debug!("Skipping filtered file: {:?}", path);
                            continue;
                        }
                    }
                    // Process regular file
                    debug!("Processing file: {:?}", path);
                    match retry_scan_io(context, || fstat::stat_file(&path)) {
                        Ok((file_meta, _)) => {
                            let file_size = file_meta.size;
                            dir_result.files.push(file_meta);
                            stats.add_file_size(file_size);
                            stats.inc_files();
                        }
                        Err(e) => {
                            error!("Failed to stat file {:?}: {}", path, e);
                            stats.inc_failed_files();
                            record_scan_failure(
                                context,
                                "stat_file",
                                FailureItemType::File,
                                &path,
                                e.to_string(),
                                context.scan_option.retry_policy.max_retries + 1,
                            );
                        }
                    }
                } else {
                    if let (Some(filters), Some(ref lp)) = (path_filters, logical_path.as_ref()) {
                        if !filters.should_emit_file(lp) {
                            debug!("Skipping filtered special entry: {:?}", path);
                            continue;
                        }
                    }
                    // Handle special files (block devices, character devices, FIFOs, sockets)
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::FileTypeExt;
                        if file_type.is_block_device() {
                            if scan_option.meta_option.skip_block_devices {
                                debug!("Skipping block device: {:?}", path);
                                continue;
                            }
                        }
                    }

                    // For other special files, try to stat them but don't fail if unsupported
                    debug!(
                        "Processing special file: {:?} (type: {:?})",
                        path, file_type
                    );
                    match retry_scan_io(context, || fstat::stat_file(&path)) {
                        Ok((file_meta, _)) => {
                            let file_size = file_meta.size;
                            dir_result.files.push(file_meta);
                            stats.add_file_size(file_size);
                            stats.inc_files();
                        }
                        Err(e) => {
                            warn!("Failed to stat special file {:?}: {} (skipping)", path, e);
                            stats.inc_failed_files();
                            record_scan_failure(
                                context,
                                "stat_special",
                                FailureItemType::Special,
                                &path,
                                e.to_string(),
                                context.scan_option.retry_policy.max_retries + 1,
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            error!("Failed to open directory {:?}: {}", dir_entry.path, e);
            stats.inc_failed_dirs();
            record_scan_failure(
                context,
                "open_dir",
                FailureItemType::Directory,
                &dir_entry.path,
                e.to_string(),
                context.scan_option.retry_policy.max_retries + 1,
            );
            // Still push an empty result to avoid losing the directory
        }
    }

    if let (Some(filters), Some(logical_path)) = (path_filters, current_logical_path.as_deref()) {
        if !filters.should_emit_dir(logical_path) && dir_result.files.is_empty() {
            debug!("Dropping filtered directory batch: {:?}", dir_entry.path);
            return;
        }
    }

    dir_result.complete = true;
    output_queue.push(dir_result);
}

/// Spawns a pool of worker threads to perform parallel directory scanning.
///
/// Each worker continuously:
/// 1. Pops a directory from the shared input queue.
/// 2. Processes it via [`process_dir_entry`].
/// 3. Sleeps briefly if the queue is empty.
/// 4. Exits only when the queue is empty **and** no other workers are active.
///
/// Returns a vector of `JoinHandle`s to allow the caller to wait for completion.
pub fn start_workers(
    context: &ScanWorkerContext,
    workers_count: usize,
) -> Vec<thread::JoinHandle<()>> {
    let mut worker_handles = Vec::with_capacity(workers_count);
    let active_workers = Arc::new(AtomicI32::new(0));

    for i in 0..workers_count {
        let active_workers = Arc::clone(&active_workers);
        let context = context.clone(); // Assumes ScanWorkerContext implements Clone

        let handle = thread::spawn(move || {
            debug!("Worker thread {} started", i);
            let dirent_queue = &context.dirent_queue;

            loop {
                match dirent_queue.pop() {
                    Ok(Some(dir_entry)) => {
                        debug!("Worker {} processing: {:?}", i, dir_entry.path);
                        active_workers.fetch_add(1, Ordering::SeqCst);
                        process_dir_entry(dir_entry, &context);
                        active_workers.fetch_sub(1, Ordering::SeqCst);
                    }
                    Ok(None) => {
                        // Queue is empty; check if we should terminate
                        thread::sleep(time::Duration::from_millis(100));
                        let queue_empty = dirent_queue.is_empty();
                        let no_active_workers = active_workers.load(Ordering::SeqCst) == 0;
                        if queue_empty && no_active_workers {
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Worker {} failed to pop from queue: {:?}", i, e);
                        // Depending on your queue impl, you might want to break or retry
                        thread::sleep(time::Duration::from_millis(100));
                    }
                }
            }
            debug!("Worker thread {} exited", i);
        });

        worker_handles.push(handle);
    }

    worker_handles
}
