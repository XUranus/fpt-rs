//! # Aggregate Restore Engine
//!
//! This module implements the unaggregation logic for restore operations.
//! It reads blob files and extracts individual files based on the index.

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

/// Engine for performing aggregate restores.
pub struct AggregateRestoreEngine {
    source_dir: PathBuf,
    index: Arc<Mutex<AggregateIndex>>,
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
        source_dir: PathBuf,
        index_path: &Path,
    ) -> Result<Self, AggregateRestoreError> {
        let index = AggregateIndex::open(index_path)?;
        
        Ok(Self {
            source_dir,
            index: Arc::new(Mutex::new(index)),
            blob_cache: Mutex::new(HashMap::new()),
            stats: Arc::new(Mutex::new(AggregateRestoreStats::default())),
        })
    }

    /// Checks if a file is aggregated (exists in the index).
    pub fn is_aggregated(&self, file_name: &str, dir_path: &str) -> Result<bool, AggregateRestoreError> {
        let index = self.index.lock().unwrap();
        Ok(index.is_aggregated(file_name, dir_path)?)
    }

    /// Gets restore info for a file.
    pub fn get_restore_info(&self, file_name: &str, dir_path: &str) -> Result<Option<AggregateRestoreInfo>, AggregateRestoreError> {
        let index = self.index.lock().unwrap();
        Ok(index.query_file(file_name, dir_path)?)
    }

    /// Reads a file from a blob.
    pub fn read_from_blob(
        &self,
        blob_name: &str,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, AggregateRestoreError> {
        // Check cache first
        {
            let cache = self.blob_cache.lock().unwrap();
            if let Some(blob_data) = cache.get(blob_name) {
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
        let blob_path = self.source_dir.join(blob_name);
        let mut blob_file = File::open(&blob_path)?;
        
        let mut blob_data = Vec::new();
        blob_file.read_to_end(&mut blob_data)?;
        
        // Update cache
        {
            let mut cache = self.blob_cache.lock().unwrap();
            cache.insert(blob_name.to_string(), blob_data.clone());
            
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
        info: &AggregateRestoreInfo,
        target_path: &Path,
    ) -> Result<(), AggregateRestoreError> {
        // Read file data from blob
        let data = self.read_from_blob(&info.blob_name, info.offset, info.size)?;
        
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

/// Errors that can occur during aggregate restore.
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

/// Restore extended attributes to the target file
#[cfg(target_os = "linux")]
fn restore_xattrs(path: &Path, xattrs: &str) {
    use base64::Engine as _;
    
    for line in xattrs.lines() {
        if let Some((name, b64_value)) = line.split_once('=') {
            if let Ok(value) = base64::engine::general_purpose::STANDARD.decode(b64_value) {
                if let Err(e) = xattr::set(path, name, &value) {
                    log::error!("Failed to set xattr {} on {:?}: {}", name, path, e);
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_xattrs(_path: &Path, _xattrs: &str) {}

/// Restore ACL to the target file
#[cfg(target_os = "linux")]
fn restore_acl(path: &Path, acl: &str) {
    use exacl::{setfacl, AclEntry};
    
    let mut acl_entries = Vec::new();
    
    for line in acl.lines() {
        if let Ok(entry) = line.parse::<AclEntry>() {
            acl_entries.push(entry);
        }
    }
    
    if !acl_entries.is_empty() {
        if let Err(e) = setfacl(&[path], &acl_entries, None) {
            log::error!("Failed to set ACL on {:?}: {}", path, e);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_acl(_path: &Path, _acl: &str) {}

/// Spawns the aggregate restore coordinator thread.
pub fn spawn_aggregate_restore_coordinator(
    engine: Arc<AggregateRestoreEngine>,
    fcb_rx: mpsc::Receiver<ControlBlockVarient>,
    writer_tx: mpsc::Sender<ControlBlockVarient>,
    shared_state: Arc<SharedState>,
    backup_stats: Arc<BackupStats>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        info!("Aggregate restore coordinator started");
        
        loop {
            match fcb_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(item) => {
                    match item {
                        ControlBlockVarient::DirControlBlock(dcb) => {
                            // Forward directories directly to writer
                            let _ = writer_tx.send(ControlBlockVarient::DirControlBlock(dcb));
                        }
                        ControlBlockVarient::FileControlBlock(fcb) => {
                            // Check if file is aggregated
                            let dir_path = fcb.src_path.parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let file_name = fcb.meta.common.name.clone();
                            
                            match engine.is_aggregated(&file_name, &dir_path) {
                                Ok(true) => {
                                    // File is aggregated, restore from blob
                                    match engine.get_restore_info(&file_name, &dir_path) {
                                        Ok(Some(info)) => {
                                            if let Err(e) = engine.restore_file(&info, &fcb.dst_path) {
                                                error!("Failed to restore aggregated file {:?}: {}", 
                                                    fcb.dst_path, e);
                                                backup_stats.files_failed.fetch_add(1, Ordering::Relaxed);
                                            } else {
                                                backup_stats.files_copied.fetch_add(1, Ordering::Relaxed);
                                                backup_stats.bytes_copied.fetch_add(info.size, Ordering::Relaxed);
                                            }
                                        }
                                        Ok(None) => {
                                            warn!("File {} not found in aggregate index", file_name);
                                            // Forward to normal restore
                                            let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                                        }
                                        Err(e) => {
                                            error!("Failed to query aggregate index: {}", e);
                                            let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                                        }
                                    }
                                }
                                Ok(false) => {
                                    // File is not aggregated, forward to normal restore pipeline
                                    let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                                }
                                Err(e) => {
                                    error!("Failed to check if file is aggregated: {}", e);
                                    let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                                }
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Check if we should exit
                    if shared_state.entry_produce_done.load(Ordering::Relaxed)
                        && shared_state.active_reader_io_workers.load(Ordering::Relaxed) == 0 {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        // Print final stats
        let stats = engine.stats();
        info!(
            "Aggregate restore complete: {} files from blobs, {} files normal, {} cache hits, {} cache misses",
            stats.files_from_blobs, stats.files_normal, stats.cache_hits, stats.cache_misses
        );

        info!("Aggregate restore coordinator ended");
    })
}
