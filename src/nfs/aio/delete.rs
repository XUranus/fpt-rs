//! NFS delete phase for the AIO pipeline.
//!
//! Reads the same delete control file as the BIO delete phase and removes
//! the corresponding files and directories from the NFS target using `remove`
//! and `rmdir` RPCs.

use std::path::Path;
use std::sync::Arc;

use log::{debug, error, info, warn};
use nfs3_client::nfs3_types::nfs3::{
    diropargs3, filename3, nfs_fh3, nfsstat3, Nfs3Result, REMOVE3args, RMDIR3args,
};

use crate::frame::control_files::find_primary_control_file;
use crate::nfs::aio::reader::{resolve_path, FileHandleCache};
use crate::nfs::connection::NfsConnectionPool;
use crate::nfs::error::NfsError;
use crate::scanner::metadata::{DeleteControlFileReader, DeleteEntryType};

/// Statistics for the NFS delete phase.
#[derive(Debug, Default, Clone)]
pub struct NfsDeleteStats {
    pub entries_processed: u64,
    pub files_deleted: u64,
    pub dirs_deleted: u64,
    pub entries_failed: u64,
    pub entries_skipped: u64,
}

/// Run the NFS delete phase.
///
/// Reads the delete control file from `ctrl_dir` and calls `remove` / `rmdir` on the NFS target
/// for each entry whose source path (relative to `source_dir_base`) exists.
pub async fn run_nfs_delete_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_prefix: &str,
    pool: Arc<NfsConnectionPool>,
    dir_cache: FileHandleCache,
) -> NfsDeleteStats {
    let mut stats = NfsDeleteStats::default();
    let Some(delete_ctrl_path) = find_primary_control_file(ctrl_dir, "delete") else {
        info!("NFS delete phase: no delete control file found, skipping");
        return stats;
    };

    info!("NFS delete phase: processing {:?}", delete_ctrl_path);

    let root_fh = pool.root_fh();

    let reader = match DeleteControlFileReader::open(&delete_ctrl_path) {
        Ok(r) => r,
        Err(e) => {
            error!("NFS delete phase: failed to open delete control file: {e}");
            return stats;
        }
    };

    // Collect entries; delete files first, then directories deepest-first.
    let mut file_paths: Vec<String> = Vec::new();
    let mut dir_paths: Vec<String> = Vec::new();

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("NFS delete phase: read error: {e}");
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

    // Delete files.
    for path_str in &file_paths {
        let nfs_path = crate::path_util::target_relative_path(source_dir_base, target_prefix, path_str);
        match delete_file(&pool, &dir_cache, &root_fh, &nfs_path).await {
            Ok(true) => {
                debug!("NFS deleted file {nfs_path}");
                stats.files_deleted += 1;
            }
            Ok(false) => {
                stats.entries_skipped += 1;
            }
            Err(e) => {
                error!("NFS delete file {nfs_path}: {e}");
                stats.entries_failed += 1;
            }
        }
    }

    // Delete directories deepest-first (reverse sort).
    dir_paths.sort_by(|a, b| b.cmp(a));
    for path_str in &dir_paths {
        let nfs_path = crate::path_util::target_relative_path(source_dir_base, target_prefix, path_str);
        match delete_dir(&pool, &dir_cache, &root_fh, &nfs_path).await {
            Ok(true) => {
                debug!("NFS deleted dir {nfs_path}");
                stats.dirs_deleted += 1;
            }
            Ok(false) => {
                stats.entries_skipped += 1;
            }
            Err(e) => {
                error!("NFS delete dir {nfs_path}: {e}");
                stats.entries_failed += 1;
            }
        }
    }

    info!(
        "NFS delete phase complete: {} files, {} dirs deleted, {} failed, {} skipped",
        stats.files_deleted, stats.dirs_deleted, stats.entries_failed, stats.entries_skipped
    );
    stats
}

/// Remove a regular file via the NFS `remove` RPC.
/// Returns `Ok(true)` if removed, `Ok(false)` if not found (NFS3ERR_NOENT).
async fn delete_file(
    pool: &NfsConnectionPool,
    dir_cache: &FileHandleCache,
    root_fh: &nfs_fh3,
    path: &str,
) -> Result<bool, NfsError> {
    let (parent, name) = split_path(path);
    let dir_fh = resolve_path(pool, dir_cache, &parent, root_fh).await?;
    let mut conn = pool.acquire().await;
    let res = conn
        .remove(&REMOVE3args {
            object: diropargs3 {
                dir: dir_fh,
                name: filename3::from(name.as_bytes()),
            },
        })
        .await?;

    match res {
        Nfs3Result::Ok(_) => Ok(true),
        Nfs3Result::Err((nfsstat3::NFS3ERR_NOENT, _)) => Ok(false),
        Nfs3Result::Err((stat, _)) => Err(NfsError::Nfs(stat, format!("remove {path}"))),
    }
}

/// Remove a directory via the NFS `rmdir` RPC.
/// Returns `Ok(true)` if removed, `Ok(false)` if not found.
async fn delete_dir(
    pool: &NfsConnectionPool,
    dir_cache: &FileHandleCache,
    root_fh: &nfs_fh3,
    path: &str,
) -> Result<bool, NfsError> {
    let (parent, name) = split_path(path);
    let parent_fh = resolve_path(pool, dir_cache, &parent, root_fh).await?;
    let mut conn = pool.acquire().await;
    let res = conn
        .rmdir(&RMDIR3args {
            object: diropargs3 {
                dir: parent_fh,
                name: filename3::from(name.as_bytes()),
            },
        })
        .await?;

    match res {
        Nfs3Result::Ok(_) => Ok(true),
        Nfs3Result::Err((nfsstat3::NFS3ERR_NOENT, _)) => Ok(false),
        Nfs3Result::Err((stat, _)) => Err(NfsError::Nfs(stat, format!("rmdir {path}"))),
    }
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
