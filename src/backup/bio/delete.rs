//! # Delete Backup Phase
//!
//! This module implements the delete phase of the backup process.
//!
//! ## Overview
//!
//! The delete phase runs between the hardlink phase and mtime phase.
//! It removes files and directories from the target that were deleted
//! from the source since the last backup.
//!
//! ## Process
//!
//! 1. Read the delete control file (`delete.txt`)
//! 2. For each entry:
//!    - Calculate the target path
//!    - Delete the file or directory
//! 3. Directories are deleted recursively after all files are deleted
//!
//! ## Control File Format
//!
//! See [`crate::scanner::metadata::delete`] for the control file format.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use log::{debug, error, info, warn};

use crate::failure::{retry_sync, FailureItemType, FailureRecord, FailureRecorder, RetryPolicy};
use crate::scanner::metadata::{DeleteControlFileReader, DeleteEntryType};

/// Statistics for the delete backup phase.
#[derive(Debug, Default)]
pub struct DeleteStats {
    /// Number of entries processed
    pub entries_processed: AtomicU64,
    /// Number of files deleted successfully
    pub files_deleted: AtomicU64,
    /// Number of directories deleted successfully
    pub dirs_deleted: AtomicU64,
    /// Number of entries that failed to delete
    pub entries_failed: AtomicU64,
    /// Number of entries skipped (not found)
    pub entries_skipped: AtomicU64,
}

impl DeleteStats {
    pub fn snapshot(&self) -> DeleteStatsSnapshot {
        DeleteStatsSnapshot {
            entries_processed: self.entries_processed.load(Ordering::Relaxed),
            files_deleted: self.files_deleted.load(Ordering::Relaxed),
            dirs_deleted: self.dirs_deleted.load(Ordering::Relaxed),
            entries_failed: self.entries_failed.load(Ordering::Relaxed),
            entries_skipped: self.entries_skipped.load(Ordering::Relaxed),
        }
    }
}

/// A serializable snapshot of delete statistics.
#[derive(Debug, Clone, Default)]
pub struct DeleteStatsSnapshot {
    pub entries_processed: u64,
    pub files_deleted: u64,
    pub dirs_deleted: u64,
    pub entries_failed: u64,
    pub entries_skipped: u64,
}

/// Processes the delete control file and removes entries from target.
///
/// This function reads the delete control file and deletes the corresponding
/// files and directories from the target.
///
/// # Arguments
///
/// * `delete_ctrl_path` - Path to the delete control file
/// * `source_dir_base` - Base directory of the source (for path calculation)
/// * `target_dir_base` - Base directory of the target (for path calculation)
///
/// # Returns
///
/// Returns statistics about the delete operation.
pub fn process_deletes(
    delete_ctrl_path: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
) -> io::Result<DeleteStatsSnapshot> {
    let stats = Arc::new(DeleteStats::default());

    // Check if delete control file exists
    if !delete_ctrl_path.exists() {
        info!("No delete control file found at {:?}", delete_ctrl_path);
        return Ok(stats.snapshot());
    }

    info!("Processing deletes from {:?}", delete_ctrl_path);

    // Collect directories to delete (delete them after files)
    let mut dirs_to_delete: Vec<String> = Vec::new();

    // Read all entries from the control file
    let reader = DeleteControlFileReader::open(delete_ctrl_path)?;
    let logical_paths = reader.header().contains(" SOURCE_ROOT=");

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to read delete entry: {}", e);
                stats.entries_failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        stats.entries_processed.fetch_add(1, Ordering::Relaxed);

        // Calculate target path for files
        let target_path = make_relative_and_join(
            source_dir_base,
            target_dir_base.to_path_buf(),
            entry.path.clone(),
            logical_paths,
        );

        match entry.entry_type {
            DeleteEntryType::File => {
                // Delete file
                if !target_path.exists() {
                    debug!("Target file does not exist, skipping: {:?}", target_path);
                    stats.entries_skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }

                if target_path.is_file() {
                    match retry_sync(retry_policy, || std::fs::remove_file(&target_path)) {
                        Ok(()) => {
                            debug!("Deleted file: {:?}", target_path);
                            stats.files_deleted.fetch_add(1, Ordering::Relaxed);
                        }
                        Err((e, attempts)) => {
                            error!("Failed to delete file {:?}: {}", target_path, e);
                            stats.entries_failed.fetch_add(1, Ordering::Relaxed);
                            record_delete_failure(
                                failure_recorder,
                                "delete_file",
                                FailureItemType::File,
                                &target_path,
                                &e,
                                attempts,
                            );
                        }
                    }
                } else {
                    warn!("Target path is not a file: {:?}", target_path);
                    stats.entries_skipped.fetch_add(1, Ordering::Relaxed);
                }
            }
            DeleteEntryType::Dir => {
                // Collect directories for later deletion
                dirs_to_delete.push(entry.path.clone());
            }
        }
    }

    // Delete directories (in reverse order to delete deepest first)
    dirs_to_delete.sort_by(|a, b| b.cmp(a)); // Reverse sort
    for dir_path in dirs_to_delete {
        let target_path = make_relative_and_join(
            source_dir_base,
            target_dir_base.to_path_buf(),
            dir_path,
            logical_paths,
        );

        if !target_path.exists() {
            debug!(
                "Target directory does not exist, skipping: {:?}",
                target_path
            );
            stats.entries_skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        if target_path.is_dir() {
            // Only delete if directory is empty
            match retry_sync(retry_policy, || std::fs::remove_dir(&target_path)) {
                Ok(()) => {
                    debug!("Deleted directory: {:?}", target_path);
                    stats.dirs_deleted.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    match retry_sync(retry_policy, || std::fs::remove_dir_all(&target_path)) {
                        Ok(()) => {
                            debug!("Recursively deleted directory: {:?}", target_path);
                            stats.dirs_deleted.fetch_add(1, Ordering::Relaxed);
                        }
                        Err((e, attempts)) => {
                            error!("Failed to delete directory {:?}: {}", target_path, e);
                            stats.entries_failed.fetch_add(1, Ordering::Relaxed);
                            record_delete_failure(
                                failure_recorder,
                                "delete_dir",
                                FailureItemType::Directory,
                                &target_path,
                                &e,
                                attempts,
                            );
                        }
                    }
                }
            }
        } else {
            warn!("Target path is not a directory: {:?}", target_path);
            stats.entries_skipped.fetch_add(1, Ordering::Relaxed);
        }
    }

    let snapshot = stats.snapshot();
    info!(
        "Delete phase complete: {} processed, {} files deleted, {} dirs deleted, {} failed, {} skipped",
        snapshot.entries_processed,
        snapshot.files_deleted,
        snapshot.dirs_deleted,
        snapshot.entries_failed,
        snapshot.entries_skipped
    );

    Ok(snapshot)
}

/// Make path relative to base_dir and then join with target_base.
fn make_relative_and_join(
    base_dir: &Path,
    target_base: PathBuf,
    path: String,
    logical_paths: bool,
) -> PathBuf {
    let path_buf = PathBuf::from(&path);

    let relative_path = if path_buf.starts_with(base_dir) {
        path_buf
            .strip_prefix(base_dir)
            .map(|p| p.to_path_buf())
            .unwrap_or(path_buf)
    } else if path_buf.is_absolute() {
        if logical_paths {
            let rel = path_buf
                .strip_prefix("/")
                .map(|p| p.to_path_buf())
                .unwrap_or(path_buf);
            return target_base.join(rel);
        }
        let logical_root_name = base_dir.file_name().and_then(|n| n.to_str());
        let first_segment = path_buf
            .strip_prefix("/")
            .ok()
            .and_then(|p| p.iter().next())
            .and_then(|s| s.to_str());
        if logical_root_name.is_some() && logical_root_name == first_segment {
            path_buf
                .strip_prefix("/")
                .map(|p| p.to_path_buf())
                .unwrap_or(path_buf)
        } else {
            path_buf.file_name().map(PathBuf::from).unwrap_or(path_buf)
        }
    } else {
        path_buf
    };

    target_base.join(relative_path)
}

/// Runs the delete phase as a separate backup phase.
///
/// This is typically called after the hardlink phase and before the mtime phase.
pub fn run_delete_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
) -> io::Result<DeleteStatsSnapshot> {
    let delete_ctrl_path = ctrl_dir.join("delete.txt");
    process_deletes(
        &delete_ctrl_path,
        source_dir_base,
        target_dir_base,
        retry_policy,
        failure_recorder,
    )
}

fn record_delete_failure(
    recorder: Option<&FailureRecorder>,
    operation: &str,
    item_type: FailureItemType,
    path: &Path,
    err: &io::Error,
    attempts: u32,
) {
    if let Some(recorder) = recorder {
        recorder.record(FailureRecord::from_io_error(
            "backup",
            operation,
            item_type,
            path.to_string_lossy(),
            err,
            attempts,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_relative_and_join() {
        let base = PathBuf::from("/home/user/source");
        let target = PathBuf::from("/backup/target");

        let result = make_relative_and_join(
            &base,
            target.clone(),
            "/home/user/source/docs".to_string(),
            false,
        );
        assert_eq!(result, PathBuf::from("/backup/target/docs"));

        // Test with non-matching absolute path
        let result =
            make_relative_and_join(&base, target.clone(), "/other/path".to_string(), false);
        assert_eq!(result, PathBuf::from("/backup/target/path"));
    }
}
