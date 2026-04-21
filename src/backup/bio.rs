use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use log::info;

use crate::backup::aggregate::AggregateConfig;
use crate::backup::aggregate_engine;
use crate::backup::fcb::ControlBlockVarient;
use crate::backup::stats::BackupStats;
use crate::backup::{
    bio::copy::{ReaderBioResult, ReaderBioTask, WriterBioResult, WriterBioTask},
    SharedState,
};

pub mod copy;
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
    shared_state: Arc<SharedState>,
    stats: Arc<BackupStats>,
    terminate_indicator: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let enable_aggregation = aggregate_config.enabled;
    let io_capacity = worker_count.max(1) * 2;
    let fcb_capacity = worker_count.max(1) * 4;

    if !enable_aggregation {
        return local_copy::spawn_local_common_copy_pipeline(
            control_file,
            source_dir_base,
            target_dir_base,
            meta_dir,
            ctrl_dir,
            worker_count,
            copy_buffer_size,
            enable_hardlink_phase,
            enable_delete_phase,
            enable_mtime_phase,
            stats,
            terminate_indicator,
        );
    }

    thread::spawn(move || {
        let aggregate_engine = if enable_aggregation {
            info!(
                "Aggregation enabled: max_blob_size={}, file_threshold={}",
                aggregate_config.max_blob_size, aggregate_config.file_threshold
            );

            match aggregate_engine::AggregateBackupEngine::new(
                aggregate_config,
                source_dir_base.clone(),
                target_dir_base.clone(),
            ) {
                Ok(engine) => Some(Arc::new(engine)),
                Err(e) => {
                    eprintln!(
                        "Failed to create aggregate engine: {}. Continuing without aggregation.",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        let (fcb_reader_tx, fcb_reader_rx) =
            mpsc::sync_channel::<ControlBlockVarient>(fcb_capacity);
        let (fcb_writer_tx, fcb_writer_rx) =
            mpsc::sync_channel::<ControlBlockVarient>(fcb_capacity);
        let (reader_io_task_tx, reader_io_task_rx) =
            mpsc::sync_channel::<ReaderBioTask>(io_capacity);
        let (reader_io_result_tx, reader_io_result_rx) =
            mpsc::sync_channel::<ReaderBioResult>(io_capacity);
        let (writer_io_task_tx, writer_io_task_rx) =
            mpsc::sync_channel::<WriterBioTask>(io_capacity);
        let (writer_io_result_tx, writer_io_result_rx) = mpsc::channel::<WriterBioResult>();

        let reader_io_task_rx = Arc::new(Mutex::new(reader_io_task_rx));
        let writer_io_task_rx = Arc::new(Mutex::new(writer_io_task_rx));

        let entry_producer_handle = copy::spawn_file_entry_producer(
            control_file,
            meta_dir.clone(),
            source_dir_base.clone(),
            target_dir_base.clone(),
            fcb_reader_tx.clone(),
            Arc::clone(&shared_state),
        );

        let reader_handle = if let Some(ref engine) = aggregate_engine {
            copy::spawn_reader_with_aggregation(
                fcb_reader_rx,
                reader_io_task_tx.clone(),
                fcb_writer_tx.clone(),
                Arc::clone(&shared_state),
                Arc::clone(engine),
                Arc::clone(&stats),
            )
        } else {
            copy::spawn_reader(
                fcb_reader_rx,
                reader_io_task_tx.clone(),
                fcb_writer_tx.clone(),
                Arc::clone(&shared_state),
            )
        };

        let reader_io_pool = copy::spawn_reader_io_pool(
            Arc::clone(&reader_io_task_rx),
            reader_io_result_tx.clone(),
            worker_count,
            Arc::clone(&shared_state),
        );

        let reader_io_result_poll = if let Some(ref engine) = aggregate_engine {
            copy::spawn_reader_io_result_poll_with_aggregation(
                reader_io_result_rx,
                fcb_reader_tx.clone(),
                fcb_writer_tx.clone(),
                Arc::clone(&stats),
                Arc::clone(engine),
            )
        } else {
            copy::spawn_reader_io_result_poll(
                reader_io_result_rx,
                fcb_reader_tx.clone(),
                fcb_writer_tx.clone(),
                Arc::clone(&stats),
            )
        };

        let writer_handle = copy::spawn_writer(
            fcb_writer_rx,
            writer_io_task_tx.clone(),
            Arc::clone(&shared_state),
            Arc::clone(&stats),
        );
        let writer_io_pool = copy::spawn_writer_io_pool(
            writer_io_task_rx,
            writer_io_result_tx.clone(),
            worker_count,
            Arc::clone(&shared_state),
        );
        let writer_io_result_poll = copy::spawn_writer_io_result_poll(
            writer_io_result_rx,
            fcb_writer_tx.clone(),
            Arc::clone(&stats),
        );

        entry_producer_handle.join().unwrap();
        reader_handle.join().unwrap();

        // The parent thread still owns these senders. Drop them before joining
        // workers/pollers, otherwise those threads can block forever in recv()
        // even after the pipeline is logically complete.
        drop(reader_io_task_tx);
        for handle in reader_io_pool {
            handle.join().unwrap();
        }
        drop(reader_io_result_tx);
        reader_io_result_poll.join().unwrap();
        drop(fcb_reader_tx);

        writer_handle.join().unwrap();
        drop(writer_io_task_tx);
        for handle in writer_io_pool {
            handle.join().unwrap();
        }
        drop(writer_io_result_tx);
        writer_io_result_poll.join().unwrap();
        drop(fcb_writer_tx);

        if let Some(ref engine) = aggregate_engine {
            info!("Flushing aggregate buffers...");
            let agg_stats = engine.stats();
            info!(
                "Aggregate stats: {} blobs created, {} files aggregated",
                agg_stats.blobs_created, agg_stats.files_aggregated
            );
        }

        if enable_hardlink_phase {
            info!("Starting hardlink phase...");
            match hardlink::run_hardlink_phase(
                &ctrl_dir,
                &meta_dir,
                &source_dir_base,
                &target_dir_base,
            ) {
                Ok(hl_stats) => {
                    info!(
                        "Hardlink phase completed: {} created, {} failed",
                        hl_stats.hardlinks_created, hl_stats.hardlinks_failed
                    );
                }
                Err(e) => {
                    eprintln!("Hardlink phase failed: {}", e);
                }
            }
        }

        if enable_delete_phase {
            info!("Starting delete phase...");
            match delete::run_delete_phase(&ctrl_dir, &source_dir_base, &target_dir_base) {
                Ok(del_stats) => {
                    info!(
                        "Delete phase completed: {} files deleted, {} dirs deleted",
                        del_stats.files_deleted, del_stats.dirs_deleted
                    );
                }
                Err(e) => {
                    eprintln!("Delete phase failed: {}", e);
                }
            }
        }

        if enable_mtime_phase {
            info!("Starting mtime phase...");
            match mtime::run_mtime_phase(&ctrl_dir, &source_dir_base, &target_dir_base) {
                Ok(mt_stats) => {
                    info!(
                        "Mtime phase completed: {} restored, {} failed",
                        mt_stats.dirs_restored, mt_stats.dirs_failed
                    );
                }
                Err(e) => {
                    eprintln!("Mtime phase failed: {}", e);
                }
            }
        }

        terminate_indicator.store(true, Ordering::Relaxed);
    })
}
