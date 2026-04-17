//! # Aggregate Restore Engine
//!
//! This module implements the unaggregation logic for restore operations.
//! It reads blob files from .AGGR_DIR/ subdirectories and extracts individual
//! files based on per-directory SQLite indexes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use log::{debug, error, info, warn};

use crate::backup::aggregate::AggregateRestoreInfo;
use crate::backup::aggregate_index::AggregateIndex;
use crate::backup::fcb::{ControlBlockVarient, FileControlBlock, TargetHandleState};
use crate::backup::stats::BackupStats;
use crate::backup::SharedState;

/// Per-directory restore information
struct DirRestoreInfo {
    /// Path to the source directory
    source_dir: PathBuf,
    /// Path to the .AGGR_DIR subdirectory
    aggr_dir: PathBuf,
    /// The index (opened on first use)
    index: Option<AggregateIndex>,
}

impl DirRestoreInfo {
    fn new(source_dir: PathBuf) -> Self {
        let aggr_dir = source_dir.join(".AGGR_DIR");
        
        Self {
            source_dir,
            aggr_dir,
            index: None,
        }
    }
    
    fn has_index(&self) -> bool {
        self.aggr_dir.join("AGGREGATE_IDX.sqlite").exists()
    }
    
    fn get_or_open_index(&mut self) -> Result<&AggregateIndex, AggregateRestoreError> {
        if self.index.is_none() {
            let index_path = self.aggr_dir.join("AGGREGATE_IDX.sqlite");
            if !index_path.exists() {
                return Err(AggregateRestoreError::Other(
                    format!("Index not found: {}", index_path.display())
                ));
            }
            self.index = Some(AggregateIndex::open(&index_path)?);
        }
        Ok(self.index.as_ref().unwrap())
    }
}

/// Engine for performing aggregate restores.
pub struct AggregateRestoreEngine {
    source_base: PathBuf,
    /// Cache of directory info by source directory path
    dir_info: Mutex<HashMap<String, DirRestoreInfo>>,
    /// Blob cache: blob_path -> blob_data
    blob_cache: Mutex<HashMap<String, Vec<u8>>>,
    stats: Arc<Mutex<AggregateRestoreStats>>,
}

/// Statistics for aggregate restore operations.
#[derive(Debug, Default, Clone)]
pub struct AggregateRestoreStats {
    /// Number of files restored from blobs
    pub files_from_blobs: u64,
    /// Number of files restored normally (non-aggregated)
    pub files_normal: u64,
    /// Total bytes restored from blobs
    pub bytes_from_blobs: u64,
    /// Number of blob files read
    pub blobs_read: u64,
    /// Number of cache hits
    pub cache_hits: u64,
    /// Number of cache misses
    pub cache_misses: u64,
}

impl AggregateRestoreEngine {
    /// Creates a new aggregate restore engine.
    pub fn new(
        source_base: PathBuf,
    ) -> Result<Self, AggregateRestoreError> {
        Ok(Self {
            source_base,
            dir_info: Mutex::new(HashMap::new()),
            blob_cache: Mutex::new(HashMap::new()),
            stats: Arc::new(Mutex::new(AggregateRestoreStats::default())),
        })
    }

    /// Gets or creates directory restore info for a source directory
    fn get_dir_info(&self, source_dir: &str) -> Option<DirRestoreInfo> {
        let mut dir_info_map = self.dir_info.lock().unwrap();

        if let Some(info) = dir_info_map.get(source_dir) {
            // Clone the basic info (without the open index)
            return Some(DirRestoreInfo {
                source_dir: info.source_dir.clone(),
                aggr_dir: info.aggr_dir.clone(),
                index: None, // Will be opened on demand
            });
        }

        // Create new dir info
        let source_path = Path::new(source_dir);
        let dir_info = DirRestoreInfo::new(source_path.to_path_buf());

        if dir_info.has_index() {
            let result = Some(DirRestoreInfo {
                source_dir: dir_info.source_dir.clone(),
                aggr_dir: dir_info.aggr_dir.clone(),
                index: None,
            });
            dir_info_map.insert(source_dir.to_string(), dir_info);
            result
        } else {
            None
        }
    }

    /// Checks if a file is aggregated (exists in the per-directory index).
    pub fn is_aggregated(&self, file_name: &str, dir_path: &str) -> Result<bool, AggregateRestoreError> {
        if let Some(mut dir_info) = self.get_dir_info(dir_path) {
            if let Ok(index) = dir_info.get_or_open_index() {
                return Ok(index.is_aggregated(file_name, dir_path)?);
            }
        }
        Ok(false)
    }

    /// Gets restore info for a file.
    pub fn get_restore_info(&self, file_name: &str, dir_path: &str) -> Result<Option<AggregateRestoreInfo>, AggregateRestoreError> {
        if let Some(mut dir_info) = self.get_dir_info(dir_path) {
            if let Ok(index) = dir_info.get_or_open_index() {
                return Ok(index.query_file(file_name, dir_path)?);
            }
        }
        Ok(None)
    }

    /// Reads a file from a blob.
    pub fn read_from_blob(
        &self,
        dir_path: &str,
        blob_name: &str,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, AggregateRestoreError> {
        // Get the .AGGR_DIR directory for this directory
        let aggr_dir = Path::new(dir_path).join(".AGGR_DIR");
        let blob_path = aggr_dir.join(blob_name);
        let blob_path_str = blob_path.to_string_lossy().to_string();
        
        // Check cache first
        {
            let cache = self.blob_cache.lock().unwrap();
            if let Some(blob_data) = cache.get(&blob_path_str) {
                let mut stats = self.stats.lock().unwrap();
                stats.cache_hits += 1;
                
                let start = offset as usize;
                let end = (offset + size) as usize;
                if end <= blob_data.len() {
                    return Ok(blob_data[start..end].to_vec());
                } else {
                    return Err(AggregateRestoreError::Other(
                        format!("Offset {} + size {} exceeds blob size {}", 
                            offset, size, blob_data.len())
                    ));
                }
            }
        }

        // Cache miss - read blob from disk
        let mut blob_file = File::open(&blob_path)?;
        
        let mut blob_data = Vec::new();
        blob_file.read_to_end(&mut blob_data)?;
        
        // Update cache
        {
            let mut cache = self.blob_cache.lock().unwrap();
            cache.insert(blob_path_str.clone(), blob_data.clone());
            
            let mut stats = self.stats.lock().unwrap();
            stats.cache_misses += 1;
            stats.blobs_read += 1;
        }

        // Extract file data
        let start = offset as usize;
        let end = (offset + size) as usize;
        if end <= blob_data.len() {
            Ok(blob_data[start..end].to_vec())
        } else {
            Err(AggregateRestoreError::Other(
                format!("Offset {} + size {} exceeds blob size {}", 
                    offset, size, blob_data.len())
            ))
        }
    }

    /// Restores a single file from a blob to the target path.
    pub fn restore_file(
        &self,
        dir_path: &str,
        info: &AggregateRestoreInfo,
        target_path: &Path,
    ) -> Result<(), AggregateRestoreError> {
        // Read file data from blob
        let data = self.read_from_blob(dir_path, &info.blob_name, info.offset, info.size)?;
        
        // Create parent directory if needed
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // Write file
        let mut file = File::create(target_path)?;
        file.write_all(&data)?;
        file.flush()?;
        drop(file);

        // Restore metadata
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            
            // Set permissions
            let permissions = std::fs::Permissions::from_mode(info.mode);
            std::fs::set_permissions(target_path, permissions)?;
            
            // Set modification time
            let mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(info.mtime);
            let atime = mtime; // Use mtime for atime too
            let times = std::fs::FileTimes::new()
                .set_modified(mtime)
                .set_accessed(atime);
            File::open(target_path)?.set_times(times)?;
            
            // Restore xattrs
            if let Some(ref xattrs) = info.xattrs {
                restore_xattrs(target_path, xattrs);
            }
            
            // Restore ACL
            if let Some(ref acl) = info.acl {
                restore_acl(target_path, acl);
            }
        }

        // Update stats
        let mut stats = self.stats.lock().unwrap();
        stats.files_from_blobs += 1;
        stats.bytes_from_blobs += info.size;

        debug!("Restored {} from blob {} (offset: {}, size: {})",
            target_path.display(), info.blob_name, info.offset, info.size);

        Ok(())
    }

    /// Gets current statistics.
    pub fn stats(&self) -> AggregateRestoreStats {
        self.stats.lock().unwrap().clone()
    }

    /// Clears the blob cache to free memory.
    pub fn clear_cache(&self) {
        let mut cache = self.blob_cache.lock().unwrap();
        cache.clear();
        info!("Blob cache cleared");
    }
}

/// Errors that can occur in the aggregate restore engine.
#[derive(Debug)]
pub enum AggregateRestoreError {
    Io(io::Error),
    Index(crate::backup::aggregate_index::AggregateIndexError),
    Other(String),
}

impl std::fmt::Display for AggregateRestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregateRestoreError::Io(e) => write!(f, "IO error: {}", e),
            AggregateRestoreError::Index(e) => write!(f, "Index error: {}", e),
            AggregateRestoreError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for AggregateRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AggregateRestoreError::Io(e) => Some(e),
            AggregateRestoreError::Index(e) => Some(e),
            AggregateRestoreError::Other(_) => None,
        }
    }
}

impl From<io::Error> for AggregateRestoreError {
    fn from(e: io::Error) -> Self {
        AggregateRestoreError::Io(e)
    }
}

impl From<crate::backup::aggregate_index::AggregateIndexError> for AggregateRestoreError {
    fn from(e: crate::backup::aggregate_index::AggregateIndexError) -> Self {
        AggregateRestoreError::Index(e)
    }
}

/// Restores extended attributes on Linux.
#[cfg(target_os = "linux")]
fn restore_xattrs(path: &Path, xattrs: &str) {
    // Parse xattrs from base64-encoded string
    if let Ok(decoded) = base64::decode(xattrs) {
        // Simple format: name\0value\0name\0value\0
        let mut parts = decoded.split(|&b| b == 0);
        while let Some(name) = parts.next() {
            if let Some(value) = parts.next() {
                let name_str = String::from_utf8_lossy(name);
                if let Err(e) = xattr::set(path, name_str.as_ref(), value) {
                    warn!("Failed to set xattr {} on {}: {}", name_str, path.display(), e);
                }
            }
        }
    }
}

/// Restores ACL on Linux.
#[cfg(target_os = "linux")]
fn restore_acl(path: &Path, acl: &str) {
    // ACL is stored as a string representation
    // This is a simplified version - full implementation would parse and apply ACL
    debug!("Restoring ACL on {}: {}", path.display(), acl);
}

#[cfg(not(target_os = "linux"))]
fn restore_xattrs(_path: &Path, _xattrs: &str) {}

#[cfg(not(target_os = "linux"))]
fn restore_acl(_path: &Path, _acl: &str) {}
