//! Generic restore-side copy pipeline.
//!
//! Restore is the inverse of backup copy:
//! - control-file entries are mapped back to paths relative to the original source
//! - file data is read from local `D_REPO`
//! - small aggregated files are extracted from `.AGGR_DIR` blobs on demand
//! - the selected target transport writes the restored bytes

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use log::{debug, error, info, warn};
use tokio::sync::{Semaphore, mpsc};

use crate::backup::RestorePolicy;
use crate::backup::RestoreStats;
use crate::backup::aggregate_index::AggregateIndex;
use crate::backup::aggregate_restore::AggregateRestoreEngine;
use crate::backup::aio::entry::{EntryMapping, produce_entries};
use crate::backup::aio::transport::{SourceReader, TargetWriter};
use crate::backup::fcb::{ControlBlockVarient, FileControlBlock, SourceHandleState};

#[derive(Clone)]
pub struct LocalRepoRestoreSource {
    pub d_repo_base: PathBuf,
    pub original_source_base: PathBuf,
    aggregate: Arc<AggregateRestoreEngine>,
}

impl LocalRepoRestoreSource {
    pub fn new(d_repo_base: PathBuf, original_source_base: PathBuf) -> Result<Self, String> {
        let aggregate = AggregateRestoreEngine::new(d_repo_base.clone())
            .map_err(|e| format!("init aggregate restore engine: {e}"))?;
        Ok(Self {
            d_repo_base,
            original_source_base,
            aggregate: Arc::new(aggregate),
        })
    }
}

impl SourceReader for LocalRepoRestoreSource {
    fn read_file(
        &self,
        mut fcb: FileControlBlock,
    ) -> futures_util::future::BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            let rel_path = fcb.src_path.clone();
            let file_name = match rel_path.file_name() {
                Some(name) => name.to_string_lossy().into_owned(),
                None => return Err((fcb, format!("invalid restore path {:?}", rel_path))),
            };
            let rel_dir = rel_path.parent().unwrap_or_else(|| Path::new(""));
            let repo_dir = this.d_repo_base.join(rel_dir);
            let repo_dir_str = repo_dir.to_string_lossy().into_owned();
            let dir_key = rel_dir.to_string_lossy().to_string();
            let source_dir_key = this
                .original_source_base
                .join(rel_dir)
                .to_string_lossy()
                .to_string();

            let restore_info = repo_dir
                .join(".AGGR_DIR")
                .join("AGGREGATE_IDX.sqlite");
            let aggregate_info = if restore_info.exists() {
                let index = AggregateIndex::open(&restore_info)
                    .map_err(|e| format!("open aggregate index {}: {e}", restore_info.display()));
                match index {
                    Ok(index) => index
                        .query_file(&file_name, &source_dir_key)
                        .or_else(|_| index.query_file(&file_name, &dir_key))
                        .map_err(|e| format!("query aggregate index {}: {e}", restore_info.display())),
                    Err(e) => Err(e),
                }
            } else {
                Ok(None)
            };

            let data = match aggregate_info {
                Ok(Some(info)) => this
                    .aggregate
                    .read_from_blob(&repo_dir_str, &info.blob_name, info.offset, info.size)
                    .map_err(|e| format!("read aggregated {:?}: {e}", rel_path)),
                Ok(None) => {
                    let full_path = this.d_repo_base.join(&rel_path);
                    let expected_size = fcb.meta.size;
                    tokio::task::spawn_blocking(move || {
                        crate::backup::aio::local_fs::read_local_file(&full_path, expected_size)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")))
                }
                Err(e) => Err(format!("lookup aggregated {:?}: {e}", rel_path)),
            };

            match data {
                Ok(buf) => {
                    fcb.buffer_len = buf.len();
                    fcb.buffer = buf;
                    fcb.src_state = SourceHandleState::Read;
                    Ok(fcb)
                }
                Err(msg) => Err((fcb, msg)),
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
    let (entry_tx, mut entry_rx) = mpsc::channel::<ControlBlockVarient>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
        let _ = original_source_base;
        let mapping = EntryMapping::remote_to_local();
        tokio::task::spawn_blocking(move || {
            produce_entries(control_file, meta_dir, mapping, entry_tx, log_prefix);
        })
    };
    drop(entry_tx);

    let mut task_handles = Vec::new();

    while let Some(item) = entry_rx.recv().await {
        match item {
            ControlBlockVarient::DirControlBlock(dcb) => {
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let path = dcb.dst_path.clone();
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
            ControlBlockVarient::FileControlBlock(fcb) => {
                if fcb.meta.common.symlink_target_path.is_some() {
                    debug!("{log_prefix}: skipping symlink {:?}", fcb.src_path);
                    continue;
                }

                let target2 = target.clone();
                let source2 = source.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let local_target_base2 = target_local_base.clone();

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();

                    if should_skip_restore(&fcb, local_target_base2.as_ref(), policy).await {
                        let mut guard = stats2.lock().unwrap();
                        guard.files_skipped += 1;
                        guard.bytes_skipped += fcb.meta.size;
                        return;
                    }

                    let restore_rel = fcb.dst_path.clone();
                    let fcb = match source2.read_file(fcb).await {
                        Ok(fcb) => fcb,
                        Err((failed_fcb, msg)) => {
                            error!("{log_prefix}: read {:?}: {msg}", failed_fcb.src_path);
                            stats2.lock().unwrap().files_failed += 1;
                            return;
                        }
                    };

                    match target2.write_file(fcb).await {
                        Ok(done_fcb) => {
                            debug!("{log_prefix}: restored {:?}", restore_rel);
                            let mut guard = stats2.lock().unwrap();
                            guard.files_restored += 1;
                            guard.bytes_restored += done_fcb.meta.size;
                        }
                        Err((failed_fcb, msg)) => {
                            error!("{log_prefix}: write {:?}: {msg}", failed_fcb.dst_path);
                            stats2.lock().unwrap().files_failed += 1;
                        }
                    }
                });
                task_handles.push(h);
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
    fcb: &FileControlBlock,
    target_local_base: Option<&PathBuf>,
    policy: RestorePolicy,
) -> bool {
    let Some(base) = target_local_base else {
        return false;
    };
    let target_path = base.join(&fcb.dst_path);
    let source_mtime = Some(UNIX_EPOCH + Duration::from_secs(fcb.meta.common.mtime as u64));
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
