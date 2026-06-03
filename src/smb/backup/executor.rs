//! SMB source file copy execution.
//!
//! This module contains the SMB-specific file copy logic that bypasses the
//! generic `SourceReader` trait, using direct SMB streaming reads instead.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use log::{debug, error};

use crate::backup::aggregate::{should_aggregate, AggregateConfig};
use crate::backup::aio::transport::{clamp_copy_buffer_size, TargetWriter};
use crate::backup::copy_block::CopyBlock;
use crate::backup::copy_plan::FileCopyPlan;
use crate::backup::stats::BackupStats;
use crate::failure::{
    retry_async_item, FailureItemType, FailureRecord, FailureRecorder, RetryPolicy,
};
use crate::smb::SmbLocation;

/// Execute a single SMB source file copy plan.
///
/// Unlike the generic `execute_async_file_plan` which uses `SourceReader`,
/// this function reads directly from SMB using `read_block` on the SMB file
/// handle, because SMB source reads don't fit the `SourceReader` trait pattern.
pub(crate) async fn execute_smb_source_file_plan<T>(
    plan: FileCopyPlan,
    location: SmbLocation,
    pool: Arc<crate::smb::SmbClientPool>,
    target: T,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
    buffer_size: usize,
    aggregate_config: AggregateConfig,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
) where
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
    let rel_path = src_path.to_string_lossy().replace('\\', "/");
    let client = pool.client();
    let unc = match crate::smb::relative_unc_path(&location, &rel_path) {
        Ok(unc) => unc,
        Err(msg) => {
            error!("{log_prefix}: read {:?}: {msg}", src_path);
            stats.files_failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let open_args = smb_client::FileCreateArgs::make_open_existing(
        smb_client::FileAccessMask::new().with_generic_read(true),
    );
    let resource = match crate::failure::retry_async(retry_policy, || client.create_file(&unc, &open_args)).await {
        Ok(resource) => resource,
        Err((e, attempts)) => {
            error!("{log_prefix}: read {:?}: open {}: {e}", src_path, unc);
            stats.files_failed.fetch_add(1, Ordering::Relaxed);
            if let Some(recorder) = &failure_recorder {
                recorder.record(FailureRecord::from_detail(
                    "backup",
                    "open_source",
                    FailureItemType::File,
                    src_path.to_string_lossy(),
                    format!("open {}: {e}", unc),
                    attempts,
                ));
            }
            return;
        }
    };
    let file = match resource {
        smb_client::Resource::File(file) => file,
        other => {
            let _ = crate::smb::close_resource(other).await;
            error!(
                "{log_prefix}: read {:?}: {} did not resolve to a file handle",
                src_path, unc
            );
            stats.files_failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let meta = Arc::new(meta);
    let collect_for_aggregation =
        aggregate_config.enabled && should_aggregate(file_size, &aggregate_config);
    let read_cap = clamp_copy_buffer_size(buffer_size)
        .min(crate::smb::SMB_MAX_SAFE_READ_CHUNK);

    if collect_for_aggregation {
        let mut data = Vec::with_capacity(file_size as usize);
        let mut offset = 0u64;
        while offset < file_size {
            let chunk_len = read_cap.min((file_size - offset) as usize);
            let chunk = vec![0u8; chunk_len];
            let (chunk, read_len) =
                match retry_smb_read_chunk(&file, chunk, offset, retry_policy).await {
                    Ok(result) => result,
                    Err((_, e, attempts)) => {
                        error!(
                            "{log_prefix}: read {:?}: read {} @{}: {e}",
                            src_path, unc, offset
                        );
                        stats.files_failed.fetch_add(1, Ordering::Relaxed);
                        if let Some(recorder) = &failure_recorder {
                            recorder.record(FailureRecord::from_detail(
                                "backup",
                                "read_block",
                                FailureItemType::File,
                                src_path.to_string_lossy(),
                                e,
                                attempts,
                            ));
                        }
                        drop(file);
                        return;
                    }
                };
            if read_len == 0 {
                error!(
                    "{log_prefix}: read {:?}: zero-length chunk before EOF",
                    src_path
                );
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
                drop(file);
                return;
            }
            data.extend_from_slice(&chunk[..read_len]);
            offset += read_len as u64;
        }
        let block = CopyBlock {
            meta,
            src_path,
            dst_path,
            src_offset: file_size,
            dst_offset: 0,
            file_size,
            data,
            is_last: true,
        };
        if let Err((block, msg, attempts)) = retry_write_block(&target, block, retry_policy).await {
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
            drop(file);
            return;
        }
    } else {
        let mut offset = 0u64;
        while offset < file_size {
            let chunk_len = read_cap.min((file_size - offset) as usize);
            let chunk = vec![0u8; chunk_len];
            let (chunk, read_len) =
                match retry_smb_read_chunk(&file, chunk, offset, retry_policy).await {
                    Ok(result) => result,
                    Err((_, e, attempts)) => {
                        error!(
                            "{log_prefix}: read {:?}: read {} @{}: {e}",
                            src_path, unc, offset
                        );
                        stats.files_failed.fetch_add(1, Ordering::Relaxed);
                        if let Some(recorder) = &failure_recorder {
                            recorder.record(FailureRecord::from_detail(
                                "backup",
                                "read_block",
                                FailureItemType::File,
                                src_path.to_string_lossy(),
                                e,
                                attempts,
                            ));
                        }
                        drop(file);
                        return;
                    }
                };
            if read_len == 0 {
                error!(
                    "{log_prefix}: read {:?}: zero-length chunk before EOF",
                    src_path
                );
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
                drop(file);
                return;
            }

            let next_offset = offset + read_len as u64;
            let block = CopyBlock {
                meta: Arc::clone(&meta),
                src_path: src_path.clone(),
                dst_path: dst_path.clone(),
                src_offset: next_offset,
                dst_offset: offset,
                file_size,
                data: chunk[..read_len].to_vec(),
                is_last: next_offset >= file_size,
            };
            if let Err((block, msg, attempts)) =
                retry_write_block(&target, block, retry_policy).await
            {
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
                drop(file);
                return;
            }
            offset = next_offset;
        }
    }

    drop(file);
    debug!("{log_prefix}: copied {:?} -> {:?}", read_path, write_path);
    stats.files_copied.fetch_add(1, Ordering::Relaxed);
    stats.bytes_copied.fetch_add(file_size, Ordering::Relaxed);
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

async fn retry_smb_read_chunk(
    file: &smb_client::resource::file::File,
    chunk: Vec<u8>,
    offset: u64,
    retry_policy: RetryPolicy,
) -> Result<(Vec<u8>, usize), (Vec<u8>, String, u32)> {
    retry_async_item(retry_policy, chunk, |mut chunk| async move {
        match file.read_block(&mut chunk, offset, None, false).await {
            Ok(read_len) => Ok((chunk, read_len)),
            Err(e) => Err((chunk, e.to_string())),
        }
    })
    .await
}
