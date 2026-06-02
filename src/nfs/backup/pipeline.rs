//! NFS-specific copy pipeline functions.
//!
//! These functions construct NFS transport objects ([`NfsSource`], [`NfsTarget`])
//! and delegate to the generic [`run_copy_pipeline`].

use std::path::PathBuf;
use std::sync::Arc;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::aio::aggregation::AggregatingTarget;
use crate::backup::aio::entry::EntryMapping;
use crate::backup::aio::pipeline::run_copy_pipeline;
use crate::backup::aio::transport::{
    clamp_copy_buffer_size, LocalSource, LocalTarget, NfsSource, NfsTarget,
};
use crate::backup::stats::BackupStats;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::nfs::aio::{reader::new_file_handle_cache, writer::new_dir_handle_cache};
use crate::nfs::connection::NfsConnectionPool;

const NFS_MAX_CONCURRENT_TASKS: usize = 16;

pub async fn run_local_to_nfs(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let target_prefix = PathBuf::from(target_prefix);
    let mapping = EntryMapping::local_to_prefixed_target(source_dir_base, target_prefix.clone());
    let target = NfsTarget {
        pool: Arc::clone(&pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: pool.root_fh(),
        write_chunk: pool.server_wtmax,
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
        "local->NFS",
        NFS_MAX_CONCURRENT_TASKS,
        retry_policy,
        failure_recorder,
    )
    .await;
}

pub async fn run_nfs_to_local(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    local_target_base: PathBuf,
    aggregate_config: AggregateConfig,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
    let _ = nfs_source_base;
    let mapping = EntryMapping::remote_to_local();
    let source = NfsSource {
        pool: Arc::clone(&pool),
        dir_cache: new_file_handle_cache(),
        root_fh: pool.root_fh(),
        read_chunk: pool.server_rtmax,
        buffer_size: copy_buffer_size,
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
        retry_policy,
        failure_recorder,
    )
    .await;
}

pub async fn run_nfs_to_nfs(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    source_pool: Arc<NfsConnectionPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) {
    let copy_buffer_size = clamp_copy_buffer_size(copy_buffer_size);
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
    let target = NfsTarget {
        pool: Arc::clone(&target_pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: target_pool.root_fh(),
        write_chunk: target_pool.server_wtmax,
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
        "NFS->NFS",
        NFS_MAX_CONCURRENT_TASKS,
        retry_policy,
        failure_recorder,
    )
    .await;
}
