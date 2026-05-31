//! # Incremental Diff Algorithm
//!
//! This module implements the diff algorithm for incremental backup.
//!
//! ## Overview
//!
//! The diff algorithm compares the current scan results (dcache/fcache) with
//! the previous backup's dcache/fcache to identify:
//! - New files/directories (NN)
//! - Modified files (DM, MM)
//! - Deleted files/directories (for `delete_<hash>.control.bin`)
//!
//! ## Algorithm
//!
//! 1. Load previous and current dcache entries into sorted structures (BTreeMap)
//! 2. Use heap-based merge to diff directories by inode:
//!    - If inodes match: check if hash changed (modified or unchanged)
//!    - If only in current: new directory
//!    - If only in previous: deleted directory
//! 3. For changed directories, load and diff their files using the same heap approach
//! 4. Generate `copy_<hash>.control.bin` with only NN/MM/DM entries (files to copy)
//! 5. Generate `delete_<hash>.control.bin` with deleted entries

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::scanner::metadata::{
    ControlFileHeader, ControlFileWriter, DeleteControlFileWriter, DeleteEntry, DeleteEntryType,
    DirCacheEntry, DirCacheRandomReader, DirControlEntry, DirDiff, FileCacheEntry,
    FileCacheRandomReader, FileControlEntry, FileDiff, FixedSize, MetaRepoReader,
};
use crate::frame::control_files::primary_control_file_path;

/// Represents the type of difference detected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiffType {
    /// New entry (not in previous backup)
    New,
    /// Data modified (content changed)
    DataModified,
    /// Metadata modified (only metadata changed)
    MetaModified,
    /// Both data and metadata modified
    BothModified,
    /// Deleted entry (in previous but not current)
    Deleted,
}

/// A directory entry with its source (previous or current)
#[derive(Debug)]
#[allow(dead_code)]
enum DirSource {
    Prev(DirCacheEntry),
    Curr(DirCacheEntry),
}

/// A file entry with its source (previous or current)
#[derive(Debug)]
#[allow(dead_code)]
enum FileSource {
    Prev(FileCacheEntry),
    Curr(FileCacheEntry),
}

/// Performs incremental diff between previous and current cache files.
pub struct IncrementalDiff {
    /// Previous directory cache (inode -> entry)
    prev_dcache: BTreeMap<u64, DirCacheEntry>,
    /// Current directory cache (inode -> entry)
    curr_dcache: BTreeMap<u64, DirCacheEntry>,
    /// Current metadata directory path
    curr_meta_dir: PathBuf,
    /// Previous metadata directory path (optional)
    prev_meta_dir: Option<PathBuf>,
}

impl IncrementalDiff {
    /// Creates a new IncrementalDiff by loading cache files from directories.
    ///
    /// Loads all dcache_* files from the provided directories.
    pub fn from_dirs(prev_meta_dir: Option<&Path>, curr_meta_dir: &Path) -> io::Result<Self> {
        // Load previous dcache if available
        let prev_dcache = if let Some(dir) = prev_meta_dir {
            Self::load_all_dcache(dir)?
        } else {
            BTreeMap::new()
        };

        // Load current dcache
        let curr_dcache = Self::load_all_dcache(curr_meta_dir)?;

        Ok(Self {
            prev_dcache,
            curr_dcache,
            curr_meta_dir: curr_meta_dir.to_path_buf(),
            prev_meta_dir: prev_meta_dir.map(|p| p.to_path_buf()),
        })
    }

    /// Loads all directory cache files from a metadata directory.
    fn load_all_dcache(meta_dir: &Path) -> io::Result<BTreeMap<u64, DirCacheEntry>> {
        let mut map = BTreeMap::new();

        // Find all dcache_*.dat files
        let entries = std::fs::read_dir(meta_dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if file_name.starts_with("dcache_") && file_name.ends_with(".dat") {
                Self::load_dcache_file(&path, &mut map)?;
            }
        }

        Ok(map)
    }

    /// Loads a single directory cache file into the map.
    fn load_dcache_file(path: &Path, map: &mut BTreeMap<u64, DirCacheEntry>) -> io::Result<()> {
        let mut reader = DirCacheRandomReader::open(path)?;
        let count = reader.total_count();

        for i in 0..count {
            if let Ok(entry) = reader.read_object(i) {
                map.insert(entry.id, entry);
            }
        }

        Ok(())
    }

    /// Performs the diff and generates control files using heap-based merge.
    ///
    /// Generates:
    /// - `copy_<hash>.control.bin`: contains only NN (new) and MM/DM (modified) entries
    /// - `delete_<hash>.control.bin`: contains deleted entries
    pub fn generate_control_files(
        &self,
        copy_file_path: &Path,
        delete_file_path: &Path,
        source_kind: &str,
        source_root: &str,
    ) -> io::Result<DiffStats> {
        let mut copy_writer = ControlFileWriter::new_with_header(
            copy_file_path,
            &ControlFileHeader {
                source_kind: source_kind.to_string(),
                source_root: source_root.to_string(),
                ..ControlFileHeader::default()
            },
        )?;
        let mut delete_writer =
            DeleteControlFileWriter::new_with_source(delete_file_path, source_kind, source_root)?;
        let curr_meta_reader = MetaRepoReader::new(self.curr_meta_dir.clone())?;
        let prev_meta_reader = self
            .prev_meta_dir
            .as_ref()
            .and_then(|dir| MetaRepoReader::new(dir.clone()).ok());

        let mut stats = DiffStats::default();

        // Get sorted iterators for directories
        let prev_dirs: Vec<_> = self.prev_dcache.values().collect();
        let curr_dirs: Vec<_> = self.curr_dcache.values().collect();

        // Perform heap-based merge diff on directories
        let dir_diffs = Self::heap_diff(&prev_dirs, &curr_dirs, |e| e.id);

        for dir_diff in dir_diffs {
            match dir_diff {
                DiffItem::LeftOnly(prev_entry) => {
                    // Deleted directory
                    stats.deleted_dirs += 1;
                    if let Some(ref reader) = prev_meta_reader {
                        if let Ok(dmeta) = reader.get_dmeta(prev_entry.meta_loc) {
                            let delete_entry = DeleteEntry {
                                entry_type: DeleteEntryType::Dir,
                                path: dmeta.path,
                            };
                            if let Err(e) = delete_writer.write_entry(&delete_entry) {
                                log::warn!("Failed to write delete entry for dir: {e}");
                            }
                        }
                    }
                }
                DiffItem::RightOnly(curr_entry) => {
                    // New directory - write all files
                    stats.new_dirs += 1;
                    if let Ok(dmeta) = curr_meta_reader.get_dmeta(curr_entry.meta_loc) {
                        let dctrl_entry = DirControlEntry {
                            path: dmeta.path.clone(),
                            diff: DirDiff::New,
                            meta_fid: curr_entry.meta_loc.0,
                            meta_offset: curr_entry.meta_loc.1,
                            files_count: curr_entry.files_count,
                        };
                        copy_writer.write_dir(&dctrl_entry)?;

                        // Write all files in this new directory
                        self.write_all_directory_files(
                            curr_entry,
                            &curr_meta_reader,
                            &mut copy_writer,
                            &mut stats,
                        )?;
                    }
                }
                DiffItem::Both(prev_entry, curr_entry) => {
                    if let Ok(dmeta) = curr_meta_reader.get_dmeta(curr_entry.meta_loc) {
                        let file_diff = self.diff_directory_files(
                            prev_entry,
                            curr_entry,
                            &dmeta.path,
                            prev_meta_reader.as_ref(),
                            &curr_meta_reader,
                        )?;
                        let dir_meta_changed = prev_entry.hash != curr_entry.hash
                            || prev_entry.files_count != curr_entry.files_count;

                        if dir_meta_changed || file_diff.has_changes() {
                            stats.modified_dirs += 1;
                            let dctrl_entry = DirControlEntry {
                                path: dmeta.path.clone(),
                                diff: DirDiff::MetaModified,
                                meta_fid: curr_entry.meta_loc.0,
                                meta_offset: curr_entry.meta_loc.1,
                                files_count: curr_entry.files_count,
                            };
                            copy_writer.write_dir(&dctrl_entry)?;

                            for entry in file_diff.file_entries {
                                copy_writer.write_file(&entry)?;
                            }
                            for entry in file_diff.delete_entries {
                                if let Err(e) = delete_writer.write_entry(&entry) {
                                    log::warn!("Failed to write delete entry: {e}");
                                }
                            }

                            stats.new_files += file_diff.new_files;
                            stats.modified_files += file_diff.modified_files;
                            stats.deleted_files += file_diff.deleted_files;
                        }
                    }
                    // If unchanged, skip entirely
                }
            }
        }

        copy_writer.finish()?;
        delete_writer.finish()?;

        Ok(stats)
    }

    /// Heap-based diff of two sorted slices.
    ///
    /// Returns a vector of DiffItem indicating which side each element is from.
    fn heap_diff<T, K: Ord + Copy>(
        left: &[T],
        right: &[T],
        key_fn: impl Fn(&T) -> K,
    ) -> Vec<DiffItem<T>>
    where
        T: Clone,
    {
        let mut result = Vec::new();
        let mut i = 0;
        let mut j = 0;

        while i < left.len() && j < right.len() {
            let left_key = key_fn(&left[i]);
            let right_key = key_fn(&right[j]);

            match left_key.cmp(&right_key) {
                std::cmp::Ordering::Less => {
                    // Only in left (deleted)
                    result.push(DiffItem::LeftOnly(left[i].clone()));
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    // Only in right (new)
                    result.push(DiffItem::RightOnly(right[j].clone()));
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    // In both (may be modified)
                    result.push(DiffItem::Both(left[i].clone(), right[j].clone()));
                    i += 1;
                    j += 1;
                }
            }
        }

        // Remaining items in left are deleted
        while i < left.len() {
            result.push(DiffItem::LeftOnly(left[i].clone()));
            i += 1;
        }

        // Remaining items in right are new
        while j < right.len() {
            result.push(DiffItem::RightOnly(right[j].clone()));
            j += 1;
        }

        result
    }

    /// Write all files from a directory (for new directories)
    fn write_all_directory_files(
        &self,
        dir_entry: &DirCacheEntry,
        meta_reader: &MetaRepoReader,
        copy_writer: &mut ControlFileWriter,
        stats: &mut DiffStats,
    ) -> io::Result<()> {
        // Load files from fcache for this directory
        let fcache_path = self
            .curr_meta_dir
            .join(format!("fcache_{}.dat", dir_entry.fcache_fid));
        if let Ok(mut reader) = FileCacheRandomReader::open(&fcache_path) {
            let start_idx = dir_entry.fcache_offset / FileCacheEntry::SIZE as u32;
            for i in 0..dir_entry.files_count {
                if let Ok(fcache_entry) = reader.read_object(start_idx + i) {
                    if let Ok(fmeta) = meta_reader.get_fmeta(fcache_entry.meta_loc) {
                        stats.new_files += 1;
                        let fctrl_entry = FileControlEntry {
                            name: fmeta.common.name.clone(),
                            diff: FileDiff::New,
                            meta_fid: fcache_entry.meta_loc.0,
                            meta_offset: fcache_entry.meta_loc.1,
                        };
                        copy_writer.write_file(&fctrl_entry)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Diff files in a modified directory and write changes
    fn diff_directory_files(
        &self,
        prev_dir_entry: &DirCacheEntry,
        curr_dir_entry: &DirCacheEntry,
        dir_path: &str,
        _prev_meta_reader: Option<&MetaRepoReader>,
        _curr_meta_reader: &MetaRepoReader,
    ) -> io::Result<DirectoryFileDiff> {
        // Load previous files for this directory
        let prev_files = self.load_directory_files(prev_dir_entry, true);
        // Load current files for this directory
        let curr_files = self.load_directory_files(curr_dir_entry, false);

        let mut diff = DirectoryFileDiff::default();

        for (file_name, prev_entry) in &prev_files {
            match curr_files.get(file_name) {
                Some(curr_entry) => {
                    if prev_entry.hash != curr_entry.hash {
                        diff.modified_files += 1;
                        diff.file_entries.push(FileControlEntry {
                            name: file_name.clone(),
                            diff: FileDiff::DataModified,
                            meta_fid: curr_entry.meta_loc.0,
                            meta_offset: curr_entry.meta_loc.1,
                        });
                    }
                }
                None => {
                    diff.deleted_files += 1;
                    diff.delete_entries.push(DeleteEntry {
                        entry_type: DeleteEntryType::File,
                        path: crate::path_util::join_logical(dir_path, file_name),
                    });
                }
            }
        }

        for (file_name, curr_entry) in &curr_files {
            if !prev_files.contains_key(file_name) {
                diff.new_files += 1;
                diff.file_entries.push(FileControlEntry {
                    name: file_name.clone(),
                    diff: FileDiff::New,
                    meta_fid: curr_entry.meta_loc.0,
                    meta_offset: curr_entry.meta_loc.1,
                });
            }
        }

        Ok(diff)
    }

    /// Load files for a specific directory, keyed by file name.
    fn load_directory_files(
        &self,
        dir_entry: &DirCacheEntry,
        is_prev: bool,
    ) -> BTreeMap<String, FileCacheEntry> {
        let mut result = BTreeMap::new();

        let meta_dir = if is_prev {
            self.prev_meta_dir
                .as_ref()
                .map(|p| p.as_path())
                .unwrap_or(&self.curr_meta_dir)
        } else {
            &self.curr_meta_dir
        };

        let meta_reader = match MetaRepoReader::new(meta_dir.to_path_buf()) {
            Ok(reader) => reader,
            Err(_) => return result,
        };

        let fcache_path = meta_dir.join(format!("fcache_{}.dat", dir_entry.fcache_fid));
        let mut reader = match FileCacheRandomReader::open(&fcache_path) {
            Ok(r) => r,
            Err(_) => return result,
        };

        let start_idx = dir_entry.fcache_offset / FileCacheEntry::SIZE as u32;
        for i in 0..dir_entry.files_count {
            if let Ok(fcache_entry) = reader.read_object(start_idx + i) {
                // Get the filename from metadata
                if let Ok(fmeta) = meta_reader.get_fmeta(fcache_entry.meta_loc) {
                    result.insert(fmeta.common.name, fcache_entry);
                }
            }
        }

        result
    }
}

/// Represents a diff result between two sorted sequences
#[derive(Debug)]
enum DiffItem<T> {
    /// Only in left (previous) - deleted
    LeftOnly(T),
    /// Only in right (current) - new
    RightOnly(T),
    /// In both - may be modified
    Both(T, T),
}

/// Statistics from the diff operation.
#[derive(Debug, Default)]
pub struct DiffStats {
    /// Number of new directories
    pub new_dirs: u64,
    /// Number of modified directories
    pub modified_dirs: u64,
    /// Number of deleted directories
    pub deleted_dirs: u64,
    /// Number of new files
    pub new_files: u64,
    /// Number of modified files
    pub modified_files: u64,
    /// Number of deleted files
    pub deleted_files: u64,
}

#[derive(Debug, Default)]
struct DirectoryFileDiff {
    file_entries: Vec<FileControlEntry>,
    delete_entries: Vec<DeleteEntry>,
    new_files: u64,
    modified_files: u64,
    deleted_files: u64,
}

impl DirectoryFileDiff {
    fn has_changes(&self) -> bool {
        self.new_files > 0 || self.modified_files > 0 || self.deleted_files > 0
    }
}

/// Generates incremental control files by comparing previous and current scans.
///
/// This is the main entry point for incremental backup.
///
/// # Arguments
/// * `prev_meta_dir` - Path to previous backup's metadata directory (None for full backup)
/// * `curr_meta_dir` - Path to current scan's metadata directory
/// * `ctrl_dir` - Output directory for copy/delete control files
///
/// # Returns
/// Statistics about the differences found.
pub fn generate_incremental_control_files(
    prev_meta_dir: Option<&Path>,
    curr_meta_dir: &Path,
    ctrl_dir: &Path,
    source_kind: &str,
    source_root: &str,
) -> io::Result<DiffStats> {
    std::fs::create_dir_all(ctrl_dir)?;

    let copy_file_path = primary_control_file_path(ctrl_dir, "copy");
    let delete_file_path = primary_control_file_path(ctrl_dir, "delete");

    let diff = IncrementalDiff::from_dirs(prev_meta_dir, curr_meta_dir)?;
    diff.generate_control_files(&copy_file_path, &delete_file_path, source_kind, source_root)
}

/// A simplified diff generator that works with sorted inode lists.
///
/// This is a more practical implementation that compares two sorted lists
/// of inodes and identifies differences.
pub fn diff_sorted_inodes(prev: &[u64], curr: &[u64]) -> Vec<(u64, DiffType)> {
    let mut result = Vec::new();
    let mut i = 0;
    let mut j = 0;

    while i < prev.len() && j < curr.len() {
        match prev[i].cmp(&curr[j]) {
            std::cmp::Ordering::Less => {
                // In prev but not in curr -> Deleted
                result.push((prev[i], DiffType::Deleted));
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                // In curr but not in prev -> New
                result.push((curr[j], DiffType::New));
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                // In both -> need hash comparison
                // For now, assume unchanged
                i += 1;
                j += 1;
            }
        }
    }

    // Remaining entries in prev are deleted
    while i < prev.len() {
        result.push((prev[i], DiffType::Deleted));
        i += 1;
    }

    // Remaining entries in curr are new
    while j < curr.len() {
        result.push((curr[j], DiffType::New));
        j += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_sorted_inodes() {
        let prev = vec![1, 2, 3, 5, 8];
        let curr = vec![2, 3, 4, 5, 6];

        let result = diff_sorted_inodes(&prev, &curr);

        // 1 is deleted
        assert_eq!(result[0], (1, DiffType::Deleted));
        // 2, 3, 5 unchanged
        // 4 is new
        assert_eq!(result[1], (4, DiffType::New));
        // 6 is new
        assert_eq!(result[2], (6, DiffType::New));
        // 8 is deleted
        assert_eq!(result[3], (8, DiffType::Deleted));
    }
}
