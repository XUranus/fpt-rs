//! Local filesystem restore operations implementation.

use std::path::Path;

use crate::backup::aio::restore_ops::RestoreOps;
use crate::scanner::metadata::MetaCommon;

/// Local filesystem restore operations (symlink creation, metadata restoration).
pub struct LocalRestoreOps;

impl RestoreOps for LocalRestoreOps {
    fn create_symlink(&self, link_path: &Path, target: &str) -> Result<(), String> {
        super::local_metadata::create_symlink(link_path, target)
            .map_err(|e| format!("create_symlink {:?}: {e}", link_path))
    }

    fn restore_metadata(&self, path: &Path, meta: &MetaCommon) {
        super::local_metadata::restore_common_metadata(path, meta);
    }
}
