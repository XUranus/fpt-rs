//! NFS hardlink phase for the AIO pipeline.
//!
//! Reads the same hardlink control file as the BIO hardlink phase and
//! creates NFS hard links for all files in each inode group (except the first,
//! which was already written by the copy phase).
//!
//! The reader iterates `HardlinkEntry` as interleaved `Inode` / `File` variants.
//! We accumulate them into groups and process each group once complete.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{debug, error, info, warn};
use nfs3_client::nfs3_types::nfs3::{diropargs3, filename3, nfs_fh3, LINK3args, Nfs3Result};

use crate::frame::control_files::find_primary_control_file;
use crate::nfs::aio::reader::{resolve_path, FileHandleCache};
use crate::nfs::aio::writer::{get_or_create_dir, DirHandleCache};
use crate::nfs::connection::NfsConnectionPool;
use crate::scanner::metadata::{HardlinkControlFileReader, HardlinkEntry};

/// Statistics for the NFS hardlink phase.
#[derive(Debug, Default, Clone)]
pub struct NfsHardlinkStats {
    pub groups_processed: u64,
    pub hardlinks_created: u64,
    pub hardlinks_failed: u64,
    pub files_skipped: u64,
}

/// Run the NFS hardlink phase.
///
/// For each inode group in the hardlink control file in `ctrl_dir`, the first file (already
/// written by the copy phase) is the "primary"; all subsequent files in the
/// group are created as hard links via the NFS `link` RPC.
pub async fn run_nfs_hardlink_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_prefix: &str,
    pool: Arc<NfsConnectionPool>,
    src_cache: FileHandleCache,
    dst_dir_cache: DirHandleCache,
) -> NfsHardlinkStats {
    let mut stats = NfsHardlinkStats::default();
    let Some(ctrl_path) = find_primary_control_file(ctrl_dir, "hardlink") else {
        info!("NFS hardlink phase: no hardlink control file found, skipping");
        return stats;
    };

    info!("NFS hardlink phase: processing {:?}", ctrl_path);

    let root_fh = pool.root_fh();

    let reader = match HardlinkControlFileReader::open(&ctrl_path) {
        Ok(r) => r,
        Err(e) => {
            error!("NFS hardlink phase: cannot open hardlink control file: {e}");
            return stats;
        }
    };

    // The control file interleaves Inode and File entries.
    // Accumulate files per inode group and process on next Inode or at end.
    let mut current_files: Vec<String> = Vec::new();

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("NFS hardlink phase: read error: {e}");
                continue;
            }
        };

        match entry {
            HardlinkEntry::Inode(_) => {
                // Process the previous group (if any) before starting a new one.
                if !current_files.is_empty() {
                    process_group(
                        &pool,
                        &src_cache,
                        &dst_dir_cache,
                        &root_fh,
                        source_dir_base,
                        target_prefix,
                        &current_files,
                        &mut stats,
                    )
                    .await;
                    current_files.clear();
                }
                stats.groups_processed += 1;
            }
            HardlinkEntry::File(f) => {
                current_files.push(f.path);
            }
        }
    }

    // Process the last group.
    if !current_files.is_empty() {
        process_group(
            &pool,
            &src_cache,
            &dst_dir_cache,
            &root_fh,
            source_dir_base,
            target_prefix,
            &current_files,
            &mut stats,
        )
        .await;
    }

    info!(
        "NFS hardlink phase complete: {} created, {} failed, {} skipped",
        stats.hardlinks_created, stats.hardlinks_failed, stats.files_skipped
    );
    stats
}

/// Process one inode group: resolve the primary file handle then create `link`
/// RPCs for all secondary files in the group.
async fn process_group(
    pool: &NfsConnectionPool,
    src_cache: &FileHandleCache,
    dst_dir_cache: &DirHandleCache,
    root_fh: &nfs_fh3,
    source_dir_base: &Path,
    target_prefix: &str,
    files: &[String],
    stats: &mut NfsHardlinkStats,
) {
    if files.is_empty() {
        return;
    }

    // The first file is the primary (already backed up in the copy phase).
    let primary_nfs = to_target_relative_path(source_dir_base, target_prefix, &files[0]);
    let primary_fh = match resolve_path(pool, src_cache, &primary_nfs, root_fh).await {
        Ok(fh) => fh,
        Err(e) => {
            warn!("NFS hardlink: cannot resolve primary {primary_nfs}: {e}");
            stats.hardlinks_failed += (files.len() - 1) as u64;
            stats.files_skipped += 1;
            return;
        }
    };
    stats.files_skipped += 1; // primary is skipped (already exists)

    // Create hard links for secondary files.
    for secondary_path in files.iter().skip(1) {
        let nfs_path = to_target_relative_path(source_dir_base, target_prefix, secondary_path);
        let (parent, link_name) = split_path(&nfs_path);

        let parent_fh = match get_or_create_dir(pool, dst_dir_cache, &parent, root_fh).await {
            Ok(fh) => fh,
            Err(e) => {
                error!("NFS hardlink: mkdir {parent}: {e}");
                stats.hardlinks_failed += 1;
                continue;
            }
        };

        let link_res = {
            let mut conn = pool.acquire().await;
            conn.link(&LINK3args {
                file: primary_fh.clone(),
                link: diropargs3 {
                    dir: parent_fh,
                    name: filename3::from(link_name.as_bytes()),
                },
            })
            .await
        };

        match link_res {
            Ok(Nfs3Result::Ok(_)) => {
                debug!("NFS link {nfs_path} -> {primary_nfs}");
                stats.hardlinks_created += 1;
            }
            Ok(Nfs3Result::Err((stat, _))) => {
                error!("NFS link {nfs_path}: NFS error {stat}");
                stats.hardlinks_failed += 1;
            }
            Err(e) => {
                error!("NFS link {nfs_path}: {e}");
                stats.hardlinks_failed += 1;
            }
        }
    }
}

fn to_target_relative_path(base: &Path, target_prefix: &str, path: &str) -> String {
    let rel = Path::new(path)
        .strip_prefix(base)
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| {
            let p = Path::new(path);
            let logical_root_name = base.file_name().and_then(|n| n.to_str());
            let first_segment = p
                .strip_prefix("/")
                .ok()
                .and_then(|p| p.iter().next())
                .and_then(|s| s.to_str());
            if logical_root_name.is_some() && logical_root_name == first_segment {
                p.strip_prefix("/")
                    .map(|r| r.to_path_buf())
                    .unwrap_or_else(|_| PathBuf::from(path))
            } else {
                p.file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(path))
            }
        });
    let prefixed = if target_prefix.is_empty() {
        rel
    } else {
        Path::new(target_prefix).join(rel)
    };
    prefixed.to_string_lossy().into_owned()
}

fn split_path(path: &str) -> (String, String) {
    let p = Path::new(path);
    let parent = p
        .parent()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = p
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    (parent, name)
}
