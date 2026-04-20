//! NFS mtime phase for the AIO pipeline.
//!
//! Reads the same `mtime.txt` control file as the BIO mtime phase and restores
//! directory modification times on the NFS target using `setattr` RPCs with
//! `SET_TO_CLIENT_TIME`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use log::{debug, error, info, warn};
use nfs3_client::nfs3_types::nfs3::{
    Nfs3Result, SETATTR3args, nfstime3, sattr3, sattrguard3,
    set_atime, set_gid3, set_mode3, set_mtime, set_size3, set_uid3,
};

use crate::nfs::aio::reader::{FileHandleCache, resolve_path};
use crate::nfs::connection::NfsConnectionPool;
use crate::scanner::metadata::MtimeControlFileReader;

/// Statistics for the NFS mtime phase.
#[derive(Debug, Default, Clone)]
pub struct NfsMtimeStats {
    pub dirs_processed: u64,
    pub dirs_restored: u64,
    pub dirs_failed: u64,
    pub dirs_skipped: u64,
}

/// Run the NFS mtime phase.
///
/// Reads `ctrl_dir/mtime.txt` and calls `setattr` with `SET_TO_CLIENT_TIME`
/// on each directory's NFS target path to restore the original mtime.
pub async fn run_nfs_mtime_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_prefix: &str,
    pool: Arc<NfsConnectionPool>,
    dir_cache: FileHandleCache,
) -> NfsMtimeStats {
    let ctrl_path = ctrl_dir.join("mtime.txt");
    let mut stats = NfsMtimeStats::default();

    if !ctrl_path.exists() {
        info!("NFS mtime phase: no mtime.txt found, skipping");
        return stats;
    }

    info!("NFS mtime phase: processing {:?}", ctrl_path);

    let root_fh = pool.root_fh();

    let reader = match MtimeControlFileReader::open(&ctrl_path) {
        Ok(r) => r,
        Err(e) => {
            error!("NFS mtime phase: cannot open mtime.txt: {e}");
            return stats;
        }
    };

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("NFS mtime phase: read error: {e}");
                stats.dirs_failed += 1;
                continue;
            }
        };

        stats.dirs_processed += 1;
        let nfs_path = to_target_relative_path(source_dir_base, target_prefix, &entry.path);

        let dir_fh = match resolve_path(&pool, &dir_cache, &nfs_path, &root_fh).await {
            Ok(fh) => fh,
            Err(e) => {
                warn!("NFS mtime: cannot resolve {nfs_path}: {e}");
                stats.dirs_skipped += 1;
                continue;
            }
        };

        // `MtimeDirEntry.mtime` is u64; nfstime3.seconds is u32.
        // Truncate to u32 (year 2106 safe).
        let mtime_secs = entry.mtime as u32;

        let new_attrs = sattr3 {
            mode: set_mode3::None,
            uid: set_uid3::None,
            gid: set_gid3::None,
            size: set_size3::None,
            atime: set_atime::SET_TO_SERVER_TIME,
            mtime: set_mtime::SET_TO_CLIENT_TIME(nfstime3 {
                seconds: mtime_secs,
                nseconds: 0,
            }),
        };

        let res = {
            let mut conn = pool.acquire().await;
            conn.setattr(&SETATTR3args {
                object: dir_fh,
                new_attributes: new_attrs,
                guard: sattrguard3::None,
            })
            .await
        };

        match res {
            Ok(Nfs3Result::Ok(_)) => {
                debug!("NFS mtime restored: {nfs_path}");
                stats.dirs_restored += 1;
            }
            Ok(Nfs3Result::Err((stat, _))) => {
                error!("NFS setattr mtime {nfs_path}: NFS error {stat}");
                stats.dirs_failed += 1;
            }
            Err(e) => {
                error!("NFS setattr mtime {nfs_path}: {e}");
                stats.dirs_failed += 1;
            }
        }
    }

    info!(
        "NFS mtime phase complete: {} restored, {} failed, {} skipped",
        stats.dirs_restored, stats.dirs_failed, stats.dirs_skipped
    );
    stats
}

fn to_target_relative_path(base: &Path, target_prefix: &str, path: &str) -> String {
    let rel = Path::new(path)
        .strip_prefix(base)
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| {
            let p = Path::new(path);
            let logical_root_name = base.file_name().and_then(|n| n.to_str());
            let first_segment = p.strip_prefix("/").ok().and_then(|p| p.iter().next()).and_then(|s| s.to_str());
            if logical_root_name.is_some() && logical_root_name == first_segment {
                p.strip_prefix("/").map(|r| r.to_path_buf()).unwrap_or_else(|_| PathBuf::from(path))
            } else {
                p.file_name().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(path))
            }
        });
    let prefixed = if target_prefix.is_empty() {
        rel
    } else {
        Path::new(target_prefix).join(rel)
    };
    prefixed.to_string_lossy().into_owned()
}
