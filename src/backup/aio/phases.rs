//! Post-copy phase runners for async backup directions.

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(any(feature = "nfs", feature = "smb"))]
use log::error;
use log::info;

#[cfg(any(feature = "nfs", feature = "smb"))]
use crate::backup::bio::{delete, hardlink, mtime};
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
#[cfg(feature = "nfs")]
use crate::nfs::connection::NfsConnectionPool;
#[cfg(feature = "smb")]
use crate::smb::SmbLocation;

#[cfg(any(feature = "nfs", feature = "smb"))]
pub(super) fn run_local_target_phases(
    ctrl_dir: &PathBuf,
    meta_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_dir_base: &PathBuf,
    phase_flags: PhaseFlags,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
) {
    if phase_flags.hardlink {
        info!("Starting hardlink phase...");
        match hardlink::run_hardlink_phase(
            ctrl_dir,
            meta_dir,
            source_dir_base,
            target_dir_base,
            retry_policy,
            failure_recorder,
        ) {
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

    if phase_flags.delete {
        info!("Starting delete phase...");
        match delete::run_delete_phase(
            ctrl_dir,
            source_dir_base,
            target_dir_base,
            retry_policy,
            failure_recorder,
        ) {
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

    if phase_flags.mtime {
        info!("Starting mtime phase...");
        match mtime::run_mtime_phase(
            ctrl_dir,
            source_dir_base,
            target_dir_base,
            retry_policy,
            failure_recorder,
        ) {
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
pub(super) async fn run_nfs_target_phases(
    ctrl_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_prefix: &str,
    pool: Arc<NfsConnectionPool>,
    file_cache: crate::nfs::aio::reader::FileHandleCache,
    dir_cache: crate::nfs::aio::writer::DirHandleCache,
    phase_flags: PhaseFlags,
) {
    if phase_flags.hardlink {
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

    if phase_flags.delete {
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

    if phase_flags.mtime {
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
pub(super) async fn run_smb_target_phases(
    ctrl_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_prefix: &str,
    location: &SmbLocation,
    phase_flags: PhaseFlags,
) {
    if phase_flags.hardlink {
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

    if phase_flags.delete {
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

    if phase_flags.mtime {
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
