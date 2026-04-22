use std::sync::atomic::Ordering;
use std::sync::Arc;

use log::{debug, error};

use crate::backup::aio::transport::{SourceReader, TargetWriter};
use crate::backup::copy_block::CopyBlock;
use crate::backup::copy_plan::FileCopyPlan;
use crate::backup::stats::BackupStats;

pub(crate) async fn execute_async_file_plan<S, T>(
    plan: FileCopyPlan,
    source: S,
    target: T,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
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
        block = match source.read_block(block).await {
            Ok(block) => block,
            Err((block, msg)) => {
                error!("{log_prefix}: read {:?}: {msg}", block.src_path);
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
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

        block = match target.write_block(block).await {
            Ok(done_block) => done_block,
            Err((block, msg)) => {
                error!("{log_prefix}: write {:?}: {msg}", block.dst_path);
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
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
