//! Generic restore-side copy pipeline.
//!
//! Restore is the inverse of backup copy:
//! - control-file entries are mapped back to paths relative to the original source
//! - file data is read from local `D_REPO`
//! - small aggregated files are extracted from shared `.AGGR/shard-*` blobs on demand
//! - the selected target transport writes the restored bytes

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use log::{debug, error, info, warn};
use tokio::sync::{mpsc, Semaphore};

use crate::backup::aggregate::{AggregateLayout, AGGREGATE_DIR_NAME, AGGREGATE_ROOT_DIR};
use crate::backup::aggregate::aggregate_dir_index::{read_dir_index, SQLITE_INDEX_FILE_NAME};
use crate::backup::aggregate::aggregate_index::{AggregateIndex, BINARY_INDEX_FILE_NAME};
use crate::backup::aggregate::aggregate_restore::AggregateRestoreEngine;
use crate::backup::aio::entry::EntryMapping;
use crate::backup::aio::transport::{SourceReader, TargetWriter};
use crate::backup::copy_block::CopyBlock;
use crate::backup::copy_plan::{produce_copy_plan, CopyPlanEntry, FileCopyPlan};
use crate::backup::RestorePolicy;
use crate::backup::RestoreStats;
use crate::scanner::metadata::FileMeta;

#[derive(Clone)]
pub struct LocalRepoRestoreSource {
    pub d_repo_base: PathBuf,
    layout: AggregateLayout,
    aggregate: Arc<AggregateRestoreEngine>,
    index_cache: Arc<Mutex<std::collections::HashMap<PathBuf, Arc<AggregateIndex>>>>,
}

impl LocalRepoRestoreSource {
    pub fn new(
        d_repo_base: PathBuf,
        _original_source_base: PathBuf,
        layout: AggregateLayout,
    ) -> Result<Self, String> {
        let aggregate = AggregateRestoreEngine::new(d_repo_base.clone())
            .map_err(|e| format!("init aggregate restore engine: {e}"))?;
        Ok(Self {
            d_repo_base,
            layout,
            aggregate: Arc::new(aggregate),
            index_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    fn get_or_open_index(&self, index_path: &Path) -> Result<Arc<AggregateIndex>, String> {
        let mut cache = self.index_cache.lock().unwrap();
        if let Some(index) = cache.get(index_path) {
            return Ok(Arc::clone(index));
        }
        let index = Arc::new(
            AggregateIndex::open(index_path)
                .map_err(|e| format!("open aggregate index {}: {e}", index_path.display()))?,
        );
        cache.insert(index_path.to_path_buf(), Arc::clone(&index));
        Ok(index)
    }
}

impl SourceReader for LocalRepoRestoreSource {
    fn read_block(
        &self,
        mut block: CopyBlock,
    ) -> futures_util::future::BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            let rel_path = block.src_path.clone();
            let rel_path_string = rel_path.to_string_lossy().replace('\\', "/");
            let aggregate_info = match this.layout {
                AggregateLayout::Shard => {
                    let index_path = this
                        .d_repo_base
                        .join(AGGREGATE_ROOT_DIR)
                        .join(BINARY_INDEX_FILE_NAME);
                    this.get_or_open_index(&index_path).and_then(|index| {
                        index.query_file(&rel_path_string).map_err(|e| {
                            format!("query aggregate index {}: {e}", index_path.display())
                        })
                    })
                }
                AggregateLayout::DirLevel => {
                    let rel_path_obj = PathBuf::from(&rel_path_string);
                    let file_name = rel_path_obj
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or_else(|| format!("invalid restore path {}", rel_path_string));
                    let parent = rel_path_obj.parent().unwrap_or_else(|| Path::new(""));
                    let blob_dir = if parent.as_os_str().is_empty() {
                        PathBuf::from(AGGREGATE_DIR_NAME)
                    } else {
                        parent.join(AGGREGATE_DIR_NAME)
                    };
                    let index_path = this
                        .d_repo_base
                        .join(&blob_dir)
                        .join(SQLITE_INDEX_FILE_NAME);
                    match file_name {
                        Ok(name) => read_dir_index(
                            &index_path,
                            name,
                            &blob_dir.to_string_lossy().replace('\\', "/"),
                        )
                        .map_err(|e| {
                            format!("query dir aggregate index {}: {e}", index_path.display())
                        }),
                        Err(e) => Err(e),
                    }
                }
            };

            let data = match aggregate_info {
                Ok(Some(info)) => this
                    .aggregate
                    .read_from_blob(&info.blob_path, info.offset, info.size)
                    .map_err(|e| format!("read aggregated {:?}: {e}", rel_path)),
                Ok(None) => {
                    let full_path = this.d_repo_base.join(&rel_path);
                    let expected_size = block.file_size;
                    let offset = block.src_offset;
                    tokio::task::spawn_blocking(move || {
                        crate::backup::aio::local_fs::read_local_file_chunk(
                            &full_path,
                            offset,
                            expected_size,
                            crate::backup::aio::transport::DEFAULT_COPY_BUFFER_SIZE,
                        )
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")))
                }
                Err(e) => Err(format!("lookup aggregated {:?}: {e}", rel_path)),
            };

            match data {
                Ok(buf) => {
                    block.src_offset = block.src_offset.saturating_add(buf.len() as u64);
                    block.is_last = block.src_offset >= block.file_size;
                    block.data = buf;
                    Ok(block)
                }
                Err(msg) => Err((block, msg)),
            }
        })
    }
}

pub async fn run_restore_copy_pipeline<T>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    original_source_base: PathBuf,
    source: LocalRepoRestoreSource,
    target: T,
    target_local_base: Option<PathBuf>,
    policy: RestorePolicy,
    stats: Arc<Mutex<RestoreStats>>,
    log_prefix: &'static str,
    max_concurrent_tasks: usize,
) where
    T: TargetWriter,
{
    if target_local_base.is_none() && policy != RestorePolicy::Replace {
        warn!("{log_prefix}: restore policy {policy:?} is only enforced for local targets; remote target will use Replace semantics");
    }

    let task_sem = Arc::new(Semaphore::new(max_concurrent_tasks.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<CopyPlanEntry>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        let _ = original_source_base;
        let mapping = EntryMapping::remote_to_local();
        tokio::task::spawn_blocking(move || {
            produce_copy_plan(
                control_file,
                meta_dir,
                mapping,
                log_prefix,
                |_| false,
                |entry| entry_tx.blocking_send(entry).is_ok(),
            );
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            CopyPlanEntry::Directory { dst_path, .. } => {
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let path = dst_path;
                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    match target2.create_dir(path.clone()).await {
                        Ok(()) => {
                            stats2.lock().unwrap().dirs_created += 1;
                        }
                        Err(e) => {
                            error!("{log_prefix}: mkdir {:?}: {e}", path);
                        }
                    }
                });
                task_handles.push(h);
            }
            CopyPlanEntry::File(FileCopyPlan::Direct {
                meta,
                src_path,
                dst_path,
            }) => {
                if let Some(ref symlink_target) = meta.common.symlink_target_path {
                    // Create symlink instead of copying file content
                    let symlink_full_path = target_local_base
                        .as_ref()
                        .map(|base| base.join(&dst_path))
                        .unwrap_or_else(|| dst_path.clone());
                    debug!("{log_prefix}: restoring symlink {:?} -> {:?}", symlink_full_path, symlink_target);
                    if let Some(parent) = symlink_full_path.parent() {
                        if let Err(e) = tokio::fs::create_dir_all(parent).await {
                            log::warn!("{log_prefix}: failed to create symlink parent dir {:?}: {e}", parent);
                        }
                    }
                    match crate::native::backup::local_metadata::create_symlink(&symlink_full_path, symlink_target) {
                        Ok(()) => {
                            crate::native::backup::local_metadata::restore_common_metadata(
                                &symlink_full_path, &meta.common,
                            );
                            stats.lock().unwrap().files_restored += 1;
                        }
                        Err(e) => {
                            error!("{log_prefix}: symlink {:?}: {e}", symlink_full_path);
                            stats.lock().unwrap().files_failed += 1;
                        }
                    }
                    continue;
                }

                let target2 = target.clone();
                let source2 = source.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let local_target_base2 = target_local_base.clone();

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();

                    if should_skip_restore(&meta, &dst_path, local_target_base2.as_ref(), policy)
                        .await
                    {
                        let mut guard = stats2.lock().unwrap();
                        guard.files_skipped += 1;
                        guard.bytes_skipped += meta.size;
                        return;
                    }

                    let restore_rel = dst_path.clone();
                    let restore_full_path = local_target_base2
                        .as_ref()
                        .map(|base| base.join(&restore_rel))
                        .unwrap_or_else(|| restore_rel.clone());
                    let file_size = meta.size;
                    let mut block = CopyBlock {
                        meta: Arc::new(meta),
                        src_path,
                        dst_path,
                        src_offset: 0,
                        dst_offset: 0,
                        file_size,
                        data: Vec::new(),
                        is_last: file_size == 0,
                    };
                    loop {
                        block = match source2.read_block(block).await {
                            Ok(block) => block,
                            Err((failed_block, msg)) => {
                                error!("{log_prefix}: read {:?}: {msg}", failed_block.src_path);
                                stats2.lock().unwrap().files_failed += 1;
                                return;
                            }
                        };

                        if block.data_len() == 0 && !block.read_complete() {
                            error!(
                                "{log_prefix}: read {:?}: zero-length chunk before EOF",
                                block.src_path
                            );
                            stats2.lock().unwrap().files_failed += 1;
                            return;
                        }

                        block = match target2.write_block(block).await {
                            Ok(block) => block,
                            Err((failed_block, msg)) => {
                                error!("{log_prefix}: write {:?}: {msg}", failed_block.dst_path);
                                stats2.lock().unwrap().files_failed += 1;
                                return;
                            }
                        };

                        if block.read_complete() && block.write_complete() {
                            debug!("{log_prefix}: restored {:?} (attr=0x{:x})", restore_rel, block.meta.common.attr);
                            // Restore metadata (attributes, xattrs, ACLs)
                            crate::native::backup::local_metadata::restore_common_metadata(
                                &restore_full_path, &block.meta.common,
                            );
                            let mut guard = stats2.lock().unwrap();
                            guard.files_restored += 1;
                            guard.bytes_restored += block.file_size;
                            break;
                        }

                        block.clear_data();
                    }
                });
                task_handles.push(h);
            }
            CopyPlanEntry::File(FileCopyPlan::Aggregate { src_path, .. }) => {
                error!(
                    "{log_prefix}: unexpected aggregate restore plan for {:?}",
                    src_path
                );
                stats.lock().unwrap().files_failed += 1;
            }
        }
    }

    if let Err(e) = producer_handle.await {
        error!("{log_prefix}: entry producer panicked: {e}");
    }

    for h in task_handles {
        if let Err(e) = h.await {
            error!("{log_prefix}: task panicked: {e}");
        }
    }

    if let Err(e) = source.finish().await {
        error!("{log_prefix}: source finalization failed: {e}");
    }
    if let Err(e) = target.finish().await {
        error!("{log_prefix}: target finalization failed: {e}");
    }

    let snapshot = stats.lock().unwrap().clone();
    info!(
        "{log_prefix}: complete: {} restored, {} skipped, {} bytes, {} failed",
        snapshot.files_restored,
        snapshot.files_skipped,
        snapshot.bytes_restored,
        snapshot.files_failed,
    );
}

async fn should_skip_restore(
    meta: &FileMeta,
    dst_path: &Path,
    target_local_base: Option<&PathBuf>,
    policy: RestorePolicy,
) -> bool {
    let Some(base) = target_local_base else {
        return false;
    };
    let target_path = base.join(dst_path);
    let source_mtime = Some(UNIX_EPOCH + Duration::from_secs(meta.common.mtime as u64));
    let target_path2 = target_path.clone();
    tokio::task::spawn_blocking(move || {
        let metadata = std::fs::metadata(&target_path2).ok();
        let target_exists = metadata.is_some();
        let target_mtime = metadata.and_then(|m| m.modified().ok());
        !policy.should_restore(source_mtime, target_exists, target_mtime)
    })
    .await
    .unwrap_or(false)
}
