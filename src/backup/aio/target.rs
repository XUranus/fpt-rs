//! Backup target transport abstraction.
//!
//! Encapsulates the target side of a backup pipeline — how to connect to
//! and write to a backup destination (local filesystem, NFS, or SMB),
//! plus running post-copy phases (hardlink, delete, mtime).

#![allow(dead_code)]

use std::path::PathBuf;
#[cfg(any(feature = "nfs", feature = "smb"))]
use std::sync::Arc;

use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};

/// A connected backup target, ready to receive data.
pub enum BackupTarget {
    /// Local filesystem target.
    Local {
        target_dir_base: PathBuf,
    },
    /// NFS target (connected pool + file handle).
    #[cfg(feature = "nfs")]
    Nfs {
        pool: Arc<crate::nfs::connection::NfsConnectionPool>,
    },
    /// SMB target (connected pool + location).
    #[cfg(feature = "smb")]
    Smb {
        location: crate::smb::SmbLocation,
        pool: Arc<crate::smb::SmbClientPool>,
    },
}

impl BackupTarget {
    /// Connect to the target from a [`crate::frame::location::DataLocation`].
    pub async fn connect(
        target: &crate::frame::location::DataLocation,
        #[cfg(feature = "smb")] smb_connection_count: usize,
    ) -> Result<Self, String> {
        use crate::frame::location::DataLocation;
        match target {
            DataLocation::Local(p) => Ok(BackupTarget::Local {
                target_dir_base: p.clone(),
            }),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(loc) => {
                let pool = crate::nfs::connection::NfsConnectionPool::new(loc)
                    .await
                    .map_err(|e| format!("NFS target connect failed: {e}"))?;
                Ok(BackupTarget::Nfs { pool })
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(loc) => {
                let pool = crate::smb::SmbClientPool::connect(
                    loc,
                    smb_connection_count.max(1),
                )
                .await
                .map_err(|e| format!("SMB target connect failed: {e}"))?;
                Ok(BackupTarget::Smb {
                    location: loc.clone(),
                    pool,
                })
            }
        }
    }

    /// Run post-copy phases (hardlink, delete, mtime) for this target.
    ///
    /// Uses the [`PostCopyPhases`] trait to dispatch to the correct
    /// transport-specific phase runner.
    #[allow(unused_variables)]
    pub async fn run_post_copy_phases(
        &self,
        ctrl_dir: &PathBuf,
        source_dir_base: &PathBuf,
        target_prefix: &str,
        phase_flags: PhaseFlags,
        retry_policy: RetryPolicy,
        failure_recorder: Option<&FailureRecorder>,
    ) {
        use crate::backup::aio::phases_trait::PostCopyPhases;

        match self {
            BackupTarget::Local { .. } => {
                let phases = crate::native::backup::phases_impl::LocalPostCopyPhases;
                phases.run_all_phases(ctrl_dir, source_dir_base, target_prefix, phase_flags, retry_policy, failure_recorder).await;
            }
            #[cfg(feature = "nfs")]
            BackupTarget::Nfs { pool } => {
                let file_cache = crate::nfs::backup::reader::new_file_handle_cache();
                let dir_cache = crate::nfs::backup::writer::new_dir_handle_cache();
                let phases = crate::nfs::backup::phases_impl::NfsPostCopyPhases {
                    pool: Arc::clone(pool),
                    file_cache,
                    dir_cache,
                };
                phases.run_all_phases(ctrl_dir, source_dir_base, target_prefix, phase_flags, retry_policy, failure_recorder).await;
            }
            #[cfg(feature = "smb")]
            BackupTarget::Smb { location, .. } => {
                let phases = crate::smb::backup::phases_impl::SmbPostCopyPhases {
                    location,
                };
                phases.run_all_phases(ctrl_dir, source_dir_base, target_prefix, phase_flags, retry_policy, failure_recorder).await;
            }
        }
    }

    /// Returns true if this is an SMB target.
    pub fn is_smb(&self) -> bool {
        #[cfg(feature = "smb")]
        { matches!(self, BackupTarget::Smb { .. }) }
        #[cfg(not(feature = "smb"))]
        { false }
    }

    /// Returns true if this is a local target.
    pub fn is_local(&self) -> bool {
        matches!(self, BackupTarget::Local { .. })
    }
}
