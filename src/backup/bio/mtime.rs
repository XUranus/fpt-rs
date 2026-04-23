//! # Mtime Backup Phase
//!
//! This module implements the mtime phase of the backup process.
//!
//! ## Overview
//!
//! The copy and hardlink phases may affect directory modification times (mtime).
//! The mtime phase runs after these phases to restore the original directory
//! timestamps from the source.
//!
//! ## Process
//!
//! 1. Read the mtime control file
//! 2. For each directory entry:
//!    - Calculate the target path
//!    - Set the directory's atime and mtime to the original values
//!
//! ## Control File Format
//!
//! See [`crate::scanner::metadata::mtime`] for the control file format.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use log::{debug, error, info, warn};

use crate::failure::{retry_sync, FailureItemType, FailureRecord, FailureRecorder, RetryPolicy};
use crate::frame::control_files::find_primary_control_file;
use crate::scanner::metadata::MtimeControlFileReader;

/// Statistics for the mtime backup phase.
#[derive(Debug, Default)]
pub struct MtimeStats {
    /// Number of directories processed
    pub dirs_processed: AtomicU64,
    /// Number of directories with mtime restored successfully
    pub dirs_restored: AtomicU64,
    /// Number of directories that failed to restore
    pub dirs_failed: AtomicU64,
    /// Number of directories skipped (not found)
    pub dirs_skipped: AtomicU64,
}

impl MtimeStats {
    pub fn snapshot(&self) -> MtimeStatsSnapshot {
        MtimeStatsSnapshot {
            dirs_processed: self.dirs_processed.load(Ordering::Relaxed),
            dirs_restored: self.dirs_restored.load(Ordering::Relaxed),
            dirs_failed: self.dirs_failed.load(Ordering::Relaxed),
            dirs_skipped: self.dirs_skipped.load(Ordering::Relaxed),
        }
    }
}

/// A serializable snapshot of mtime statistics.
#[derive(Debug, Clone, Default)]
pub struct MtimeStatsSnapshot {
    pub dirs_processed: u64,
    pub dirs_restored: u64,
    pub dirs_failed: u64,
    pub dirs_skipped: u64,
}

/// Processes the mtime control file and restores directory timestamps.
///
/// This function reads the mtime control file and restores the original
/// atime and mtime for each directory.
///
/// # Arguments
///
/// * `mtime_ctrl_path` - Path to the mtime control file
/// * `source_dir_base` - Base directory of the source (for path calculation)
/// * `target_dir_base` - Base directory of the target (for path calculation)
///
/// # Returns
///
/// Returns statistics about the mtime operation.
pub fn process_mtime(
    mtime_ctrl_path: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
) -> io::Result<MtimeStatsSnapshot> {
    let stats = Arc::new(MtimeStats::default());

    // Check if mtime control file exists
    if !mtime_ctrl_path.exists() {
        info!("No mtime control file found at {:?}", mtime_ctrl_path);
        return Ok(stats.snapshot());
    }

    info!("Processing mtime from {:?}", mtime_ctrl_path);

    // Read all directory entries from the control file
    let reader = MtimeControlFileReader::open(mtime_ctrl_path)?;
    let logical_paths = !reader.header().source_root.is_empty();

    for entry_result in reader {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to read mtime entry: {}", e);
                stats.dirs_failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        stats.dirs_processed.fetch_add(1, Ordering::Relaxed);

        // Calculate target path
        let target_path = make_relative_and_join(
            source_dir_base,
            target_dir_base.to_path_buf(),
            entry.path,
            logical_paths,
        );

        // Check if directory exists
        if !target_path.exists() {
            warn!("Target directory does not exist: {:?}", target_path);
            stats.dirs_skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        // Set directory timestamps
        match retry_sync(retry_policy, || {
            set_dir_times(&target_path, entry.atime, entry.mtime)
        }) {
            Ok(()) => {
                debug!(
                    "Restored mtime for {:?}: atime={}, mtime={}",
                    target_path, entry.atime, entry.mtime
                );
                stats.dirs_restored.fetch_add(1, Ordering::Relaxed);
            }
            Err((e, attempts)) => {
                error!("Failed to set times for {:?}: {}", target_path, e);
                stats.dirs_failed.fetch_add(1, Ordering::Relaxed);
                record_mtime_failure(failure_recorder, &target_path, &e, attempts);
            }
        }
    }

    let snapshot = stats.snapshot();
    info!(
        "Mtime phase complete: {} processed, {} restored, {} failed, {} skipped",
        snapshot.dirs_processed,
        snapshot.dirs_restored,
        snapshot.dirs_failed,
        snapshot.dirs_skipped
    );

    Ok(snapshot)
}

/// Sets the access and modification times for a directory.
#[cfg(unix)]
fn set_dir_times(path: &Path, atime: u64, mtime: u64) -> io::Result<()> {
    // Convert seconds to timespec
    let times = [
        libc::timespec {
            tv_sec: atime as i64,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: mtime as i64,
            tv_nsec: 0,
        },
    ];

    let path_cstr = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let result = unsafe { libc::utimensat(libc::AT_FDCWD, path_cstr.as_ptr(), times.as_ptr(), 0) };

    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn set_dir_times(path: &Path, atime: u64, mtime: u64) -> io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    // Open directory with write access for setting times
    let file = OpenOptions::new()
        .write(true)
        .custom_flags(0x02000000) // FILE_FLAG_BACKUP_SEMANTICS
        .open(path)?;

    let atime_system = UNIX_EPOCH + Duration::from_secs(atime);
    let mtime_system = UNIX_EPOCH + Duration::from_secs(mtime);

    file.set_times(
        std::fs::FileTimes::new()
            .accessed(atime_system)
            .modified(mtime_system),
    )
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

/// Runs the mtime phase as a separate backup phase.
///
/// This is typically called after the copy and hardlink phases complete.
pub fn run_mtime_phase(
    ctrl_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
) -> io::Result<MtimeStatsSnapshot> {
    let Some(mtime_ctrl_path) = find_primary_control_file(ctrl_dir, "mtime") else {
        info!("No mtime control file found under {:?}", ctrl_dir);
        return Ok(MtimeStatsSnapshot::default());
    };
    process_mtime(
        &mtime_ctrl_path,
        source_dir_base,
        target_dir_base,
        retry_policy,
        failure_recorder,
    )
}

fn record_mtime_failure(
    recorder: Option<&FailureRecorder>,
    path: &Path,
    err: &io::Error,
    attempts: u32,
) {
    if let Some(recorder) = recorder {
        recorder.record(FailureRecord::from_io_error(
            "backup",
            "set_mtime",
            FailureItemType::Directory,
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
