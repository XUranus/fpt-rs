//! SMB mtime phase for the async backup pipeline.

use std::path::Path;

use log::{debug, error, info, warn};

use crate::frame::control_files::find_primary_control_file;
use crate::scanner::metadata::MtimeControlFileReader;
use crate::smb::SmbLocation;

#[derive(Debug, Default, Clone)]
pub struct SmbMtimeStats {
    pub dirs_processed: u64,
    pub dirs_restored: u64,
    pub dirs_failed: u64,
    pub dirs_skipped: u64,
}

pub async fn run_smb_mtime_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_prefix: &str,
    location: &SmbLocation,
) -> SmbMtimeStats {
    let mut stats = SmbMtimeStats::default();
    let Some(ctrl_path) = find_primary_control_file(ctrl_dir, "mtime") else {
        info!("SMB mtime phase: no mtime control file found, skipping");
        return stats;
    };

    let client = match crate::smb::connect_client(location).await {
        Ok(client) => client,
        Err(e) => {
            error!("SMB mtime phase: connect failed: {e}");
            return stats;
        }
    };

    let reader = match MtimeControlFileReader::open(&ctrl_path) {
        Ok(r) => r,
        Err(e) => {
            error!("SMB mtime phase: cannot open mtime control file: {e}");
            let _ = client.close().await;
            return stats;
        }
    };

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("SMB mtime phase: read error: {e}");
                stats.dirs_failed += 1;
                continue;
            }
        };

        stats.dirs_processed += 1;
        let rel =
            crate::smb::target_relative_path(source_dir_base, target_prefix, &entry.path);
        let unc = match crate::smb::relative_unc_path(location, &rel) {
            Ok(unc) => unc,
            Err(e) => {
                warn!("SMB mtime: invalid path {}: {}", rel, e);
                stats.dirs_skipped += 1;
                continue;
            }
        };

        let open_args = smb_client::FileCreateArgs::make_open_existing(
            smb_client::FileAccessMask::new().with_generic_all(true),
        );
        let resource = match client.create_file(&unc, &open_args).await {
            Ok(r) => r,
            Err(e) => {
                warn!("SMB mtime: open {} failed: {}", unc, e);
                stats.dirs_skipped += 1;
                continue;
            }
        };

        let dir = match resource {
            smb_client::Resource::Directory(dir) => dir,
            other => {
                let _ = crate::smb::close_resource(other).await;
                stats.dirs_skipped += 1;
                continue;
            }
        };

        let current = match dir.query_info::<smb_client::FileBasicInformation>().await {
            Ok(info) => info,
            Err(e) => {
                warn!("SMB mtime: query basic {} failed: {}", unc, e);
                let _ = dir.close().await;
                stats.dirs_failed += 1;
                continue;
            }
        };

        let updated = smb_client::FileBasicInformation {
            creation_time: current.creation_time,
            last_access_time: unix_secs_to_filetime(entry.atime),
            last_write_time: unix_secs_to_filetime(entry.mtime),
            change_time: current.change_time,
            file_attributes: current.file_attributes,
        };

        match dir.set_info(updated).await {
            Ok(()) => {
                debug!("SMB mtime restored {}", rel);
                stats.dirs_restored += 1;
            }
            Err(e) => {
                error!("SMB mtime {}: {}", rel, e);
                stats.dirs_failed += 1;
            }
        }

        if let Err(e) = dir.close().await {
            warn!("SMB mtime: close {} failed: {}", unc, e);
        }
    }

    if let Err(e) = client.close().await {
        warn!("SMB mtime phase: client close failed: {e}");
    }

    info!(
        "SMB mtime phase complete: {} restored, {} failed, {} skipped",
        stats.dirs_restored, stats.dirs_failed, stats.dirs_skipped
    );
    stats
}

fn unix_secs_to_filetime(secs: u64) -> smb_client::binrw_util::prelude::FileTime {
    const FILETIME_EPOCH_OFFSET_SECS: u64 = 11_644_473_600;
    const TICKS_PER_SEC: u64 = 10_000_000;
    smb_client::binrw_util::prelude::FileTime::from(
        (secs + FILETIME_EPOCH_OFFSET_SECS) * TICKS_PER_SEC,
    )
}
