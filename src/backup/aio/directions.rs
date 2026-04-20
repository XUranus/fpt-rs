//! Thin direction wrappers over the generic async copy executor.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use log::{debug, error, info};
use tokio::sync::{mpsc, Semaphore};

use crate::backup::aggregate::AggregateConfig;
use crate::backup::aio::aggregation::AggregatingTarget;
use crate::backup::aio::entry::produce_entries;
use crate::backup::aio::entry::EntryMapping;
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
    let mapping =
        EntryMapping::local_to_prefixed_target(source_dir_base, PathBuf::from(target_prefix));
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
    let mapping =
        EntryMapping::local_to_prefixed_target(source_dir_base, PathBuf::from(target_prefix));
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
    let _ = nfs_source_base;
    let mapping = EntryMapping::remote_to_local();
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
    let _ = nfs_source_base;
    let mapping = EntryMapping::remote_to_prefixed_target(PathBuf::from(target_prefix));
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
    let _ = smb_source_base;
    let mapping = EntryMapping::remote_to_local();
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

    let _ = smb_source_base;
    let mapping = EntryMapping::remote_to_prefixed_target(PathBuf::from(target_prefix));
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
    let pipeline_started = Instant::now();
    let _ = smb_source_base;
    let mapping = EntryMapping::remote_to_prefixed_target(PathBuf::from(target_prefix));
    let dir_target = SmbTarget {
        location: target_location.clone(),
        pool: Arc::clone(&target_pool),
        dir_cache: crate::smb::aio::new_dir_cache(),
    };
    let task_sem = Arc::new(Semaphore::new(SMB_MAX_CONCURRENT_TASKS.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);
    let target_dir_cache = dir_target.dir_cache.clone();
    let copy_metrics = Arc::new(crate::smb::aio::SmbCopyMetrics::default());
    let mut dir_entries = 0u64;
    let mut file_entries = 0u64;
    let dispatch_started = Instant::now();
    let mut dir_paths = Vec::new();
    let mut file_jobs = Vec::new();

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, mapping, entry_tx, "SMB->SMB");
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                dir_entries += 1;
                dir_paths.push(dcb.dst_path);
            }
            ControlBlockVarient::FileControlBlock(fcb) => {
                file_entries += 1;
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!("SMB->SMB: skipping symlink {:?}", fcb.src_path);
                    continue;
                }

                let read_path = fcb.src_path.clone();
                let write_path = fcb.dst_path.clone();
                let src_rel = fcb.src_path.to_string_lossy().replace('\\', "/");
                let dst_rel = fcb.dst_path.to_string_lossy().replace('\\', "/");
                let file_size = fcb.meta.size;
                file_jobs.push((read_path, write_path, src_rel, dst_rel, file_size));
            }
        }
    }
    let dispatch_elapsed = dispatch_started.elapsed();

    let producer_wait_started = Instant::now();
    if let Err(e) = producer_handle.await {
        error!("SMB->SMB: entry producer panicked: {e}");
    }
    let producer_wait_elapsed = producer_wait_started.elapsed();

    let mkdir_started = Instant::now();
    for path in dir_paths {
        let target2 = dir_target.clone();
        let stats2 = Arc::clone(&stats);
        let task_sem2 = Arc::clone(&task_sem);
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
    for h in task_handles {
        if let Err(e) = h.await {
            error!("SMB->SMB: mkdir task panicked: {e}");
        }
    }
    let mkdir_elapsed = mkdir_started.elapsed();

    let copy_started = Instant::now();
    let mut task_handles = Vec::new();
    for (read_path, write_path, src_rel, dst_rel, file_size) in file_jobs {
        let source_pool2 = Arc::clone(&source_pool);
        let target_pool2 = Arc::clone(&target_pool);
        let source_location2 = source_location.clone();
        let target_location2 = target_location.clone();
        let target_dir_cache2 = target_dir_cache.clone();
        let stats2 = Arc::clone(&stats);
        let copy_metrics2 = Arc::clone(&copy_metrics);
        let task_sem2 = Arc::clone(&task_sem);

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
                false,
                Some(copy_metrics2),
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
    for h in task_handles {
        if let Err(e) = h.await {
            error!("SMB->SMB: copy task panicked: {e}");
        }
    }
    let copy_elapsed = copy_started.elapsed();

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
    info!(
        "SMB->SMB timing: total={}, dispatch={} for {} file entries and {} dir entries, producer_wait={}, mkdir_wait={}, copy_wait={}, copy_ops: {}",
        format_elapsed(pipeline_started.elapsed()),
        format_elapsed(dispatch_elapsed),
        file_entries,
        dir_entries,
        format_elapsed(producer_wait_elapsed),
        format_elapsed(mkdir_elapsed),
        format_elapsed(copy_elapsed),
        copy_metrics.timing_summary(),
    );
}

#[cfg(feature = "smb")]
fn format_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    if secs >= 60 {
        format!("{}m {}.{:03}s", secs / 60, secs % 60, millis)
    } else {
        format!("{}.{:03}s", secs, millis)
    }
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
    let _ = nfs_source_base;
    let mapping = EntryMapping::remote_to_prefixed_target(PathBuf::from(target_prefix));
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
    let _ = smb_source_base;
    let mapping = EntryMapping::remote_to_prefixed_target(PathBuf::from(target_prefix));
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
