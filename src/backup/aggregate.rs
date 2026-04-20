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

/// Snowflake-like unique ID generator for blob filenames.
/// Generates unique 64-bit IDs using timestamp, process ID, and sequence number.
/// This ensures unique IDs even when multiple processes write to the same directory.
pub struct SnowflakeIdGenerator {
    /// Last timestamp (milliseconds since custom epoch)
    last_timestamp: u64,
    /// Sequence number (12 bits)
    sequence: u16,
    /// Process ID (10 bits)
    process_id: u16,
    /// Custom epoch (milliseconds) - Bifrost project start date
    epoch: u64,
}

impl SnowflakeIdGenerator {
    /// Custom epoch: 2024-01-01 00:00:00 UTC in milliseconds
    const BIFROST_EPOCH: u64 = 1704067200000;
    
    /// Maximum sequence number (12 bits)
    const MAX_SEQUENCE: u16 = 4095;
    
    /// Creates a new Snowflake ID generator.
    /// process_id should be unique per process (0-1023).
    pub fn new(process_id: u16) -> Self {
        Self {
            last_timestamp: 0,
            sequence: 0,
            process_id: process_id & 0x3FF, // 10 bits
            epoch: Self::BIFROST_EPOCH,
        }
    }
    
    /// Creates a new Snowflake ID generator with default process ID.
    /// Uses process ID derived from current process ID modulo 1024.
    pub fn default() -> Self {
        let pid = std::process::id() as u16 & 0x3FF;
        Self::new(pid)
    }
    
    /// Gets current timestamp in milliseconds since epoch.
    fn current_timestamp(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        now.saturating_sub(self.epoch)
    }
    
    /// Waits until the next millisecond.
    fn wait_next_millis(&self, last: u64) -> u64 {
        let mut timestamp = self.current_timestamp();
        while timestamp <= last {
            std::thread::yield_now();
            timestamp = self.current_timestamp();
        }
        timestamp
    }
    
    /// Generates a new unique 64-bit ID.
    /// 
    /// ID structure (64 bits):
    /// - 41 bits: Timestamp (milliseconds since epoch, ~69 years)
    /// - 10 bits: Process ID (0-1023, unique per process)
    /// - 12 bits: Sequence number (0-4095, per millisecond)
    /// - 1 bit: Reserved (0)
    pub fn next_id(&mut self) -> u64 {
        let mut timestamp = self.current_timestamp();
        
        if timestamp < self.last_timestamp {
            // Clock moved backwards, wait until we catch up
            timestamp = self.wait_next_millis(self.last_timestamp);
        }
        
        if timestamp == self.last_timestamp {
            // Same millisecond, increment sequence
            self.sequence += 1;
            if self.sequence > Self::MAX_SEQUENCE {
                // Sequence overflow, wait for next millisecond
                timestamp = self.wait_next_millis(timestamp);
                self.sequence = 0;
            }
        } else {
            // New millisecond, reset sequence
            self.sequence = 0;
        }
        
        self.last_timestamp = timestamp;
        
        // Compose the ID
        // | 41 bits timestamp | 10 bits process | 12 bits sequence | 1 bit reserved |
        (timestamp << 23) | ((self.process_id as u64) << 12) | (self.sequence as u64)
    }
    
    /// Generates a unique blob filename.
    pub fn generate_blob_name(&mut self) -> String {
        format!("{:016x}.bifrost.blob", self.next_id())
    }
}

/// Thread-safe Snowflake ID generator wrapper.
pub struct ThreadSafeSnowflake {
    inner: std::sync::Mutex<SnowflakeIdGenerator>,
}

impl ThreadSafeSnowflake {
    /// Creates a new thread-safe Snowflake generator.
    pub fn new(process_id: u16) -> Self {
        Self {
            inner: std::sync::Mutex::new(SnowflakeIdGenerator::new(process_id)),
        }
    }
    
    /// Creates a new thread-safe Snowflake generator with default process ID.
    pub fn default() -> Self {
        Self {
            inner: std::sync::Mutex::new(SnowflakeIdGenerator::default()),
        }
    }
    
    /// Generates a new unique blob filename.
    pub fn generate_blob_name(&self) -> String {
        let mut generator = self.inner.lock().unwrap();
        generator.generate_blob_name()
    }
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
