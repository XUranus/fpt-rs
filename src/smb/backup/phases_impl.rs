//! Post-copy phases implementation for SMB targets.

use std::path::Path;

use log::info;

use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use crate::smb::SmbLocation;

/// SMB post-copy phases (hardlink, delete, mtime).
#[allow(dead_code)]
pub struct SmbPostCopyPhases<'a> {
    pub location: &'a SmbLocation,
}

#[allow(async_fn_in_trait)]
impl<'a> crate::backup::aio::phases_trait::PostCopyPhases for SmbPostCopyPhases<'a> {
    async fn run_hardlink_phase(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        target_prefix: &str,
        _phase_flags: PhaseFlags,
        _retry_policy: RetryPolicy,
        _failure_recorder: Option<&FailureRecorder>,
    ) {
        info!("SMB: starting hardlink phase...");
        let hl_stats = crate::smb::backup::hardlink::run_smb_hardlink_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            self.location,
        )
        .await;
        info!(
            "SMB hardlink phase complete: {} created, {} failed",
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
        info!("SMB: starting delete phase...");
        let del_stats = crate::smb::backup::delete::run_smb_delete_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            self.location,
        )
        .await;
        info!(
            "SMB delete phase complete: {} files, {} dirs deleted, {} failed",
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
        info!("SMB: starting mtime phase...");
        let mt_stats = crate::smb::backup::mtime::run_smb_mtime_phase(
            ctrl_dir,
            source_dir_base,
            target_prefix,
            self.location,
        )
        .await;
        info!(
            "SMB mtime phase complete: {} dirs restored, {} failed",
            mt_stats.dirs_restored, mt_stats.dirs_failed
        );
    }
}
