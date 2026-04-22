//! Generic async copy pipeline for remote-capable backup transports.
//!
//! Direction-specific modules only need to provide:
//! - the control-file path mapping
//! - a [`SourceReader`] implementation
//! - a [`TargetWriter`] implementation

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use log::{debug, error, info};
use tokio::sync::{mpsc, Semaphore};

use crate::backup::aio::entry::EntryMapping;
use crate::backup::aio::executor::execute_async_file_plan;
use crate::backup::aio::transport::{SourceReader, TargetWriter};
use crate::backup::copy_plan::{produce_copy_plan, CopyPlanEntry};
use crate::backup::stats::BackupStats;
use crate::failure::{FailureItemType, FailureRecord, FailureRecorder, RetryPolicy};

pub async fn run_copy_pipeline<S, T>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    mapping: EntryMapping,
    source: S,
    target: T,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
    max_concurrent_tasks: usize,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) where
    S: SourceReader,
    T: TargetWriter,
{
    let task_sem = Arc::new(Semaphore::new(max_concurrent_tasks.max(1)));
    let (entry_tx, mut entry_rx) = mpsc::channel::<CopyPlanEntry>(256);

    let producer_handle = {
        let entry_tx = entry_tx.clone();
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
                let failure_recorder2 = failure_recorder.clone();
                let path = dst_path;
                debug!("{log_prefix}: mkdir {:?}", path);

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    match retry_create_dir(&target2, path.clone(), retry_policy).await {
                        Ok(()) => {
                            stats2.dirs_created.fetch_add(1, Ordering::Relaxed);
                        }
                        Err((e, attempts)) => {
                            error!("{log_prefix}: mkdir {:?}: {e}", path);
                            stats2.dirs_failed.fetch_add(1, Ordering::Relaxed);
                            if let Some(recorder) = &failure_recorder2 {
                                recorder.record(FailureRecord::from_detail(
                                    "backup",
                                    "create_dir",
                                    FailureItemType::Directory,
                                    path.to_string_lossy(),
                                    e,
                                    attempts,
                                ));
                            }
                        }
                    }
                });
                task_handles.push(h);
            }
            CopyPlanEntry::File(plan) => {
                let source2 = source.clone();
                let target2 = target.clone();
                let stats2 = Arc::clone(&stats);
                let task_sem2 = Arc::clone(&task_sem);
                let failure_recorder2 = failure_recorder.clone();

                let h = tokio::spawn(async move {
                    let _permit = task_sem2.acquire_owned().await.unwrap();
                    execute_async_file_plan(
                        plan,
                        source2,
                        target2,
                        stats2,
                        log_prefix,
                        retry_policy,
                        failure_recorder2,
                    )
                    .await;
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
        stats.files_failed.fetch_add(1, Ordering::Relaxed);
    }
    if let Err(e) = target.finish().await {
        error!("{log_prefix}: target finalization failed: {e}");
        stats.files_failed.fetch_add(1, Ordering::Relaxed);
    }

    info!(
        "{log_prefix}: complete: {} files, {} bytes, {} dirs, {} failed",
        stats.files_copied.load(Ordering::Relaxed),
        stats.bytes_copied.load(Ordering::Relaxed),
        stats.dirs_created.load(Ordering::Relaxed),
        stats.files_failed.load(Ordering::Relaxed),
    );
}

async fn retry_create_dir<T: TargetWriter>(
    target: &T,
    path: PathBuf,
    retry_policy: RetryPolicy,
) -> Result<(), (String, u32)> {
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        match target.create_dir(path.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) if retry_policy.should_retry(attempts) => {
                tokio::time::sleep(retry_policy.delay_for_attempt(attempts)).await;
                let _ = &e;
            }
            Err(e) => return Err((e, attempts)),
        }
    }
}
