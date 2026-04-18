//! AIO copy pipeline for NFS backup/restore.
//!
//! [`run_aio_copy_pipeline`] is the async counterpart of the BIO
//! `spawn_file_entry_producer` + `spawn_reader` + `spawn_writer` thread
//! pipeline.  It reads the same control file format, processes each FCB through
//! a local-read → NFS-write path, and updates the same [`BackupStats`] counters.
//!
//! ## Concurrency model
//!
//! - An entry producer runs inside `spawn_blocking` (it does synchronous I/O).
//! - Each file's write is a `tokio::spawn` task bounded by `write_sem`.
//! - Directory creation is also async via the NFS connection pool.
//! - A `Semaphore` with `MAX_CONCURRENT_WRITE_TASKS` permits caps parallelism.
//!
//! ```text
//! control file (blocking) → entry_tx
//!                                 │  mpsc::channel
//!                                 ▼
//!                        main loop: dirs → get_or_create_dir
//!                                   files → spawn(nfs_write_task) ─► BackupStats
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::fcb::{
    ControlBlockVarient, DirControlBlock, FileControlBlock, SourceHandleState,
};
use crate::backup::stats::BackupStats;
use crate::nfs::aio::reader::new_file_handle_cache;
use crate::nfs::aio::writer::{
    DirHandleCache, NfsWriterResult, get_or_create_dir, new_dir_handle_cache, nfs_write_task,
};
use crate::nfs::connection::NfsConnectionPool;
use crate::scanner::metadata::{ControlEntry, ControlFileReader, MetaRepoReader};

/// Maximum number of concurrent NFS write tasks in flight.
const MAX_CONCURRENT_WRITE_TASKS: usize = 16;

/// Run the full AIO copy pipeline for an NFS target.
///
/// Reads `control_file`, resolves metadata from `meta_dir`, and for each entry:
/// - Directories: created on the NFS target via `get_or_create_dir`.
/// - Files: read from `source_dir_base` (local FS) then written to the NFS
///   target via [`nfs_write_task`].
///
/// Blocks (via Tokio's async machinery) until all entries are processed.
pub async fn run_aio_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let root_fh = pool.root_fh();
    let write_chunk = pool.server_wtmax;

    let dir_cache: DirHandleCache = new_dir_handle_cache();
    let write_sem = Arc::new(Semaphore::new(MAX_CONCURRENT_WRITE_TASKS));

    // Channel: blocking entry producer → async consumer.
    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);

    // Spawn entry producer in a blocking thread.
    let producer_handle = {
        let entry_tx = entry_tx.clone();
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, source_dir_base, entry_tx);
        })
    };
    // Drop our clone so the channel closes when the producer is done.
    drop(entry_tx);

    let mut write_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                let dir_path = dcb.dst_path.to_string_lossy().into_owned();
                debug!("AIO copy: mkdir {dir_path}");
                let pool2 = Arc::clone(&pool);
                let dir_cache2 = Arc::clone(&dir_cache);
                let root_fh2 = root_fh.clone();
                let stats2 = Arc::clone(&stats);

                let h = tokio::spawn(async move {
                    match get_or_create_dir(&pool2, &dir_cache2, &dir_path, &root_fh2).await {
                        Ok(_) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("AIO copy: mkdir {dir_path}: {e}");
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                write_handles.push(h);
            }

            ControlBlockVarient::FileControlBlock(mut fcb) => {
                // Symlinks: write via symlink RPC (TODO: implement NFS symlink).
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!(
                        "AIO copy: skipping symlink {:?} (NFS symlink not yet implemented)",
                        fcb.src_path
                    );
                    continue;
                }

                // Read the source file from local FS in a blocking task.
                let src_path = fcb.src_path.clone();
                let dst_path = fcb.dst_path.clone();
                let meta_size = fcb.meta.size;
                debug!("AIO copy: file src={src_path:?} dst={dst_path:?} size={meta_size}");
                let read_result: Result<Vec<u8>, String> =
                    tokio::task::spawn_blocking(move || read_local_file(&src_path, meta_size))
                        .await
                        .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")));

                match read_result {
                    Ok(buf) => {
                        let buf_len = buf.len();
                        fcb.buffer = buf;
                        fcb.buffer_len = buf_len;
                        fcb.src_state = SourceHandleState::Read;
                    }
                    Err(msg) => {
                        error!("AIO copy: read {:?}: {msg}", fcb.src_path);
                        stats.files_failed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                }

                // Spawn NFS write task with backpressure via semaphore.
                let pool2 = Arc::clone(&pool);
                let dir_cache2 = Arc::clone(&dir_cache);
                let root_fh2 = root_fh.clone();
                let stats2 = Arc::clone(&stats);
                let write_sem2 = Arc::clone(&write_sem);

                let h = tokio::spawn(async move {
                    let _permit = write_sem2.acquire_owned().await.unwrap();
                    match nfs_write_task(fcb, pool2, dir_cache2, root_fh2, write_chunk).await {
                        NfsWriterResult::Written(done_fcb) => {
                            stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                            stats2.bytes_copied.fetch_add(done_fcb.meta.size, Ordering::Relaxed);
                        }
                        NfsWriterResult::Failed(done_fcb, msg) => {
                            error!("AIO copy: write {:?}: {msg}", done_fcb.dst_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                write_handles.push(h);
            }
        }
    }

    // Wait for the entry producer.
    if let Err(e) = producer_handle.await {
        error!("AIO copy: entry producer panicked: {e}");
    }

    // Wait for all write tasks.
    for h in write_handles {
        if let Err(e) = h.await {
            error!("AIO copy: write task panicked: {e}");
        }
    }

    info!(
        "AIO copy pipeline complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read the entire content of a local source file into a `Vec<u8>`.
/// Called from `spawn_blocking` to avoid blocking the Tokio executor.
fn read_local_file(path: &PathBuf, expected_size: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("open {path:?}: {e}"))?;
    let cap = (expected_size as usize).min(64 * 1024 * 1024); // 64 MiB cap
    let mut buf = Vec::with_capacity(cap);
    file.read_to_end(&mut buf)
        .map_err(|e| format!("read {path:?}: {e}"))?;
    Ok(buf)
}

/// Produce `ControlBlockVarient` items from a control file and send them on `tx`.
/// This is a blocking function run via `spawn_blocking`.
fn produce_entries(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    tx: mpsc::Sender<ControlBlockVarient>,
) {
    let meta_repo = match MetaRepoReader::new(meta_dir) {
        Ok(r) => r,
        Err(e) => {
            error!("AIO entry producer: cannot open meta repo: {e}");
            return;
        }
    };

    let reader = match ControlFileReader::open(control_file) {
        Ok(r) => r,
        Err(e) => {
            error!("AIO entry producer: cannot open control file: {e}");
            return;
        }
    };

    let mut dirpath = PathBuf::new();

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                error!("AIO entry producer: read error: {e}");
                continue;
            }
        };

        let item = match entry {
            ControlEntry::Dir(dentry) => {
                let dmeta = match meta_repo.get_dmeta((dentry.meta_fid, dentry.meta_offset)) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("AIO entry producer: get_dmeta error: {e}");
                        continue;
                    }
                };
                let mut dcb = DirControlBlock::from(dmeta);
                dcb.src_path = PathBuf::from(&dentry.path);
                dcb.dst_path = make_relative(&source_dir_base, &dentry.path);
                dirpath = PathBuf::from(dentry.path);
                ControlBlockVarient::DirControlBlock(dcb)
            }
            ControlEntry::File(fentry) => {
                let fmeta = match meta_repo.get_fmeta((fentry.meta_fid, fentry.meta_offset)) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("AIO entry producer: get_fmeta error: {e}");
                        continue;
                    }
                };
                let mut fcb = FileControlBlock::from(fmeta);
                fcb.src_path = dirpath.join(&fentry.name);
                fcb.dst_path =
                    make_relative(&source_dir_base, &dirpath.to_string_lossy()).join(&fentry.name);
                ControlBlockVarient::FileControlBlock(fcb)
            }
        };

        if tx.blocking_send(item).is_err() {
            break; // Receiver dropped; pipeline cancelled.
        }
    }

    info!("AIO entry producer: done");
}

/// Strip the `base` prefix from `path` and return a relative `PathBuf`.
fn make_relative(base: &PathBuf, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if let Ok(rel) = p.strip_prefix(base) {
        rel.to_path_buf()
    } else if p.is_absolute() {
        p.file_name().map(PathBuf::from).unwrap_or(p)
    } else {
        p
    }
}
