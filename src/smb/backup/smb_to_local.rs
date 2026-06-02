//! SMB → Local backup direction.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use log::info;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::stats::BackupStats;
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::smb::backup::pipeline;
use crate::smb::SmbLocation;

/// Spawn a backup thread that copies files from an SMB source to a local target.
pub fn spawn(
    smb_source: SmbLocation,
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
    smb_connection_count: usize,
    smb_copy_task_count: usize,
    phase_flags: PhaseFlags,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-smb-to-local")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SMB->local: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let pool_size = smb_connection_count.max(1);
            let pool = match crate::smb::aio::SmbClientPool::connect(&smb_source, pool_size).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("SMB->local: failed to connect: {e}");
                    return;
                }
            };

            info!(
                "SMB->local: connected to {} (pool_size={})",
                smb_source.display_string(),
                pool_size
            );

            run(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_dir_base,
                aggregate_config,
                copy_buffer_size,
                smb_copy_task_count,
                retry_policy,
                failure_recorder,
                smb_source,
                pool,
                stats,
                phase_flags,
            )
            .await;
        });

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

/// Run a full backup pipeline for SMB source -> local target.
pub async fn run(
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_dir_base: PathBuf,
    aggregate_config: AggregateConfig,
    copy_buffer_size: usize,
    smb_copy_task_count: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
    location: SmbLocation,
    pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    pipeline::run_smb_to_local_copy_pipeline(
        control_file,
        meta_dir.clone(),
        source_dir_base.clone(),
        target_dir_base.clone(),
        aggregate_config,
        location,
        pool,
        stats,
        copy_buffer_size,
        smb_copy_task_count,
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
