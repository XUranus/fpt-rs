//! # Aggregate Backup Module
//!
//! This module implements file aggregation for efficient backup of small files.
//! Multiple small files are combined into larger "blob" files to reduce overhead
//! when handling millions of small files.
//!
//! ## Key Concepts
//!
//! - **Blob File**: A large file containing multiple small files concatenated together.
//! - **Aggregate Index**: SQLite database mapping original filenames to their locations
//!   within blob files (blob filename, offset, size).
//! - **Threshold**: Files smaller than `aggregate_file_threshold` are candidates for aggregation.
//! - **Blob Size**: Maximum size of a blob file (default 64MB).
//!
//! ## Backup Process
//!
//! 1. Small files are collected and buffered in memory
//! 2. When buffer reaches `blob_size` or directory is complete, create blob file
//! 3. Write SQLite index mapping filenames to blob locations
//! 4. Large files are backed up normally (non-aggregated)
//!
//! ## Restore Process
//!
//! 1. Query SQLite index to find blob file, offset, and size for each file
//! 2. Read blob file and extract specific byte ranges
//! 3. Write extracted files to destination

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// Metadata for a single file within an aggregate blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateFileEntry {
    /// Original filename (relative to backup root)
    pub file_name: String,
    /// Offset within the blob file where this file's data starts
    pub offset: u64,
    /// Size of the file in bytes
    pub size: u64,
    /// Creation time (seconds since Unix epoch)
    pub ctime: u64,
    /// Modification time (seconds since Unix epoch)
    pub mtime: u64,
    /// File permissions/mode
    pub mode: u32,
    /// Extended attributes (serialized)
    pub xattrs: Option<String>,
    /// ACL (serialized)
    pub acl: Option<String>,
}

/// Metadata for an aggregate blob file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateBlobMeta {
    /// Unique blob filename (e.g., "1234567890123456789.bifrost.blob")
    pub blob_name: String,
    /// Total size of the blob file
    pub blob_size: u64,
    /// Number of files in this blob
    pub file_count: u32,
    /// List of files contained in this blob
    pub files: Vec<AggregateFileEntry>,
    /// Directory path (all files in blob are from same directory)
    pub dir_path: String,
}

/// Configuration for aggregation behavior.
#[derive(Debug, Clone, Copy)]
pub struct AggregateConfig {
    /// Whether aggregation is enabled
    pub enabled: bool,
    /// Maximum size of a blob file in bytes (default: 64MB)
    pub max_blob_size: u64,
    /// Files smaller than this threshold are aggregated (default: 1MB)
    pub file_threshold: u64,
}

impl Default for AggregateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_blob_size: 64 * 1024 * 1024, // 64MB
            file_threshold: 1024 * 1024,      // 1MB
        }
    }
}

impl AggregateConfig {
    /// Creates a new config with aggregation enabled.
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }

    /// Sets the maximum blob size.
    pub fn max_blob_size(mut self, size: u64) -> Self {
        self.max_blob_size = size;
        self
    }

    /// Sets the file threshold for aggregation.
    pub fn file_threshold(mut self, threshold: u64) -> Self {
        self.file_threshold = threshold;
        self
    }
}

/// In-memory index for aggregate files during backup.
/// Maps directory paths to their pending aggregation buffers.
#[derive(Debug, Default)]
pub struct AggregateBuffer {
    /// Directory path -> pending files for aggregation
    pub dir_buffers: HashMap<String, DirAggregateBuffer>,
}

/// Buffer for aggregating files within a single directory.
#[derive(Debug)]
pub struct DirAggregateBuffer {
    /// Directory path
    pub dir_path: String,
    /// Files pending aggregation
    pub pending_files: Vec<PendingFile>,
    /// Current buffer size in bytes
    pub current_size: u64,
    /// Maximum buffer size before flushing
    pub max_size: u64,
}

/// A file waiting to be aggregated.
#[derive(Debug)]
pub struct PendingFile {
    /// File metadata
    pub file_name: String,
    /// File content
    pub data: Vec<u8>,
    /// File metadata
    pub ctime: u64,
    pub mtime: u64,
    pub mode: u32,
    pub xattrs: Option<String>,
    pub acl: Option<String>,
}

impl DirAggregateBuffer {
    /// Creates a new directory buffer with the specified max size.
    pub fn new(dir_path: String, max_size: u64) -> Self {
        Self {
            dir_path,
            pending_files: Vec::new(),
            current_size: 0,
            max_size,
        }
    }

    /// Adds a file to the buffer. Returns true if buffer should be flushed.
    pub fn add_file(&mut self, file: PendingFile) -> bool {
        self.current_size += file.data.len() as u64;
        self.pending_files.push(file);
        self.current_size >= self.max_size
    }

    /// Checks if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.pending_files.is_empty()
    }

    /// Clears the buffer and returns pending files.
    pub fn flush(&mut self) -> Vec<PendingFile> {
        let files = std::mem::take(&mut self.pending_files);
        self.current_size = 0;
        files
    }
}

/// Information for restoring a file from an aggregate blob.
#[derive(Debug, Clone)]
pub struct AggregateRestoreInfo {
    /// Blob filename
    pub blob_name: String,
    /// Offset within the blob
    pub offset: u64,
    /// Size of the file
    pub size: u64,
    /// File metadata
    pub mtime: u64,
    pub mode: u32,
    pub xattrs: Option<String>,
    pub acl: Option<String>,
}

/// Statistics for aggregate operations.
#[derive(Debug, Default, Clone)]
pub struct AggregateStats {
    /// Number of blob files created
    pub blobs_created: u64,
    /// Number of files aggregated
    pub files_aggregated: u64,
    /// Number of files backed up normally (non-aggregated)
    pub files_normal: u64,
    /// Total bytes in blob files
    pub blob_bytes: u64,
    /// Total bytes of original files
    pub original_bytes: u64,
}

/// Generate a unique blob filename using timestamp and counter.
pub fn generate_blob_name(counter: u64) -> String {
    format!("{:016x}.bifrost.blob", counter)
}

/// Check if a file should be aggregated based on size.
pub fn should_aggregate(file_size: u64, config: &AggregateConfig) -> bool {
    config.enabled && file_size > 0 && file_size < config.file_threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aggregate_config_default() {
        let config = AggregateConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_blob_size, 64 * 1024 * 1024);
        assert_eq!(config.file_threshold, 1024 * 1024);
    }

    #[test]
    fn test_should_aggregate() {
        let config = AggregateConfig::enabled();
        assert!(should_aggregate(100, &config));
        assert!(should_aggregate(1024 * 1024 - 1, &config));
        assert!(!should_aggregate(1024 * 1024, &config));
        assert!(!should_aggregate(0, &config));
    }

    #[test]
    fn test_dir_buffer() {
        let mut buffer = DirAggregateBuffer::new("/test".to_string(), 100);
        assert!(buffer.is_empty());

        let file = PendingFile {
            file_name: "test.txt".to_string(),
            data: vec![0u8; 50],
            ctime: 0,
            mtime: 0,
            mode: 0o644,
            xattrs: None,
            acl: None,
        };

        let should_flush = buffer.add_file(file);
        assert!(!should_flush);
        assert!(!buffer.is_empty());

        let file2 = PendingFile {
            file_name: "test2.txt".to_string(),
            data: vec![0u8; 60],
            ctime: 0,
            mtime: 0,
            mode: 0o644,
            xattrs: None,
            acl: None,
        };

        let should_flush = buffer.add_file(file2);
        assert!(should_flush);

        let files = buffer.flush();
        assert_eq!(files.len(), 2);
        assert!(buffer.is_empty());
    }
}
