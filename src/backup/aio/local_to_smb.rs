//! AIO copy pipeline: local filesystem source -> SMB target.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::aio::entry::{EntryMapping, produce_entries};
use crate::backup::aio::local_fs::read_local_file;
use crate::backup::fcb::{ControlBlockVarient, SourceHandleState};
use crate::backup::stats::BackupStats;
use crate::smb::SmbLocation;

const MAX_CONCURRENT_WRITE_TASKS: usize = 16;

pub async fn run_local_to_smb_copy_pipeline(
    control_file: std::path::PathBuf,
    meta_dir: std::path::PathBuf,
    source_dir_base: std::path::PathBuf,
    target_prefix: String,
    location: SmbLocation,
    client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
) {
    let dir_cache = crate::smb::aio::new_dir_cache();
    let write_sem = Arc::new(Semaphore::new(MAX_CONCURRENT_WRITE_TASKS));

    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);
    let producer_handle = {
        let entry_tx = entry_tx.clone();
        let mapping = EntryMapping::local_to_prefixed_target(
            source_dir_base.clone(),
            std::path::PathBuf::from(target_prefix),
        );
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, mapping, entry_tx, "SMB AIO entry producer");
        })
    };
    drop(entry_tx);

    let mut write_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                let rel_dir = dcb.dst_path.to_string_lossy().replace('\\', "/");
                debug!("SMB AIO copy: mkdir {rel_dir}");
                let client2 = Arc::clone(&client);
                let location2 = location.clone();
                let dir_cache2 = Arc::clone(&dir_cache);
                let stats2 = Arc::clone(&stats);

                let h = tokio::spawn(async move {
                    match crate::smb::aio::ensure_relative_directory(
                        &client2,
                        &location2,
                        &dir_cache2,
                        &rel_dir,
                    )
                    .await
                    {
                        Ok(()) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("SMB AIO copy: mkdir {rel_dir}: {e}");
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                write_handles.push(h);
            }
            ControlBlockVarient::FileControlBlock(mut fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!(
                        "SMB AIO copy: skipping symlink {:?} (SMB symlink write not yet implemented)",
                        fcb.src_path
                    );
                    continue;
                }

                let src_path = fcb.src_path.clone();
                let dst_path = fcb.dst_path.clone();
                let meta_size = fcb.meta.size;
                debug!("SMB AIO copy: file src={src_path:?} dst={dst_path:?} size={meta_size}");
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
                        error!("SMB AIO copy: read {:?}: {msg}", fcb.src_path);
                        stats.files_failed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                }

                let rel_path = fcb.dst_path.to_string_lossy().replace('\\', "/");
                let client2 = Arc::clone(&client);
                let location2 = location.clone();
                let dir_cache2 = Arc::clone(&dir_cache);
                let stats2 = Arc::clone(&stats);
                let write_sem2 = Arc::clone(&write_sem);

                let h = tokio::spawn(async move {
                    let _permit = write_sem2.acquire_owned().await.unwrap();
                    match crate::smb::aio::write_relative_file(
                        &client2,
                        &location2,
                        &dir_cache2,
                        &rel_path,
                        &fcb.buffer,
                    )
                    .await
                    {
                        Ok(()) => {
                            stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                            stats2.bytes_copied.fetch_add(fcb.meta.size, Ordering::Relaxed);
                        }
                        Err(msg) => {
                            error!("SMB AIO copy: write {:?}: {msg}", dst_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
                write_handles.push(h);
            }
        }
    }

    if let Err(e) = producer_handle.await {
        error!("SMB AIO copy: entry producer panicked: {e}");
    }

    for h in write_handles {
        if let Err(e) = h.await {
            error!("SMB AIO copy: write task panicked: {e}");
        }
    }

    if let Err(e) = client.close().await {
        error!("SMB AIO copy: client close failed: {e}");
    }

    info!(
        "SMB AIO copy pipeline complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}
