use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::Ordering;

use log::{error, info};

use crate::backup::aggregate_engine::PendingLocalFile;
use crate::backup::aggregate_local::LocalAggregateState;
use crate::backup::copy_plan::{CopyPlanEntry, FileCopyPlan};
use crate::backup::local_block::copy_stream;
use crate::backup::local_metadata::{create_symlink, restore_common_metadata};
use crate::backup::stats::BackupStats;
use crate::scanner::metadata::FileMeta;

pub(crate) fn execute_local_plan_entry(
    entry: CopyPlanEntry,
    stats: &BackupStats,
    job_tx: &std::sync::mpsc::SyncSender<FileCopyPlan>,
) -> io::Result<bool> {
    match entry {
        CopyPlanEntry::Directory { meta, dst_path } => {
            if let Err(e) = std::fs::create_dir_all(&dst_path) {
                error!("Failed to create target directory {:?}: {}", dst_path, e);
                stats.inc_dirs_failed();
            } else {
                restore_common_metadata(&dst_path, &meta.common);
                stats.inc_dirs_created();
            }
            Ok(true)
        }
        CopyPlanEntry::File(plan) => {
            if job_tx.send(plan).is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "local copy workers disconnected",
                ));
            }
            Ok(true)
        }
    }
}

pub(crate) fn execute_local_file_plan(
    plan: FileCopyPlan,
    aggregate_state: Option<&LocalAggregateState>,
    stats: &BackupStats,
    buffer: &mut [u8],
) -> io::Result<()> {
    match plan {
        FileCopyPlan::Direct {
            meta,
            src_path,
            dst_path,
        } => copy_one_local_file(&meta, &src_path, &dst_path, stats, buffer),
        FileCopyPlan::Aggregate { meta, src_path } => {
            let Some(agg_state) = aggregate_state else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "missing aggregate state",
                ));
            };
            aggregate_one_local_file(agg_state, stats, &meta, &src_path, buffer)
        }
    }
}

pub(crate) fn flush_local_aggregate_state(
    agg_state: &LocalAggregateState,
    stats: &BackupStats,
    copy_buffer_size: usize,
) {
    let mut buffer = vec![0_u8; copy_buffer_size.clamp(256 * 1024, 4 * 1024 * 1024)];
    for (bucket_key, files) in agg_state.flush_all() {
        write_aggregate_blob(agg_state, stats, &bucket_key, files, &mut buffer);
    }
    if let Err(e) = agg_state.engine.flush_all_indexes() {
        error!("Failed to flush aggregate indexes: {}", e);
    }
}

fn aggregate_one_local_file(
    agg_state: &LocalAggregateState,
    stats: &BackupStats,
    meta: &FileMeta,
    src_path: &Path,
    buffer: &mut [u8],
) -> io::Result<()> {
    let relative_path = agg_state.engine.relative_path_for_source(src_path);
    let pending =
        agg_state.pending_file_for_source(relative_path.clone(), src_path.to_path_buf(), meta);

    if let Some((bucket_key, files)) = agg_state.add_file(&relative_path, pending) {
        write_aggregate_blob(agg_state, stats, &bucket_key, files, buffer);
    }
    Ok(())
}

fn write_aggregate_blob(
    agg_state: &LocalAggregateState,
    stats: &BackupStats,
    bucket_key: &str,
    files: Vec<PendingLocalFile>,
    buffer: &mut [u8],
) {
    let file_count = files.len() as u64;
    let bytes_in_blob: u64 = files.iter().map(|f| f.size).sum();
    match agg_state
        .engine
        .create_blob_from_local_files(bucket_key, files, buffer)
    {
        Ok(blob_meta) => {
            info!(
                "Created blob {} for bucket {} with {} files",
                blob_meta.blob_path, bucket_key, blob_meta.file_count
            );
            stats.files_copied.fetch_add(file_count, Ordering::Relaxed);
            stats
                .bytes_copied
                .fetch_add(bytes_in_blob, Ordering::Relaxed);
        }
        Err(e) => {
            error!(
                "Failed to create aggregate blob for bucket {}: {}",
                bucket_key, e
            );
            stats
                .files_failed
                .fetch_add(file_count.max(1), Ordering::Relaxed);
        }
    }
}

fn copy_one_local_file(
    meta: &FileMeta,
    src_path: &Path,
    dst_path: &Path,
    stats: &BackupStats,
    buffer: &mut [u8],
) -> io::Result<()> {
    if let Some(ref symlink_target) = meta.common.symlink_target_path {
        if let Some(parent) = dst_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        create_symlink(dst_path, symlink_target)?;
        restore_common_metadata(dst_path, &meta.common);
        stats.inc_files_copied();
        return Ok(());
    }

    if let Some(parent) = dst_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut src = File::open(src_path)?;
    stats.inc_src_opened();
    let mut dst = File::create(dst_path)?;
    stats.inc_dst_opened();

    let copied = copy_stream(&mut src, &mut dst, buffer)?;
    stats.add_bytes_copied(copied);
    dst.flush()?;
    drop(dst);
    stats.inc_dst_closed();

    restore_common_metadata(dst_path, &meta.common);

    drop(src);
    stats.inc_src_closed();
    stats.inc_files_copied();
    Ok(())
}
