//! # Hardlink Backup Engine
//!
//! This module implements the hardlink phase of the backup process.
//!
//! ## Overview
//!
//! After the copy phase completes, the hardlink phase processes files that share
//! the same inode (hardlinks). Instead of copying the content again, hardlinks
//! are created by linking to the first file in each inode group.
//!
//! ## Process
//!
//! 1. Read the hardlink control file (`hardlink.txt`)
//! 2. For each inode group:
//!    - Skip the first file (already copied during the copy phase)
//!    - Create hardlinks for subsequent files pointing to the first file
//! 3. Restore metadata (timestamps, permissions) for all hardlinked files
//!
//! ## Control File Format
//!
//! See [`crate::scanner::metadata::hardlink`] for the control file format.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use log::{debug, error, info, warn};

use crate::scanner::metadata::{
    HardlinkControlFileReader, HardlinkEntry,
    MetaRepoReader,
};

/// Statistics for the hardlink backup phase.
#[derive(Debug, Default)]
pub struct HardlinkStats {
    /// Number of hardlink groups processed
    pub groups_processed: AtomicU64,
    /// Number of hardlinks created
    pub hardlinks_created: AtomicU64,
    /// Number of hardlinks that failed to create
    pub hardlinks_failed: AtomicU64,
    /// Number of files skipped (first in group, already exists)
    pub files_skipped: AtomicU64,
}

impl HardlinkStats {
    pub fn snapshot(&self) -> HardlinkStatsSnapshot {
        HardlinkStatsSnapshot {
            groups_processed: self.groups_processed.load(Ordering::Relaxed),
            hardlinks_created: self.hardlinks_created.load(Ordering::Relaxed),
            hardlinks_failed: self.hardlinks_failed.load(Ordering::Relaxed),
            files_skipped: self.files_skipped.load(Ordering::Relaxed),
        }
    }
}

/// A serializable snapshot of hardlink statistics.
#[derive(Debug, Clone, Default)]
pub struct HardlinkStatsSnapshot {
    pub groups_processed: u64,
    pub hardlinks_created: u64,
    pub hardlinks_failed: u64,
    pub files_skipped: u64,
}

/// Represents a group of hardlinked files sharing the same inode.
#[derive(Debug)]
#[allow(dead_code)]
struct HardlinkGroup {
    inode: u64,
    device: u64,
    link_count: u32,
    files: Vec<HardlinkFileInfo>,
}

/// Information about a file in a hardlink group.
#[derive(Debug)]
#[allow(dead_code)]
struct HardlinkFileInfo {
    meta_fid: u32,
    meta_offset: u32,
    src_path: PathBuf,
    dst_path: PathBuf,
}

/// Processes the hardlink control file and creates hardlinks.
///
/// This function reads the hardlink control file and creates hardlinks
/// for all files in each inode group (except the first one which was
/// already copied during the copy phase).
///
/// # Arguments
///
/// * `hardlink_ctrl_path` - Path to the hardlink control file
/// * `meta_dir` - Directory containing metadata files
/// * `source_dir_base` - Base directory of the source (for path calculation)
/// * `target_dir_base` - Base directory of the target (for path calculation)
///
/// # Returns
///
/// Returns statistics about the hardlink operation.
pub fn process_hardlinks(
    hardlink_ctrl_path: &Path,
    meta_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
) -> io::Result<HardlinkStatsSnapshot> {
    let stats = Arc::new(HardlinkStats::default());

    // Check if hardlink control file exists
    if !hardlink_ctrl_path.exists() {
        info!("No hardlink control file found at {:?}", hardlink_ctrl_path);
        return Ok(stats.snapshot());
    }

    info!("Processing hardlinks from {:?}", hardlink_ctrl_path);

    // Read all hardlink groups from the control file
    let groups = read_hardlink_groups(hardlink_ctrl_path, meta_dir, source_dir_base, target_dir_base)?;

    info!("Found {} hardlink groups to process", groups.len());

    // Process each group
    for group in groups {
        process_hardlink_group(group, &stats);
    }

    let snapshot = stats.snapshot();
    info!(
        "Hardlink phase complete: {} groups, {} created, {} failed, {} skipped",
        snapshot.groups_processed,
        snapshot.hardlinks_created,
        snapshot.hardlinks_failed,
        snapshot.files_skipped
    );

    Ok(snapshot)
}

/// Reads all hardlink groups from the control file.
fn read_hardlink_groups(
    hardlink_ctrl_path: &Path,
    meta_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
) -> io::Result<Vec<HardlinkGroup>> {
    let reader = HardlinkControlFileReader::open(hardlink_ctrl_path)?;
    let logical_paths = reader.header().contains(" SOURCE_ROOT=");
    let meta_repo = MetaRepoReader::new(meta_dir)?;

    let mut groups: Vec<HardlinkGroup> = Vec::new();
    let mut current_group: Option<HardlinkGroup> = None;

    for entry_result in reader {
        let entry = entry_result?;

        match entry {
            HardlinkEntry::Inode(inode_entry) => {
                // Save previous group if exists
                if let Some(group) = current_group.take() {
                    if !group.files.is_empty() {
                        groups.push(group);
                    }
                }

                // Start new group
                current_group = Some(HardlinkGroup {
                    inode: inode_entry.inode,
                    device: inode_entry.device,
                    link_count: inode_entry.link_count,
                    files: Vec::with_capacity(inode_entry.link_count as usize),
                });
            }
            HardlinkEntry::File(file_entry) => {
                if let Some(ref mut group) = current_group {
                    // Get metadata to find the source path
                    match meta_repo.get_fmeta((file_entry.meta_fid, file_entry.meta_offset)) {
                        Ok(_fmeta) => {
                            // Build source and destination paths
                            let src_path = PathBuf::from(&file_entry.path);
                            let dst_path = make_relative_and_join(
                                source_dir_base,
                                target_dir_base.to_path_buf(),
                                file_entry.path.clone(),
                                logical_paths,
                            );

                            group.files.push(HardlinkFileInfo {
                                meta_fid: file_entry.meta_fid,
                                meta_offset: file_entry.meta_offset,
                                src_path,
                                dst_path,
                            });
                        }
                        Err(e) => {
                            warn!(
                                "Failed to read metadata for {}: {}",
                                file_entry.path, e
                            );
                        }
                    }
                }
            }
        }
    }

    // Don't forget the last group
    if let Some(group) = current_group {
        if !group.files.is_empty() {
            groups.push(group);
        }
    }

    Ok(groups)
}

/// Processes a single hardlink group.
/// The first file in the group is the target (already copied), and
/// subsequent files are created as hardlinks to it.
fn process_hardlink_group(group: HardlinkGroup, stats: &Arc<HardlinkStats>) {
    if group.files.len() < 2 {
        // Need at least 2 files to create a hardlink
        stats.files_skipped.fetch_add(1, Ordering::Relaxed);
        return;
    }

    stats.groups_processed.fetch_add(1, Ordering::Relaxed);

    // The first file is the target (already copied)
    let target_info = &group.files[0];
    let target_path = &target_info.dst_path;

    // Verify the target exists
    if !target_path.exists() {
        warn!(
            "Target file for hardlink group does not exist: {:?}",
            target_path
        );
        // Try to find an existing file in the group
        let existing = group.files.iter().find(|f| f.dst_path.exists());
        match existing {
            Some(existing_file) => {
                debug!("Using existing file as hardlink target: {:?}", existing_file.dst_path);
                create_hardlinks_for_group(existing_file, &group.files, stats);
            }
            None => {
                error!("No existing file found in hardlink group for inode {}", group.inode);
                stats.hardlinks_failed.fetch_add(group.files.len() as u64, Ordering::Relaxed);
            }
        }
        return;
    }

    create_hardlinks_for_group(target_info, &group.files[1..], stats);
}

/// Creates hardlinks for all files in a group pointing to the target.
fn create_hardlinks_for_group(
    target_info: &HardlinkFileInfo,
    link_files: &[HardlinkFileInfo],
    stats: &Arc<HardlinkStats>,
) {
    let target_path = &target_info.dst_path;

    for link_info in link_files {
        // Skip if this is the target file itself
        if link_info.dst_path == *target_path {
            stats.files_skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        // If the file already exists (copied during copy phase), we need to:
        // 1. Remove the existing file
        // 2. Create a hardlink to the target
        if link_info.dst_path.exists() {
            // Check if it's already a hardlink (same inode as target)
            match (fs::metadata(&link_info.dst_path), fs::metadata(target_path)) {
                (Ok(link_meta), Ok(target_meta)) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if link_meta.ino() == target_meta.ino() && link_meta.dev() == target_meta.dev() {
                            debug!("File is already a hardlink: {:?}", link_info.dst_path);
                            stats.files_skipped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                }
                _ => {}
            }
            
            // Remove the existing file so we can create a hardlink
            debug!("Removing existing file to create hardlink: {:?}", link_info.dst_path);
            if let Err(e) = fs::remove_file(&link_info.dst_path) {
                error!("Failed to remove existing file {:?}: {}", link_info.dst_path, e);
                stats.hardlinks_failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }

        // Create parent directory if needed
        if let Some(parent) = link_info.dst_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                error!("Failed to create directory {:?}: {}", parent, e);
                stats.hardlinks_failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }

        // Create the hardlink
        match create_hardlink(target_path, &link_info.dst_path) {
            Ok(()) => {
                debug!(
                    "Created hardlink: {:?} -> {:?}",
                    link_info.dst_path, target_path
                );
                stats.hardlinks_created.fetch_add(1, Ordering::Relaxed);

                // Restore metadata (timestamps, permissions)
                if let Err(e) = restore_file_metadata(link_info) {
                    warn!("Failed to restore metadata for {:?}: {}", link_info.dst_path, e);
                }
            }
            Err(e) => {
                error!(
                    "Failed to create hardlink {:?} -> {:?}: {}",
                    link_info.dst_path, target_path, e
                );
                stats.hardlinks_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Creates a hardlink at `link_path` pointing to `target_path`.
fn create_hardlink(target_path: &Path, link_path: &Path) -> io::Result<()> {
    std::fs::hard_link(target_path, link_path)
}

/// Restores file metadata (timestamps, permissions) for a hardlinked file.
fn restore_file_metadata(_file_info: &HardlinkFileInfo) -> io::Result<()> {
    // Note: For hardlinks, we can't restore permissions separately since
    // they share the same inode. However, we can restore timestamps.
    
    // For now, we just ensure the file exists with correct hardlink relationship.
    // The metadata was already set on the target file during the copy phase.
    
    Ok(())
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
            let rel = path_buf.strip_prefix("/").map(|p| p.to_path_buf()).unwrap_or(path_buf);
            return target_base.join(rel);
        }
        let logical_root_name = base_dir.file_name().and_then(|n| n.to_str());
        let first_segment = path_buf.strip_prefix("/").ok().and_then(|p| p.iter().next()).and_then(|s| s.to_str());
        if logical_root_name.is_some() && logical_root_name == first_segment {
            path_buf.strip_prefix("/").map(|p| p.to_path_buf()).unwrap_or(path_buf)
        } else {
            path_buf.file_name().map(PathBuf::from).unwrap_or(path_buf)
        }
    } else {
        path_buf
    };

    target_base.join(relative_path)
}

/// Runs the hardlink phase as a separate backup phase.
///
/// This is typically called after the copy phase completes.
pub fn run_hardlink_phase(
    ctrl_dir: &Path,
    meta_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
) -> io::Result<HardlinkStatsSnapshot> {
    let hardlink_ctrl_path = ctrl_dir.join("hardlink.txt");
    process_hardlinks(&hardlink_ctrl_path, meta_dir, source_dir_base, target_dir_base)
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
            "/home/user/source/docs/file.txt".to_string(),
            false,
        );
        assert_eq!(result, PathBuf::from("/backup/target/docs/file.txt"));

        // Test with non-matching absolute path
        let result = make_relative_and_join(
            &base,
            target.clone(),
            "/other/path/file.txt".to_string(),
            false,
        );
        assert_eq!(result, PathBuf::from("/backup/target/file.txt"));
    }
}
