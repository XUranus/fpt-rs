//! AIO copy pipeline: NFS source → NFS target.
//!
//! [`run_aio_nfs_to_nfs_pipeline`] reads data from an NFS source server
//! and writes it directly to an NFS target server, bypassing local staging
//! for D_REPO data.  M_REPO and C_REPO are still written locally during
//! the scan phase and uploaded in the post-job phase.
//!
//! ## Concurrency model
//!
//! - An entry producer runs inside `spawn_blocking` (synchronous I/O).
//! - Each file is read from NFS source then written to NFS target via
//!   separate Tokio tasks using independent connection pools.
//! - A `Semaphore` caps concurrent in-flight read+write tasks.
//!
//! ```text
//! control file (blocking) → entry_tx
//!                                 │  mpsc::channel
//!                                 ▼
//!                    main loop: dirs → mkdir on NFS target
//!                               files → NFS read → NFS write ─► BackupStats
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::fcb::{ControlBlockVarient, DirControlBlock, FileControlBlock};
use crate::backup::stats::BackupStats;
use crate::nfs::aio::reader::{FileHandleCache, new_file_handle_cache, nfs_read_task};
use crate::nfs::aio::writer::{
    DirHandleCache, NfsWriterResult, get_or_create_dir, new_dir_handle_cache, nfs_write_task,
};
use crate::nfs::connection::NfsConnectionPool;
use crate::nfs::NfsLocation;
use crate::scanner::metadata::{ControlEntry, ControlFileReader, MetaRepoReader};

/// Maximum number of concurrent NFS read+write tasks in flight.
const MAX_CONCURRENT_TASKS: usize = 16;

/// Run the NFS source → NFS target copy pipeline.
///
/// Reads `control_file`, resolves metadata from `meta_dir`, and for each entry:
/// - **Directories** — created on the NFS target via `get_or_create_dir`.
/// - **Files** — read from the NFS source server via [`nfs_read_task`], then
///   written to the NFS target server via [`nfs_write_task`].
///
/// `nfs_source_base` is the full absolute path that control-file entries are
/// recorded under (e.g. `/opt/dataset/ds2`).  It is stripped from entry paths
/// to produce relative paths used for NFS source reads.
///
/// `target_prefix` is the path within the NFS target's sub_path where D_REPO
/// data should be written (e.g. `COPY_COMMON_FULL_xxx/D_REPO`). It is prepended
/// to each `dst_path` so files are created under the correct copy structure.
pub async fn run_aio_nfs_to_nfs_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    target_prefix: String,
    source_pool: Arc<NfsConnectionPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let src_root_fh = source_pool.root_fh();
    let src_read_chunk = source_pool.server_rtmax;

    let tgt_root_fh = target_pool.root_fh();
    let tgt_write_chunk = target_pool.server_wtmax;

    let src_dir_cache: FileHandleCache = new_file_handle_cache();
    let tgt_dir_cache: DirHandleCache = new_dir_handle_cache();
    let task_sem = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));

    // Channel: blocking entry producer → async consumer.
    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        let nfs_source_base2 = nfs_source_base.clone();
        let target_prefix2 = target_prefix.clone();
        tokio::task::spawn_blocking(move || {
            produce_entries(
                control_file,
                meta_dir,
                nfs_source_base2,
                &target_prefix2,
                entry_tx,
            );
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                // Create the directory on the NFS target.
                let dir_path = dcb.dst_path.to_string_lossy().into_owned();
                debug!("NFS→NFS: mkdir {dir_path}");
                let pool2 = Arc::clone(&target_pool);
                let dir_cache2 = Arc::clone(&tgt_dir_cache);
                let root_fh2 = tgt_root_fh.clone();
                let stats2 = Arc::clone(&stats);

                let h = tokio::spawn(async move {
                    match get_or_create_dir(&pool2, &dir_cache2, &dir_path, &root_fh2).await {
                        Ok(_) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("NFS→NFS: mkdir {dir_path}: {e}");
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }

            ControlBlockVarient::FileControlBlock(fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!(
                        "NFS→NFS: skipping symlink {:?} (not yet implemented)",
                        fcb.src_path
                    );
                    continue;
                }

                let src_pool2 = Arc::clone(&source_pool);
                let src_dir_cache2 = Arc::clone(&src_dir_cache);
                let src_root_fh2 = src_root_fh.clone();
                let tgt_pool2 = Arc::clone(&target_pool);
                let tgt_dir_cache2 = Arc::clone(&tgt_dir_cache);
                let tgt_root_fh2 = tgt_root_fh.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let src_read_chunk2 = src_read_chunk;
                let tgt_write_chunk2 = tgt_write_chunk;

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();

                    // Read file from NFS source.
                    let read_result = nfs_read_task(
                        fcb,
                        src_pool2,
                        src_dir_cache2,
                        src_root_fh2,
                        src_read_chunk2,
                    )
                    .await;

                    use crate::nfs::aio::reader::NfsReaderResult;
                    match read_result {
                        NfsReaderResult::Read(fcb) => {
                            // Write file to NFS target.
                            match nfs_write_task(
                                fcb,
                                tgt_pool2,
                                tgt_dir_cache2,
                                tgt_root_fh2,
                                tgt_write_chunk2,
                            )
                            .await
                            {
                                NfsWriterResult::Written(done_fcb) => {
                                    stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                                    stats2.bytes_copied.fetch_add(
                                        done_fcb.meta.size,
                                        Ordering::Relaxed,
                                    );
                                }
                                NfsWriterResult::Failed(done_fcb, msg) => {
                                    error!("NFS→NFS: write {:?}: {msg}", done_fcb.dst_path);
                                    stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        NfsReaderResult::Failed(fcb, msg) => {
                            error!("NFS→NFS: read {:?}: {msg}", fcb.src_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
        }
    }

    if let Err(e) = producer_handle.await {
        error!("NFS→NFS: entry producer panicked: {e}");
    }

    for h in task_handles {
        if let Err(e) = h.await {
            error!("NFS→NFS: task panicked: {e}");
        }
    }

    info!(
        "NFS→NFS pipeline complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Produce `ControlBlockVarient` items from a control file and send them on `tx`.
///
/// For NFS→NFS:
/// - `src_path` is made relative to `nfs_source_base` for NFS LOOKUP RPCs.
/// - `dst_path` is the relative path prepended with `target_prefix` so files
///   are written under the correct copy structure on the NFS target.
fn produce_entries(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    target_prefix: &str,
    tx: mpsc::Sender<ControlBlockVarient>,
) {
    let meta_repo = match MetaRepoReader::new(meta_dir) {
        Ok(r) => r,
        Err(e) => {
            error!("NFS→NFS entry producer: cannot open meta repo: {e}");
            return;
        }
    };

    let reader = match ControlFileReader::open(control_file) {
        Ok(r) => r,
        Err(e) => {
            error!("NFS→NFS entry producer: cannot open control file: {e}");
            return;
        }
    };

    let target_prefix_buf = PathBuf::from(target_prefix);
    let mut dirpath = PathBuf::new();
    let mut entry_count: usize = 0;

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                error!("NFS→NFS entry producer: read error: {e}");
                continue;
            }
        };

        let item = match entry {
            ControlEntry::Dir(dentry) => {
                let dmeta = match meta_repo.get_dmeta((dentry.meta_fid, dentry.meta_offset)) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("NFS→NFS entry producer: get_dmeta error: {e}");
                        continue;
                    }
                };
                let mut dcb = DirControlBlock::from(dmeta);
                let rel_path = make_relative(&nfs_source_base, &dentry.path);
                dcb.src_path = rel_path.clone();
                dcb.dst_path = target_prefix_buf.join(&rel_path);
                dirpath = PathBuf::from(dentry.path);
                ControlBlockVarient::DirControlBlock(dcb)
            }
            ControlEntry::File(fentry) => {
                let fmeta = match meta_repo.get_fmeta((fentry.meta_fid, fentry.meta_offset)) {
                    Ok(m) => m,
                    Err(e) => {
                        error!("NFS→NFS entry producer: get_fmeta error: {e}");
                        continue;
                    }
                };
                let mut fcb = FileControlBlock::from(fmeta);
                let abs_dir = PathBuf::from(&dirpath);
                let rel_dir = make_relative(&nfs_source_base, &abs_dir.to_string_lossy());
                fcb.src_path = rel_dir.join(&fentry.name);
                // Prepend target_prefix so files go under COPY_.../D_REPO/
                fcb.dst_path = target_prefix_buf.join(fcb.src_path.to_str().unwrap_or(""));
                ControlBlockVarient::FileControlBlock(fcb)
            }
        };

        if tx.blocking_send(item).is_err() {
            break;
        }
        entry_count += 1;
    }

    info!(
        "NFS→NFS entry producer: done, {entry_count} entries produced"
    );
}

/// Strip `base` prefix from `path` and return a relative `PathBuf`.
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
