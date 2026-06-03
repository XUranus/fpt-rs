//! Post-copy phases implementation for local filesystem targets.

use std::path::Path;

use log::{error, info};

use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};

/// Local filesystem post-copy phases (hardlink, delete, mtime).
#[allow(dead_code)]
pub struct LocalPostCopyPhases;

#[allow(async_fn_in_trait)]
impl crate::backup::aio::phases_trait::PostCopyPhases for LocalPostCopyPhases {
    async fn run_hardlink_phase(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        _target_prefix: &str,
        _phase_flags: PhaseFlags,
        retry_policy: RetryPolicy,
        failure_recorder: Option<&FailureRecorder>,
    ) {
        info!("Starting hardlink phase...");
        match super::hardlink::run_hardlink_phase(
            ctrl_dir,
            &Path::new(""),
            source_dir_base,
            &ctrl_dir.join("target"),
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

    async fn run_delete_phase(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        _target_prefix: &str,
        _phase_flags: PhaseFlags,
        retry_policy: RetryPolicy,
        failure_recorder: Option<&FailureRecorder>,
    ) {
        info!("Starting delete phase...");
        match super::delete::run_delete_phase(
            ctrl_dir,
            source_dir_base,
            &ctrl_dir.join("target"),
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

    async fn run_mtime_phase(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        _target_prefix: &str,
        _phase_flags: PhaseFlags,
        retry_policy: RetryPolicy,
        failure_recorder: Option<&FailureRecorder>,
    ) {
        info!("Starting mtime phase...");
        match super::mtime::run_mtime_phase(
            ctrl_dir,
            source_dir_base,
            &ctrl_dir.join("target"),
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
