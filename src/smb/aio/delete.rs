//! SMB delete phase for the async backup pipeline.

use std::path::Path;

use log::{debug, error, info, warn};

use crate::scanner::metadata::{DeleteControlFileReader, DeleteEntryType};
use crate::smb::SmbLocation;

#[derive(Debug, Default, Clone)]
pub struct SmbDeleteStats {
    pub entries_processed: u64,
    pub files_deleted: u64,
    pub dirs_deleted: u64,
    pub entries_failed: u64,
    pub entries_skipped: u64,
}

pub async fn run_smb_delete_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_prefix: &str,
    location: &SmbLocation,
) -> SmbDeleteStats {
    let ctrl_path = ctrl_dir.join("delete.txt");
    let mut stats = SmbDeleteStats::default();

    if !ctrl_path.exists() {
        info!("SMB delete phase: no delete.txt found, skipping");
        return stats;
    }

    let client = match crate::smb::aio::connect_client(location).await {
        Ok(client) => client,
        Err(e) => {
            error!("SMB delete phase: connect failed: {e}");
            return stats;
        }
    };

    let reader = match DeleteControlFileReader::open(&ctrl_path) {
        Ok(r) => r,
        Err(e) => {
            error!("SMB delete phase: cannot open delete.txt: {e}");
            let _ = client.close().await;
            return stats;
        }
    };

    let mut file_paths = Vec::new();
    let mut dir_paths = Vec::new();
    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("SMB delete phase: read error: {e}");
                stats.entries_failed += 1;
                continue;
            }
        };
        stats.entries_processed += 1;
        match entry.entry_type {
            DeleteEntryType::File => file_paths.push(entry.path),
            DeleteEntryType::Dir => dir_paths.push(entry.path),
        }
    }

    for path in &file_paths {
        let rel = crate::smb::aio::target_relative_path(source_dir_base, target_prefix, path);
        match mark_delete_pending(&client, location, &rel, false).await {
            Ok(true) => {
                debug!("SMB deleted file {}", rel);
                stats.files_deleted += 1;
            }
            Ok(false) => {
                stats.entries_skipped += 1;
            }
            Err(e) => {
                error!("SMB delete file {}: {}", rel, e);
                stats.entries_failed += 1;
            }
        }
    }

    dir_paths.sort_by(|a, b| b.cmp(a));
    for path in &dir_paths {
        let rel = crate::smb::aio::target_relative_path(source_dir_base, target_prefix, path);
        match mark_delete_pending(&client, location, &rel, true).await {
            Ok(true) => {
                debug!("SMB deleted dir {}", rel);
                stats.dirs_deleted += 1;
            }
            Ok(false) => {
                stats.entries_skipped += 1;
            }
            Err(e) => {
                error!("SMB delete dir {}: {}", rel, e);
                stats.entries_failed += 1;
            }
        }
    }

    if let Err(e) = client.close().await {
        warn!("SMB delete phase: client close failed: {e}");
    }

    info!(
        "SMB delete phase complete: {} files, {} dirs deleted, {} failed, {} skipped",
        stats.files_deleted, stats.dirs_deleted, stats.entries_failed, stats.entries_skipped
    );
    stats
}

async fn mark_delete_pending(
    client: &smb_client::Client,
    location: &SmbLocation,
    relative_path: &str,
    expect_dir: bool,
) -> Result<bool, String> {
    let unc = crate::smb::aio::relative_unc_path(location, relative_path)?;
    let open_args = smb_client::FileCreateArgs::make_open_existing(
        smb_client::FileAccessMask::new().with_generic_all(true),
    );

    let resource = match client.create_file(&unc, &open_args).await {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("Object Name Not Found") || msg.contains("Object Path Not Found") {
                return Ok(false);
            }
            return Err(format!("open {}: {}", unc, msg));
        }
    };

    let delete_info = smb_client::FileDispositionInformation::default();
    match resource {
        smb_client::Resource::File(file) => {
            if expect_dir {
                let _ = file.close().await;
                return Ok(false);
            }
            file.set_info(delete_info)
                .await
                .map_err(|e| format!("set delete {}: {e}", unc))?;
            file.close()
                .await
                .map_err(|e| format!("close {}: {e}", unc))?;
        }
        smb_client::Resource::Directory(dir) => {
            if !expect_dir {
                let _ = dir.close().await;
                return Ok(false);
            }
            dir.set_info(delete_info)
                .await
                .map_err(|e| format!("set delete {}: {e}", unc))?;
            dir.close()
                .await
                .map_err(|e| format!("close {}: {e}", unc))?;
        }
        other => {
            crate::smb::aio::close_resource(other).await?;
            return Ok(false);
        }
    }

    Ok(true)
}
