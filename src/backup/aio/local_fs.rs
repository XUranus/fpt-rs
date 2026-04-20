//! Local filesystem helpers shared by async transport pipelines.

use std::io::{Read, Write};
use std::path::PathBuf;

/// Read the entire content of a local file into memory.
///
/// Called from `spawn_blocking` by async copy pipelines to avoid blocking the
/// Tokio executor.
pub fn read_local_file(path: &PathBuf, expected_size: u64) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    let cap = (expected_size as usize).min(64 * 1024 * 1024);
    let mut buf = Vec::with_capacity(cap);
    file.read_to_end(&mut buf)
        .map_err(|e| format!("read {path:?}: {e}"))?;
    Ok(buf)
}

/// Write a byte buffer to a local file, creating parent directories as needed.
pub fn write_local_file(path: &PathBuf, buf: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {e}", parent))?;
    }
    let mut file = std::fs::File::create(path).map_err(|e| format!("create {:?}: {e}", path))?;
    file.write_all(buf)
        .map_err(|e| format!("write {:?}: {e}", path))?;
    Ok(())
}
