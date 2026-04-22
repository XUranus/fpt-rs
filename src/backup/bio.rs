use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::stats::BackupStats;
use crate::backup::SharedState;

pub mod delete;
pub mod hardlink;
pub mod local_copy;
pub mod mtime;

pub(crate) fn spawn_local_backup_pipeline(
    control_file: PathBuf,
    source_dir_base: PathBuf,
    target_dir_base: PathBuf,
    meta_dir: PathBuf,
    ctrl_dir: PathBuf,
    worker_count: usize,
    copy_buffer_size: usize,
    aggregate_config: AggregateConfig,
    enable_hardlink_phase: bool,
    enable_delete_phase: bool,
    enable_mtime_phase: bool,
    _shared_state: Arc<SharedState>,
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    local_copy::spawn_local_common_copy_pipeline(
        control_file,
        source_dir_base,
        target_dir_base,
        meta_dir,
        ctrl_dir,
        worker_count,
        copy_buffer_size,
        aggregate_config,
        enable_hardlink_phase,
        enable_delete_phase,
        enable_mtime_phase,
        stats,
        terminate_indicator,
    )
}
