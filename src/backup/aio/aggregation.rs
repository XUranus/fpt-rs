//! Transport-agnostic aggregation decorator for async target writers.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tempfile::NamedTempFile;

use crate::backup::aggregate::{
    AggregateBlobMeta, AggregateConfig, PendingFile, ThreadSafeSnowflake, should_aggregate,
};
use crate::backup::aggregate_index::AggregateIndex;
use crate::backup::aio::transport::TargetWriter;
use crate::backup::fcb::{FileControlBlock, SourceHandleState};
use crate::scanner::metadata::FileMeta;

struct DirAggregationState {
    pending_files: Vec<PendingFile>,
    pending_size: u64,
    blobs: Vec<AggregateBlobMeta>,
}

impl DirAggregationState {
    fn new() -> Self {
        Self {
            pending_files: Vec::new(),
            pending_size: 0,
            blobs: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct AggregatingTarget<T: TargetWriter> {
    inner: T,
    config: AggregateConfig,
    state: Arc<Mutex<HashMap<String, DirAggregationState>>>,
    ids: Arc<ThreadSafeSnowflake>,
}

impl<T: TargetWriter> AggregatingTarget<T> {
    pub fn new(inner: T, config: AggregateConfig) -> Self {
        Self {
            inner,
            config,
            state: Arc::new(Mutex::new(HashMap::new())),
            ids: Arc::new(ThreadSafeSnowflake::default()),
        }
    }

    fn should_aggregate_fcb(&self, fcb: &FileControlBlock) -> bool {
        fcb.meta.common.symlink_target_path.is_none()
            && should_aggregate(fcb.meta.size, &self.config)
            && fcb.src_state == SourceHandleState::Read
    }

    #[allow(dead_code)]
    async fn flush_dir_locked(
        &self,
        dir_key: &str,
        dir_state: &mut DirAggregationState,
    ) -> Result<(), String> {
        if dir_state.pending_files.is_empty() {
            return Ok(());
        }

        let files = std::mem::take(&mut dir_state.pending_files);
        dir_state.pending_size = 0;
        let blob_meta = self.write_blob(dir_key, files).await?;
        dir_state.blobs.push(blob_meta);
        Ok(())
    }

    async fn write_blob(
        &self,
        dir_key: &str,
        files: Vec<PendingFile>,
    ) -> Result<AggregateBlobMeta, String> {
        let blob_name = self.ids.generate_blob_name();
        let mut blob_bytes = Vec::new();
        let mut entries = Vec::with_capacity(files.len());
        let mut offset = 0u64;

        for file in files {
            let size = file.data.len() as u64;
            blob_bytes.extend_from_slice(&file.data);
            entries.push(crate::backup::aggregate::AggregateFileEntry {
                file_name: file.file_name,
                offset,
                size,
                ctime: file.ctime,
                mtime: file.mtime,
                mode: file.mode,
                xattrs: file.xattrs,
                acl: file.acl,
            });
            offset += size;
        }

        let blob_meta = AggregateBlobMeta {
            blob_name: blob_name.clone(),
            blob_size: blob_bytes.len() as u64,
            file_count: entries.len() as u32,
            files: entries,
            dir_path: dir_key.to_string(),
        };

        let blob_path = Path::new(dir_key).join(".AGGR_DIR").join(&blob_name);
        let blob_fcb = synthetic_fcb(blob_path, blob_bytes, 0o644);
        self.inner.write_file(blob_fcb).await.map_err(|(_, e)| e)?;
        Ok(blob_meta)
    }

    async fn flush_indexes(&self, snapshot: Vec<(String, Vec<AggregateBlobMeta>)>) -> Result<(), String> {
        for (dir_key, blobs) in snapshot {
            if blobs.is_empty() {
                continue;
            }
            let temp = NamedTempFile::new().map_err(|e| e.to_string())?;
            {
                let index = AggregateIndex::open(temp.path()).map_err(|e| e.to_string())?;
                for blob in &blobs {
                    index.add_blob(blob).map_err(|e| e.to_string())?;
                }
            }
            let mut bytes = Vec::new();
            temp.reopen()
                .map_err(|e| e.to_string())?
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;

            let idx_path = Path::new(&dir_key).join(".AGGR_DIR").join("AGGREGATE_IDX.sqlite");
            let idx_fcb = synthetic_fcb(idx_path, bytes, 0o644);
            self.inner.write_file(idx_fcb).await.map_err(|(_, e)| e)?;
        }
        Ok(())
    }
}

impl<T: TargetWriter> TargetWriter for AggregatingTarget<T> {
    fn create_dir(&self, path: PathBuf) -> futures_util::future::BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move { this.inner.create_dir(path).await })
    }

    fn write_file(&self, fcb: FileControlBlock) -> futures_util::future::BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            if !this.should_aggregate_fcb(&fcb) {
                return this.inner.write_file(fcb).await;
            }

            let dir_key = fcb
                .dst_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_string_lossy()
                .to_string();
            let pending = PendingFile {
                file_name: fcb.meta.common.name.clone(),
                data: fcb.buffer.clone(),
                ctime: fcb.meta.common.ctime as u64,
                mtime: fcb.meta.common.mtime as u64,
                mode: fcb.meta.common.mode,
                xattrs: fcb.meta.common.xattributes.clone(),
                acl: fcb.meta.common.posix_access_acl.clone(),
            };

            let to_flush = {
                let mut state = this.state.lock().unwrap();
                let dir_state = state.entry(dir_key.clone()).or_insert_with(DirAggregationState::new);
                dir_state.pending_size += pending.data.len() as u64;
                dir_state.pending_files.push(pending);
                dir_state.pending_size >= this.config.max_blob_size
            };

            if to_flush {
                let files = {
                    let mut state = this.state.lock().unwrap();
                    let dir_state = state.get_mut(&dir_key).expect("dir state missing");
                    let files = std::mem::take(&mut dir_state.pending_files);
                    dir_state.pending_size = 0;
                    files
                };
                match this.write_blob(&dir_key, files).await {
                    Ok(blob) => {
                        let mut state = this.state.lock().unwrap();
                        let dir_state = state.get_mut(&dir_key).expect("dir state missing");
                        dir_state.blobs.push(blob);
                    }
                    Err(e) => return Err((fcb, e)),
                }
            }

            Ok(fcb)
        })
    }

    fn finish(&self) -> futures_util::future::BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            let dir_keys: Vec<String> = {
                let state = this.state.lock().unwrap();
                state.keys().cloned().collect()
            };

            for dir_key in dir_keys {
                let pending = {
                    let mut state = this.state.lock().unwrap();
                    let dir_state = state.get_mut(&dir_key).expect("dir state missing");
                    if dir_state.pending_files.is_empty() {
                        None
                    } else {
                        let files = std::mem::take(&mut dir_state.pending_files);
                        dir_state.pending_size = 0;
                        Some(files)
                    }
                };

                if let Some(files) = pending {
                    let blob = this.write_blob(&dir_key, files).await?;
                    let mut state = this.state.lock().unwrap();
                    let dir_state = state.get_mut(&dir_key).expect("dir state missing");
                    dir_state.blobs.push(blob);
                }
            }

            let snapshot: Vec<(String, Vec<AggregateBlobMeta>)> = {
                let state = this.state.lock().unwrap();
                state.iter().map(|(k, v)| (k.clone(), v.blobs.clone())).collect()
            };
            this.flush_indexes(snapshot).await?;
            this.inner.finish().await
        })
    }
}

fn synthetic_fcb(dst_path: PathBuf, bytes: Vec<u8>, mode: u32) -> FileControlBlock {
    let name = dst_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = FileMeta {
        common: crate::scanner::metadata::MetaCommon {
            name,
            mode,
            ..Default::default()
        },
        size: bytes.len() as u64,
        ..Default::default()
    };
    let mut fcb = FileControlBlock::from(meta);
    fcb.buffer_len = bytes.len();
    fcb.buffer = bytes;
    fcb.src_state = SourceHandleState::Read;
    fcb.dst_path = dst_path;
    fcb
}
