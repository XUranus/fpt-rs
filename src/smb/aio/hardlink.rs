//! SMB hardlink phase for the async backup pipeline.

use std::path::Path;

use log::{debug, error, info, warn};

use crate::scanner::metadata::{HardlinkControlFileReader, HardlinkEntry};
use crate::smb::SmbLocation;

#[derive(Debug, Default, Clone)]
pub struct SmbHardlinkStats {
    pub groups_processed: u64,
    pub hardlinks_created: u64,
    pub hardlinks_failed: u64,
    pub files_skipped: u64,
}

pub async fn run_smb_hardlink_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_prefix: &str,
    location: &SmbLocation,
) -> SmbHardlinkStats {
    let ctrl_path = ctrl_dir.join("hardlink.txt");
    let mut stats = SmbHardlinkStats::default();

    if !ctrl_path.exists() {
        info!("SMB hardlink phase: no hardlink.txt found, skipping");
        return stats;
    }

    let client = match crate::smb::aio::connect_client(location).await {
        Ok(client) => client,
        Err(e) => {
            error!("SMB hardlink phase: connect failed: {e}");
            return stats;
        }
    };

    let reader = match HardlinkControlFileReader::open(&ctrl_path) {
        Ok(r) => r,
        Err(e) => {
            error!("SMB hardlink phase: cannot open hardlink.txt: {e}");
            let _ = client.close().await;
            return stats;
        }
    };

    let mut current_files: Vec<String> = Vec::new();
    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("SMB hardlink phase: read error: {e}");
                continue;
            }
        };

        match entry {
            HardlinkEntry::Inode(_) => {
                if !current_files.is_empty() {
                    process_group(&client, source_dir_base, target_prefix, location, &current_files, &mut stats)
                        .await;
                    current_files.clear();
                }
                stats.groups_processed += 1;
            }
            HardlinkEntry::File(f) => current_files.push(f.path),
        }
    }

    if !current_files.is_empty() {
        process_group(&client, source_dir_base, target_prefix, location, &current_files, &mut stats)
            .await;
    }

    if let Err(e) = client.close().await {
        warn!("SMB hardlink phase: client close failed: {e}");
    }

    info!(
        "SMB hardlink phase complete: {} created, {} failed, {} skipped",
        stats.hardlinks_created, stats.hardlinks_failed, stats.files_skipped
    );
    stats
}

async fn process_group(
    client: &smb_client::Client,
    source_dir_base: &Path,
    target_prefix: &str,
    location: &SmbLocation,
    files: &[String],
    stats: &mut SmbHardlinkStats,
) {
    if files.is_empty() {
        return;
    }

    let primary_rel = crate::smb::aio::target_relative_path(source_dir_base, target_prefix, &files[0]);
    let primary_unc = match crate::smb::aio::relative_unc_path(location, &primary_rel) {
        Ok(unc) => unc,
        Err(e) => {
            error!("SMB hardlink: invalid primary path {}: {}", files[0], e);
            stats.hardlinks_failed += files.len().saturating_sub(1) as u64;
            return;
        }
    };
    let primary_share_path = crate::smb::aio::share_relative_path(location, &primary_rel);

    let access = smb_client::FileAccessMask::new().with_generic_all(true);
    let open_args = smb_client::FileCreateArgs::make_open_existing(access);
    let primary = match client.create_file(&primary_unc, &open_args).await {
        Ok(resource) => match resource {
            smb_client::Resource::File(file) => file,
            other => {
                let _ = crate::smb::aio::close_resource(other).await;
                warn!("SMB hardlink: primary {} is not a file", primary_unc);
                stats.hardlinks_failed += files.len().saturating_sub(1) as u64;
                return;
            }
        },
        Err(e) => {
            warn!("SMB hardlink: cannot open primary {}: {}", primary_unc, e);
            stats.hardlinks_failed += files.len().saturating_sub(1) as u64;
            return;
        }
    };
    stats.files_skipped += 1;

    for secondary_path in files.iter().skip(1) {
        let secondary_rel =
            crate::smb::aio::target_relative_path(source_dir_base, target_prefix, secondary_path);
        let share_path = crate::smb::aio::share_relative_path(location, &secondary_rel);
        let link_info = smb_client::FileLinkInformation {
            replace_if_exists: false.into(),
            file_name: share_path.as_str().into(),
        };

        match primary.set_info(link_info).await {
            Ok(()) => {
                debug!("SMB hardlink {} -> {}", share_path, primary_share_path);
                stats.hardlinks_created += 1;
            }
            Err(e) => {
                error!("SMB hardlink {} -> {}: {}", share_path, primary_share_path, e);
                stats.hardlinks_failed += 1;
            }
        }
    }

    if let Err(e) = primary.close().await {
        warn!("SMB hardlink: close primary {} failed: {}", primary_unc, e);
    }
}
