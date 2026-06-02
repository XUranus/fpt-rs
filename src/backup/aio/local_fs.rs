//! Local filesystem helpers shared by async transport pipelines.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Read one bounded chunk of a local file into memory.
///
/// Called from `spawn_blocking` by async copy pipelines to avoid blocking the
/// Tokio executor.
pub fn read_local_file_chunk(
    path: &PathBuf,
    offset: u64,
    expected_size: u64,
    max_len: usize,
) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("seek {path:?} @ {offset}: {e}"))?;
    let remaining = expected_size.saturating_sub(offset) as usize;
    let len = remaining.min(max_len.max(1));
    let mut buf = vec![0u8; len];
    let n = file
        .read(&mut buf)
        .map_err(|e| format!("read {path:?}: {e}"))?;
    buf.truncate(n);
    Ok(buf)
}

/// Write a byte buffer to a local file at the given offset.
///
/// If `mark_sparse` is true and this is the first write (offset == 0),
/// the file is marked as sparse on Windows so NTFS doesn't pre-allocate
/// space for zero-filled regions.
pub fn write_local_file_chunk(
    path: &PathBuf,
    offset: u64,
    buf: &[u8],
    mark_sparse: bool,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {:?}: {e}", parent))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    if offset == 0 {
        options.truncate(true);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("open target {:?}: {e}", path))?;

    // Mark file as sparse on first write (Windows only)
    if mark_sparse && offset == 0 {
        #[cfg(windows)]
        crate::native::backup::local_metadata::mark_file_sparse(path);
    }

    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("seek target {:?} @ {offset}: {e}", path))?;
    file.write_all(buf)
        .map_err(|e| format!("write {:?}: {e}", path))?;
    Ok(())
}
