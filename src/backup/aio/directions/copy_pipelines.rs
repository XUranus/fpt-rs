//! SMB direction copy pipeline functions.
//!
//! NFS direction pipelines have moved to [`crate::nfs::backup::pipeline`].

#[cfg(feature = "smb")]
use std::path::PathBuf;
#[cfg(feature = "smb")]
use std::sync::Arc;

#[cfg(feature = "smb")]
use crate::backup::aggregate::AggregateConfig;
#[cfg(feature = "smb")]
use crate::backup::aio::aggregation::AggregatingTarget;
#[cfg(feature = "smb")]
use crate::backup::aio::entry::EntryMapping;
#[cfg(feature = "smb")]
use crate::backup::aio::pipeline::run_copy_pipeline;
#[cfg(feature = "smb")]
use crate::backup::aio::transport::{clamp_copy_buffer_size, LocalSource, LocalTarget};
#[cfg(feature = "smb")]
use crate::backup::stats::BackupStats;
#[cfg(feature = "smb")]
use crate::failure::{FailureRecorder, RetryPolicy};

#[cfg(feature = "smb")]
use std::sync::atomic::Ordering;
#[cfg(feature = "smb")]
use std::time::Instant;
#[cfg(feature = "smb")]
use log::{debug, error, info};
#[cfg(feature = "smb")]
use tokio::sync::{mpsc, Semaphore};
#[cfg(feature = "smb")]
use crate::backup::aggregate::should_aggregate;
#[cfg(feature = "smb")]
use crate::backup::aio::transport::TargetWriter;
#[cfg(feature = "smb")]
use crate::backup::copy_plan::{produce_copy_plan, CopyPlanEntry, FileCopyPlan};
#[cfg(feature = "smb")]
use crate::failure::{FailureItemType, FailureRecord};

#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::backup::aio::transport::{NfsSource, NfsTarget};
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::nfs::aio::{reader::new_file_handle_cache, writer::new_dir_handle_cache};
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::nfs::connection::NfsConnectionPool;

#[cfg(feature = "smb")]
use crate::backup::aio::executor::execute_smb_source_file_plan;
#[cfg(feature = "smb")]
use crate::backup::aio::transport::SmbTarget;
#[cfg(feature = "smb")]
use crate::smb::SmbLocation;

#[cfg(feature = "smb")]
const SMB_MAX_CONCURRENT_TASKS: usize = 32;
#[cfg(feature = "smb")]
const SMB_TASKS_PER_CONNECTION: usize = 8;
#[cfg(all(feature = "nfs", feature = "smb"))]
const NFS_MAX_CONCURRENT_TASKS: usize = 16;

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
    copy_buffer_size: usize,
    smb_copy_task_count: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let max_concurrent_tasks = smb_copy_task_limit(pool.size(), smb_copy_task_count);
    let target_prefix = PathBuf::from(target_prefix);
    let mapping = EntryMapping::local_to_prefixed_target(source_dir_base, target_prefix.clone());
    let target = SmbTarget {
        location,
        pool,
        dir_cache: crate::smb::aio::new_dir_cache(),
        buffer_size: copy_buffer_size,
    };
    let target = AggregatingTarget::with_repo_prefix(target, aggregate_config, target_prefix);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        LocalSource {
            buffer_size: copy_buffer_size,
        },
        target,
        stats,
        "local->SMB",
        max_concurrent_tasks,
        retry_policy,
        failure_recorder,
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
    copy_buffer_size: usize,
    smb_copy_task_count: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let max_concurrent_tasks = smb_copy_task_limit(pool.size(), smb_copy_task_count);
    let _ = smb_source_base;
    let mapping = EntryMapping::remote_to_local();
    let target = LocalTarget {
        base: local_target_base,
    };
    let target = AggregatingTarget::new(target, aggregate_config);

    run_smb_source_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        location,
        pool,
        target,
        stats,
        "SMB->local",
        max_concurrent_tasks,
        copy_buffer_size,
        aggregate_config,
        retry_policy,
        failure_recorder,
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
    copy_buffer_size: usize,
    smb_copy_task_count: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let max_concurrent_tasks = smb_copy_task_limit(
        source_pool.size().min(target_pool.size()),
        smb_copy_task_count,
    );
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
            max_concurrent_tasks,
            copy_buffer_size,
            retry_policy,
            failure_recorder.clone(),
        )
        .await;
        return;
    }

    let _ = smb_source_base;
    let target_prefix = PathBuf::from(target_prefix);
    let mapping = EntryMapping::remote_to_prefixed_target(target_prefix.clone());
    let target_pool_for_streaming = Arc::clone(&target_pool);
    let target_dir_cache = crate::smb::aio::new_dir_cache();
    let target = SmbTarget {
        location: target_location.clone(),
        pool: target_pool,
        dir_cache: target_dir_cache.clone(),
        buffer_size: copy_buffer_size,
    };
    let target = AggregatingTarget::with_repo_prefix(target, aggregate_config, target_prefix);

    run_smb_to_smb_aggregate_pipeline(
        control_file,
        meta_dir,
        mapping,
        source_location,
        target_location,
        source_pool,
        target_pool_for_streaming,
        target_dir_cache,
        target,
        stats,
        "SMB->SMB",
        max_concurrent_tasks,
        copy_buffer_size,
        aggregate_config,
        retry_policy,
        failure_recorder,
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
    max_concurrent_tasks: usize,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let pipeline_started = Instant::now();
    let _ = smb_source_base;
    let target_prefix = PathBuf::from(target_prefix);
    let mapping = EntryMapping::remote_to_prefixed_target(target_prefix.clone());
    let dir_target = SmbTarget {
        location: target_location.clone(),
        pool: Arc::clone(&target_pool),
        dir_cache: crate::smb::aio::new_dir_cache(),
        buffer_size: copy_buffer_size,
    };
    let task_sem = Arc::new(Semaphore::new(max_concurrent_tasks.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<CopyPlanEntry>(256);
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
            produce_copy_plan(
                control_file,
                meta_dir,
                mapping,
                "SMB->SMB",
                |_| false,
                |entry| entry_tx.blocking_send(entry).is_ok(),
            );
        })
    };
    drop(entry_tx);

    while let Some(item) = entry_rx.recv().await {
        match item {
            CopyPlanEntry::Directory { dst_path, .. } => {
                dir_entries += 1;
                dir_paths.push(dst_path);
            }
            CopyPlanEntry::File(FileCopyPlan::Direct {
                meta,
                src_path,
                dst_path,
            }) => {
                file_entries += 1;
                if meta.common.symlink_target_path.is_some() {
                    debug!("SMB->SMB: skipping symlink {:?}", src_path);
                    continue;
                }

                let read_path = src_path.clone();
                let write_path = dst_path.clone();
                let src_rel = src_path.to_string_lossy().replace('\\', "/");
                let dst_rel = dst_path.to_string_lossy().replace('\\', "/");
                let file_size = meta.size;
                file_jobs.push((read_path, write_path, src_rel, dst_rel, file_size));
            }
            CopyPlanEntry::File(FileCopyPlan::Aggregate { src_path, .. }) => {
                file_entries += 1;
                error!("SMB->SMB: unexpected aggregate plan for {:?}", src_path);
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
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
    let mut task_handles = Vec::new();
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
        let failure_recorder2 = failure_recorder.clone();

        task_handles.push(tokio::spawn(async move {
            let _permit = task_sem2.acquire_owned().await.unwrap();
            match crate::smb::aio::copy_relative_file_streaming(
                &source_pool2,
                &source_location2,
                &src_rel,
                file_size,
                &target_pool2,
                &target_location2,
                &target_dir_cache2,
                &dst_rel,
                false,
                Some(copy_metrics2),
                copy_buffer_size,
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
                    if let Some(recorder) = failure_recorder2 {
                        recorder.record(FailureRecord::from_detail(
                            "backup",
                            "streaming_copy",
                            FailureItemType::File,
                            write_path.to_string_lossy(),
                            msg,
                            retry_policy.max_retries + 1,
                        ));
                    }
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
        "SMB->SMB timing: total={}, dispatch={} for {} file entries and {} dir entries, producer_wait={}, mkdir_wait={}, copy_wait={}, copy_task_limit={}, copy_ops: {}",
        format_elapsed(pipeline_started.elapsed()),
        format_elapsed(dispatch_elapsed),
        file_entries,
        dir_entries,
        format_elapsed(producer_wait_elapsed),
        format_elapsed(mkdir_elapsed),
        format_elapsed(copy_elapsed),
        max_concurrent_tasks,
        copy_metrics.timing_summary(),
    );
}

#[cfg(feature = "smb")]
async fn run_smb_source_copy_pipeline<T>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    mapping: EntryMapping,
    source_location: SmbLocation,
    source_pool: Arc<crate::smb::aio::SmbClientPool>,
    target: T,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
    max_concurrent_tasks: usize,
    copy_buffer_size: usize,
    aggregate_config: AggregateConfig,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) where
    T: TargetWriter,
{
    let task_sem = Arc::new(Semaphore::new(max_concurrent_tasks.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<CopyPlanEntry>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        tokio::task::spawn_blocking(move || {
            produce_copy_plan(
                control_file,
                meta_dir,
                mapping,
                log_prefix,
                |_| false,
                |entry| entry_tx.blocking_send(entry).is_ok(),
            );
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            CopyPlanEntry::Directory { dst_path, .. } => {
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let path = dst_path;
                let failure_recorder2 = failure_recorder.clone();
                debug!("{log_prefix}: mkdir {:?}", path);

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    match retry_create_dir(&target2, path.clone(), retry_policy).await {
                        Ok(()) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("{log_prefix}: mkdir {:?}: {e}", path);
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                            if let Some(recorder) = failure_recorder2 {
                                recorder.record(FailureRecord::from_detail(
                                    "backup",
                                    "create_dir",
                                    FailureItemType::Directory,
                                    path.to_string_lossy(),
                                    e,
                                    retry_policy.max_retries + 1,
                                ));
                            }
                        }
                    }
                });
                task_handles.push(h);
            }
            CopyPlanEntry::File(plan) => {
                let source_location2 = source_location.clone();
                let source_pool2 = Arc::clone(&source_pool);
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let failure_recorder2 = failure_recorder.clone();

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    execute_smb_source_file_plan(
                        plan,
                        source_location2,
                        source_pool2,
                        target2,
                        stats2,
                        log_prefix,
                        copy_buffer_size,
                        aggregate_config,
                        retry_policy,
                        failure_recorder2,
                    )
                    .await;
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

    if let Err(e) = source_pool.close().await {
        error!("{log_prefix}: source finalization failed: {e}");
        stats.files_failed.fetch_add(1, Ordering::Relaxed);
    }
    if let Err(e) = target.finish().await {
        error!("{log_prefix}: target finalization failed: {e}");
        stats.files_failed.fetch_add(1, Ordering::Relaxed);
    }

    info!(
        "{log_prefix}: complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}

#[cfg(feature = "smb")]
async fn run_smb_to_smb_aggregate_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    mapping: EntryMapping,
    source_location: SmbLocation,
    target_location: SmbLocation,
    source_pool: Arc<crate::smb::aio::SmbClientPool>,
    target_pool: Arc<crate::smb::aio::SmbClientPool>,
    target_dir_cache: crate::smb::aio::DirCache,
    target: AggregatingTarget<SmbTarget>,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
    max_concurrent_tasks: usize,
    copy_buffer_size: usize,
    aggregate_config: AggregateConfig,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let task_sem = Arc::new(Semaphore::new(max_concurrent_tasks.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<CopyPlanEntry>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        tokio::task::spawn_blocking(move || {
            produce_copy_plan(
                control_file,
                meta_dir,
                mapping,
                log_prefix,
                |_| false,
                |entry| entry_tx.blocking_send(entry).is_ok(),
            );
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            CopyPlanEntry::Directory { dst_path, .. } => {
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let path = dst_path;
                let failure_recorder2 = failure_recorder.clone();
                debug!("{log_prefix}: mkdir {:?}", path);

                task_handles.push(tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    match retry_create_dir(&target2, path.clone(), retry_policy).await {
                        Ok(()) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            error!("{log_prefix}: mkdir {:?}: {e}", path);
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                            if let Some(recorder) = failure_recorder2 {
                                recorder.record(FailureRecord::from_detail(
                                    "backup",
                                    "create_dir",
                                    FailureItemType::Directory,
                                    path.to_string_lossy(),
                                    e,
                                    retry_policy.max_retries + 1,
                                ));
                            }
                        }
                    }
                }));
            }
            CopyPlanEntry::File(FileCopyPlan::Direct {
                meta,
                src_path,
                dst_path,
            }) => {
                if meta.common.symlink_target_path.is_some() {
                    debug!("{log_prefix}: skipping symlink {:?}", src_path);
                    continue;
                }

                let source_location2 = source_location.clone();
                let target_location2 = target_location.clone();
                let source_pool2 = Arc::clone(&source_pool);
                let target_pool2 = Arc::clone(&target_pool);
                let target_dir_cache2 = target_dir_cache.clone();
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let failure_recorder2 = failure_recorder.clone();
                let src_rel = src_path.to_string_lossy().replace('\\', "/");
                let dst_rel = dst_path.to_string_lossy().replace('\\', "/");
                let read_path = src_path.clone();
                let write_path = dst_path.clone();
                let file_size = meta.size;

                task_handles.push(tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    if should_aggregate(file_size, &aggregate_config) {
                        execute_smb_source_file_plan(
                            FileCopyPlan::Direct {
                                meta,
                                src_path,
                                dst_path,
                            },
                            source_location2,
                            source_pool2,
                            target2,
                            stats2,
                            log_prefix,
                            copy_buffer_size,
                            aggregate_config,
                            retry_policy,
                            failure_recorder2,
                        )
                        .await;
                        return;
                    }

                    match crate::smb::aio::copy_relative_file_streaming(
                        &source_pool2,
                        &source_location2,
                        &src_rel,
                        meta.size,
                        &target_pool2,
                        &target_location2,
                        &target_dir_cache2,
                        &dst_rel,
                        true,
                        None,
                        copy_buffer_size,
                    )
                    .await
                    {
                        Ok(()) => {
                            debug!("{log_prefix}: copied {:?} -> {:?}", read_path, write_path);
                            stats2.files_copied.fetch_add(1, Ordering::Relaxed);
                            stats2.bytes_copied.fetch_add(file_size, Ordering::Relaxed);
                        }
                        Err(msg) => {
                            error!("{log_prefix}: write {:?}: {msg}", write_path);
                            stats2.files_failed.fetch_add(1, Ordering::Relaxed);
                            if let Some(recorder) = failure_recorder2 {
                                recorder.record(FailureRecord::from_detail(
                                    "backup",
                                    "streaming_copy",
                                    FailureItemType::File,
                                    write_path.to_string_lossy(),
                                    msg,
                                    retry_policy.max_retries + 1,
                                ));
                            }
                        }
                    }
                }));
            }
            CopyPlanEntry::File(FileCopyPlan::Aggregate { src_path, .. }) => {
                error!("{log_prefix}: unexpected aggregate plan for {:?}", src_path);
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
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

    if let Err(e) = source_pool.close().await {
        error!("{log_prefix}: source finalization failed: {e}");
        stats.files_failed.fetch_add(1, Ordering::Relaxed);
    }
    if let Err(e) = target.finish().await {
        error!("{log_prefix}: target finalization failed: {e}");
        stats.files_failed.fetch_add(1, Ordering::Relaxed);
    }

    info!(
        "{log_prefix}: complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}

#[cfg(feature = "smb")]
fn smb_copy_task_limit(pool_size: usize, configured_tasks: usize) -> usize {
    if configured_tasks > 0 {
        return configured_tasks.clamp(1, SMB_MAX_CONCURRENT_TASKS);
    }
    pool_size
        .max(1)
        .saturating_mul(SMB_TASKS_PER_CONNECTION)
        .min(SMB_MAX_CONCURRENT_TASKS)
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
    copy_buffer_size: usize,
    smb_copy_task_count: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let max_concurrent_tasks =
        smb_copy_task_limit(target_pool.size(), smb_copy_task_count).min(NFS_MAX_CONCURRENT_TASKS);
    let _ = nfs_source_base;
    let target_prefix = PathBuf::from(target_prefix);
    let mapping = EntryMapping::remote_to_prefixed_target(target_prefix.clone());
    let source = NfsSource {
        pool: Arc::clone(&source_pool),
        dir_cache: new_file_handle_cache(),
        root_fh: source_pool.root_fh(),
        read_chunk: source_pool.server_rtmax,
        buffer_size: copy_buffer_size,
    };
    let target = SmbTarget {
        location: target_location,
        pool: target_pool,
        dir_cache: crate::smb::aio::new_dir_cache(),
        buffer_size: copy_buffer_size,
    };
    let target = AggregatingTarget::with_repo_prefix(target, aggregate_config, target_prefix);

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "NFS->SMB",
        max_concurrent_tasks,
        retry_policy,
        failure_recorder,
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
    copy_buffer_size: usize,
    smb_copy_task_count: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let max_concurrent_tasks =
        smb_copy_task_limit(source_pool.size(), smb_copy_task_count).min(NFS_MAX_CONCURRENT_TASKS);
    let _ = smb_source_base;
    let target_prefix = PathBuf::from(target_prefix);
    let mapping = EntryMapping::remote_to_prefixed_target(target_prefix.clone());
    let target = NfsTarget {
        pool: Arc::clone(&target_pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: target_pool.root_fh(),
        write_chunk: target_pool.server_wtmax,
        buffer_size: copy_buffer_size,
    };
    let target = AggregatingTarget::with_repo_prefix(target, aggregate_config, target_prefix);

    run_smb_source_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source_location,
        source_pool,
        target,
        stats,
        "SMB->NFS",
        max_concurrent_tasks,
        copy_buffer_size,
        aggregate_config,
        retry_policy,
        failure_recorder,
    )
    .await;
}

#[cfg(feature = "smb")]
async fn retry_create_dir<T>(
    target: &T,
    path: PathBuf,
    retry_policy: RetryPolicy,
) -> Result<(), String>
where
    T: TargetWriter,
{
    let mut attempts = 0;
    loop {
        attempts += 1;
        match target.create_dir(path.clone()).await {
            Ok(()) => return Ok(()),
            Err(err) if retry_policy.should_retry(attempts) => {
                tokio::time::sleep(retry_policy.delay_for_attempt(attempts)).await;
                let _ = err;
            }
            Err(err) => return Err(err),
        }
    }
}
