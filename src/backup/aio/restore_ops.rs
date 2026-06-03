//! Trait for transport-specific restore operations.
//!
//! The restore pipeline needs to perform transport-specific operations
//! (symlink creation, metadata restoration) that differ between local,
//! NFS, and SMB targets.

use std::path::Path;

use crate::scanner::metadata::MetaCommon;

/// Transport-specific operations needed during restore.
///
/// The default implementations are no-ops, so transports only override
/// what they support.
#[allow(async_fn_in_trait)]
pub trait RestoreOps: Send + Sync {
    /// Create a symlink at `link_path` pointing to `target`.
    ///
    /// Only meaningful for local targets; remote targets should no-op.
    fn create_symlink(&self, _link_path: &Path, _target: &str) -> Result<(), String> {
        Ok(())
    }

    /// Restore common metadata (permissions, timestamps, xattrs, ACLs) on a file.
    ///
    /// Only meaningful for local targets; remote targets handle metadata
    /// through their own transport-specific mechanisms.
    fn restore_metadata(&self, _path: &Path, _meta: &MetaCommon) {}
}
