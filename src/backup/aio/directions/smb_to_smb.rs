//! SMB → SMB backup direction.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use log::info;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::aio::phases::run_smb_target_phases;
use crate::backup::stats::BackupStats;
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::smb::SmbLocation;

/// Spawn a backup thread that copies files between two SMB endpoints.
pub fn spawn(
    smb_source: SmbLocation,
    smb_target: SmbLocation,
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
    smb_connection_count: usize,
    smb_copy_task_count: usize,
    phase_flags: PhaseFlags,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-smb-to-smb")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SMB->SMB: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let pool_size = smb_connection_count.max(1);
            let source_pool =
                match crate::smb::aio::SmbClientPool::connect(&smb_source, pool_size).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("SMB->SMB: failed to connect to source: {e}");
                        return;
                    }
                };
            let target_pool =
                match crate::smb::aio::SmbClientPool::connect(&smb_target, pool_size).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("SMB->SMB: failed to connect to target: {e}");
                        let _ = source_pool.close().await;
                        return;
                    }
                };

            info!(
                "SMB->SMB: connected source {} and target {} (pool_size={} each)",
                smb_source.display_string(),
                smb_target.display_string(),
                pool_size,
            );

            run(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
                aggregate_config,
                copy_buffer_size,
                smb_copy_task_count,
                retry_policy,
                failure_recorder,
                smb_source,
                smb_target,
                source_pool,
                target_pool,
                stats,
                phase_flags,
            )
            .await;
        });

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

/// Run a full backup pipeline for SMB source -> SMB target.
pub async fn run(
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    copy_buffer_size: usize,
    smb_copy_task_count: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
    source_location: SmbLocation,
    target_location: SmbLocation,
    source_pool: Arc<crate::smb::aio::SmbClientPool>,
    target_pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    super::copy_pipelines::run_smb_to_smb_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        source_location,
        target_location.clone(),
        source_pool,
        target_pool,
        stats,
        copy_buffer_size,
        smb_copy_task_count,
        retry_policy,
        failure_recorder,
    )
    .await;

    run_smb_target_phases(
        &ctrl_dir,
        &source_dir_base,
        &target_prefix,
        &target_location,
        phase_flags,
    )
    .await;
}
