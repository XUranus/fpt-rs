//! Transport-agnostic aggregation decorator for async target writers.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::tempdir;

use crate::backup::aggregate::{
    should_aggregate, AggregateBlobMeta, AggregateConfig, AggregateFileEntry, AggregateLayout,
    PendingFile, ThreadSafeSnowflake, AGGREGATE_DIR_NAME, AGGREGATE_ROOT_DIR,
};
use crate::backup::aggregate::aggregate_dir_index::{write_dir_index, SQLITE_INDEX_FILE_NAME};
use crate::backup::aggregate::aggregate_engine::parent_dir_of;
use crate::backup::aggregate::aggregate_index::{AggregateIndex, BINARY_INDEX_FILE_NAME};
use crate::backup::aio::transport::TargetWriter;
use crate::backup::copy_block::CopyBlock;
use crate::backup::fcb::{FileControlBlock, SourceHandleState};
use crate::scanner::metadata::FileMeta;

struct BucketAggregationState {
    pending_files: Vec<PendingFile>,
    pending_size: u64,
}

impl BucketAggregationState {
    fn new() -> Self {
        Self {
            pending_files: Vec::new(),
            pending_size: 0,
        }
    }
}

#[derive(Clone)]
pub struct AggregatingTarget<T: TargetWriter> {
    inner: T,
    config: AggregateConfig,
    repo_prefix: PathBuf,
    state: Arc<Mutex<HashMap<String, BucketAggregationState>>>,
    blobs: Arc<Mutex<Vec<AggregateBlobMeta>>>,
    bytes_seen: Arc<AtomicU64>,
    ids: Arc<ThreadSafeSnowflake>,
}

impl<T: TargetWriter> AggregatingTarget<T> {
    pub fn new(inner: T, config: AggregateConfig) -> Self {
        Self::with_repo_prefix(inner, config, PathBuf::new())
    }

    pub fn with_repo_prefix(inner: T, config: AggregateConfig, repo_prefix: PathBuf) -> Self {
        Self {
            inner,
            config,
            repo_prefix,
            state: Arc::new(Mutex::new(HashMap::new())),
            blobs: Arc::new(Mutex::new(Vec::new())),
            bytes_seen: Arc::new(AtomicU64::new(0)),
            ids: Arc::new(ThreadSafeSnowflake::default()),
        }
    }

    fn should_aggregate_block(&self, block: &CopyBlock) -> bool {
        block.meta.common.symlink_target_path.is_none()
            && should_aggregate(block.file_size, &self.config)
            && block.is_last
            && block.data.len() as u64 == block.file_size
    }

    fn logical_relative_path(&self, dst_path: &PathBuf) -> String {
        let rel = if self.repo_prefix.as_os_str().is_empty() {
            dst_path.as_path()
        } else {
            dst_path
                .strip_prefix(&self.repo_prefix)
                .unwrap_or(dst_path.as_path())
        };
        rel.to_string_lossy().replace('\\', "/")
    }

    fn bucket_for_relative_path(&self, relative_path: &str) -> String {
        match self.config.layout {
            AggregateLayout::DirLevel => parent_dir_of(relative_path),
            AggregateLayout::Shard => {
                let mut hash: u64 = 1469598103934665603;
                for b in relative_path.as_bytes() {
                    hash ^= *b as u64;
                    hash = hash.wrapping_mul(1099511628211);
                }
                let max_shards = self.config.shard_count.max(1) as u64;
                let bytes_seen = self.bytes_seen.load(Ordering::Relaxed);
                let desired = ((bytes_seen / self.config.max_blob_size.max(1)).saturating_add(1))
                    .clamp(1, max_shards);
                format!("shard-{:03}", hash % desired)
            }
        }
    }

    fn blob_rel_path(&self, bucket: &str, blob_name: &str) -> PathBuf {
        let layout_path = match self.config.layout {
            AggregateLayout::DirLevel => {
                if bucket.is_empty() {
                    PathBuf::from(AGGREGATE_DIR_NAME).join(blob_name)
                } else {
                    PathBuf::from(bucket)
                        .join(AGGREGATE_DIR_NAME)
                        .join(blob_name)
                }
            }
            AggregateLayout::Shard => PathBuf::from(AGGREGATE_ROOT_DIR)
                .join(bucket)
                .join(blob_name),
        };
        if self.repo_prefix.as_os_str().is_empty() {
            layout_path
        } else {
            self.repo_prefix.join(layout_path)
        }
    }

    fn index_relative_path(&self, target_path: &PathBuf) -> String {
        let rel = if self.repo_prefix.as_os_str().is_empty() {
            target_path.as_path()
        } else {
            target_path
                .strip_prefix(&self.repo_prefix)
                .unwrap_or(target_path.as_path())
        };
        rel.to_string_lossy().replace('\\', "/")
    }

    async fn write_blob(
        &self,
        bucket: &str,
        files: Vec<PendingFile>,
    ) -> Result<AggregateBlobMeta, String> {
        let blob_name = self.ids.generate_blob_name();
        let blob_rel_path = self.blob_rel_path(bucket, &blob_name);
        if let Some(parent) = blob_rel_path.parent() {
            self.inner.create_dir(parent.to_path_buf()).await?;
        }

        let mut entries = Vec::with_capacity(files.len());
        let mut offset = 0u64;
        for file in &files {
            let size = file.data.len() as u64;
            entries.push(AggregateFileEntry {
                relative_path: file.relative_path.clone(),
                offset,
                size,
                ctime: file.ctime,
                mtime: file.mtime,
                mode: file.mode,
                xattrs: file.xattrs.clone(),
                acl: file.acl.clone(),
            });
            offset += size;
        }

        let blob_target_path_str = blob_rel_path.to_string_lossy().replace('\\', "/");
        let blob_index_path_str = self.index_relative_path(&blob_rel_path);
        let blob_size = offset;
        let mut blob_data = Vec::with_capacity(blob_size as usize);
        for file in files {
            blob_data
                .write_all(&file.data)
                .map_err(|e| format!("build aggregate blob {blob_target_path_str}: {e}"))?;
        }
        let block = synthetic_block(
            PathBuf::from(&blob_target_path_str),
            blob_data,
            0,
            blob_size,
            0o644,
        );
        self.inner.write_block(block).await.map_err(|(_, e)| e)?;

        Ok(AggregateBlobMeta {
            blob_path: blob_index_path_str,
            blob_size,
            file_count: entries.len() as u32,
            files: entries,
            shard_id: 0,
        })
    }

    async fn flush_shard_index(&self) -> Result<(), String> {
        let snapshot = self.blobs.lock().unwrap().clone();
        if snapshot.is_empty() {
            return Ok(());
        }

        let temp = tempdir().map_err(|e| e.to_string())?;
        let temp_index_path = temp.path().join(BINARY_INDEX_FILE_NAME);
        {
            let index = AggregateIndex::open(&temp_index_path).map_err(|e| e.to_string())?;
            for blob in &snapshot {
                index.add_blob(blob).map_err(|e| e.to_string())?;
            }
            index.flush().map_err(|e| e.to_string())?;
        }

        let mut bytes = Vec::new();
        std::fs::File::open(&temp_index_path)
            .map_err(|e| e.to_string())?
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let index_dir = self.repo_prefix.join(AGGREGATE_ROOT_DIR);
        self.inner.create_dir(index_dir.clone()).await?;
        let idx_fcb = synthetic_fcb(index_dir.join(BINARY_INDEX_FILE_NAME), bytes, 0o644);
        self.inner.write_file(idx_fcb).await.map_err(|(_, e)| e)?;
        Ok(())
    }

    async fn flush_dir_level_indexes(&self) -> Result<(), String> {
        let mut blobs_by_dir: HashMap<String, Vec<AggregateBlobMeta>> = HashMap::new();
        for blob in self.blobs.lock().unwrap().iter().cloned() {
            let dir_key = blob
                .files
                .first()
                .map(|f| parent_dir_of(&f.relative_path))
                .unwrap_or_default();
            blobs_by_dir.entry(dir_key).or_default().push(blob);
        }

        for (dir_key, blobs) in blobs_by_dir {
            let temp = tempdir().map_err(|e| e.to_string())?;
            let temp_index_path = temp.path().join(SQLITE_INDEX_FILE_NAME);
            write_dir_index(&temp_index_path, &blobs)?;
            let mut bytes = Vec::new();
            std::fs::File::open(&temp_index_path)
                .map_err(|e| e.to_string())?
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            let layout_idx_rel = if dir_key.is_empty() {
                PathBuf::from(AGGREGATE_DIR_NAME).join(SQLITE_INDEX_FILE_NAME)
            } else {
                PathBuf::from(&dir_key)
                    .join(AGGREGATE_DIR_NAME)
                    .join(SQLITE_INDEX_FILE_NAME)
            };
            let idx_rel = if self.repo_prefix.as_os_str().is_empty() {
                layout_idx_rel
            } else {
                self.repo_prefix.join(layout_idx_rel)
            };
            if let Some(parent) = idx_rel.parent() {
                self.inner.create_dir(parent.to_path_buf()).await?;
            }
            let idx_fcb = synthetic_fcb(idx_rel, bytes, 0o644);
            self.inner.write_file(idx_fcb).await.map_err(|(_, e)| e)?;
        }
        Ok(())
    }
}

impl<T: TargetWriter> TargetWriter for AggregatingTarget<T> {
    fn create_dir(
        &self,
        path: PathBuf,
    ) -> futures_util::future::BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move { this.inner.create_dir(path).await })
    }

    fn write_block(
        &self,
        block: CopyBlock,
    ) -> futures_util::future::BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            if !this.should_aggregate_block(&block) {
                return this.inner.write_block(block).await;
            }

            let relative_path = this.logical_relative_path(&block.dst_path);
            let pending = PendingFile {
                relative_path: relative_path.clone(),
                data: block.data.clone(),
                ctime: block.meta.common.ctime as u64,
                mtime: block.meta.common.mtime as u64,
                mode: block.meta.common.mode,
                xattrs: block.meta.common.xattributes.clone(),
                acl: block.meta.common.posix_access_acl.clone(),
            };
            this.bytes_seen
                .fetch_add(pending.data.len() as u64, Ordering::Relaxed);
            let bucket = this.bucket_for_relative_path(&relative_path);

            let to_flush = {
                let mut state = this.state.lock().unwrap();
                let shard_state = state
                    .entry(bucket.clone())
                    .or_insert_with(BucketAggregationState::new);
                shard_state.pending_size += pending.data.len() as u64;
                shard_state.pending_files.push(pending);
                shard_state.pending_size >= this.config.max_blob_size
            };

            if to_flush {
                let files = {
                    let mut state = this.state.lock().unwrap();
                    let shard_state = state.get_mut(&bucket).expect("bucket state missing");
                    let files = std::mem::take(&mut shard_state.pending_files);
                    shard_state.pending_size = 0;
                    files
                };
                match this.write_blob(&bucket, files).await {
                    Ok(blob) => this.blobs.lock().unwrap().push(blob),
                    Err(e) => return Err((block, e)),
                }
            }
            let mut block = block;
            block.dst_offset = block.file_size;
            Ok(block)
        })
    }

    fn finish(&self) -> futures_util::future::BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            let bucket_keys: Vec<String> = {
                let state = this.state.lock().unwrap();
                state.keys().cloned().collect()
            };

            for bucket in bucket_keys {
                let pending = {
                    let mut state = this.state.lock().unwrap();
                    let shard_state = state.get_mut(&bucket).expect("bucket state missing");
                    if shard_state.pending_files.is_empty() {
                        None
                    } else {
                        let files = std::mem::take(&mut shard_state.pending_files);
                        shard_state.pending_size = 0;
                        Some(files)
                    }
                };

                if let Some(files) = pending {
                    let blob = this.write_blob(&bucket, files).await?;
                    this.blobs.lock().unwrap().push(blob);
                }
            }

            match this.config.layout {
                AggregateLayout::DirLevel => this.flush_dir_level_indexes().await?,
                AggregateLayout::Shard => this.flush_shard_index().await?,
            }
            this.inner.finish().await
        })
    }
}

fn synthetic_fcb(dst_path: PathBuf, bytes: Vec<u8>, mode: u32) -> FileControlBlock {
    let mut meta = FileMeta::default();
    meta.common.mode = mode;
    meta.common.name = dst_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    meta.size = bytes.len() as u64;
    let mut fcb = FileControlBlock::from(meta);
    fcb.dst_path = dst_path;
    fcb.buffer_len = bytes.len();
    fcb.buffer = bytes;
    fcb.src_state = SourceHandleState::Read;
    fcb
}

fn synthetic_block(
    dst_path: PathBuf,
    bytes: Vec<u8>,
    dst_offset: u64,
    file_size: u64,
    mode: u32,
) -> CopyBlock {
    let mut meta = FileMeta::default();
    meta.common.mode = mode;
    meta.common.name = dst_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    meta.size = file_size;
    let data_len = bytes.len() as u64;
    CopyBlock {
        meta: std::sync::Arc::new(meta),
        src_path: PathBuf::new(),
        dst_path,
        src_offset: dst_offset + data_len,
        dst_offset,
        file_size,
        data: bytes,
        is_last: dst_offset + data_len >= file_size,
    }
}
