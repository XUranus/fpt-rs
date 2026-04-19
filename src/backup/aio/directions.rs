//! Thin direction wrappers over the generic async copy executor.

use std::path::PathBuf;
use std::sync::Arc;

use crate::backup::aio::entry::EntryMapping;
use crate::backup::aio::pipeline::run_copy_pipeline;
use crate::backup::aio::transport::{LocalSource, LocalTarget};
use crate::backup::stats::BackupStats;

#[cfg(feature = "nfs")]
use crate::backup::aio::transport::{NfsSource, NfsTarget};
#[cfg(feature = "nfs")]
use crate::nfs::aio::{reader::new_file_handle_cache, writer::new_dir_handle_cache};
#[cfg(feature = "nfs")]
use crate::nfs::connection::NfsConnectionPool;

#[cfg(feature = "smb")]
use crate::backup::aio::transport::{SmbSource, SmbTarget};
#[cfg(feature = "smb")]
use crate::smb::SmbLocation;

#[cfg(feature = "smb")]
const SMB_MAX_CONCURRENT_TASKS: usize = 16;
#[cfg(feature = "nfs")]
const NFS_MAX_CONCURRENT_TASKS: usize = 16;

#[cfg(feature = "nfs")]
pub async fn run_local_to_nfs_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::local_to_prefixed_target(
        source_dir_base,
        PathBuf::from(target_prefix),
    );
    let target = NfsTarget {
        pool: Arc::clone(&pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: pool.root_fh(),
        write_chunk: pool.server_wtmax,
    };

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        LocalSource,
        target,
        stats,
        "local->NFS",
        NFS_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "smb")]
pub async fn run_local_to_smb_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source_dir_base: PathBuf,
    target_prefix: String,
    location: SmbLocation,
    client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::local_to_prefixed_target(
        source_dir_base,
        PathBuf::from(target_prefix),
    );
    let target = SmbTarget {
        location,
        client,
        dir_cache: crate::smb::aio::new_dir_cache(),
    };

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        LocalSource,
        target,
        stats,
        "local->SMB",
        SMB_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "nfs")]
pub async fn run_aio_nfs_to_local_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    local_target_base: PathBuf,
    pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_local(nfs_source_base);
    let source = NfsSource {
        pool: Arc::clone(&pool),
        dir_cache: new_file_handle_cache(),
        root_fh: pool.root_fh(),
        read_chunk: pool.server_rtmax,
    };
    let target = LocalTarget {
        base: local_target_base,
    };

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "NFS->local",
        NFS_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "nfs")]
pub async fn run_aio_nfs_to_nfs_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    nfs_source_base: PathBuf,
    target_prefix: String,
    source_pool: Arc<NfsConnectionPool>,
    target_pool: Arc<NfsConnectionPool>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_prefixed_target(
        nfs_source_base,
        PathBuf::from(target_prefix),
    );
    let source = NfsSource {
        pool: Arc::clone(&source_pool),
        dir_cache: new_file_handle_cache(),
        root_fh: source_pool.root_fh(),
        read_chunk: source_pool.server_rtmax,
    };
    let target = NfsTarget {
        pool: Arc::clone(&target_pool),
        dir_cache: new_dir_handle_cache(),
        root_fh: target_pool.root_fh(),
        write_chunk: target_pool.server_wtmax,
    };

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "NFS->NFS",
        NFS_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "smb")]
pub async fn run_smb_to_local_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    local_target_base: PathBuf,
    location: SmbLocation,
    client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_local(smb_source_base);
    let source = SmbSource { location, client };
    let target = LocalTarget {
        base: local_target_base,
    };

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "SMB->local",
        SMB_MAX_CONCURRENT_TASKS,
    )
    .await;
}

#[cfg(feature = "smb")]
pub async fn run_smb_to_smb_copy_pipeline(
    control_file: PathBuf,
    meta_dir: PathBuf,
    smb_source_base: PathBuf,
    target_prefix: String,
    source_location: SmbLocation,
    target_location: SmbLocation,
    source_client: Arc<smb_client::Client>,
    target_client: Arc<smb_client::Client>,
    stats: Arc<BackupStats>,
) {
    let mapping = EntryMapping::remote_to_prefixed_target(
        smb_source_base,
        PathBuf::from(target_prefix),
    );
    let source = SmbSource {
        location: source_location,
        client: source_client,
    };
    let target = SmbTarget {
        location: target_location,
        client: target_client,
        dir_cache: crate::smb::aio::new_dir_cache(),
    };

    run_copy_pipeline(
        control_file,
        meta_dir,
        mapping,
        source,
        target,
        stats,
        "SMB->SMB",
        SMB_MAX_CONCURRENT_TASKS,
    )
    .await;
}
