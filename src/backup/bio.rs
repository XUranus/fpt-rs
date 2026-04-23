use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::stats::BackupStats;
use crate::backup::{PhaseFlags, SharedState};
use crate::failure::{FailureRecorder, RetryPolicy};

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
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
    aggregate_config: AggregateConfig,
    phase_flags: PhaseFlags,
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
        retry_policy,
        failure_recorder,
        aggregate_config,
        phase_flags,
        stats,
        terminate_indicator,
    )
}
