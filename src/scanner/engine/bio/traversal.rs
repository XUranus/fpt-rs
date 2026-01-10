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

use std::{
    sync::{Arc, atomic::{AtomicI32, Ordering}},
    thread,
    time,
};
use std::fs;
use log::{debug, error};

use crate::{
    native::fstat,
    scanner::{
        ScanWorkerContext,
        models::{DirBatchScanResult, DirScanEntry},
    },
};

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

    // Stat the directory itself
    match fstat::stat_dir(&dir_entry.path) {
        Ok(dir_meta) => dir_result.dir = dir_meta,
        Err(e) => {
            error!("Failed to stat directory {:?}: {}", dir_entry.path, e);
            stats.inc_failed_dirs();
            return; // Skip scanning contents if we can't even stat the dir
        }
    }

    // Read and process directory entries
    match fs::read_dir(&dir_entry.path) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        error!("Failed to read directory entry in {:?}: {}", dir_entry.path, e);
                        stats.inc_failed_files(); // Treat as file error (conservative)
                        continue;
                    }
                };

                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(e) => {
                        error!("Failed to determine file type for {:?}: {}", path, e);
                        stats.inc_failed_files();
                        continue;
                    }
                };

                // Apply hidden file filter (Unix-style: leading dot)
                let is_hidden = path
                    .file_name()
                    .map(|name| name.to_string_lossy().starts_with('.'))
                    .unwrap_or(false);

                if !scan_option.meta_option.scan_hidden && is_hidden {
                    debug!("Skipping hidden entry: {:?}", path);
                    continue;
                }

                if file_type.is_dir() {
                    // Enqueue subdirectory for recursive scanning
                    debug!("Enqueuing subdirectory: {:?}", path);
                    if let Err(e) = dirent_queue.push(DirScanEntry::new(path, depth + 1)) {
                        error!("Failed to push directory to queue: {:?}", e);
                        stats.inc_failed_dirs();
                    } else {
                        stats.inc_dirs();
                    }
                } else {
                    // Process regular file (or symlink, device, etc.)
                    debug!("Processing file: {:?}", path);
                    match fstat::stat_file(&path) {
                        Ok(file_meta) => {
                            let file_size = file_meta.size;
                            dir_result.files.push(file_meta);
                            stats.add_file_size(file_size);
                            stats.inc_files();
                        }
                        Err(e) => {
                            error!("Failed to stat file {:?}: {}", path, e);
                            stats.inc_failed_files();
                        }
                    }
                }
            }
        }
        Err(e) => {
            error!("Failed to open directory {:?}: {}", dir_entry.path, e);
            stats.inc_failed_dirs();
            // Still push an empty result to avoid losing the directory
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