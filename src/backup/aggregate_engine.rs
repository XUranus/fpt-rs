//! Aggregate backup engine supporting both dir-level and shard layouts.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use log::{debug, info};

use crate::backup::aggregate::{
    should_aggregate, AggregateBlobMeta, AggregateConfig, AggregateFileEntry, AggregateLayout,
    AggregateStats, PendingAggregateBuffer, PendingFile, ThreadSafeSnowflake, AGGREGATE_DIR_NAME,
    AGGREGATE_ROOT_DIR,
};
use crate::backup::aggregate_dir_index::{write_dir_index, SQLITE_INDEX_FILE_NAME};
use crate::backup::aggregate_index::{AggregateIndex, BINARY_INDEX_FILE_NAME};
use crate::backup::fcb::FileControlBlock;

pub struct AggregateBackupEngine {
    pub config: AggregateConfig,
    pub(crate) source_base: PathBuf,
    target_base: PathBuf,
    shard_index: Option<Arc<AggregateIndex>>,
    dir_indexes: Mutex<HashMap<String, Vec<AggregateBlobMeta>>>,
    stats: Arc<Mutex<AggregateStats>>,
    id_generator: ThreadSafeSnowflake,
}

impl AggregateBackupEngine {
    pub fn new(
        config: AggregateConfig,
        source_base: PathBuf,
        target_base: PathBuf,
    ) -> Result<Self, AggregateEngineError> {
        let shard_index = if config.layout == AggregateLayout::Shard {
            Some(Arc::new(AggregateIndex::open(
                &target_base
                    .join(AGGREGATE_ROOT_DIR)
                    .join(BINARY_INDEX_FILE_NAME),
            )?))
        } else {
            None
        };
        Ok(Self {
            config,
            source_base,
            target_base,
            shard_index,
            dir_indexes: Mutex::new(HashMap::new()),
            stats: Arc::new(Mutex::new(AggregateStats::default())),
            id_generator: ThreadSafeSnowflake::default(),
        })
    }

    pub fn should_aggregate(&self, file_size: u64) -> bool {
        should_aggregate(file_size, &self.config)
    }

    pub fn relative_path_for_source(&self, source_path: &Path) -> String {
        source_path
            .strip_prefix(&self.source_base)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| source_path.to_string_lossy().replace('\\', "/"))
    }

    pub fn bucket_key_for_relative_path(&self, relative_path: &str, extra_bytes: u64) -> String {
        match self.config.layout {
            AggregateLayout::DirLevel => parent_dir_of(relative_path),
            AggregateLayout::Shard => format!(
                "shard-{:03}",
                self.shard_for_relative_path_with_hint(relative_path, extra_bytes)
            ),
        }
    }

    pub fn create_blob(
        &self,
        bucket_key: &str,
        files: Vec<PendingFile>,
    ) -> Result<AggregateBlobMeta, AggregateEngineError> {
        let blob_name = self.id_generator.generate_blob_name();
        let (blob_rel_path, blob_path, shard_id) = match self.config.layout {
            AggregateLayout::Shard => {
                let shard_dir = PathBuf::from(AGGREGATE_ROOT_DIR).join(bucket_key);
                std::fs::create_dir_all(self.target_base.join(&shard_dir))?;
                let rel = shard_dir
                    .join(&blob_name)
                    .to_string_lossy()
                    .replace('\\', "/");
                let shard_id = bucket_key
                    .strip_prefix("shard-")
                    .and_then(|n| n.parse::<u16>().ok())
                    .unwrap_or(0);
                (rel.clone(), self.target_base.join(&rel), shard_id)
            }
            AggregateLayout::DirLevel => {
                let aggr_dir = aggregate_dir_for(bucket_key);
                std::fs::create_dir_all(self.target_base.join(&aggr_dir))?;
                let rel = aggr_dir
                    .join(&blob_name)
                    .to_string_lossy()
                    .replace('\\', "/");
                (rel.clone(), self.target_base.join(&rel), 0)
            }
        };

        debug!(
            "Writing aggregate blob {:?} ({} files)",
            blob_path,
            files.len()
        );
        let mut blob_file = File::create(&blob_path)?;
        let mut entries = Vec::with_capacity(files.len());
        let mut current_offset = 0_u64;
        let mut total_size = 0_u64;

        for file in files {
            let file_size = file.data.len() as u64;
            blob_file.write_all(&file.data)?;
            entries.push(AggregateFileEntry {
                relative_path: file.relative_path,
                offset: current_offset,
                size: file_size,
                ctime: file.ctime,
                mtime: file.mtime,
                mode: file.mode,
                xattrs: file.xattrs,
                acl: file.acl,
            });
            current_offset += file_size;
            total_size += file_size;
        }

        blob_file.flush()?;
        drop(blob_file);

        let blob_meta = AggregateBlobMeta {
            blob_path: blob_rel_path.clone(),
            blob_size: total_size,
            file_count: entries.len() as u32,
            files: entries,
            shard_id,
        };

        match self.config.layout {
            AggregateLayout::Shard => {
                self.shard_index
                    .as_ref()
                    .expect("missing shard index")
                    .add_blob(&blob_meta)?;
            }
            AggregateLayout::DirLevel => {
                self.dir_indexes
                    .lock()
                    .unwrap()
                    .entry(bucket_key.to_string())
                    .or_default()
                    .push(blob_meta.clone());
            }
        }

        let active_buckets = match self.config.layout {
            AggregateLayout::Shard => {
                let blob_size = self.config.max_blob_size.max(1);
                let written_so_far = {
                    let stats = self.stats.lock().unwrap();
                    stats.original_bytes.saturating_add(total_size)
                };
                ((written_so_far / blob_size).saturating_add(1) as u16)
                    .clamp(1, self.config.shard_count.max(1)) as u64
            }
            AggregateLayout::DirLevel => self.dir_indexes.lock().unwrap().len() as u64,
        };
        let mut stats = self.stats.lock().unwrap();
        stats.blobs_created += 1;
        stats.files_aggregated += blob_meta.file_count as u64;
        stats.blob_bytes += total_size;
        stats.original_bytes += total_size;
        stats.active_shards = active_buckets;

        info!(
            "Created {} aggregate blob {} with {} files ({} bytes)",
            self.config.layout.as_str(),
            blob_rel_path,
            blob_meta.file_count,
            total_size
        );
        Ok(blob_meta)
    }

    pub fn flush_all_indexes(&self) -> Result<(), AggregateEngineError> {
        match self.config.layout {
            AggregateLayout::Shard => {
                self.shard_index
                    .as_ref()
                    .expect("missing shard index")
                    .flush()?;
                info!("Aggregate shard index flushed");
            }
            AggregateLayout::DirLevel => {
                let dir_indexes = self.dir_indexes.lock().unwrap();
                for (dir_rel, blobs) in dir_indexes.iter() {
                    let path = self
                        .target_base
                        .join(aggregate_dir_for(dir_rel))
                        .join(SQLITE_INDEX_FILE_NAME);
                    write_dir_index(&path, blobs).map_err(AggregateEngineError::Other)?;
                }
                info!("Aggregate dir-level indexes flushed");
            }
        }
        Ok(())
    }

    pub fn stats(&self) -> AggregateStats {
        self.stats.lock().unwrap().clone()
    }

    fn shard_for_relative_path_with_hint(&self, relative_path: &str, extra_bytes: u64) -> u16 {
        let mut hash: u64 = 1469598103934665603;
        for b in relative_path.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        (hash % self.preferred_shard_count(extra_bytes) as u64) as u16
    }

    fn preferred_shard_count(&self, extra_bytes: u64) -> u16 {
        let max_shards = self.config.shard_count.max(1);
        let written_bytes = self.stats.lock().unwrap().original_bytes;
        let bytes_seen = written_bytes.saturating_add(extra_bytes);
        let blob_size = self.config.max_blob_size.max(1);
        let desired = ((bytes_seen / blob_size).saturating_add(1)) as u16;
        desired.clamp(1, max_shards)
    }
}

pub struct AggregateBackupState {
    pub buffers: Mutex<HashMap<String, PendingAggregateBuffer>>,
    pub engine: Arc<AggregateBackupEngine>,
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

    pub fn add_file(
        &self,
        relative_path: &str,
        file: PendingFile,
    ) -> Option<(String, Vec<PendingFile>)> {
        let bucket_key = self
            .engine
            .bucket_key_for_relative_path(relative_path, file.data.len() as u64);
        let mut buffers = self.buffers.lock().unwrap();
        let buffer = buffers.entry(bucket_key.clone()).or_insert_with(|| {
            PendingAggregateBuffer::new(bucket_key.clone(), self.engine.config.max_blob_size)
        });
        if buffer.add_file(file) {
            Some((bucket_key, buffer.flush()))
        } else {
            None
        }
    }

    pub fn flush_all(&self) -> Vec<(String, Vec<PendingFile>)> {
        let mut buffers = self.buffers.lock().unwrap();
        let mut result = Vec::new();
        for (key, buffer) in buffers.iter_mut() {
            if !buffer.is_empty() {
                result.push((key.clone(), buffer.flush()));
            }
        }
        result
    }
}

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
            AggregateEngineError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for AggregateEngineError {}

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

pub fn parent_dir_of(relative_path: &str) -> String {
    Path::new(relative_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

pub fn aggregate_dir_for(dir_rel: &str) -> PathBuf {
    if dir_rel.is_empty() {
        PathBuf::from(AGGREGATE_DIR_NAME)
    } else {
        PathBuf::from(dir_rel).join(AGGREGATE_DIR_NAME)
    }
}

pub fn fcb_to_pending_file(fcb: &FileControlBlock) -> PendingFile {
    PendingFile {
        relative_path: fcb.dst_path.to_string_lossy().replace('\\', "/"),
        data: fcb.buffer[..fcb.buffer_len].to_vec(),
        ctime: fcb.meta.common.ctime as u64,
        mtime: fcb.meta.common.mtime as u64,
        mode: fcb.meta.common.mode,
        xattrs: fcb.meta.common.xattributes.clone(),
        acl: fcb.meta.common.posix_access_acl.clone(),
    }
}
