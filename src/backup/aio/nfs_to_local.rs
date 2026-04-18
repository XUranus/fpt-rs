//! AIO copy pipeline: NFS source → local filesystem target.
//!
//! [`run_aio_nfs_to_local_pipeline`] is the counterpart of
//! [`super::local_to_nfs::run_local_to_nfs_copy_pipeline`] for the case where data lives on an
//! NFS server and must be written to a local directory.
//!
//! ## Concurrency model
//!
//! - An entry producer runs inside `spawn_blocking` (synchronous I/O).
//! - Each file read is a Tokio task using [`nfs_read_task`].
//! - The local write is done inside `spawn_blocking` after the NFS read.
//! - A `Semaphore` caps concurrent in-flight read+write tasks.
//!
//! ```text
//! control file (blocking) → entry_tx
//!                                 │  mpsc::channel
//!                                 ▼
//!                    main loop: dirs → create_dir_all (blocking)
//!                               files → spawn(nfs_read + local_write) ─► BackupStats
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::fcb::{ControlBlockVarient, FileControlBlock};
use crate::backup::stats::BackupStats;
use crate::backup::aio::entry::{EntryMapping, produce_entries};
use crate::backup::aio::local_fs::write_local_file;
use crate::nfs::aio::reader::{FileHandleCache, new_file_handle_cache, nfs_read_task};
use crate::nfs::connection::NfsConnectionPool;

/// Maximum number of concurrent NFS-read+local-write tasks in flight.
const MAX_CONCURRENT_TASKS: usize = 16;

/// Run the NFS-source → local-target copy pipeline.
///
/// Reads `control_file`, resolves metadata from `meta_dir`, and for each entry:
/// - **Directories** — created on the local target via `std::fs::create_dir_all`.
/// - **Files** — read from the NFS server via [`nfs_read_task`], then written
///   locally via `std::fs`.
///
/// `nfs_source_base` is the full absolute path that control-file entries are
/// recorded under (e.g. `/opt/dataset/ds2`).  It is stripped from entry paths
/// to produce both relative local target paths and NFS-relative src paths
/// (for LOOKUP RPCs from the pool's effective root_fh, which already points
/// to the sub_path directory).
pub async fn run_aio_nfs_to_local_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    local_target_base: PathBuf,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let root_fh = pool.root_fh();
    let read_chunk = pool.server_rtmax;

    let dir_cache: FileHandleCache = new_file_handle_cache();
    let task_sem = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));

    // Channel: blocking entry producer → async consumer.
    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        let mapping = EntryMapping::remote_to_local(nfs_source_base.clone());
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, mapping, entry_tx, "NFS→local entry producer");
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                // Create the directory on the local target.
                let rel_path = dcb.dst_path.clone();
                let target_dir = local_target_base.join(&rel_path);
                debug!("NFS→local: create_dir {:?}", target_dir);
                let stats2 = Arc::clone(&stats);

                let h = tokio::task::spawn_blocking(move || {
                    match std::fs::create_dir_all(&target_dir) {
                        Ok(_) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("NFS→local: mkdir {:?}: {e}", target_dir);
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }

            ControlBlockVarient::FileControlBlock(fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!(
                        "NFS→local: skipping symlink {:?} (not yet implemented)",
                        fcb.src_path
                    );
                    continue;
                }

                let pool2 = Arc::clone(&pool);
                let dir_cache2 = Arc::clone(&dir_cache);
                let root_fh2 = root_fh.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let local_target_base2 = local_target_base.clone();

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();

                    // Read file from NFS.
                    let read_result =
                        nfs_read_task(fcb, pool2, dir_cache2, root_fh2, read_chunk).await;

                    use crate::nfs::aio::reader::NfsReaderResult;
                    match read_result {
                        NfsReaderResult::Read(fcb) => {
                            let dst_path = local_target_base2.join(&fcb.dst_path);
                            let buf = fcb.buffer.clone();
                            let file_size = fcb.meta.size;
                            debug!("NFS→local: read {:?} -> write {:?} size={}", fcb.src_path, dst_path, file_size);

                            // Write to local filesystem in a blocking task.
                            let write_result = tokio::task::spawn_blocking(move || {
                                write_local_file(&dst_path, &buf)
                            })
                            .await
                            .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")));

                            match write_result {
                                Ok(()) => {
                                    stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                                    stats2.bytes_copied.fetch_add(file_size, Ordering::Relaxed);
                                }
                                Err(msg) => {
                                    error!("NFS→local: write {:?}: {msg}", fcb.dst_path);
                                    stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        NfsReaderResult::Failed(fcb, msg) => {
                            error!("NFS→local: read {:?}: {msg}", fcb.src_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
        }
    }

    if let Err(e) = producer_handle.await {
        error!("NFS→local: entry producer panicked: {e}");
    }

    for h in task_handles {
        if let Err(e) = h.await {
            error!("NFS→local: task panicked: {e}");
        }
    }

    info!(
        "NFS→local pipeline complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}
