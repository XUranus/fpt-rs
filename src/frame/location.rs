//! Data location abstraction for the backup/restore framework.
//!
//! [`DataLocation`] describes where the *data* (source or target) lives.
//! It is distinct from the internal repo paths (M\_REPO, C\_REPO, D\_REPO),
//! which are always kept on the local filesystem during a job.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// DataLocation
// ---------------------------------------------------------------------------

/// Where the user's data lives — either a local path or an NFSv3 export.
///
/// Used for both source and target sides of a backup or restore job.
#[derive(Debug, Clone)]
pub enum DataLocation {
    /// Standard local filesystem path.
    Local(PathBuf),

    /// NFSv3 export accessed via direct RPC (no kernel mount required).
    #[cfg(feature = "nfs")]
    Nfs(crate::nfs::NfsLocation),
}

impl DataLocation {
    /// Construct a `Local` location from any path-like value.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        DataLocation::Local(path.into())
    }

    /// Construct an `Nfs` location from an [`NfsLocation`].
    #[cfg(feature = "nfs")]
    pub fn nfs(loc: crate::nfs::NfsLocation) -> Self {
        DataLocation::Nfs(loc)
    }

    /// Parse an NFS URL (`nfs://host/export[?sub=path]`) into a `DataLocation`.
    ///
    /// Returns `Err` when the `nfs` Cargo feature is not enabled.
    pub fn from_nfs_url(url: &str) -> Result<Self, String> {
        #[cfg(feature = "nfs")]
        {
            let loc = crate::nfs::NfsLocation::from_url(url)?;
            Ok(DataLocation::Nfs(loc))
        }
        #[cfg(not(feature = "nfs"))]
        {
            let _ = url;
            Err("NFS support is not compiled in — rebuild with `--features nfs`".to_string())
        }
    }

    /// Return `true` if this location is a local filesystem path.
    pub fn is_local(&self) -> bool {
        matches!(self, DataLocation::Local(_))
    }

    /// Return `true` if this location is an NFS export.
    pub fn is_nfs(&self) -> bool {
        #[cfg(feature = "nfs")]
        return matches!(self, DataLocation::Nfs(_));
        #[cfg(not(feature = "nfs"))]
        false
    }

    /// Return the local `PathBuf`, or `None` if this is an NFS location.
    pub fn local_path(&self) -> Option<&PathBuf> {
        match self {
            DataLocation::Local(p) => Some(p),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(_) => None,
        }
    }

    /// Return the [`NfsLocation`], or `None` if this is a local location.
    #[cfg(feature = "nfs")]
    pub fn nfs_location(&self) -> Option<&crate::nfs::NfsLocation> {
        match self {
            DataLocation::Nfs(l) => Some(l),
            DataLocation::Local(_) => None,
        }
    }

    /// Human-readable display string (used in logs and manifests).
    pub fn display_string(&self) -> String {
        match self {
            DataLocation::Local(p) => p.to_string_lossy().into_owned(),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(l) => {
                if l.sub_path.is_empty() {
                    format!("nfs://{}{}", l.host, l.export)
                } else {
                    format!("nfs://{}{}/{}", l.host, l.export, l.sub_path.trim_start_matches('/'))
                }
            }
        }
    }
    /// Return the effective root path for path-stripping purposes.
    ///
    /// - For `Local`, this is the path itself.
    /// - For `Nfs`, this is `PathBuf::from("{export}/{sub_path}")` — the
    ///   absolute path that NFS control-file entries are recorded under.
    pub fn base_path(&self) -> PathBuf {
        match self {
            DataLocation::Local(p) => p.clone(),
            #[cfg(feature = "nfs")]
            DataLocation::Nfs(l) => {
                if l.sub_path.is_empty() {
                    PathBuf::from(&l.export)
                } else {
                    PathBuf::from(&l.export).join(l.sub_path.trim_start_matches('/'))
                }
            }
        }
    }
}

impl std::fmt::Display for DataLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display_string())
    }
}
