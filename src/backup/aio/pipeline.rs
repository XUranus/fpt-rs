//! Generic async copy pipeline for remote-capable backup transports.
//!
//! Direction-specific modules only need to provide:
//! - the control-file path mapping
//! - a [`SourceReader`] implementation
//! - a [`TargetWriter`] implementation

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::aio::entry::{EntryMapping, produce_entries};
use crate::backup::aio::transport::{SourceReader, TargetWriter};
use crate::backup::fcb::ControlBlockVarient;
use crate::backup::stats::BackupStats;

pub async fn run_copy_pipeline<S, T>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    mapping: EntryMapping,
    source: S,
    target: T,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
    max_concurrent_tasks: usize,
) where
    S: SourceReader,
    T: TargetWriter,
{
    let task_sem = Arc::new(Semaphore::new(max_concurrent_tasks.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, mapping, entry_tx, log_prefix);
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let path = dcb.dst_path.clone();
                debug!("{log_prefix}: mkdir {:?}", path);

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    match target2.create_dir(path.clone()).await {
                        Ok(()) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("{log_prefix}: mkdir {:?}: {e}", path);
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
            ControlBlockVarient::FileControlBlock(fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!("{log_prefix}: skipping symlink {:?}", fcb.src_path);
                    continue;
                }

                let source2 = source.clone();
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();

                    let read_path = fcb.src_path.clone();
                    let write_path = fcb.dst_path.clone();

                    let fcb = match source2.read_file(fcb).await {
                        Ok(fcb) => fcb,
                        Err((fcb, msg)) => {
                            error!("{log_prefix}: read {:?}: {msg}", fcb.src_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                    };

                    match target2.write_file(fcb).await {
                        Ok(done_fcb) => {
                            debug!("{log_prefix}: copied {:?} -> {:?}", read_path, write_path);
                            stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                            stats2
                                .bytes_copied
                                .fetch_add(done_fcb.meta.size, Ordering::Relaxed);
                        }
                        Err((fcb, msg)) => {
                            error!("{log_prefix}: write {:?}: {msg}", fcb.dst_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
        }
    }

    if let Err(e) = producer_handle.await {
        error!("{log_prefix}: entry producer panicked: {e}");
    }

    for h in task_handles {
        if let Err(e) = h.await {
            error!("{log_prefix}: task panicked: {e}");
        }
    }

    if let Err(e) = source.finish().await {
        error!("{log_prefix}: source finalization failed: {e}");
    }
    if let Err(e) = target.finish().await {
        error!("{log_prefix}: target finalization failed: {e}");
    }

    info!(
        "{log_prefix}: complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}
