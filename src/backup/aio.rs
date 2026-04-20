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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

#[cfg(feature = "nfs")]
use log::error;
use log::info;

use crate::backup::aggregate::AggregateConfig;
#[cfg(feature = "nfs")]
use crate::backup::bio::{delete, hardlink, mtime};
use crate::backup::stats::BackupStats;
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

pub(crate) mod entry;
pub(crate) mod local_fs;
mod pipeline;
pub(crate) mod transport;
mod aggregation;
mod directions;

#[cfg(feature = "nfs")]
pub fn spawn_local_to_nfs_backup(
    nfs_target: NfsLocation,
    control_file: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    aggregate_config: AggregateConfig,
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-local-to-nfs")
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

            info!("local→NFS: connected to {} (wtmax={})", nfs_target.host, pool.server_wtmax);

            run_local_to_nfs_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
                aggregate_config,
                pool,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            ).await;
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-local-to-smb")
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
            let client = match crate::smb::aio::connect_client(&smb_target).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("local→SMB: failed to connect: {e}");
                    return;
                }
            };

            info!("local→SMB: connected to {}", smb_target.display_string());

            run_local_to_smb_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
                aggregate_config,
                smb_target,
                client,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            ).await;
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-smb-to-local")
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
            let client = match crate::smb::aio::connect_client(&smb_source).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("SMB->local: failed to connect: {e}");
                    return;
                }
            };

            info!("SMB->local: connected to {}", smb_source.display_string());

            run_smb_to_local_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_dir_base,
                aggregate_config,
                smb_source,
                client,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            ).await;
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-smb-to-smb")
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
            let source_client = match crate::smb::aio::connect_client(&smb_source).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("SMB->SMB: failed to connect to source: {e}");
                    return;
                }
            };
            let target_client = match crate::smb::aio::connect_client(&smb_target).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("SMB->SMB: failed to connect to target: {e}");
                    let _ = source_client.close().await;
                    return;
                }
            };

            info!(
                "SMB->SMB: connected source {} and target {}",
                smb_source.display_string(),
                smb_target.display_string(),
            );

            run_smb_to_smb_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
                aggregate_config,
                smb_source,
                smb_target,
                source_client,
                target_client,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            ).await;
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-nfs-to-smb")
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
            let source_pool = match NfsConnectionPool::new(&nfs_source).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("NFS->SMB: failed to connect to source: {e}");
                    return;
                }
            };
            let target_client = match crate::smb::aio::connect_client(&smb_target).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("NFS->SMB: failed to connect to target: {e}");
                    return;
                }
            };

            run_nfs_to_smb_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
                aggregate_config,
                source_pool,
                smb_target,
                target_client,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-smb-to-nfs")
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
            let source_client = match crate::smb::aio::connect_client(&smb_source).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("SMB->NFS: failed to connect to source: {e}");
                    return;
                }
            };
            let target_pool = match NfsConnectionPool::new(&nfs_target).await {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("SMB->NFS: failed to connect to target: {e}");
                    let _ = source_client.close().await;
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
                smb_source,
                source_client,
                target_pool,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-nfs-to-local")
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

            info!("NFS→local: connected to {} (rtmax={})", nfs_source.host, pool.server_rtmax);

            run_nfs_to_local_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_dir_base,
                aggregate_config,
                pool,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            ).await;
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("bifrost-nfs-to-nfs")
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
                nfs_source.host, src_pool.server_rtmax,
                nfs_target.host, tgt_pool.server_wtmax
            );

            run_nfs_to_nfs_backup(
                control_file,
                meta_dir,
                ctrl_dir,
                source_dir_base,
                target_prefix,
                aggregate_config,
                src_pool,
                tgt_pool,
                stats,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
            ).await;
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
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    directions::run_local_to_nfs_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        Arc::clone(&pool),
        Arc::clone(&stats),
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
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
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
    location: SmbLocation,
    client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    directions::run_local_to_smb_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        location.clone(),
        client,
        stats,
    )
    .await;

    run_smb_target_phases(
        &ctrl_dir,
        &source_dir_base,
        &target_prefix,
        &location,
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
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
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    directions::run_aio_nfs_to_local_pipeline(
        control_file,
        meta_dir.clone(),
        source_dir_base.clone(),
        target_dir_base.clone(),
        aggregate_config,
        pool,
        stats,
    )
    .await;

    run_local_target_phases(
        &ctrl_dir,
        &meta_dir,
        &source_dir_base,
        &target_dir_base,
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
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
    source_pool: Arc<NfsConnectionPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
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
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
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
    location: SmbLocation,
    client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    directions::run_smb_to_local_copy_pipeline(
        control_file,
        meta_dir.clone(),
        source_dir_base.clone(),
        target_dir_base.clone(),
        aggregate_config,
        location,
        client,
        stats,
    )
    .await;

    run_local_target_phases(
        &ctrl_dir,
        &meta_dir,
        &source_dir_base,
        &target_dir_base,
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
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
    source_location: SmbLocation,
    target_location: SmbLocation,
    source_client: Arc<smb_client::Client>,
    target_client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    directions::run_smb_to_smb_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        source_location,
        target_location.clone(),
        source_client,
        target_client,
        stats,
    )
    .await;

    run_smb_target_phases(
        &ctrl_dir,
        &source_dir_base,
        &target_prefix,
        &target_location,
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
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
    source_pool: Arc<NfsConnectionPool>,
    target_location: SmbLocation,
    target_client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    directions::run_nfs_to_smb_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        source_pool,
        target_location.clone(),
        target_client,
        stats,
    )
    .await;

    run_smb_target_phases(
        &ctrl_dir,
        &source_dir_base,
        &target_prefix,
        &target_location,
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
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
    source_location: SmbLocation,
    source_client: Arc<smb_client::Client>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    directions::run_smb_to_nfs_copy_pipeline(
        control_file,
        meta_dir,
        source_dir_base.clone(),
        target_prefix.clone(),
        aggregate_config,
        source_location,
        source_client,
        Arc::clone(&target_pool),
        stats,
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
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
    )
    .await;
}

#[cfg(any(feature = "nfs", feature = "smb"))]
fn run_local_target_phases(
    ctrl_dir: &PathBuf,
    meta_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_dir_base: &PathBuf,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    if enable_hardlink_phase {
        info!("Starting hardlink phase...");
        match hardlink::run_hardlink_phase(ctrl_dir, meta_dir, source_dir_base, target_dir_base) {
            Ok(hl_stats) => {
                info!(
                    "Hardlink phase completed: {} created, {} failed",
                    hl_stats.hardlinks_created, hl_stats.hardlinks_failed
                );
            }
            Err(e) => {
                error!("Hardlink phase failed: {e}");
            }
        }
    }

    if enable_delete_phase {
        info!("Starting delete phase...");
        match delete::run_delete_phase(ctrl_dir, source_dir_base, target_dir_base) {
            Ok(del_stats) => {
                info!(
                    "Delete phase completed: {} files deleted, {} dirs deleted",
                    del_stats.files_deleted, del_stats.dirs_deleted
                );
            }
            Err(e) => {
                error!("Delete phase failed: {e}");
            }
        }
    }

    if enable_mtime_phase {
        info!("Starting mtime phase...");
        match mtime::run_mtime_phase(ctrl_dir, source_dir_base, target_dir_base) {
            Ok(mt_stats) => {
                info!(
                    "Mtime phase completed: {} restored, {} failed",
                    mt_stats.dirs_restored, mt_stats.dirs_failed
                );
            }
            Err(e) => {
                error!("Mtime phase failed: {e}");
            }
        }
    }
}

#[cfg(feature = "nfs")]
async fn run_nfs_target_phases(
    ctrl_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_prefix: &str,
    pool: Arc<NfsConnectionPool>,
    file_cache: crate::nfs::aio::reader::FileHandleCache,
    dir_cache: crate::nfs::aio::writer::DirHandleCache,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    if enable_hardlink_phase {
        info!("NFS: starting hardlink phase...");
        let hl_stats = crate::nfs::aio::hardlink::run_nfs_hardlink_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            Arc::clone(&pool),
            Arc::clone(&file_cache),
            Arc::clone(&dir_cache),
        )
        .await;
        info!(
            "NFS hardlink phase complete: {} created, {} failed",
            hl_stats.hardlinks_created, hl_stats.hardlinks_failed
        );
    }

    if enable_delete_phase {
        info!("NFS: starting delete phase...");
        let del_stats = crate::nfs::aio::delete::run_nfs_delete_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            Arc::clone(&pool),
            Arc::clone(&file_cache),
        )
        .await;
        info!(
            "NFS delete phase complete: {} files, {} dirs deleted, {} failed",
            del_stats.files_deleted, del_stats.dirs_deleted, del_stats.entries_failed
        );
    }

    if enable_mtime_phase {
        info!("NFS: starting mtime phase...");
        let mt_stats = crate::nfs::aio::mtime::run_nfs_mtime_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            pool,
            file_cache,
        )
        .await;
        info!(
            "NFS mtime phase complete: {} dirs restored, {} failed",
            mt_stats.dirs_restored, mt_stats.dirs_failed
        );
    }
}

#[cfg(feature = "smb")]
async fn run_smb_target_phases(
    ctrl_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_prefix: &str,
    location: &SmbLocation,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
) {
    if enable_hardlink_phase {
        info!("SMB: starting hardlink phase...");
        let hl_stats = crate::smb::aio::hardlink::run_smb_hardlink_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            location,
        )
        .await;
        info!(
            "SMB hardlink phase complete: {} created, {} failed",
            hl_stats.hardlinks_created, hl_stats.hardlinks_failed
        );
    }

    if enable_delete_phase {
        info!("SMB: starting delete phase...");
        let del_stats = crate::smb::aio::delete::run_smb_delete_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            location,
        )
        .await;
        info!(
            "SMB delete phase complete: {} files, {} dirs deleted, {} failed",
            del_stats.files_deleted, del_stats.dirs_deleted, del_stats.entries_failed
        );
    }

    if enable_mtime_phase {
        info!("SMB: starting mtime phase...");
        let mt_stats = crate::smb::aio::mtime::run_smb_mtime_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            location,
        )
        .await;
        info!(
            "SMB mtime phase complete: {} dirs restored, {} failed",
            mt_stats.dirs_restored, mt_stats.dirs_failed
        );
    }
}
