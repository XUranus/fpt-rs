use std::sync::atomic::Ordering;
use std::sync::Arc;

use log::{debug, error};

use crate::backup::aggregate::{should_aggregate, AggregateConfig};
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

#[cfg(feature = "smb")]
pub(crate) async fn execute_smb_source_file_plan<T>(
    plan: FileCopyPlan,
    location: crate::smb::SmbLocation,
    pool: std::sync::Arc<crate::smb::aio::SmbClientPool>,
    target: T,
    stats: Arc<BackupStats>,
    log_prefix: &'static str,
    buffer_size: usize,
    aggregate_config: AggregateConfig,
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
    let unc = match crate::smb::aio::relative_unc_path(&location, &rel_path) {
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
    let resource = match client.create_file(&unc, &open_args).await {
        Ok(resource) => resource,
        Err(e) => {
            error!("{log_prefix}: read {:?}: open {}: {e}", src_path, unc);
            stats.files_failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let file = match resource {
        smb_client::Resource::File(file) => file,
        other => {
            let _ = crate::smb::aio::close_resource(other).await;
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
    let read_cap = crate::backup::aio::transport::clamp_copy_buffer_size(buffer_size)
        .min(crate::smb::aio::SMB_MAX_SAFE_READ_CHUNK);

    if collect_for_aggregation {
        let mut data = Vec::with_capacity(file_size as usize);
        let mut offset = 0u64;
        while offset < file_size {
            let chunk_len = read_cap.min((file_size - offset) as usize);
            let mut chunk = vec![0u8; chunk_len];
            let read_len = match file.read_block(&mut chunk, offset, None, false).await {
                Ok(read_len) => read_len,
                Err(e) => {
                    error!(
                        "{log_prefix}: read {:?}: read {} @{}: {e}",
                        src_path, unc, offset
                    );
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
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
        if let Err((block, msg)) = target.write_block(block).await {
            error!("{log_prefix}: write {:?}: {msg}", block.dst_path);
            stats.files_failed.fetch_add(1, Ordering::Relaxed);
            drop(file);
            return;
        }
    } else {
        let mut offset = 0u64;
        while offset < file_size {
            let chunk_len = read_cap.min((file_size - offset) as usize);
            let mut chunk = vec![0u8; chunk_len];
            let read_len = match file.read_block(&mut chunk, offset, None, false).await {
                Ok(read_len) => read_len,
                Err(e) => {
                    error!(
                        "{log_prefix}: read {:?}: read {} @{}: {e}",
                        src_path, unc, offset
                    );
                    stats.files_failed.fetch_add(1, Ordering::Relaxed);
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
            if let Err((block, msg)) = target.write_block(block).await {
                error!("{log_prefix}: write {:?}: {msg}", block.dst_path);
                stats.files_failed.fetch_add(1, Ordering::Relaxed);
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
