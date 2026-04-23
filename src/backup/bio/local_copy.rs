use crate::backup::aggregate::AggregateConfig;
use crate::backup::aggregate_engine::AggregateBackupEngine;
use crate::backup::aggregate_local::LocalAggregateState;
use crate::backup::copy_plan::{produce_local_copy_plan, FileCopyPlan};
use crate::backup::local_executor::{
    execute_local_file_plan, execute_local_plan_entry, flush_local_aggregate_state,
};
use crate::backup::phases::run_local_followup_phases;
use crate::backup::stats::BackupStats;
use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};
use log::{error, info};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

pub(crate) fn spawn_local_common_copy_pipeline(
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
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let aggregate_state = if aggregate_config.enabled {
            match AggregateBackupEngine::new(
                aggregate_config.clone(),
                source_dir_base.clone(),
                target_dir_base.clone(),
            ) {
                Ok(engine) => Some(Arc::new(LocalAggregateState::new(Arc::new(engine)))),
                Err(e) => {
                    error!("Failed to create aggregate engine: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let queue_capacity = worker_count.max(1) * 2;
        let (job_tx, job_rx) = mpsc::sync_channel::<FileCopyPlan>(queue_capacity);
        let worker_rx = Arc::new(std::sync::Mutex::new(job_rx));

        let mut workers = Vec::with_capacity(worker_count.max(1));
        for i in 0..worker_count.max(1) {
            let rx = Arc::clone(&worker_rx);
            let stats = Arc::clone(&stats);
            let aggregate_state = aggregate_state.clone();
            let worker_failure_recorder = failure_recorder.clone();
            workers.push(thread::spawn(move || {
                let mut buffer = vec![0_u8; copy_buffer_size.clamp(256 * 1024, 4 * 1024 * 1024)];
                loop {
                    let recv_result = {
                        let rx = rx.lock().unwrap();
                        rx.recv()
                    };
                    let job = match recv_result {
                        Ok(job) => job,
                        Err(_) => break,
                    };
                    if let Err(e) = execute_local_file_plan(
                        job,
                        aggregate_state.as_deref(),
                        &stats,
                        &mut buffer,
                        retry_policy,
                        worker_failure_recorder.as_ref(),
                    ) {
                        error!("Local copy worker {} failed: {}", i, e);
                        stats.inc_files_failed();
                    }
                }
            }));
        }

        if let Err(e) = produce_local_copy_jobs(
            &control_file,
            &meta_dir,
            &source_dir_base,
            &target_dir_base,
            &job_tx,
            &stats,
            retry_policy,
            failure_recorder.as_ref(),
            aggregate_state.as_ref(),
        ) {
            error!("Local copy producer failed: {}", e);
        }
        drop(job_tx);

        for handle in workers {
            if let Err(e) = handle.join() {
                error!("Local copy worker join failed: {:?}", e);
            }
        }

        if let Some(agg_state) = aggregate_state {
            flush_local_aggregate_state(
                &agg_state,
                &stats,
                copy_buffer_size,
                retry_policy,
                failure_recorder.as_ref(),
            );
            let agg_stats = agg_state.engine.stats();
            info!(
                "Aggregate stats: {} blobs created, {} files aggregated",
                agg_stats.blobs_created, agg_stats.files_aggregated
            );
        }

        run_local_followup_phases(
            phase_flags,
            &ctrl_dir,
            &meta_dir,
            &source_dir_base,
            &target_dir_base,
            retry_policy,
            failure_recorder.as_ref(),
        );

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}

fn produce_local_copy_jobs(
    control_file: &Path,
    meta_dir: &Path,
    source_dir_base: &Path,
    target_dir_base: &Path,
    job_tx: &mpsc::SyncSender<FileCopyPlan>,
    stats: &Arc<BackupStats>,
    retry_policy: RetryPolicy,
    failure_recorder: Option<&FailureRecorder>,
    aggregate_state: Option<&Arc<LocalAggregateState>>,
) -> io::Result<()> {
    let mut send_error = None;

    produce_local_copy_plan(
        control_file.to_path_buf(),
        meta_dir.to_path_buf(),
        source_dir_base.to_path_buf(),
        target_dir_base.to_path_buf(),
        |meta| {
            aggregate_state
                .map(|agg_state| {
                    meta.common.symlink_target_path.is_none()
                        && agg_state.engine.should_aggregate(meta.size)
                })
                .unwrap_or(false)
        },
        |entry| match execute_local_plan_entry(entry, stats, job_tx, retry_policy, failure_recorder)
        {
            Ok(keep_going) => keep_going,
            Err(e) => {
                send_error = Some(e);
                false
            }
        },
    );

    if let Some(e) = send_error {
        Err(e)
    } else {
        Ok(())
    }
}
