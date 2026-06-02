//! NFS → Local backup direction.

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

/// Spawn a backup thread that copies files from an NFS source to a local target.
pub fn spawn(
    nfs_source: NfsLocation,
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_dir_base: PathBuf,
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
            .thread_name("fpt-nfs-to-local")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("NFS→local: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let pool = match NfsConnectionPool::new(&nfs_source).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("NFS→local: failed to connect: {e}");
                    return;
                }
            };

            info!(
                "NFS→local: connected to {} (rtmax={})",
                nfs_source.host, pool.server_rtmax
            );

            run(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_dir_base,
                aggregate_config,
                copy_buffer_size,
                retry_policy,
                failure_recorder,
                pool,
                stats,
                phase_flags,
            )
            .await;
        });

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

/// Run a full backup pipeline for NFS source → local target.
pub async fn run(
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_dir_base: PathBuf,
    aggregate_config: AggregateConfig,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    pipeline::run_nfs_to_local(
        control_file,
        meta_dir.clone(),
        source_dir_base.clone(),
        target_dir_base.clone(),
        aggregate_config,
        pool,
        stats,
        copy_buffer_size,
        retry_policy,
        failure_recorder.clone(),
    )
    .await;

    crate::backup::aio::phases::run_local_target_phases(
        &ctrl_dir,
        &meta_dir,
        &source_dir_base,
        &target_dir_base,
        phase_flags,
        retry_policy,
        failure_recorder.as_ref(),
    );
}
