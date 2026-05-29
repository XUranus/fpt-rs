//! Async backup execution for remote-involved data paths.
//!
//! This module is the remote counterpart to [`crate::backup::bio`]:
//! - Direction-specific **copy** pipelines live under `backup/aio/`.
//! - NFS target **post-copy phases** (hardlink/delete/mtime) reuse the RPC
//!   helpers under [`crate::nfs::aio`].
//! - SMB target post-copy phases reuse the helpers under [`crate::smb::aio`].
//! - Local target post-copy phases reuse the existing BIO phase handlers.
//!
//! The public entry points here are direction-level orchestrators so callers
//! do not need to manually stitch together copy and post-copy phases.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[cfg(any(feature = "nfs", feature = "smb"))]
use log::info;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::stats::BackupStats;
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
#[cfg(feature = "nfs")]
use crate::nfs::aio::reader::new_file_handle_cache;
#[cfg(feature = "nfs")]
use crate::nfs::aio::writer::new_dir_handle_cache;
#[cfg(feature = "nfs")]
use crate::nfs::connection::NfsConnectionPool;
#[cfg(feature = "nfs")]
use crate::nfs::NfsLocation;
#[cfg(feature = "smb")]
use crate::smb::SmbLocation;

#[cfg(feature = "smb")]
pub const DEFAULT_SMB_POOL_SIZE: usize = 2;

mod aggregation;
mod directions;
pub(crate) mod entry;
mod executor;
pub(crate) mod local_fs;
mod phases;
mod pipeline;
pub(crate) mod transport;

use phases::run_local_target_phases;
#[cfg(feature = "nfs")]
use phases::run_nfs_target_phases;
#[cfg(feature = "smb")]
use phases::run_smb_target_phases;

#[cfg(feature = "nfs")]
pub fn spawn_local_to_nfs_backup(
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
            .thread_name("fpt-local-to-nfs")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("local→NFS: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let pool = match NfsConnectionPool::new(&nfs_target).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("local→NFS: failed to connect: {e}");
                    return;
                }
            };

            info!(
                "local→NFS: connected to {} (wtmax={})",
                nfs_target.host, pool.server_wtmax
            );

            run_local_to_nfs_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
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

#[cfg(feature = "smb")]
pub fn spawn_local_to_smb_backup(
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
            .thread_name("fpt-local-to-smb")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("local→SMB: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let pool_size = smb_connection_count.max(1);
            let pool = match crate::smb::aio::SmbClientPool::connect(&smb_target, pool_size).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("local→SMB: failed to connect: {e}");
                    return;
                }
            };

            info!(
                "local→SMB: connected to {} (pool_size={})",
                smb_target.display_string(),
                pool_size
            );

            run_local_to_smb_backup(
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
                smb_target,
                pool,
                stats,
                phase_flags,
            )
            .await;
        });

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

#[cfg(feature = "smb")]
pub fn spawn_smb_to_local_backup(
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

            run_smb_to_local_backup(
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

#[cfg(feature = "smb")]
pub fn spawn_smb_to_smb_backup(
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

            run_smb_to_smb_backup(
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

#[cfg(all(feature = "nfs", feature = "smb"))]
pub fn spawn_nfs_to_smb_backup(
    nfs_source: NfsLocation,
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
            .thread_name("fpt-nfs-to-smb")
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("NFS->SMB: failed to build async runtime: {e}");
                terminate_indicator.store(true, Ordering::Relaxed);
                return;
            }
        };

        rt.block_on(async {
            let smb_pool_size = smb_connection_count.max(1);
            let source_pool = match NfsConnectionPool::new(&nfs_source).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("NFS->SMB: failed to connect to source: {e}");
                    return;
                }
            };
            let target_pool =
                match crate::smb::aio::SmbClientPool::connect(&smb_target, smb_pool_size).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("NFS->SMB: failed to connect to target: {e}");
                        return;
                    }
                };
            info!(
                "NFS->SMB: connected SMB target {} (pool_size={})",
                smb_target.display_string(),
                smb_pool_size
            );

            run_nfs_to_smb_backup(
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
                source_pool,
                smb_target,
                target_pool,
                stats,
                phase_flags,
            )
            .await;
        });

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

#[cfg(all(feature = "nfs", feature = "smb"))]
pub fn spawn_smb_to_nfs_backup(
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

            run_smb_to_nfs_backup(
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

#[cfg(feature = "nfs")]
pub fn spawn_nfs_to_local_backup(
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

            run_nfs_to_local_backup(
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

#[cfg(feature = "nfs")]
pub fn spawn_nfs_to_nfs_backup(
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

            run_nfs_to_nfs_backup(
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

/// Run a full backup pipeline for local source → NFS target.
#[cfg(feature = "nfs")]
pub async fn run_local_to_nfs_backup(
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    copy_buffer_size: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    directions::run_local_to_nfs_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        Arc::clone(&pool),
        Arc::clone(&stats),
        copy_buffer_size,
        retry_policy,
        failure_recorder.clone(),
    )
    .await;

    let file_cache = new_file_handle_cache();
    let dir_cache = new_dir_handle_cache();

    run_nfs_target_phases(
        &ctrl_dir,
        &source_dir_base,
        &target_prefix,
        pool,
        file_cache,
        dir_cache,
        phase_flags,
    )
    .await;
}

/// Run a full backup pipeline for local source → SMB target.
#[cfg(feature = "smb")]
pub async fn run_local_to_smb_backup(
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
    location: SmbLocation,
    pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    directions::run_local_to_smb_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        location.clone(),
        pool,
        stats,
        copy_buffer_size,
        smb_copy_task_count,
        retry_policy,
        failure_recorder.clone(),
    )
    .await;

    run_smb_target_phases(
        &ctrl_dir,
        &source_dir_base,
        &target_prefix,
        &location,
        phase_flags,
    )
    .await;
}

/// Run a full backup pipeline for NFS source → local target.
#[cfg(feature = "nfs")]
pub async fn run_nfs_to_local_backup(
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
    directions::run_aio_nfs_to_local_pipeline(
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

    run_local_target_phases(
        &ctrl_dir,
        &meta_dir,
        &source_dir_base,
        &target_dir_base,
        phase_flags,
        retry_policy,
        failure_recorder.as_ref(),
    );
}

/// Run a full backup pipeline for NFS source → NFS target.
#[cfg(feature = "nfs")]
pub async fn run_nfs_to_nfs_backup(
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
    directions::run_aio_nfs_to_nfs_pipeline(
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

/// Run a full backup pipeline for SMB source -> local target.
#[cfg(feature = "smb")]
pub async fn run_smb_to_local_backup(
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
    directions::run_smb_to_local_copy_pipeline(
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

    run_local_target_phases(
        &ctrl_dir,
        &meta_dir,
        &source_dir_base,
        &target_dir_base,
        phase_flags,
        retry_policy,
        failure_recorder.as_ref(),
    );
}

/// Run a full backup pipeline for SMB source -> SMB target.
#[cfg(feature = "smb")]
pub async fn run_smb_to_smb_backup(
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
    directions::run_smb_to_smb_copy_pipeline(
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

#[cfg(all(feature = "nfs", feature = "smb"))]
pub async fn run_nfs_to_smb_backup(
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
    source_pool: Arc<NfsConnectionPool>,
    target_location: SmbLocation,
    target_pool: Arc<crate::smb::aio::SmbClientPool>,
    stats: Arc<BackupStats>,
    phase_flags: PhaseFlags,
) {
    directions::run_nfs_to_smb_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        source_pool,
        target_location.clone(),
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

#[cfg(all(feature = "nfs", feature = "smb"))]
pub async fn run_smb_to_nfs_backup(
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
    directions::run_smb_to_nfs_copy_pipeline(
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
