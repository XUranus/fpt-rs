//! NFS → NFS backup direction.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use log::info;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::stats::BackupStats;
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::nfs::backup::pipeline;
use crate::nfs::connection::NfsConnectionPool;
use crate::nfs::NfsLocation;

/// Spawn a backup thread that copies files between two NFS endpoints.
pub fn spawn(
    nfs_source: NfsLocation,
    nfs_target: NfsLocation,
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    phase_flags: PhaseFlags,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-nfs-to-nfs")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("NFS→NFS: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let src_pool = match NfsConnectionPool::new(&nfs_source).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("NFS→NFS: failed to connect to source: {e}");
                    return;
                }
            };

            let tgt_pool = match NfsConnectionPool::new(&nfs_target).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("NFS→NFS: failed to connect to target: {e}");
                    return;
                }
            };

            info!(
                "NFS→NFS: connected source {} (rtmax={}), target {} (wtmax={})",
                nfs_source.host, src_pool.server_rtmax, nfs_target.host, tgt_pool.server_wtmax
            );

            run(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
                aggregate_config,
                copy_buffer_size,
                retry_policy,
                failure_recorder,
                src_pool,
                tgt_pool,
                stats,
                phase_flags,
            )
            .await;
        });

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

/// Run a full backup pipeline for NFS source → NFS target.
pub async fn run(
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
    source_pool: Arc<NfsConnectionPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    pipeline::run_nfs_to_nfs(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        source_pool,
        Arc::clone(&target_pool),
        stats,
        copy_buffer_size,
        retry_policy,
        failure_recorder.clone(),
    )
    .await;

    let file_cache = crate::nfs::aio::reader::new_file_handle_cache();
    let dir_cache = crate::nfs::aio::writer::new_dir_handle_cache();

    crate::backup::aio::phases::run_nfs_target_phases(
        &ctrl_dir,
        &source_dir_base,
        &target_prefix,
        target_pool,
        file_cache,
        dir_cache,
        phase_flags,
    )
    .await;
}
