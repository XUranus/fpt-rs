//! AIO copy pipeline: SMB source -> local filesystem target.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::aio::entry::{EntryMapping, produce_entries};
use crate::backup::aio::local_fs::write_local_file;
use crate::backup::fcb::ControlBlockVarient;
use crate::backup::stats::BackupStats;
use crate::smb::SmbLocation;

const MAX_CONCURRENT_TASKS: usize = 16;

pub async fn run_smb_to_local_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    local_target_base: PathBuf,
    location: SmbLocation,
    client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
) {
    let task_sem = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));

    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);
    let producer_handle = {
        let entry_tx = entry_tx.clone();
        let mapping = EntryMapping::remote_to_local(smb_source_base.clone());
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, mapping, entry_tx, "SMB->local entry producer");
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                let target_dir = local_target_base.join(&dcb.dst_path);
                debug!("SMB->local: create_dir {:?}", target_dir);
                let stats2 = Arc::clone(&stats);
                let h = tokio::task::spawn_blocking(move || {
                    match std::fs::create_dir_all(&target_dir) {
                        Ok(_) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("SMB->local: mkdir {:?}: {e}", target_dir);
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
            ControlBlockVarient::FileControlBlock(fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!("SMB->local: skipping symlink {:?}", fcb.src_path);
                    continue;
                }

                let client2 = Arc::clone(&client);
                let location2 = location.clone();
                let source_base2 = smb_source_base.clone();
                let local_target_base2 = local_target_base.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();

                    let src_rel =
                        crate::smb::aio::relative_path_from_base(&source_base2, &fcb.src_path);
                    let read_result = crate::smb::aio::read_relative_file(
                        &client2,
                        &location2,
                        &src_rel,
                        fcb.meta.size,
                    )
                    .await;

                    match read_result {
                        Ok(buf) => {
                            let dst_path = local_target_base2.join(&fcb.dst_path);
                            let file_size = fcb.meta.size;
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
                                    error!("SMB->local: write {:?}: {msg}", fcb.dst_path);
                                    stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(msg) => {
                            error!("SMB->local: read {:?}: {msg}", fcb.src_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
        }
    }

    if let Err(e) = producer_handle.await {
        error!("SMB->local: entry producer panicked: {e}");
    }

    for h in task_handles {
        if let Err(e) = h.await {
            error!("SMB->local: task panicked: {e}");
        }
    }

    if let Err(e) = client.close().await {
        error!("SMB->local: client close failed: {e}");
    }

    info!(
        "SMB->local pipeline complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}
