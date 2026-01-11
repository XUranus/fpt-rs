use std::{path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU32, Ordering}, mpsc}, thread};
use crate::backup::{
        bio::copy::{self, ReaderBioResult, ReaderBioTask, WriterBioResult, WriterBioTask}, fcb::{ControlBlockVarient, FileControlBlock}, stats::{BackupStats, BackupStatsSnapshot}
    };

mod fcb;
mod bio;
mod stats;

pub struct BackupOption {
    /// source location path prefix
    source_dir_base : PathBuf,
    /// target location path prefix
    target_dir_base : PathBuf,

    meta_dir : PathBuf,
    // path for control file
    control_file : PathBuf,

    worker_count : usize
}



// each backup task do the data copy following the instruction of one control file
pub struct BackupTask {
    option : BackupOption,
}

pub struct RunningBackup {
    option : BackupOption,
    stats : Arc<BackupStats>,
    terminate_handle : thread::JoinHandle<()>,
    terminate_indicator : Arc<AtomicBool>
}

#[derive(Debug)]
pub enum BackupError {
    InvalidMetaPath,
    InvalidControlFile,
    InsuffientDiskSpace,
}

impl BackupOption {
    pub fn new(source_dir_base : PathBuf, target_dir_base : PathBuf, meta_dir : PathBuf, control_file : PathBuf) -> Self {
        Self { worker_count : 4, source_dir_base, target_dir_base, meta_dir, control_file }
    }
}

struct SharedState {
    pub entry_produce_done : AtomicBool,
    pub reader_done : AtomicBool,
    pub writer_done : AtomicBool,
    pub active_reader_io_workers : AtomicU32,
    pub active_writer_io_workers : AtomicU32
}

impl Default for SharedState {
    fn default() -> Self {
        SharedState {
            entry_produce_done : AtomicBool::new(false),
            reader_done : AtomicBool::new(false),
            writer_done : AtomicBool::new(false),
            active_reader_io_workers : AtomicU32::new(0),
            active_writer_io_workers : AtomicU32::new(0),
        }
    }
}

impl BackupTask {
    pub fn start(self) -> Result<RunningBackup, BackupError> {
        let worker_count = self.option.worker_count;
        let control_file = self.option.control_file.clone();
        let source_dir_base = self.option.source_dir_base.clone();
        let target_dir_base = self.option.target_dir_base.clone();
        let meta_dir = self.option.meta_dir.clone();
        let stats = Arc::new(BackupStats::default());
        let shared_state = Arc::new(SharedState::default());
        let terminate_indicator = Arc::new(AtomicBool::new(false));
        let terminate_indicator_inner = Arc::clone(&terminate_indicator);

        let (fcb_reader_tx, fcb_reader_rx) = mpsc::channel::<ControlBlockVarient>();
        let (fcb_writer_tx, fcb_writer_rx) = mpsc::channel::<ControlBlockVarient>();
        let (reader_io_task_tx, reader_io_task_rx) = mpsc::channel::<ReaderBioTask>();
        let (reader_io_result_tx, reader_io_result_rx) = mpsc::channel::<ReaderBioResult>();
        let (writer_io_task_tx, writer_io_task_rx) = mpsc::channel::<WriterBioTask>();
        let (writer_io_result_tx, writer_io_result_rx) = mpsc::channel::<WriterBioResult>();

        let reader_io_task_rx = Arc::new(Mutex::new(reader_io_task_rx));
        let writer_io_task_rx = Arc::new( Mutex::new(writer_io_task_rx));
    
        let entry_producer_handle = copy::spawn_file_entry_producer(control_file, meta_dir, source_dir_base, target_dir_base, fcb_reader_tx.clone(), Arc::clone(&shared_state));

        let reader_handle = copy::spawn_reader(fcb_reader_rx, reader_io_task_tx, Arc::clone(&shared_state));
        let reader_io_pool = copy::spawn_reader_io_pool(Arc::clone(&reader_io_task_rx), reader_io_result_tx, worker_count, Arc::clone(&shared_state));
        let reader_io_result_poll = copy::spawn_reader_io_result_poll(reader_io_result_rx, fcb_reader_tx, fcb_writer_tx.clone(), Arc::clone(&stats));

        let writer_handle = copy::spawn_writer(fcb_writer_rx, writer_io_task_tx, Arc::clone(&shared_state));
        let writer_io_pool = copy::spawn_writer_io_pool(writer_io_task_rx, writer_io_result_tx, worker_count, Arc::clone(&shared_state));
        let writer_io_result_poll = copy::spawn_writer_io_result_poll(writer_io_result_rx, fcb_writer_tx, Arc::clone(&stats));

        let terminate_handle = thread::spawn(move || {
            entry_producer_handle.join().unwrap();
            reader_handle.join().unwrap();
            for handle in reader_io_pool {
                handle.join().unwrap();
            }
            reader_io_result_poll.join().unwrap();

            writer_handle.join().unwrap();
            for handle in writer_io_pool {
                handle.join().unwrap();
            }
            writer_io_result_poll.join().unwrap();
            terminate_indicator_inner.store(true, Ordering::Relaxed);
        });

        Ok(RunningBackup{
            option : self.option,
            stats,
            terminate_handle,
            terminate_indicator
        })
    }

}

impl From<BackupOption> for BackupTask {
    fn from(option: BackupOption) -> Self {
        Self {
            option
        }
    }
}

impl RunningBackup {
    pub fn stats(&self) -> BackupStatsSnapshot {
        self.stats.snapshot()
    }

    pub fn complete(&self) -> bool {
        self.terminate_indicator.load(Ordering::Relaxed)
    }

    pub fn wait(self) -> Result<(), BackupError> {
        self.terminate_handle.join().unwrap();
        Ok(())
    }
}