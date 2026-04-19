//! AIO copy pipeline: SMB source -> SMB target.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::aio::entry::{EntryMapping, produce_entries};
use crate::backup::stats::BackupStats;
use crate::backup::fcb::ControlBlockVarient;
use crate::smb::SmbLocation;

const MAX_CONCURRENT_TASKS: usize = 16;

pub async fn run_smb_to_smb_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    target_prefix: String,
    source_location: SmbLocation,
    target_location: SmbLocation,
    source_client: Arc<smb_client::Client>,
    target_client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
) {
    let dir_cache = crate::smb::aio::new_dir_cache();
    let task_sem = Arc::new(Semaphore::new(MAX_CONCURRENT_TASKS));

    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);
    let producer_handle = {
        let entry_tx = entry_tx.clone();
        let mapping = EntryMapping::remote_to_prefixed_target(
            smb_source_base.clone(),
            PathBuf::from(target_prefix.clone()),
        );
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, mapping, entry_tx, "SMB->SMB entry producer");
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                let rel_dir = dcb.dst_path.to_string_lossy().replace('\\', "/");
                debug!("SMB->SMB: mkdir {rel_dir}");
                let target_client2 = Arc::clone(&target_client);
                let target_location2 = target_location.clone();
                let dir_cache2 = Arc::clone(&dir_cache);
                let stats2 = Arc::clone(&stats);

                let h = tokio::spawn(async move {
                    match crate::smb::aio::ensure_relative_directory(
                        &target_client2,
                        &target_location2,
                        &dir_cache2,
                        &rel_dir,
                    )
                    .await
                    {
                        Ok(()) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("SMB->SMB: mkdir {rel_dir}: {e}");
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
            ControlBlockVarient::FileControlBlock(fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!("SMB->SMB: skipping symlink {:?}", fcb.src_path);
                    continue;
                }

                let source_client2 = Arc::clone(&source_client);
                let source_location2 = source_location.clone();
                let target_client2 = Arc::clone(&target_client);
                let target_location2 = target_location.clone();
                let dir_cache2 = Arc::clone(&dir_cache);
                let source_base2 = smb_source_base.clone();
                let task_sem2 = Arc::clone(&task_sem);
                let stats2 = Arc::clone(&stats);

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();

                    let src_rel =
                        crate::smb::aio::relative_path_from_base(&source_base2, &fcb.src_path);
                    match crate::smb::aio::read_relative_file(
                        &source_client2,
                        &source_location2,
                        &src_rel,
                        fcb.meta.size,
                    )
                    .await
                    {
                        Ok(buf) => {
                            let rel_path = fcb.dst_path.to_string_lossy().replace('\\', "/");
                            match crate::smb::aio::write_relative_file(
                                &target_client2,
                                &target_location2,
                                &dir_cache2,
                                &rel_path,
                                &buf,
                            )
                            .await
                            {
                                Ok(()) => {
                                    stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                                    stats2.bytes_copied.fetch_add(fcb.meta.size, Ordering::Relaxed);
                                }
                                Err(msg) => {
                                    error!("SMB->SMB: write {:?}: {msg}", fcb.dst_path);
                                    stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(msg) => {
                            error!("SMB->SMB: read {:?}: {msg}", fcb.src_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                task_handles.push(h);
            }
        }
    }

    if let Err(e) = producer_handle.await {
        error!("SMB->SMB: entry producer panicked: {e}");
    }

    for h in task_handles {
        if let Err(e) = h.await {
            error!("SMB->SMB: task panicked: {e}");
        }
    }

    if let Err(e) = source_client.close().await {
        error!("SMB->SMB: source client close failed: {e}");
    }
    if let Err(e) = target_client.close().await {
        error!("SMB->SMB: target client close failed: {e}");
    }

    info!(
        "SMB->SMB pipeline complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}
