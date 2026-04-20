//! # Aggregate Backup Engine
//!
//! This module implements the core aggregation logic for backup operations.
//! It combines multiple small files into blob files and maintains per-directory
//! SQLite indexes for later restoration.
//!
//! Design:
//! - Each directory has its own SQLite index (0 or 1 per directory)
//! - Aggregated files are stored in .AGGR_DIR/ subdirectory within each directory
//! - This provides better scalability for large file sets

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use log::{debug, error, info};

use crate::backup::aggregate::{
    should_aggregate, AggregateBlobMeta, AggregateConfig, AggregateFileEntry, AggregateStats,
    DirAggregateBuffer, PendingFile, ThreadSafeSnowflake,
};
use crate::backup::aggregate_index::AggregateIndex;
use crate::backup::fcb::{ControlBlockVarient, FileControlBlock, SourceHandleState};
use crate::backup::stats::BackupStats;
use crate::backup::SharedState;

/// Per-directory aggregate information
#[allow(dead_code)]
struct DirAggregateInfo {
    /// Path to the directory in target
    target_dir: PathBuf,
    /// Path to the .AGGR_DIR subdirectory
    aggr_dir: PathBuf,
    /// The index (opened on first use)
    index: Option<AggregateIndex>,
}

impl DirAggregateInfo {
    fn new(target_dir: PathBuf) -> Result<Self, AggregateEngineError> {
        let aggr_dir = target_dir.join(".AGGR_DIR");

        // Create .AGGR_DIR directory
        std::fs::create_dir_all(&aggr_dir)?;

        Ok(Self {
            target_dir,
            aggr_dir,
            index: None,
        })
    }

    fn get_or_open_index(&mut self) -> Result<&AggregateIndex, AggregateEngineError> {
        if self.index.is_none() {
            let index_path = self.aggr_dir.join("AGGREGATE_IDX.sqlite");
            self.index = Some(AggregateIndex::open(&index_path)?);
        }
        Ok(self.index.as_ref().unwrap())
    }
}

/// Engine for performing aggregate backups.
pub struct AggregateBackupEngine {
    /// Configuration for aggregation
    pub config: AggregateConfig,
    /// Base source directory
    source_base: PathBuf,
    /// Base target directory
    target_base: PathBuf,
    /// Source to target directory mapping
    dir_info: Mutex<HashMap<String, DirAggregateInfo>>,
    /// Global stats
    stats: Arc<Mutex<AggregateStats>>,
    /// Snowflake ID generator for unique blob filenames
    id_generator: ThreadSafeSnowflake,
}

impl AggregateBackupEngine {
    /// Creates a new aggregate backup engine.
    pub fn new(
        config: AggregateConfig,
        source_base: PathBuf,
        target_base: PathBuf,
    ) -> Result<Self, AggregateEngineError> {
        Ok(Self {
            config,
            source_base,
            target_base,
            dir_info: Mutex::new(HashMap::new()),
            stats: Arc::new(Mutex::new(AggregateStats::default())),
            id_generator: ThreadSafeSnowflake::default(),
        })
    }

    /// Creates a new aggregate backup engine with a specific process ID.
    /// process_id should be unique per process (0-1023).
    pub fn with_process_id(
        config: AggregateConfig,
        source_base: PathBuf,
        target_base: PathBuf,
        process_id: u16,
    ) -> Result<Self, AggregateEngineError> {
        Ok(Self {
            config,
            source_base,
            target_base,
            dir_info: Mutex::new(HashMap::new()),
            stats: Arc::new(Mutex::new(AggregateStats::default())),
            id_generator: ThreadSafeSnowflake::new(process_id),
        })
    }

    /// Checks if a file should be aggregated based on its size.
    pub fn should_aggregate(&self, file_size: u64) -> bool {
        should_aggregate(file_size, &self.config)
    }

    /// Creates a blob file from a directory buffer.
    pub fn create_blob(
        &self,
        source_dir: &str,
        files: Vec<PendingFile>,
    ) -> Result<AggregateBlobMeta, AggregateEngineError> {
        // Get or create directory info
        let mut dir_info_map = self.dir_info.lock().unwrap();

        let dir_info = dir_info_map
            .entry(source_dir.to_string())
            .or_insert_with(|| {
                // Create new dir info
                let target_dir = self.source_dir_to_target(source_dir);
                DirAggregateInfo::new(target_dir).expect("Failed to create dir info")
            });

        // Generate unique blob name using Snowflake algorithm
        let blob_name = self.id_generator.generate_blob_name();
        let blob_path = dir_info.aggr_dir.join(&blob_name);

        debug!(
            "Writing aggregated blob: {:?} ({} bytes from {} files)",
            blob_path,
            files.iter().map(|f| f.data.len() as u64).sum::<u64>(),
            files.len()
        );
        let mut blob_file = File::create(&blob_path)?;
        let mut entries = Vec::new();
        let mut current_offset: u64 = 0;
        let mut total_size: u64 = 0;

        // Write each file's data to the blob
        for file in files {
            let file_size = file.data.len() as u64;

            // Write file data
            blob_file.write_all(&file.data)?;

            // Create entry
            let entry = AggregateFileEntry {
                file_name: file.file_name.clone(),
                offset: current_offset,
                size: file_size,
                ctime: file.ctime,
                mtime: file.mtime,
                mode: file.mode,
                xattrs: file.xattrs,
                acl: file.acl,
            };
            entries.push(entry);

            current_offset += file_size;
            total_size += file_size;
        }

        blob_file.flush()?;
        drop(blob_file);

        let blob_meta = AggregateBlobMeta {
            blob_name: blob_name.clone(),
            blob_size: total_size,
            file_count: entries.len() as u32,
            files: entries,
            dir_path: source_dir.to_string(),
        };

        // Update per-directory index
        let index = dir_info.get_or_open_index()?;
        index.add_blob(&blob_meta)?;

        // Update stats
        let mut stats = self.stats.lock().unwrap();
        stats.blobs_created += 1;
        stats.files_aggregated += blob_meta.file_count as u64;
        stats.blob_bytes += total_size;
        stats.original_bytes += total_size;

        info!(
            "Created blob {} in {} with {} files ({} bytes)",
            blob_name, source_dir, blob_meta.file_count, total_size
        );

        Ok(blob_meta)
    }

    /// Maps a source directory path to target directory path
    /// Preserves the directory structure: source_base/... -> target_base/...
    fn source_dir_to_target(&self, source_dir: &str) -> PathBuf {
        let source_path = Path::new(source_dir);

        // Get the relative path from source_base
        let relative_path = if source_path.starts_with(&self.source_base) {
            source_path
                .strip_prefix(&self.source_base)
                .unwrap_or(source_path)
        } else {
            // If source_dir is not under source_base, use the full path
            // after stripping any leading "/"
            source_path.strip_prefix("/").unwrap_or(source_path)
        };

        // Join with target_base
        self.target_base.join(relative_path)
    }

    /// Gets current statistics.
    pub fn stats(&self) -> AggregateStats {
        self.stats.lock().unwrap().clone()
    }

    /// Flushes all directory indexes
    pub fn flush_all_indexes(&self) -> Result<(), AggregateEngineError> {
        let dir_info_map = self.dir_info.lock().unwrap();
        for (dir_path, dir_info) in dir_info_map.iter() {
            if let Some(ref _index) = dir_info.index {
                info!("Flushed index for directory: {}", dir_path);
            }
        }
        Ok(())
    }
}

/// Shared state for aggregate backup processing.
pub struct AggregateBackupState {
    /// Directory path -> buffer for pending files
    pub buffers: Mutex<HashMap<String, DirAggregateBuffer>>,
    /// Engine reference
    pub engine: Arc<AggregateBackupEngine>,
    /// Files that are being backed up normally (not aggregated)
    pub normal_files: Mutex<Vec<FileControlBlock>>,
}

impl AggregateBackupState {
    pub fn new(engine: Arc<AggregateBackupEngine>) -> Self {
        Self {
            buffers: Mutex::new(HashMap::new()),
            engine,
            normal_files: Mutex::new(Vec::new()),
        }
    }

    /// Adds a file to the appropriate directory buffer.
    /// Returns true if a blob should be created (buffer is full).
    pub fn add_file(
        &self,
        dir_path: &str,
        file: PendingFile,
    ) -> Option<(String, Vec<PendingFile>)> {
        let mut buffers = self.buffers.lock().unwrap();
        let buffer = buffers.entry(dir_path.to_string()).or_insert_with(|| {
            DirAggregateBuffer::new(dir_path.to_string(), self.engine.config.max_blob_size)
        });

        let should_flush = buffer.add_file(file);

        if should_flush {
            let files = buffer.flush();
            Some((dir_path.to_string(), files))
        } else {
            None
        }
    }

    /// Flushes all pending buffers and creates remaining blobs.
    pub fn flush_all(&self) -> Vec<(String, Vec<PendingFile>)> {
        let mut buffers = self.buffers.lock().unwrap();
        let mut result = Vec::new();

        for (dir_path, buffer) in buffers.iter_mut() {
            if !buffer.is_empty() {
                let files = buffer.flush();
                result.push((dir_path.clone(), files));
            }
        }

        result
    }
}

/// Errors that can occur in the aggregate engine.
#[derive(Debug)]
pub enum AggregateEngineError {
    Io(io::Error),
    Index(crate::backup::aggregate_index::AggregateIndexError),
    Other(String),
}

impl std::fmt::Display for AggregateEngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregateEngineError::Io(e) => write!(f, "IO error: {}", e),
            AggregateEngineError::Index(e) => write!(f, "Index error: {}", e),
            AggregateEngineError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for AggregateEngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AggregateEngineError::Io(e) => Some(e),
            AggregateEngineError::Index(e) => Some(e),
            AggregateEngineError::Other(_) => None,
        }
    }
}

impl From<io::Error> for AggregateEngineError {
    fn from(e: io::Error) -> Self {
        AggregateEngineError::Io(e)
    }
}

impl From<crate::backup::aggregate_index::AggregateIndexError> for AggregateEngineError {
    fn from(e: crate::backup::aggregate_index::AggregateIndexError) -> Self {
        AggregateEngineError::Index(e)
    }
}

/// Converts a FileControlBlock to a PendingFile for aggregation.
pub fn fcb_to_pending_file(fcb: &FileControlBlock) -> PendingFile {
    PendingFile {
        file_name: fcb.meta.common.name.clone(),
        data: fcb.buffer.clone(),
        ctime: fcb.meta.common.ctime as u64,
        mtime: fcb.meta.common.mtime as u64,
        mode: fcb.meta.common.mode,
        xattrs: fcb.meta.common.xattributes.clone(),
        acl: fcb.meta.common.posix_access_acl.clone(),
    }
}

/// Spawns the aggregate backup coordinator thread.
#[allow(dead_code)]
pub(crate) fn spawn_aggregate_coordinator(
    agg_state: Arc<AggregateBackupState>,
    fcb_rx: mpsc::Receiver<ControlBlockVarient>,
    writer_tx: mpsc::Sender<ControlBlockVarient>,
    shared_state: Arc<SharedState>,
    backup_stats: Arc<BackupStats>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        info!("Aggregate coordinator started");

        loop {
            match fcb_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(item) => {
                    match item {
                        ControlBlockVarient::DirControlBlock(dcb) => {
                            // Forward directories directly to writer
                            let _ = writer_tx.send(ControlBlockVarient::DirControlBlock(dcb));
                        }
                        ControlBlockVarient::FileControlBlock(fcb) => {
                            // Check if file should be aggregated
                            if agg_state.engine.should_aggregate(fcb.meta.size) {
                                // File has been read, convert to pending file
                                if fcb.src_state == SourceHandleState::Read {
                                    let pending = fcb_to_pending_file(&fcb);
                                    let dir_path = fcb
                                        .src_path
                                        .parent()
                                        .map(|p| p.to_string_lossy().to_string())
                                        .unwrap_or_default();

                                    // Add to buffer
                                    if let Some((dir, files)) =
                                        agg_state.add_file(&dir_path, pending)
                                    {
                                        // Buffer is full, create blob
                                        let file_count = files.len() as u64;
                                        let bytes_in_blob: u64 =
                                            files.iter().map(|f| f.data.len() as u64).sum();

                                        match agg_state.engine.create_blob(&dir, files) {
                                            Ok(blob_meta) => {
                                                debug!(
                                                    "Created blob {} for dir {}",
                                                    blob_meta.blob_name, dir
                                                );
                                                // Update stats for aggregated files
                                                backup_stats
                                                    .files_copied
                                                    .fetch_add(file_count, Ordering::Relaxed);
                                                backup_stats
                                                    .bytes_copied
                                                    .fetch_add(bytes_in_blob, Ordering::Relaxed);
                                            }
                                            Err(e) => {
                                                error!(
                                                    "Failed to create blob for dir {}: {}",
                                                    dir, e
                                                );
                                                backup_stats
                                                    .files_failed
                                                    .fetch_add(file_count, Ordering::Relaxed);
                                            }
                                        }
                                    } else {
                                        // File was added to buffer, update stats
                                        backup_stats.files_copied.fetch_add(1, Ordering::Relaxed);
                                        backup_stats
                                            .bytes_copied
                                            .fetch_add(fcb.meta.size, Ordering::Relaxed);
                                    }
                                } else {
                                    // File not yet read, forward to reader
                                    let _ =
                                        writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));
                                }
                            } else {
                                // Large file, update stats before forwarding
                                let file_size = fcb.meta.size;

                                // Forward to normal backup pipeline
                                let _ = writer_tx.send(ControlBlockVarient::FileControlBlock(fcb));

                                // Update stats
                                backup_stats.files_copied.fetch_add(1, Ordering::Relaxed);
                                backup_stats
                                    .bytes_copied
                                    .fetch_add(file_size, Ordering::Relaxed);
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Check if we should exit
                    if shared_state.entry_produce_done.load(Ordering::Relaxed)
                        && shared_state
                            .active_reader_io_workers
                            .load(Ordering::Relaxed)
                            == 0
                    {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        // Flush remaining buffers
        info!("Flushing remaining aggregate buffers...");
        let remaining = agg_state.flush_all();
        for (dir, files) in remaining {
            if !files.is_empty() {
                let file_count = files.len() as u64;
                let bytes_in_blob: u64 = files.iter().map(|f| f.data.len() as u64).sum();

                match agg_state.engine.create_blob(&dir, files) {
                    Ok(blob_meta) => {
                        debug!("Created final blob {} for dir {}", blob_meta.blob_name, dir);
                        // Update stats for aggregated files
                        backup_stats
                            .files_copied
                            .fetch_add(file_count, Ordering::Relaxed);
                        backup_stats
                            .bytes_copied
                            .fetch_add(bytes_in_blob, Ordering::Relaxed);
                    }
                    Err(e) => {
                        error!("Failed to create final blob for dir {}: {}", dir, e);
                        backup_stats
                            .files_failed
                            .fetch_add(file_count, Ordering::Relaxed);
                    }
                }
            }
        }

        // Flush all indexes
        let _ = agg_state.engine.flush_all_indexes();

        // Print final stats
        let stats = agg_state.engine.stats();
        info!(
            "Aggregate backup complete: {} blobs created, {} files aggregated, {} files normal",
            stats.blobs_created, stats.files_aggregated, stats.files_normal
        );

        info!("Aggregate coordinator ended");
    })
}
