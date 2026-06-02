//! SMB → NFS backup direction.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use log::info;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::aio::phases::run_nfs_target_phases;
use crate::backup::stats::BackupStats;
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::nfs::aio::{reader::new_file_handle_cache, writer::new_dir_handle_cache};
use crate::nfs::connection::NfsConnectionPool;
use crate::nfs::NfsLocation;
use crate::smb::SmbLocation;

/// Spawn a backup thread that copies files from an SMB source to an NFS target.
pub fn spawn(
    smb_source: SmbLocation,
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
    smb_connection_count: usize,
    smb_copy_task_count: usize,
    phase_flags: PhaseFlags,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("fpt-smb-to-nfs")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("SMB->NFS: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let smb_pool_size = smb_connection_count.max(1);
            let source_pool =
                match crate::smb::aio::SmbClientPool::connect(&smb_source, smb_pool_size).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("SMB->NFS: failed to connect to source: {e}");
                        return;
                    }
                };
            info!(
                "SMB->NFS: connected SMB source {} (pool_size={})",
                smb_source.display_string(),
                smb_pool_size
            );
            let target_pool = match NfsConnectionPool::new(&nfs_target).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("SMB->NFS: failed to connect to target: {e}");
                    let _ = source_pool.close().await;
                    return;
                }
            };

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

/// Run a full backup pipeline for SMB source -> NFS target.
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
    source_pool: Arc<crate::smb::aio::SmbClientPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    super::copy_pipelines::run_smb_to_nfs_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        source_location,
        source_pool,
        Arc::clone(&target_pool),
        stats,
        copy_buffer_size,
        smb_copy_task_count,
        retry_policy,
        failure_recorder,
    )
    .await;

    let file_cache = new_file_handle_cache();
    let dir_cache = new_dir_handle_cache();

    run_nfs_target_phases(
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
