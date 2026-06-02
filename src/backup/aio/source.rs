//! Backup source transport abstraction.
//!
//! Encapsulates the source side of a backup pipeline — how to connect to
//! and read from a data source (local filesystem, NFS, or SMB).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

/// A connected backup source, ready to produce data.
pub enum BackupSource {
    /// Local filesystem source.
    Local {
        source_dir_base: PathBuf,
    },
    /// NFS source (connected pool + file handle).
    #[cfg(feature = "nfs")]
    Nfs {
        pool: Arc<crate::nfs::connection::NfsConnectionPool>,
    },
    /// SMB source (connected pool + location).
    #[cfg(feature = "smb")]
    Smb {
        location: crate::smb::SmbLocation,
        pool: Arc<crate::smb::aio::SmbClientPool>,
    },
}

impl BackupSource {
    /// Connect to the source from a [`crate::frame::location::DataLocation`].
    pub async fn connect(
        source: &crate::frame::location::DataLocation,
        #[cfg(feature = "smb")] smb_connection_count: usize,
    ) -> Result<Self, String> {
        use crate::frame::location::DataLocation;
        match source {
            DataLocation::Local(p) => Ok(BackupSource::Local {
                source_dir_base: p.clone(),
            }),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(loc) => {
                let pool = crate::nfs::connection::NfsConnectionPool::new(loc)
                    .await
                    .map_err(|e| format!("NFS source connect failed: {e}"))?;
                Ok(BackupSource::Nfs { pool })
            }
            #[cfg(feature = "smb")]
            DataLocation::Smb(loc) => {
                let pool = crate::smb::aio::SmbClientPool::connect(
                    loc,
                    smb_connection_count.max(1),
                )
                .await
                .map_err(|e| format!("SMB source connect failed: {e}"))?;
                Ok(BackupSource::Smb {
                    location: loc.clone(),
                    pool,
                })
            }
        }
    }

    /// Returns true if this is an SMB source (uses streaming pipeline, not SourceReader).
    pub fn is_smb(&self) -> bool {
        #[cfg(feature = "smb")]
        { matches!(self, BackupSource::Smb { .. }) }
        #[cfg(not(feature = "smb"))]
        { false }
    }
}
