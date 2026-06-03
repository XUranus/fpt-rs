use std::sync::atomic::Ordering;
use std::sync::Arc;

use log::{debug, error};

use crate::backup::aio::transport::{SourceReader, TargetWriter};
use crate::backup::copy_block::CopyBlock;
use crate::backup::copy_plan::FileCopyPlan;
use crate::backup::stats::BackupStats;
use crate::failure::{retry_async_item, FailureItemType, FailureRecord, FailureRecorder, RetryPolicy};

pub(crate) async fn execute_async_file_plan<S, T>(
    plan: FileCopyPlan,
    source: S,
    target: T,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) where
    S: SourceReader,
    T: TargetWriter,
{
    let (meta, src_path, dst_path) = match plan {
        FileCopyPlan::Direct {
            meta,
            src_path,
            dst_path,
        } => (meta, src_path, dst_path),
        FileCopyPlan::Aggregate { meta: _, src_path } => {
            debug!("{log_prefix}: skipping aggregate plan for {:?}", src_path);
            stats.files_failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    if meta.common.symlink_target_path.is_some() {
        debug!("{log_prefix}: skipping symlink {:?}", src_path);
        return;
    }

    let read_path = src_path.clone();
    let write_path = dst_path.clone();
    let file_size = meta.size;
    let mut block = CopyBlock {
        meta: Arc::new(meta),
        src_path,
        dst_path,
        src_offset: 0,
        dst_offset: 0,
        file_size,
        data: Vec::new(),
        is_last: false,
    };

    loop {
        block = match retry_read_block(&source, block, retry_policy).await {
            Ok(block) => block,
            Err((block, msg, attempts)) => {
                error!("{log_prefix}: read {:?}: {msg}", block.src_path);
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
                if let Some(recorder) = &failure_recorder {
                    recorder.record(FailureRecord::from_detail(
                        "backup",
                        "read_block",
                        FailureItemType::File,
                        block.src_path.to_string_lossy(),
                        msg,
                        attempts,
                    ));
                }
                return;
            }
        };

        if block.data_len() == 0 && !block.read_complete() {
            error!(
                "{log_prefix}: read {:?}: zero-length chunk before EOF",
                block.src_path
            );
            stats.files_failed.fetch_add(1, Ordering::Relaxed);
            return;
        }

        block = match retry_write_block(&target, block, retry_policy).await {
            Ok(done_block) => done_block,
            Err((block, msg, attempts)) => {
                error!("{log_prefix}: write {:?}: {msg}", block.dst_path);
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
                if let Some(recorder) = &failure_recorder {
                    recorder.record(FailureRecord::from_detail(
                        "backup",
                        "write_block",
                        FailureItemType::File,
                        block.dst_path.to_string_lossy(),
                        msg,
                        attempts,
                    ));
                }
                return;
            }
        };

        if block.read_complete() && block.write_complete() {
            debug!("{log_prefix}: copied {:?} -> {:?}", read_path, write_path);
            stats.files_copied.fetch_add(1, Ordering::Relaxed);
            stats.bytes_copied.fetch_add(file_size, Ordering::Relaxed);
            break;
        }

        block.clear_data();
    }
}

// SMB source file execution moved to crate::smb::backup::executor

async fn retry_read_block<S: SourceReader>(
    source: &S,
    block: CopyBlock,
    retry_policy: RetryPolicy,
) -> Result<CopyBlock, (CopyBlock, String, u32)> {
    retry_async_item(retry_policy, block, |block| async move {
        source.read_block(block).await
    })
    .await
}

async fn retry_write_block<T: TargetWriter>(
    target: &T,
    block: CopyBlock,
    retry_policy: RetryPolicy,
) -> Result<CopyBlock, (CopyBlock, String, u32)> {
    retry_async_item(retry_policy, block, |block| async move {
        target.write_block(block).await
    })
    .await
}

// retry_smb_read_chunk moved to crate::smb::backup::executor
