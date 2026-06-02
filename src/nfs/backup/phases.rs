//! NFS target post-copy phases (hardlink, delete, mtime).

use std::path::PathBuf;
use std::sync::Arc;

use log::info;

use crate::backup::PhaseFlags;
use crate::nfs::connection::NfsConnectionPool;

pub(crate) async fn run_nfs_target_phases(
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
