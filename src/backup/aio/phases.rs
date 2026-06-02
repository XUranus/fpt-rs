//! Post-copy phase dispatchers.
//!
//! Each transport owns its own post-copy phase implementation.
//! This module provides thin wrappers for backward compatibility.

use std::path::PathBuf;

use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};

/// Run post-copy phases for a local target (delegates to native::backup::phases).
#[cfg(any(feature = "nfs", feature = "smb"))]
pub(crate) fn run_local_target_phases(
    ctrl_dir: &PathBuf,
    meta_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_dir_base: &PathBuf,
    phase_flags: PhaseFlags,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
) {
    crate::native::backup::phases::run_local_followup_phases(
        phase_flags,
        ctrl_dir,
        meta_dir,
        source_dir_base,
        target_dir_base,
        retry_policy,
        failure_recorder,
    );
}

/// Run post-copy phases for an NFS target (delegates to nfs::backup::phases).
#[cfg(feature = "nfs")]
pub(crate) async fn run_nfs_target_phases(
    ctrl_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_prefix: &str,
    pool: std::sync::Arc<crate::nfs::connection::NfsConnectionPool>,
    file_cache: crate::nfs::aio::reader::FileHandleCache,
    dir_cache: crate::nfs::aio::writer::DirHandleCache,
    phase_flags: PhaseFlags,
) {
    crate::nfs::backup::phases::run_nfs_target_phases(
        ctrl_dir,
        source_dir_base,
        target_prefix,
        pool,
        file_cache,
        dir_cache,
        phase_flags,
    )
    .await;
}

/// Run post-copy phases for an SMB target (delegates to smb::backup::phases).
#[cfg(feature = "smb")]
pub(crate) async fn run_smb_target_phases(
    ctrl_dir: &PathBuf,
    source_dir_base: &PathBuf,
    target_prefix: &str,
    location: &crate::smb::SmbLocation,
    phase_flags: PhaseFlags,
) {
    crate::smb::backup::phases::run_smb_target_phases(
        ctrl_dir,
        source_dir_base,
        target_prefix,
        location,
        phase_flags,
    )
    .await;
}
