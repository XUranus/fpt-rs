//! SMB target post-copy phases (hardlink, delete, mtime).

use std::path::PathBuf;

use log::info;

use crate::backup::PhaseFlags;
use crate::smb::SmbLocation;

pub(crate) async fn run_smb_target_phases(
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
