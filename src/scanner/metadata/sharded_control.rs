//! # Sharded Control File Management
//!
//! This module provides scalable control file handling for extremely large filesets
//! (100+ billion files). It supports:
//!
//! 1. **Sharded Control Files**: Control files are split into multiple shards
//!    (e.g., `copy_00000000.txt`, `copy_00000001.txt`) for parallel processing.
//!
//! 2. **Batch Processing**: Large directories are split across multiple batches
//!    with markers (BATCH=n/m) to handle billions of files per directory.
//!
//! 3. **Concurrent Execution**: Each shard can be processed independently,
//!    enabling parallel backup/restore operations.
//!
//! ## Sharding Strategy
//!
//! Directories are assigned to shards using a deterministic hash of their path:
//! ```text
//! shard_id = hash(dir_path) % num_shards
//! ```
//!
//! This ensures:
//! - All files in the same directory go to the same shard
//! - Deterministic shard assignment for reproducibility
//! - Load balancing across shards
//!
//! ## Batch Markers
//!
//! For directories with many files, entries are split with batch markers:
//! ```text
//! D NN ... BATCH=0/3
//! F NN ... file1.txt
//! ...
//! D NN ... BATCH=1/3 CONT
//! F NN ... fileN.txt
//! ...
//! D NN ... BATCH=2/3 CONT LAST
//! ```

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::scanner::metadata::{
    ControlFileWriter, DirControlEntry, FileControlEntry,
};

/// Default maximum entries per control file shard for copy phase.
/// With ~100 bytes per entry, this keeps shards under ~100MB.
pub const DEFAULT_MAX_ENTRIES_PER_SHARD_COPY: usize = 1_000_000;

/// Default maximum entries per control file shard for other phases (delete, hardlink, mtime).
/// These phases have smaller entries, so we can use a higher limit.
pub const DEFAULT_MAX_ENTRIES_PER_SHARD_OTHER: usize = 5_000_000;

/// Default maximum shard file size for copy phase (100MB).
pub const DEFAULT_MAX_SHARD_SIZE_COPY: u64 = 100 * 1024 * 1024;

/// Default maximum files per directory batch.
/// Directories with more files are split across multiple entries.
pub const DEFAULT_MAX_FILES_PER_BATCH: u32 = 100_000;

/// Split policy for control file sharding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShardSplitPolicy {
    /// Split by maximum file size (used for copy phase).
    /// Rolls over when file size exceeds limit.
    MaxSize {
        /// Maximum file size in bytes
        max_size: u64,
        /// Also check entry count as safety limit
        max_entries: usize,
    },
    /// Split by maximum entry count (used for delete, hardlink, mtime phases).
    /// Rolls over when entry count exceeds limit.
    MaxEntries {
        /// Maximum number of entries per shard
        max_entries: usize,
    },
}

impl ShardSplitPolicy {
    /// Returns the default policy for copy phase.
    pub fn copy_default() -> Self {
        Self::MaxSize {
            max_size: DEFAULT_MAX_SHARD_SIZE_COPY,
            max_entries: DEFAULT_MAX_ENTRIES_PER_SHARD_COPY,
        }
    }

    /// Returns the default policy for other phases (delete, hardlink, mtime).
    pub fn other_default() -> Self {
        Self::MaxEntries {
            max_entries: DEFAULT_MAX_ENTRIES_PER_SHARD_OTHER,
        }
    }

    /// Checks if a rollover is needed based on current file size and entry count.
    pub fn needs_rollover(&self, current_size: u64, entry_count: usize) -> bool {
        match self {
            Self::MaxSize { max_size, max_entries } => {
                current_size >= *max_size || entry_count >= *max_entries
            }
            Self::MaxEntries { max_entries } => {
                entry_count >= *max_entries
            }
        }
    }
}

/// Manages sharded control file writing.
pub struct ShardedControlFileManager {
    /// Output directory for control files
    ctrl_dir: PathBuf,
    /// Base name for control files (e.g., "copy")
    base_name: String,
    /// Number of shards
    num_shards: usize,
    /// Split policy for shard rollover
    split_policy: ShardSplitPolicy,
    /// Maximum files per directory batch
    max_files_per_batch: u32,
    /// Active shard writers
    shards: HashMap<usize, ShardWriter>,
    /// Current entry count per shard
    shard_entry_counts: HashMap<usize, usize>,
    /// Current file index per shard (for rollover)
    shard_file_indices: HashMap<usize, u32>,
}

/// A single shard writer that can roll over to new files.
struct ShardWriter {
    /// Current file writer
    writer: ControlFileWriter,
    /// Current file path
    path: PathBuf,
    /// Entry count in current file
    entry_count: usize,
    /// Current file size in bytes
    file_size: u64,
}

/// Batch information for large directories.
#[derive(Debug, Clone, Copy)]
pub struct BatchInfo {
    /// Current batch number (0-indexed)
    pub batch_num: u32,
    /// Total number of batches
    pub total_batches: u32,
    /// Whether this is a continuation batch
    pub is_continuation: bool,
    /// Whether this is the last batch
    pub is_last: bool,
}

impl ShardedControlFileManager {
    /// Creates a new sharded control file manager with default split policy.
    ///
    /// Uses `MaxSize` policy for copy phase and `MaxEntries` for others.
    /// Use `with_policy()` for explicit policy control.
    ///
    /// # Arguments
    /// * `ctrl_dir` - Directory for control files
    /// * `base_name` - Base name (e.g., "copy", "delete", "mtime")
    /// * `num_shards` - Number of shards (e.g., 16, 64, 256)
    pub fn new(
        ctrl_dir: PathBuf,
        base_name: String,
        num_shards: usize,
    ) -> io::Result<Self> {
        // Choose default policy based on phase
        let policy = if base_name == "copy" {
            ShardSplitPolicy::copy_default()
        } else {
            ShardSplitPolicy::other_default()
        };
        Self::with_policy(ctrl_dir, base_name, num_shards, policy)
    }

    /// Creates a new sharded control file manager with explicit split policy.
    ///
    /// # Arguments
    /// * `ctrl_dir` - Directory for control files
    /// * `base_name` - Base name (e.g., "copy", "delete", "mtime")
    /// * `num_shards` - Number of shards (e.g., 16, 64, 256)
    /// * `split_policy` - Split policy for shard rollover
    pub fn with_policy(
        ctrl_dir: PathBuf,
        base_name: String,
        num_shards: usize,
        split_policy: ShardSplitPolicy,
    ) -> io::Result<Self> {
        fs::create_dir_all(&ctrl_dir)?;
        
        Ok(Self {
            ctrl_dir,
            base_name,
            num_shards,
            split_policy,
            max_files_per_batch: DEFAULT_MAX_FILES_PER_BATCH,
            shards: HashMap::new(),
            shard_entry_counts: HashMap::new(),
            shard_file_indices: HashMap::new(),
        })
    }

    /// Sets the split policy.
    pub fn split_policy(mut self, policy: ShardSplitPolicy) -> Self {
        self.split_policy = policy;
        self
    }

    /// Sets the maximum files per directory batch.
    pub fn max_files_per_batch(mut self, max: u32) -> Self {
        self.max_files_per_batch = max;
        self
    }

    /// Computes the shard ID for a directory path.
    fn compute_shard_id(&self, dir_path: &str) -> usize {
        // Use a simple but effective hash function (FNV-1a inspired)
        let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
        for byte in dir_path.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3); // FNV prime
        }
        (hash % self.num_shards as u64) as usize
    }

    /// Gets or creates a shard writer.
    fn get_shard_writer(&mut self, shard_id: usize) -> io::Result<&mut ShardWriter> {
        // Check if we need to roll over to a new file
        let should_rollover = match self.shards.get(&shard_id) {
            Some(shard) => self.split_policy.needs_rollover(shard.file_size, shard.entry_count),
            None => true,
        };

        if should_rollover {
            // Close existing writer if any
            if let Some(old_shard) = self.shards.remove(&shard_id) {
                old_shard.writer.finish()?;
            }

            // Get next file index for this shard
            let file_index = self.shard_file_indices
                .get(&shard_id)
                .copied()
                .unwrap_or(0);
            
            let path = self.ctrl_dir.join(format!(
                "{}_{:08X}_{:04X}.txt",
                self.base_name, shard_id, file_index
            ));
            
            let writer = ControlFileWriter::new(&path)?;
            
            self.shards.insert(shard_id, ShardWriter {
                writer,
                path: path.clone(),
                entry_count: 0,
                file_size: 0,
            });
            
            self.shard_file_indices.insert(shard_id, file_index + 1);
            self.shard_entry_counts.insert(shard_id, 0);
        }

        Ok(self.shards.get_mut(&shard_id).unwrap())
    }

    /// Writes a directory entry with batch support.
    ///
    /// For large directories, this may write multiple entries with batch markers.
    /// Returns the number of entries written.
    pub fn write_directory(
        &mut self,
        entry: &DirControlEntry,
        batch_info: Option<BatchInfo>,
    ) -> io::Result<usize> {
        let shard_id = self.compute_shard_id(&entry.path);
        let shard = self.get_shard_writer(shard_id)?;
        
        // Write directory entry with batch marker if needed
        let entry_size = if let Some(batch) = batch_info {
            shard.writer.write_dir_with_batch(entry, batch)?;
            // Estimate: base dir entry ~60 bytes + path + batch marker ~20 bytes
            80 + entry.path.len()
        } else {
            shard.writer.write_dir(entry)?;
            // Estimate: base dir entry ~60 bytes + path
            60 + entry.path.len()
        };
        
        shard.entry_count += 1;
        shard.file_size += entry_size as u64;
        *self.shard_entry_counts.entry(shard_id).or_insert(0) += 1;
        
        Ok(1)
    }

    /// Writes a file entry.
    pub fn write_file(
        &mut self,
        dir_path: &str,
        entry: &FileControlEntry,
    ) -> io::Result<()> {
        let shard_id = self.compute_shard_id(dir_path);
        let shard = self.get_shard_writer(shard_id)?;
        
        shard.writer.write_file(entry)?;
        // Estimate: base file entry ~50 bytes + filename
        let entry_size = 50 + entry.name.len();
        shard.entry_count += 1;
        shard.file_size += entry_size as u64;
        *self.shard_entry_counts.entry(shard_id).or_insert(0) += 1;
        
        Ok(())
    }

    /// Computes batch information for a directory.
    ///
    /// Returns a vector of (batch_info, files_in_batch) tuples.
    pub fn compute_batches(&self, total_files: u32) -> Vec<(BatchInfo, u32)> {
        if total_files <= self.max_files_per_batch {
            // No batching needed
            return vec![(
                BatchInfo {
                    batch_num: 0,
                    total_batches: 1,
                    is_continuation: false,
                    is_last: true,
                },
                total_files,
            )];
        }

        let num_batches = (total_files + self.max_files_per_batch - 1) / self.max_files_per_batch;
        let mut batches = Vec::with_capacity(num_batches as usize);
        
        for batch_num in 0..num_batches {
            let start_file = batch_num * self.max_files_per_batch;
            let files_in_batch = (total_files - start_file).min(self.max_files_per_batch);
            
            batches.push((
                BatchInfo {
                    batch_num,
                    total_batches: num_batches,
                    is_continuation: batch_num > 0,
                    is_last: batch_num == num_batches - 1,
                },
                files_in_batch,
            ));
        }
        
        batches
    }

    /// Finalizes all shards and returns the list of created files.
    pub fn finish(mut self) -> io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        
        for (_shard_id, shard) in self.shards.drain() {
            shard.writer.finish()?;
            files.push(shard.path);
        }
        
        Ok(files)
    }

    /// Returns statistics about shard distribution.
    pub fn shard_stats(&self) -> HashMap<usize, usize> {
        self.shard_entry_counts.clone()
    }
}

/// Extended ControlFileWriter with batch support.
#[allow(dead_code)]
pub trait ControlFileWriterExt {
    /// Writes a directory entry with batch information.
    fn write_dir_with_batch(
        &mut self,
        entry: &DirControlEntry,
        batch: BatchInfo,
    ) -> io::Result<()>;
}

// Implementation of ControlFileWriterExt is in the same module as ControlFileWriter
// We'll add methods to ControlFileWriter instead

/// Information about a sharded control file set.
#[derive(Debug)]
pub struct ShardedControlInfo {
    /// Base name (e.g., "copy")
    pub base_name: String,
    /// Number of shards
    pub num_shards: usize,
    /// List of shard files
    pub shard_files: Vec<PathBuf>,
}

/// Discovers existing sharded control files in a directory.
pub fn discover_sharded_controls(
    ctrl_dir: &Path,
    base_name: &str,
) -> io::Result<ShardedControlInfo> {
    let mut shard_files = Vec::new();
    
    if ctrl_dir.exists() {
        for entry in fs::read_dir(ctrl_dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            
            // Match pattern: {base_name}_{shard_id}_{file_index}.txt
            if file_name.starts_with(&format!("{}_", base_name))
                && file_name.ends_with(".txt")
            {
                shard_files.push(path);
            }
        }
    }
    
    // Sort by path for consistent ordering
    shard_files.sort();
    
    // Extract num_shards from file names
    let num_shards = shard_files.iter()
        .filter_map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|name| {
                    let parts: Vec<_> = name.split('_').collect();
                    if parts.len() >= 2 {
                        u32::from_str_radix(parts[1], 16).ok()
                    } else {
                        None
                    }
                })
        })
        .max()
        .map(|max_id| max_id as usize + 1)
        .unwrap_or(0);
    
    Ok(ShardedControlInfo {
        base_name: base_name.to_string(),
        num_shards,
        shard_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_shard_id_deterministic() {
        let manager = ShardedControlFileManager::new(
            PathBuf::from("/tmp"),
            "copy".to_string(),
            16,
        ).unwrap();
        
        let path = "/home/user/documents";
        let id1 = manager.compute_shard_id(path);
        let id2 = manager.compute_shard_id(path);
        
        assert_eq!(id1, id2);
        assert!(id1 < 16);
    }

    #[test]
    fn test_compute_batches() {
        let manager = ShardedControlFileManager::new(
            PathBuf::from("/tmp"),
            "copy".to_string(),
            16,
        ).unwrap()
        .max_files_per_batch(1000);
        
        // Small directory - no batching
        let batches = manager.compute_batches(500);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].0.total_batches, 1);
        assert!(batches[0].0.is_last);
        
        // Large directory - needs batching
        let batches = manager.compute_batches(2500);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].0.batch_num, 0);
        assert!(!batches[0].0.is_continuation);
        assert!(!batches[0].0.is_last);  // First batch is not the last
        assert_eq!(batches[1].0.batch_num, 1);
        assert!(batches[1].0.is_continuation);
        assert!(!batches[1].0.is_last);  // Middle batch is not the last
        assert_eq!(batches[2].0.batch_num, 2);
        assert!(batches[2].0.is_continuation);
        assert!(batches[2].0.is_last);  // Only the last batch has is_last=true
    }
}
