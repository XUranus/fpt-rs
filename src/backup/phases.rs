use std::path::Path;

use log::{error, info};

use crate::backup::bio::{delete, hardlink, mtime};
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};

pub(crate) fn run_local_followup_phases(
    phases: PhaseFlags,
    ctrl_dir: &Path,
    meta_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
) {
    if phases.hardlink {
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
                error!("Hardlink phase failed: {}", e);
            }
        }
    }

    if phases.delete {
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
                error!("Delete phase failed: {}", e);
            }
        }
    }

    if phases.mtime {
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
                error!("Mtime phase failed: {}", e);
            }
        }
    }
}
