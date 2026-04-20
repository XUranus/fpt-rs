//! Thin direction wrappers over the generic async copy executor.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use log::{debug, error, info};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::aggregate::AggregateConfig;
use crate::backup::aio::aggregation::AggregatingTarget;
use crate::backup::aio::entry::EntryMapping;
use crate::backup::aio::entry::produce_entries;
use crate::backup::aio::pipeline::run_copy_pipeline;
use crate::backup::aio::transport::{LocalSource, LocalTarget, TargetWriter};
use crate::backup::fcb::ControlBlockVarient;
use crate::backup::stats::BackupStats;

#[cfg(feature = "nfs")]
use crate::backup::aio::transport::{NfsSource, NfsTarget};
#[cfg(feature = "nfs")]
use crate::nfs::aio::{reader::new_file_handle_cache, writer::new_dir_handle_cache};
#[cfg(feature = "nfs")]
use crate::nfs::connection::NfsConnectionPool;

#[cfg(feature = "smb")]
use crate::backup::aio::transport::{SmbSource, SmbTarget};
#[cfg(feature = "smb")]
use crate::smb::SmbLocation;

#[cfg(feature = "smb")]
const SMB_MAX_CONCURRENT_TASKS: usize = 16;
#[cfg(feature = "nfs")]
const NFS_MAX_CONCURRENT_TASKS: usize = 16;

#[cfg(feature = "nfs")]
pub async fn run_local_to_nfs_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::local_to_prefixed_target(
        source_dir_base,
        PathBuf::from(target_prefix),
    );
    let target = NfsTarget {
        pool: Arc::clone(&pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: pool.root_fh(),
        write_chunk: pool.server_wtmax,
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        LocalSource,
        target,
        stats,
        "local->NFS",
        NFS_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "smb")]
pub async fn run_local_to_smb_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    location: SmbLocation,
    pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::local_to_prefixed_target(
        source_dir_base,
        PathBuf::from(target_prefix),
    );
    let target = SmbTarget {
        location,
        pool,
        dir_cache: crate::smb::aio::new_dir_cache(),
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        LocalSource,
        target,
        stats,
        "local->SMB",
        SMB_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "nfs")]
pub async fn run_aio_nfs_to_local_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    local_target_base: PathBuf,
    aggregate_config: AggregateConfig,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_local(nfs_source_base);
    let source = NfsSource {
        pool: Arc::clone(&pool),
        dir_cache: new_file_handle_cache(),
        root_fh: pool.root_fh(),
        read_chunk: pool.server_rtmax,
    };
    let target = LocalTarget {
        base: local_target_base,
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "NFS->local",
        NFS_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "nfs")]
pub async fn run_aio_nfs_to_nfs_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    source_pool: Arc<NfsConnectionPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_prefixed_target(
        nfs_source_base,
        PathBuf::from(target_prefix),
    );
    let source = NfsSource {
        pool: Arc::clone(&source_pool),
        dir_cache: new_file_handle_cache(),
        root_fh: source_pool.root_fh(),
        read_chunk: source_pool.server_rtmax,
    };
    let target = NfsTarget {
        pool: Arc::clone(&target_pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: target_pool.root_fh(),
        write_chunk: target_pool.server_wtmax,
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "NFS->NFS",
        NFS_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "smb")]
pub async fn run_smb_to_local_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    local_target_base: PathBuf,
    aggregate_config: AggregateConfig,
    location: SmbLocation,
    pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_local(smb_source_base);
    let source = SmbSource { location, pool };
    let target = LocalTarget {
        base: local_target_base,
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "SMB->local",
        SMB_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "smb")]
pub async fn run_smb_to_smb_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    source_location: SmbLocation,
    target_location: SmbLocation,
    source_pool: Arc<crate::smb::aio::SmbClientPool>,
    target_pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
) {
    if !aggregate_config.enabled {
        run_smb_to_smb_streaming_pipeline(
            control_file,
            meta_dir,
            smb_source_base,
            target_prefix,
            source_location,
            target_location,
            source_pool,
            target_pool,
            stats,
        )
        .await;
        return;
    }

    let mapping = EntryMapping::remote_to_prefixed_target(
        smb_source_base,
        PathBuf::from(target_prefix),
    );
    let source = SmbSource {
        location: source_location,
        pool: source_pool,
    };
    let target = SmbTarget {
        location: target_location,
        pool: target_pool,
        dir_cache: crate::smb::aio::new_dir_cache(),
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "SMB->SMB",
        SMB_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "smb")]
async fn run_smb_to_smb_streaming_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    target_prefix: String,
    source_location: SmbLocation,
    target_location: SmbLocation,
    source_pool: Arc<crate::smb::aio::SmbClientPool>,
    target_pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_prefixed_target(
        smb_source_base,
        PathBuf::from(target_prefix),
    );
    let dir_target = SmbTarget {
        location: target_location.clone(),
        pool: Arc::clone(&target_pool),
        dir_cache: crate::smb::aio::new_dir_cache(),
    };
    let task_sem = Arc::new(Semaphore::new(SMB_MAX_CONCURRENT_TASKS.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);
    let target_dir_cache = dir_target.dir_cache.clone();

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        tokio::task::spawn_blocking(move || {
            produce_entries(
                control_file,
                meta_dir,
                mapping,
                entry_tx,
                "SMB->SMB",
            );
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                let target2 = dir_target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let path = dcb.dst_path.clone();
                debug!("SMB->SMB: mkdir {:?}", path);

                task_handles.push(tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    match target2.create_dir(path.clone()).await {
                        Ok(()) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("SMB->SMB: mkdir {:?}: {e}", path);
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }));
            }
            ControlBlockVarient::FileControlBlock(fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!("SMB->SMB: skipping symlink {:?}", fcb.src_path);
                    continue;
                }

                let source_pool2 = Arc::clone(&source_pool);
                let target_pool2 = Arc::clone(&target_pool);
                let source_location2 = source_location.clone();
                let target_location2 = target_location.clone();
                let target_dir_cache2 = target_dir_cache.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let read_path = fcb.src_path.clone();
                let write_path = fcb.dst_path.clone();
                let src_rel = fcb.src_path.to_string_lossy().replace('\\', "/");
                let dst_rel = fcb.dst_path.to_string_lossy().replace('\\', "/");
                let file_size = fcb.meta.size;

                task_handles.push(tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    match crate::smb::aio::copy_relative_file_streaming(
                        &source_pool2,
                        &source_location2,
                        &src_rel,
                        &target_pool2,
                        &target_location2,
                        &target_dir_cache2,
                        &dst_rel,
                    )
                    .await
                    {
                        Ok(()) => {
                            debug!("SMB->SMB: copied {:?} -> {:?}", read_path, write_path);
                            stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                            stats2.bytes_copied.fetch_add(file_size, Ordering::Relaxed);
                        }
                        Err(msg) => {
                            error!("SMB->SMB: write {:?}: {msg}", write_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }));
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

    if let Err(e) = source_pool.close().await {
        error!("SMB->SMB: source finalization failed: {e}");
    }
    if let Err(e) = target_pool.close().await {
        error!("SMB->SMB: target finalization failed: {e}");
    }

    info!(
        "SMB->SMB: complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}

#[cfg(all(feature = "nfs", feature = "smb"))]
pub async fn run_nfs_to_smb_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    source_pool: Arc<NfsConnectionPool>,
    target_location: SmbLocation,
    target_pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_prefixed_target(
        nfs_source_base,
        PathBuf::from(target_prefix),
    );
    let source = NfsSource {
        pool: Arc::clone(&source_pool),
        dir_cache: new_file_handle_cache(),
        root_fh: source_pool.root_fh(),
        read_chunk: source_pool.server_rtmax,
    };
    let target = SmbTarget {
        location: target_location,
        pool: target_pool,
        dir_cache: crate::smb::aio::new_dir_cache(),
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "NFS->SMB",
        NFS_MAX_CONCURRENT_TASKS.min(SMB_MAX_CONCURRENT_TASKS),
    )
    .await;
}

#[cfg(all(feature = "nfs", feature = "smb"))]
pub async fn run_smb_to_nfs_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    source_location: SmbLocation,
    source_pool: Arc<crate::smb::aio::SmbClientPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_prefixed_target(
        smb_source_base,
        PathBuf::from(target_prefix),
    );
    let source = SmbSource {
        location: source_location,
        pool: source_pool,
    };
    let target = NfsTarget {
        pool: Arc::clone(&target_pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: target_pool.root_fh(),
        write_chunk: target_pool.server_wtmax,
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "SMB->NFS",
        NFS_MAX_CONCURRENT_TASKS.min(SMB_MAX_CONCURRENT_TASKS),
    )
    .await;
}
