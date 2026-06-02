//! Cross-transport (NFS↔SMB) copy pipeline functions.
//!
//! NFS-only directions have moved to [`crate::nfs::backup::pipeline`].
//! SMB-only directions have moved to [`crate::smb::backup::pipeline`].

#[cfg(all(feature = "nfs", feature = "smb"))]
use std::path::PathBuf;
#[cfg(all(feature = "nfs", feature = "smb"))]
use std::sync::Arc;

#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::backup::aggregate::AggregateConfig;
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::backup::aio::aggregation::AggregatingTarget;
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::backup::aio::entry::EntryMapping;
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::backup::aio::pipeline::run_copy_pipeline;
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::backup::aio::transport::{clamp_copy_buffer_size, NfsSource, NfsTarget, SmbTarget};
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::backup::stats::BackupStats;
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::failure::{FailureRecorder, RetryPolicy};
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::nfs::aio::{reader::new_file_handle_cache, writer::new_dir_handle_cache};
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::nfs::connection::NfsConnectionPool;
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::smb::backup::pipeline::run_smb_source_copy_pipeline;
#[cfg(all(feature = "nfs", feature = "smb"))]
use crate::smb::SmbLocation;

#[cfg(all(feature = "nfs", feature = "smb"))]
const NFS_MAX_CONCURRENT_TASKS: usize = 16;
#[cfg(all(feature = "nfs", feature = "smb"))]
const SMB_TASKS_PER_CONNECTION: usize = 8;

#[cfg(all(feature = "nfs", feature = "smb"))]
fn smb_copy_task_limit(pool_size: usize, configured_tasks: usize) -> usize {
    if configured_tasks > 0 {
        return configured_tasks.clamp(1, 32);
    }
    pool_size
        .max(1)
        .saturating_mul(SMB_TASKS_PER_CONNECTION)
        .min(32)
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
