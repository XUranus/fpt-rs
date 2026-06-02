//! NFS mtime phase for the AIO pipeline.
//!
//! Reads the same mtime control file as the BIO mtime phase and restores
//! directory modification times on the NFS target using `setattr` RPCs with
//! `SET_TO_CLIENT_TIME`.

use std::path::Path;
use std::sync::Arc;

use log::{debug, error, info, warn};
use nfs3_client::nfs3_types::nfs3::{
    nfstime3, sattr3, sattrguard3, set_atime, set_gid3, set_mode3, set_mtime, set_size3, set_uid3,
    Nfs3Result, SETATTR3args,
};

use crate::frame::control_files::find_primary_control_file;
use crate::nfs::aio::reader::{resolve_path, FileHandleCache};
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
/// Reads the mtime control file from `ctrl_dir` and calls `setattr` with `SET_TO_CLIENT_TIME`
/// on each directory's NFS target path to restore the original mtime.
pub async fn run_nfs_mtime_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_prefix: &str,
    pool: Arc<NfsConnectionPool>,
    dir_cache: FileHandleCache,
) -> NfsMtimeStats {
    let mut stats = NfsMtimeStats::default();
    let Some(ctrl_path) = find_primary_control_file(ctrl_dir, "mtime") else {
        info!("NFS mtime phase: no mtime control file found, skipping");
        return stats;
    };

    info!("NFS mtime phase: processing {:?}", ctrl_path);

    let root_fh = pool.root_fh();

    let reader = match MtimeControlFileReader::open(&ctrl_path) {
        Ok(r) => r,
        Err(e) => {
            error!("NFS mtime phase: cannot open mtime control file: {e}");
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
        let nfs_path = crate::backup::aio::path_util::target_relative_path(source_dir_base, target_prefix, &entry.path);

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
