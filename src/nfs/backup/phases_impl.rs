//! Post-copy phases implementation for NFS targets.

use std::path::Path;
use std::sync::Arc;

use log::info;

use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::nfs::connection::NfsConnectionPool;

/// NFS post-copy phases (hardlink, delete, mtime).
#[allow(dead_code)]
pub struct NfsPostCopyPhases {
    pub pool: Arc<NfsConnectionPool>,
    pub file_cache: crate::nfs::aio::reader::FileHandleCache,
    pub dir_cache: crate::nfs::aio::writer::DirHandleCache,
}

#[allow(async_fn_in_trait)]
impl crate::backup::aio::phases_trait::PostCopyPhases for NfsPostCopyPhases {
    async fn run_hardlink_phase(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        target_prefix: &str,
        _phase_flags: PhaseFlags,
        _retry_policy: RetryPolicy,
        _failure_recorder: Option<&FailureRecorder>,
    ) {
        info!("NFS: starting hardlink phase...");
        let hl_stats = crate::nfs::aio::hardlink::run_nfs_hardlink_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            Arc::clone(&self.pool),
            Arc::clone(&self.file_cache),
            Arc::clone(&self.dir_cache),
        )
        .await;
        info!(
            "NFS hardlink phase complete: {} created, {} failed",
            hl_stats.hardlinks_created, hl_stats.hardlinks_failed
        );
    }

    async fn run_delete_phase(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        target_prefix: &str,
        _phase_flags: PhaseFlags,
        _retry_policy: RetryPolicy,
        _failure_recorder: Option<&FailureRecorder>,
    ) {
        info!("NFS: starting delete phase...");
        let del_stats = crate::nfs::aio::delete::run_nfs_delete_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            Arc::clone(&self.pool),
            Arc::clone(&self.file_cache),
        )
        .await;
        info!(
            "NFS delete phase complete: {} files, {} dirs deleted, {} failed",
            del_stats.files_deleted, del_stats.dirs_deleted, del_stats.entries_failed
        );
    }

    async fn run_mtime_phase(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        target_prefix: &str,
        _phase_flags: PhaseFlags,
        _retry_policy: RetryPolicy,
        _failure_recorder: Option<&FailureRecorder>,
    ) {
        info!("NFS: starting mtime phase...");
        let mt_stats = crate::nfs::aio::mtime::run_nfs_mtime_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            Arc::clone(&self.pool),
            Arc::clone(&self.file_cache),
        )
        .await;
        info!(
            "NFS mtime phase complete: {} dirs restored, {} failed",
            mt_stats.dirs_restored, mt_stats.dirs_failed
        );
    }
}
