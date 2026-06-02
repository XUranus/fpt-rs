//! Generic backup orchestrator that composes source and target transports.
//!
//! This replaces the 9 per-direction files with a single generic pipeline:
//! 1. Connect source
//! 2. Connect target
//! 3. Run copy pipeline (source → target)
//! 4. Run target post-copy phases

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use log::info;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::aio::source::BackupSource;
use crate::backup::aio::target::BackupTarget;
use crate::backup::stats::BackupStats;
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::frame::location::DataLocation;

/// Common parameters shared by all backup directions.
pub struct BackupPipelineParams {
    pub control_file: PathBuf,
    pub meta_dir: PathBuf,
    pub ctrl_dir: PathBuf,
    pub source_dir_base: PathBuf,
    pub target_prefix: String,
    pub aggregate_config: AggregateConfig,
    pub copy_buffer_size: usize,
    pub retry_policy: RetryPolicy,
    pub failure_recorder: Option<FailureRecorder>,
    pub stats: Arc<BackupStats>,
    pub phase_flags: PhaseFlags,
    #[cfg(feature = "smb")]
    pub smb_connection_count: usize,
    #[cfg(feature = "smb")]
    pub smb_copy_task_count: usize,
}

/// Spawn a backup thread that connects source and target, runs the copy
/// pipeline, then runs post-copy phases.
///
/// This replaces the 9 per-direction `spawn()` functions with a single
/// generic entry point.
pub fn spawn_backup(
    source_location: DataLocation,
    target_location: DataLocation,
    params: BackupPipelineParams,
    terminate_indicator: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-backup")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("backup: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            if let Err(e) = run_backup(source_location, target_location, params).await {
                eprintln!("backup failed: {e}");
            }
        });

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

/// Run a full backup pipeline: connect → copy → post-copy phases.
async fn run_backup(
    source_location: DataLocation,
    target_location: DataLocation,
    params: BackupPipelineParams,
) -> Result<(), String> {
    // 1. Connect source
    let source = BackupSource::connect(
        &source_location,
        #[cfg(feature = "smb")]
        params.smb_connection_count,
    )
    .await?;

    // 2. Connect target
    let target = BackupTarget::connect(
        &target_location,
        #[cfg(feature = "smb")]
        params.smb_connection_count,
    )
    .await?;

    info!(
        "backup: {} → {}",
        source_location.display_string(),
        target_location.display_string()
    );

    // 3. Run copy pipeline
    run_copy_for_source_target(&source, &target, &params).await;

    // 4. Run post-copy phases
    target
        .run_post_copy_phases(
            &params.ctrl_dir,
            &params.source_dir_base,
            &params.target_prefix,
            params.phase_flags,
            params.retry_policy,
            params.failure_recorder.as_ref(),
        )
        .await;

    info!("backup: complete");
    Ok(())
}

/// Dispatch the copy pipeline based on source+target combination.
#[allow(unused_variables)]
async fn run_copy_for_source_target(
    source: &BackupSource,
    target: &BackupTarget,
    params: &BackupPipelineParams,
) {
    match (source, target) {
        // ── Local → Local ──
        (BackupSource::Local { .. }, BackupTarget::Local { .. }) => {
            // Local→Local uses the BIO pipeline, not AIO.
            // This path should not be reached from the AIO orchestrator.
            unreachable!("local→local backup uses the BIO pipeline, not AIO");
        }

        // ── Local → NFS ──
        #[cfg(feature = "nfs")]
        (
            BackupSource::Local { source_dir_base },
            BackupTarget::Nfs { pool },
        ) => {
            crate::nfs::backup::pipeline::run_local_to_nfs(
                params.control_file.clone(),
                params.meta_dir.clone(),
                source_dir_base.clone(),
                params.target_prefix.clone(),
                params.aggregate_config.clone(),
                Arc::clone(pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }

        // ── Local → SMB ──
        #[cfg(feature = "smb")]
        (
            BackupSource::Local { source_dir_base },
            BackupTarget::Smb { location, pool },
        ) => {
            crate::smb::backup::pipeline::run_local_to_smb_copy_pipeline(
                params.control_file.clone(),
                params.meta_dir.clone(),
                source_dir_base.clone(),
                params.target_prefix.clone(),
                params.aggregate_config.clone(),
                location.clone(),
                Arc::clone(pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.smb_copy_task_count,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }

        // ── NFS → Local ──
        #[cfg(feature = "nfs")]
        (
            BackupSource::Nfs { pool },
            BackupTarget::Local { target_dir_base },
        ) => {
            crate::nfs::backup::pipeline::run_nfs_to_local(
                params.control_file.clone(),
                params.meta_dir.clone(),
                params.source_dir_base.clone(),
                target_dir_base.clone(),
                params.aggregate_config.clone(),
                Arc::clone(pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }

        // ── NFS → NFS ──
        #[cfg(feature = "nfs")]
        (
            BackupSource::Nfs { pool: source_pool },
            BackupTarget::Nfs { pool: target_pool },
        ) => {
            crate::nfs::backup::pipeline::run_nfs_to_nfs(
                params.control_file.clone(),
                params.meta_dir.clone(),
                params.source_dir_base.clone(),
                params.target_prefix.clone(),
                params.aggregate_config.clone(),
                Arc::clone(source_pool),
                Arc::clone(target_pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }

        // ── NFS → SMB ──
        #[cfg(all(feature = "nfs", feature = "smb"))]
        (
            BackupSource::Nfs { pool: source_pool },
            BackupTarget::Smb { location: target_loc, pool: target_pool },
        ) => {
            crate::nfs::backup::pipeline::run_nfs_to_smb(
                params.control_file.clone(),
                params.meta_dir.clone(),
                params.source_dir_base.clone(),
                params.target_prefix.clone(),
                params.aggregate_config.clone(),
                Arc::clone(source_pool),
                target_loc.clone(),
                Arc::clone(target_pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.smb_copy_task_count,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }

        // ── SMB → Local ──
        #[cfg(feature = "smb")]
        (
            BackupSource::Smb { location, pool },
            BackupTarget::Local { target_dir_base },
        ) => {
            crate::smb::backup::pipeline::run_smb_to_local_copy_pipeline(
                params.control_file.clone(),
                params.meta_dir.clone(),
                params.source_dir_base.clone(),
                target_dir_base.clone(),
                params.aggregate_config.clone(),
                location.clone(),
                Arc::clone(pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.smb_copy_task_count,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }

        // ── SMB → SMB ──
        #[cfg(feature = "smb")]
        (
            BackupSource::Smb { location: src_loc, pool: src_pool },
            BackupTarget::Smb { location: tgt_loc, pool: tgt_pool },
        ) => {
            crate::smb::backup::pipeline::run_smb_to_smb_copy_pipeline(
                params.control_file.clone(),
                params.meta_dir.clone(),
                params.source_dir_base.clone(),
                params.target_prefix.clone(),
                params.aggregate_config.clone(),
                src_loc.clone(),
                tgt_loc.clone(),
                Arc::clone(src_pool),
                Arc::clone(tgt_pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.smb_copy_task_count,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }

        // ── SMB → NFS ──
        #[cfg(all(feature = "nfs", feature = "smb"))]
        (
            BackupSource::Smb { location: src_loc, pool: src_pool },
            BackupTarget::Nfs { pool: tgt_pool },
        ) => {
            crate::smb::backup::pipeline::run_smb_to_nfs(
                params.control_file.clone(),
                params.meta_dir.clone(),
                params.source_dir_base.clone(),
                params.target_prefix.clone(),
                params.aggregate_config.clone(),
                src_loc.clone(),
                Arc::clone(src_pool),
                Arc::clone(tgt_pool),
                Arc::clone(&params.stats),
                params.copy_buffer_size,
                params.smb_copy_task_count,
                params.retry_policy,
                params.failure_recorder.clone(),
            )
            .await;
        }
    }
}
