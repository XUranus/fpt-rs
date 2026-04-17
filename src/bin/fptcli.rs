//!/usr/bin/env rust-script
//! fptcli - File Protection Tool CLI
//!
//! A unified CLI for backup and restore operations with support for:
//! - Common format backups (full synthesis copies)
//! - Aggregated format backups (full and incremental)
//! - Multi-subtask scheduling for large filesets
//! - Task-specific logging

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicUsize, Ordering}},
    thread,
    time::Duration,
};
use clap::{Parser, Subcommand, ValueEnum};
use log::{info, error, LevelFilter};
use uuid::Uuid;

use bifrost::scanner::{Scanner, options::ScanOption};
use bifrost::backup::{self, BackupOption, RestoreOption, RestorePolicy};

/// File Protection Tool CLI
#[derive(Parser, Debug)]
#[command(name = "fptcli")]
#[command(about = "File Protection Tool - Backup and Restore CLI")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a backup copy
    Backup {
        /// Source data directory to backup
        #[arg(long, short = 'd', required = true, value_name = "DIR")]
        data: PathBuf,

        /// Target directory where the copy will be created (will create COPY_* folder)
        #[arg(long, short = 't', required = true, value_name = "DIR")]
        target: PathBuf,

        /// Backup format: common or aggregated
        #[arg(long, short = 'f', value_enum, default_value = "common")]
        format: BackupFormat,

        /// Previous backup copy for incremental (only valid with aggregated format)
        #[arg(long, short = 'i', value_name = "DIR")]
        incremental_base: Option<PathBuf>,

        /// Maximum concurrent subtasks
        #[arg(long, short = 'j', default_value = "4", value_name = "COUNT")]
        jobs: usize,

        /// Aggregate blob size in MB (only for aggregated format)
        #[arg(long, default_value = "64", value_name = "MB")]
        blob_size: u64,

        /// Aggregate file threshold in KB (only for aggregated format)
        #[arg(long, default_value = "1024", value_name = "KB")]
        threshold: u64,

        /// Enable hardlink phase
        #[arg(long, action = clap::ArgAction::SetTrue)]
        hardlink: bool,

        /// Enable delete phase
        #[arg(long, action = clap::ArgAction::SetTrue)]
        delete: bool,

        /// Enable mtime phase
        #[arg(long, action = clap::ArgAction::SetTrue)]
        mtime: bool,

        /// Number of worker threads per subtask
        #[arg(long, short = 'w', default_value = "4", value_name = "COUNT")]
        workers: usize,

        /// Verbose logging
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,
    },

    /// Restore from a backup copy
    Restore {
        /// Source backup copy directory (containing manifest.json and D_REPO, M_REPO, C_REPO)
        #[arg(long, short = 'c', required = true, value_name = "DIR")]
        copy: PathBuf,

        /// Target restore directory
        #[arg(long, short = 't', required = true, value_name = "DIR")]
        target: PathBuf,

        /// Restore policy: replace, skip, or keep-newer
        #[arg(long, short = 'p', value_enum, default_value = "replace")]
        policy: RestorePolicyArg,

        /// Maximum concurrent subtasks
        #[arg(long, short = 'j', default_value = "4", value_name = "COUNT")]
        jobs: usize,

        /// Number of worker threads per subtask
        #[arg(long, short = 'w', default_value = "4", value_name = "COUNT")]
        workers: usize,

        /// Restore hardlinks
        #[arg(long, action = clap::ArgAction::SetTrue)]
        hardlinks: bool,

        /// Restore modification times
        #[arg(long, action = clap::ArgAction::SetTrue, default_value = "true")]
        mtime: bool,

        /// Verbose logging
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum BackupFormat {
    Common,
    Aggregated,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum RestorePolicyArg {
    Replace,
    Skip,
    KeepNewer,
}

impl From<RestorePolicyArg> for RestorePolicy {
    fn from(arg: RestorePolicyArg) -> Self {
        match arg {
            RestorePolicyArg::Replace => RestorePolicy::Replace,
            RestorePolicyArg::Skip => RestorePolicy::Skip,
            RestorePolicyArg::KeepNewer => RestorePolicy::KeepNewer,
        }
    }
}

/// Copy type for naming
#[derive(Debug, Clone, Copy)]
enum CopyType {
    Full,
    Incremental,
}

impl CopyType {
    fn as_str(&self) -> &'static str {
        match self {
            CopyType::Full => "FULL",
            CopyType::Incremental => "INC",
        }
    }
}

/// Format abbreviation for naming
fn format_abbr(format: BackupFormat) -> &'static str {
    match format {
        BackupFormat::Common => "COMMON",
        BackupFormat::Aggregated => "AGGR",
    }
}

/// Backup Copy structure containing D_REPO, M_REPO, and C_REPO
struct BackupCopy {
    copy_path: PathBuf,
    copy_uuid: String,
    d_repo: PathBuf,  // Data repository
    m_repo: PathBuf,  // Metadata repository
    c_repo: PathBuf,  // Control/logs repository
}

impl BackupCopy {
    fn new(copy_path: PathBuf, copy_uuid: String) -> Self {
        let d_repo = copy_path.join("D_REPO");
        let m_repo = copy_path.join("M_REPO");
        let c_repo = copy_path.join("C_REPO");
        Self { copy_path, copy_uuid, d_repo, m_repo, c_repo }
    }

    fn create_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.d_repo)?;
        std::fs::create_dir_all(&self.m_repo)?;
        std::fs::create_dir_all(&self.c_repo)?;
        std::fs::create_dir_all(&self.c_repo.join("ctrl"))?;
        std::fs::create_dir_all(&self.c_repo.join("logs"))?;
        std::fs::create_dir_all(&self.c_repo.join("status"))?;
        Ok(())
    }

    /// Create a status file for tracking task state
    fn create_status_file(&self, name: &str) -> std::io::Result<()> {
        let status_path = self.c_repo.join("status").join(name);
        std::fs::File::create(status_path)?;
        Ok(())
    }

    /// Remove a status file
    fn remove_status_file(&self, name: &str) -> std::io::Result<()> {
        let status_path = self.c_repo.join("status").join(name);
        if status_path.exists() {
            std::fs::remove_file(status_path)?;
        }
        Ok(())
    }

    /// Check if a status file exists
    fn has_status_file(&self, name: &str) -> bool {
        self.c_repo.join("status").join(name).exists()
    }

    fn exists(&self) -> bool {
        self.copy_path.exists()
    }

    fn write_manifest(&self, manifest: &BackupManifest) -> std::io::Result<()> {
        let manifest_path = self.copy_path.join("manifest.json");
        let content = serde_json::to_string_pretty(manifest)?;
        std::fs::write(manifest_path, content)
    }

    fn read_manifest(&self) -> Option<BackupManifest> {
        let manifest_path = self.copy_path.join("manifest.json");
        if manifest_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                if let Ok(manifest) = serde_json::from_str(&content) {
                    return Some(manifest);
                }
            }
        }
        None
    }

    /// Get relative path from copy root for a given path
    fn relative_path(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.copy_path)
            .unwrap_or(path)
            .to_path_buf()
    }
}

/// Backup manifest stored at copy root
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BackupManifest {
    version: String,
    copy_uuid: String,
    copy_type: String,  // "full" or "incremental"
    format: String,     // "common" or "aggregated"
    source_path: String,
    created_at: String,
    base_copy: Option<String>,  // For incremental copies
    subtasks: Vec<SubtaskInfo>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SubtaskInfo {
    id: String,
    control_file: String,  // Relative path or filename only
    log_file: String,      // Relative path or filename only
}

/// Task scheduler for managing concurrent subtasks
struct TaskScheduler {
    max_concurrent: usize,
    running: Arc<AtomicUsize>,
}

impl TaskScheduler {
    fn new(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            running: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn can_start_new(&self) -> bool {
        self.running.load(Ordering::Relaxed) < self.max_concurrent
    }

    fn start_task(&self) {
        self.running.fetch_add(1, Ordering::Relaxed);
    }

    fn complete_task(&self) {
        self.running.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Initialize global logging to stdout only (for early initialization)
fn init_global_logger(verbose: u8) -> Result<(), fern::InitError> {
    init_global_logger_with_file(verbose, None)
}

/// Initialize global logging.
///
/// When a log file is provided (i.e. during backup/restore), detailed output
/// (INFO and above) is written exclusively to that file. Only WARN and ERROR
/// messages are also echoed to stdout, to keep the terminal clean while
/// progress and summary information is printed via explicit `println!` calls.
///
/// When no log file is given (e.g. for quick CLI sub-commands), all output
/// goes to stdout at the requested verbosity level as before.
fn init_global_logger_with_file(verbose: u8, log_file: Option<&Path>) -> Result<(), fern::InitError> {
    let log_level = match verbose {
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    let formatter = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {} - {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.target(),
                message
            ))
        });

    let dispatch = if let Some(log_path) = log_file {
        // Backup/restore mode: detailed logs go to the file, only warnings and
        // errors are printed to stdout so the terminal stays readable.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| fern::InitError::Io(e))?;

        let stdout_dispatch = fern::Dispatch::new()
            .level(LevelFilter::Warn)   // stdout: WARN and ERROR only
            .chain(std::io::stdout());

        let file_dispatch = fern::Dispatch::new()
            .level(log_level)           // file: INFO/DEBUG/TRACE as requested
            .chain(file);

        formatter
            .chain(stdout_dispatch)
            .chain(file_dispatch)
    } else {
        // No log file: send everything to stdout (used by non-backup sub-commands).
        formatter
            .level(log_level)
            .chain(std::io::stdout())
    };

    dispatch.apply()?;
    Ok(())
}

/// Find all control files in a directory, sorted by priority
/// For backup operations, we only process copy control files
fn find_control_files(ctrl_dir: &Path) -> Vec<(PathBuf, ControlFilePriority)> {
    let mut files = Vec::new();

    // Only process copy control files for backup
    // hardlink, delete, and mtime are handled separately by the backup engine phases
    let copy_file = ctrl_dir.join("copy.txt");
    if copy_file.exists() {
        files.push((copy_file, ControlFilePriority::Copy));
    }

    // Also check for sharded control files
    if let Ok(entries) = std::fs::read_dir(ctrl_dir) {
        for entry in entries {
            if let Ok(entry) = entry {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("copy_") && name_str.ends_with(".txt") {
                    files.push((entry.path(), ControlFilePriority::Copy));
                }
            }
        }
    }

    files
}

/// Find all control files for restore operations
/// Restore processes all 4 phases: copy, hardlink, delete, mtime
fn find_restore_control_files(ctrl_dir: &Path) -> Vec<(PathBuf, ControlFilePriority)> {
    let mut files = Vec::new();

    // Process copy control files (highest priority)
    let copy_file = ctrl_dir.join("copy.txt");
    if copy_file.exists() {
        files.push((copy_file, ControlFilePriority::Copy));
    }

    // Process hardlink control files
    let hardlink_file = ctrl_dir.join("hardlink.txt");
    if hardlink_file.exists() {
        files.push((hardlink_file, ControlFilePriority::Hardlink));
    }

    // Process delete control files
    let delete_file = ctrl_dir.join("delete.txt");
    if delete_file.exists() {
        files.push((delete_file, ControlFilePriority::Delete));
    }

    // Process mtime control files
    let mtime_file = ctrl_dir.join("mtime.txt");
    if mtime_file.exists() {
        files.push((mtime_file, ControlFilePriority::Mtime));
    }

    // Also check for sharded control files
    if let Ok(entries) = std::fs::read_dir(ctrl_dir) {
        for entry in entries {
            if let Ok(entry) = entry {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(".txt") {
                    if name_str.starts_with("copy_") {
                        files.push((entry.path(), ControlFilePriority::Copy));
                    } else if name_str.starts_with("hardlink_") {
                        files.push((entry.path(), ControlFilePriority::Hardlink));
                    } else if name_str.starts_with("delete_") {
                        files.push((entry.path(), ControlFilePriority::Delete));
                    } else if name_str.starts_with("mtime_") {
                        files.push((entry.path(), ControlFilePriority::Mtime));
                    }
                }
            }
        }
    }

    // Sort by priority (Copy < Hardlink < Delete < Mtime)
    files.sort_by_key(|(_, priority)| *priority);

    files
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ControlFilePriority {
    Copy = 0,
    Hardlink = 1,
    Delete = 2,
    Mtime = 3,
}

/// Execute a single backup subtask
fn execute_backup_subtask(
    subtask_id: String,
    source_dir: PathBuf,
    copy: &BackupCopy,
    control_file: PathBuf,
    format: BackupFormat,
    workers: usize,
    enable_hardlink: bool,
    enable_delete: bool,
    enable_mtime: bool,
    blob_size: u64,
    threshold: u64,
    log_file: PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Detailed subtask output goes to the per-subtask log file only.
    // The terminal receives only the final summary line printed by the caller.
    let mut log_writer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)?;

    writeln!(log_writer, "[{}] Starting backup subtask", subtask_id)?;
    writeln!(log_writer, "[{}] Control file: {}", subtask_id, control_file.display())?;
    writeln!(log_writer, "[{}] Format: {:?}", subtask_id, format)?;

    // Create backup option
    // Meta files are in M_REPO/meta/, control files are in C_REPO/
    let meta_dir = copy.m_repo.join("meta");
    let ctrl_dir = copy.c_repo.clone();

    let mut backup_option = BackupOption::new(
        source_dir.clone(),
        copy.d_repo.clone(),
        meta_dir,
        ctrl_dir,
        control_file,
    )
    .enable_hardlink_phase(enable_hardlink)
    .enable_delete_phase(enable_delete)
    .enable_mtime_phase(enable_mtime);

    // Configure aggregation for aggregated format
    if matches!(format, BackupFormat::Aggregated) {
        let max_blob_size = blob_size * 1024 * 1024; // Convert MB to bytes
        let aggregate_threshold = threshold * 1024; // Convert KB to bytes
        backup_option = backup_option
            .enable_aggregation(true)
            .aggregate_max_blob_size(max_blob_size)
            .aggregate_file_threshold(aggregate_threshold);
        writeln!(log_writer, "[{}] Aggregation enabled: blob_size={}MB, threshold={}KB", subtask_id, blob_size, threshold)?;
    }

    let backup_task: backup::BackupTask = backup_option.into();

    let running_backup = backup_task.start()?;

    // Poll until done; write periodic progress to the log file only.
    loop {
        let stats = running_backup.stats();
        writeln!(
            log_writer,
            "[{}] Progress: {} files ({} bytes), {} dirs, {} failed",
            subtask_id,
            stats.files_copied,
            stats.bytes_copied,
            stats.dirs_created,
            stats.files_failed
        )?;

        if running_backup.complete() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }

    let final_stats = running_backup.stats();
    running_backup.wait()?;

    writeln!(
        log_writer,
        "[{}] Subtask completed: {} files ({} MB), {} dirs",
        subtask_id,
        final_stats.files_copied,
        final_stats.bytes_copied / (1024 * 1024),
        final_stats.dirs_created
    )?;

    if final_stats.files_failed > 0 {
        return Err(format!("Subtask {} failed: {} files failed", subtask_id, final_stats.files_failed).into());
    }

    Ok(())
}

/// Execute backup command
fn cmd_backup(
    data: PathBuf,
    target: PathBuf,
    format: BackupFormat,
    incremental_base: Option<PathBuf>,
    jobs: usize,
    blob_size: u64,
    threshold: u64,
    hardlink: bool,
    delete: bool,
    mtime: bool,
    workers: usize,
    verbose: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate format/incremental combination
    if incremental_base.is_some() && matches!(format, BackupFormat::Common) {
        return Err("Incremental backup is only supported with aggregated format".into());
    }

    // Generate copy UUID
    let copy_uuid = Uuid::new_v4().to_string();

    // Determine copy type
    let is_incremental = incremental_base.is_some();
    let copy_type = if is_incremental { CopyType::Incremental } else { CopyType::Full };
    let copy_type_str = copy_type.as_str();

    // Build copy folder name: COPY_{format}_{type}_{uuid}
    let copy_folder_name = format!("COPY_{}_{}_{}", format_abbr(format), copy_type_str, copy_uuid);
    let copy_path = target.join(&copy_folder_name);

    // Create backup copy structure
    let copy = BackupCopy::new(copy_path.clone(), copy_uuid.clone());

    println!("Creating {} backup copy at: {}", copy_type_str, copy_path.display());
    println!("Source: {}", data.display());
    println!("Format: {:?}", format);
    println!("Copy UUID: {}", copy_uuid);

    // Create directories
    copy.create_dirs()?;

    // Initialize global logger to write to both stdout and C_REPO/logs/backup.log
    let main_log_path = copy.c_repo.join("logs").join("backup.log");
    init_global_logger_with_file(verbose, Some(&main_log_path))?;
    info!("Starting backup copy {}", copy_uuid);

    // Create SCAN.RUNNING status file
    copy.create_status_file(&format!("SCAN_{}.RUNNING", copy_uuid))?;

    // Create scan log file (for scan-specific output, file only)
    let scan_log_path = copy.c_repo.join("logs").join("scan.log");
    let mut scan_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&scan_log_path)?;

    // Create temp scan directories within M_REPO
    let scan_ctrl_dir = copy.c_repo.join("ctrl");
    let scan_meta_dir = copy.m_repo.join("meta");
    std::fs::create_dir_all(&scan_ctrl_dir)?;
    std::fs::create_dir_all(&scan_meta_dir)?;

    // Determine previous meta for incremental scan
    let prev_meta_dir = if let Some(ref base) = incremental_base {
        let base_copy = BackupCopy::new(base.clone(), String::new());
        if !base_copy.exists() {
            return Err(format!("Base copy does not exist: {}", base.display()).into());
        }
        Some(base_copy.m_repo.join("meta"))
    } else {
        None
    };

    // Step 1: Scan the source
    writeln!(scan_log, "[SCAN] Starting scan for copy {}", copy_uuid)?;
    writeln!(scan_log, "[SCAN] Source: {}", data.display())?;
    println!("\n[1/3] Scanning source directory...");
    let scan_option = ScanOption::new(
        scan_ctrl_dir.clone(),
        scan_meta_dir.clone(),
    )
    .worker_count(workers)
    .writer_count(1)
    .prev_meta_dir(prev_meta_dir);

    let mut scanner = Scanner::new(scan_option);
    scanner.enqueue_path(data.clone())?;

    let running_scan = scanner.start()?;

    loop {
        let stats = running_scan.stats();
        print!("\r  Files: {}, Dirs: {}, Size: {:.2} MB", 
            stats.tot_files, stats.tot_dirs, stats.tot_size as f64 / (1024.0 * 1024.0));
        std::io::Write::flush(&mut std::io::stdout())?;

        if running_scan.complete() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    println!();

    let scan_stats = running_scan.stats();
    running_scan.wait();

    writeln!(scan_log, "[SCAN] Scan complete: {} files, {} dirs", scan_stats.tot_files, scan_stats.tot_dirs)?;
    println!("  Scan complete: {} files, {} dirs", scan_stats.tot_files, scan_stats.tot_dirs);
    
    // Update scan status: remove RUNNING, create DONE
    copy.remove_status_file(&format!("SCAN_{}.RUNNING", copy_uuid))?;
    copy.create_status_file(&format!("SCAN_{}.DONE", copy_uuid))?;

    // Step 2: Find control files and schedule backup subtasks
    println!("\n[2/3] Scheduling backup subtasks...");
    let control_files = find_control_files(&scan_ctrl_dir);
    println!("  Found {} control files", control_files.len());

    let scheduler = TaskScheduler::new(jobs);
    let mut subtask_handles = Vec::new();
    let mut subtask_infos = Vec::new();

    for (ctrl_file, priority) in control_files {
        // Each subtask gets its own UUID
        let subtask_id = Uuid::new_v4().to_string();
        
        // Log file goes to C_REPO/logs/ with subtask UUID as filename
        let log_file_name = format!("{}.log", subtask_id);
        let log_file = copy.c_repo.join("logs").join(&log_file_name);

        // Control file relative path (just the filename, stored in C_REPO/ctrl/)
        let ctrl_file_name = ctrl_file.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown.txt".to_string());

        let info = SubtaskInfo {
            id: subtask_id.clone(),
            control_file: format!("C_REPO/ctrl/{}", ctrl_file_name),
            log_file: format!("C_REPO/logs/{}", log_file_name),
        };
        subtask_infos.push(info);

        // Create SUBTASK_{uuid}.RUNNING status file
        copy.create_status_file(&format!("SUBTASK_{}.RUNNING", subtask_id))?;

        println!("  Subtask {}: {:?} -> {}", subtask_id, priority, ctrl_file.display());

        // Wait if we've reached max concurrent tasks
        while !scheduler.can_start_new() {
            thread::sleep(Duration::from_millis(100));
        }

        scheduler.start_task();

        let subtask_id_clone = subtask_id.clone();
        let source_dir = data.clone();
        let copy_d_repo = copy.d_repo.clone();
        let copy_m_repo = copy.m_repo.clone();
        let copy_c_repo = copy.c_repo.clone();
        let copy_uuid_clone = copy_uuid.clone();
        let ctrl_file_clone = ctrl_file.clone();
        let format_clone = format;
        let priority_clone = priority;

        // Enable phases based on command-line flags and format
        // For aggregated format, only copy phase is used (hardlink/delete/mtime are ignored)
        // For common format, all phases can be enabled via flags
        let is_aggregated = matches!(format_clone, BackupFormat::Aggregated);
        let enable_hardlink_phase = hardlink && !is_aggregated;
        let enable_delete_phase = delete && !is_aggregated;
        let enable_mtime_phase = mtime && !is_aggregated;

        if is_aggregated && (hardlink || delete || mtime) {
            println!("[{}] Note: hardlink/delete/mtime phases are ignored for aggregated format", subtask_id_clone);
        }

        let handle = thread::spawn(move || {
            // Reconstruct BackupCopy for this thread
            let thread_copy_path = copy_d_repo.parent().unwrap().to_path_buf();
            let thread_copy = BackupCopy::new(thread_copy_path, copy_uuid_clone);
            
            let result = execute_backup_subtask(
                subtask_id_clone.clone(),
                source_dir,
                &thread_copy,
                ctrl_file_clone,
                format_clone,
                workers,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
                blob_size,
                threshold,
                log_file,
            );

            let success = result.is_ok();
            if let Err(ref e) = result {
                eprintln!("[{}] Subtask failed: {}", subtask_id_clone, e);
            }

            // Update status files based on result
            let _ = thread_copy.remove_status_file(&format!("SUBTASK_{}.RUNNING", subtask_id_clone));
            if success {
                let _ = thread_copy.create_status_file(&format!("SUBTASK_{}.DONE", subtask_id_clone));
            } else {
                let _ = thread_copy.create_status_file(&format!("SUBTASK_{}.FAILED", subtask_id_clone));
            }

            success
        });

        subtask_handles.push((subtask_id, handle));
    }

    // Wait for all subtasks to complete
    println!("\n[3/3] Executing backup subtasks...");
    let mut completed = 0;
    let mut failed = 0;

    for (subtask_id, handle) in subtask_handles {
        match handle.join() {
            Ok(true) => {
                completed += 1;
                println!("  ✓ Subtask {} completed", subtask_id);
            }
            Ok(false) => {
                failed += 1;
                println!("  ✗ Subtask {} failed", subtask_id);
                // Create FAILED status file (in case thread didn't)
                let _ = copy.create_status_file(&format!("SUBTASK_{}.FAILED", subtask_id));
            }
            Err(e) => {
                failed += 1;
                println!("  ✗ Subtask {} panicked: {:?}", subtask_id, e);
                let _ = copy.create_status_file(&format!("SUBTASK_{}.FAILED", subtask_id));
            }
        }
    }

    // Step 4: Write manifest
    let manifest = BackupManifest {
        version: "1.0".to_string(),
        copy_uuid: copy_uuid.clone(),
        copy_type: copy_type_str.to_lowercase(),
        format: format_to_string(format),
        source_path: data.to_string_lossy().to_string(),
        created_at: chrono::Local::now().to_rfc3339(),
        base_copy: incremental_base.map(|p| p.to_string_lossy().to_string()),
        subtasks: subtask_infos,
    };

    copy.write_manifest(&manifest)?;

    // Summary
    println!("\n{}", "=".repeat(60));
    println!("Backup Summary");
    println!("{}", "=".repeat(60));
    println!("Copy UUID: {}", copy_uuid);
    println!("Copy type: {}", copy_type_str);
    println!("Format: {:?}", format);
    println!("Target: {}", copy_path.display());
    println!("Subtasks: {} completed, {} failed", completed, failed);
    println!("Manifest: {}", copy.copy_path.join("manifest.json").display());

    if failed > 0 {
        println!("\nWarning: Some subtasks failed. Check log files in {}/ for details.", copy.c_repo.display());
        return Err("Backup completed with failures".into());
    }

    println!("\nBackup completed successfully!");
    Ok(())
}

/// Execute restore command
fn cmd_restore(
    copy_path: PathBuf,
    target: PathBuf,
    policy: RestorePolicy,
    jobs: usize,
    workers: usize,
    hardlinks: bool,
    mtime: bool,
    verbose: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Restoring from backup copy: {}", copy_path.display());
    println!("Target: {}", target.display());
    println!("Policy: {:?}", policy);

    // Read manifest first to get UUID
    let manifest_path = copy_path.join("manifest.json");
    if !manifest_path.exists() {
        return Err(format!("Backup manifest not found: {}", manifest_path.display()).into());
    }

    let manifest_content = std::fs::read_to_string(&manifest_path)?;
    let manifest: BackupManifest = serde_json::from_str(&manifest_content)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    let copy_uuid = manifest.copy_uuid.clone();
    let copy = BackupCopy::new(copy_path.clone(), copy_uuid);

    if !copy.exists() {
        return Err(format!("Backup copy does not exist: {}", copy_path.display()).into());
    }

    println!("\nBackup info:");
    println!("  Copy UUID: {}", manifest.copy_uuid);
    println!("  Type: {}", manifest.copy_type);
    println!("  Format: {}", manifest.format);
    println!("  Created: {}", manifest.created_at);
    println!("  Source: {}", manifest.source_path);

    // Handle incremental copy chain
    if manifest.copy_type == "incremental" {
        println!("\nNote: This is an incremental copy. Full restore requires the copy chain.");
        if let Some(ref base) = manifest.base_copy {
            println!("  Base copy: {}", base);
        }
    }

    // Create target directory
    std::fs::create_dir_all(&target)?;

    // Find control files for restore
    // Restore processes all 4 phases: copy, hardlink, delete, mtime
    let ctrl_dir = copy.c_repo.join("ctrl");
    let control_files = find_restore_control_files(&ctrl_dir);

    if control_files.is_empty() {
        return Err("No control files found for restore".into());
    }

    println!("\nRestoring {} control files...", control_files.len());

    let scheduler = TaskScheduler::new(jobs);
    let mut subtask_handles = Vec::new();

    for (ctrl_file, priority) in control_files {
        // Each restore subtask gets its own UUID
        let subtask_id = Uuid::new_v4().to_string();

        println!("  Subtask {}: {:?} -> {}", subtask_id, priority, ctrl_file.display());

        while !scheduler.can_start_new() {
            thread::sleep(Duration::from_millis(100));
        }

        scheduler.start_task();

        let subtask_id_clone = subtask_id.clone();
        let copy_d_repo = copy.d_repo.clone();
        let copy_m_repo = copy.m_repo.clone();
        let copy_c_repo = copy.c_repo.clone();
        let copy_uuid_clone = copy.copy_uuid.clone();
        let target_dir = target.clone();
        let ctrl_file_clone = ctrl_file.clone();
        let policy_clone = policy;

        let handle = thread::spawn(move || {
            // Reconstruct BackupCopy for this thread
            let thread_copy_path = copy_d_repo.parent().unwrap().to_path_buf();
            let thread_copy = BackupCopy::new(thread_copy_path, copy_uuid_clone);
            
            let result = execute_restore_subtask(
                subtask_id_clone.clone(),
                thread_copy,
                target_dir,
                ctrl_file_clone,
                policy_clone,
                workers,
                hardlinks,
                mtime,
                verbose,
            );

            if let Err(ref e) = result {
                error!("Restore subtask {} failed: {}", subtask_id_clone, e);
            }

            result.is_ok()
        });

        subtask_handles.push((subtask_id, handle));
    }

    // Wait for all restore subtasks
    println!("\nExecuting restore subtasks...");
    let mut completed = 0;
    let mut failed = 0;

    for (subtask_id, handle) in subtask_handles {
        match handle.join() {
            Ok(true) => {
                completed += 1;
                println!("  ✓ Subtask {} completed", subtask_id);
            }
            Ok(false) => {
                failed += 1;
                println!("  ✗ Subtask {} failed", subtask_id);
            }
            Err(e) => {
                failed += 1;
                println!("  ✗ Subtask {} panicked: {:?}", subtask_id, e);
            }
        }
    }

    // Summary
    println!("\n{}", "=".repeat(60));
    println!("Restore Summary");
    println!("{}", "=".repeat(60));
    println!("Source: {}", copy_path.display());
    println!("Target: {}", target.display());
    println!("Subtasks: {} completed, {} failed", completed, failed);

    if failed > 0 {
        return Err("Restore completed with failures".into());
    }

    println!("\nRestore completed successfully!");
    Ok(())
}

/// Execute a single restore subtask
fn execute_restore_subtask(
    subtask_id: String,
    copy: BackupCopy,
    target_dir: PathBuf,
    control_file: PathBuf,
    policy: RestorePolicy,
    workers: usize,
    _hardlinks: bool,
    _mtime: bool,
    _verbose: u8,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("[{}] Starting restore subtask", subtask_id);
    println!("[{}] Control file: {}", subtask_id, control_file.display());

    // Create restore option
    // Meta files are in M_REPO/meta/, control files are in C_REPO/ctrl/
    let meta_dir = copy.m_repo.join("meta");
    let ctrl_dir = copy.c_repo.join("ctrl");
    let restore_option = RestoreOption::new(
        copy.d_repo.clone(),
        target_dir,
        meta_dir,
        ctrl_dir,
        control_file,
    )
    .policy(policy)
    .worker_count(workers)
    .restore_hardlinks(false) // TODO: Implement hardlink restore
    .restore_mtime(true);

    let restore_task = backup::RestoreTask::new(restore_option);

    let running_restore = restore_task.start()?;

    // Monitor progress
    loop {
        let stats = running_restore.stats();
        println!(
            "[{}] Progress: {} restored, {} skipped, {} failed",
            subtask_id,
            stats.files_restored,
            stats.files_skipped,
            stats.files_failed
        );

        if running_restore.complete() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }

    let final_stats = running_restore.stats();
    running_restore.wait()?;

    println!(
        "[{}] Subtask completed: {} files restored, {} skipped, {} failed",
        subtask_id,
        final_stats.files_restored,
        final_stats.files_skipped,
        final_stats.files_failed
    );

    if final_stats.files_failed > 0 {
        return Err(format!("Subtask {} failed: {} files failed", subtask_id, final_stats.files_failed).into());
    }

    Ok(())
}

fn format_to_string(format: BackupFormat) -> String {
    match format {
        BackupFormat::Common => "common".to_string(),
        BackupFormat::Aggregated => "aggregated".to_string(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // === File Descriptor Limit Initialization ===
    //
    // Bifrost's aggregate backup engine opens many file descriptors simultaneously:
    //   - Source file handles held open by reader I/O threads while FCBs are in-flight
    //   - Target blob files being written
    //   - SQLite database connections (one per-operation, but they overlap under concurrency)
    //   - Standard fds (stdin/stdout/stderr), log file, metadata files, etc.
    //
    // Linux's default soft limit is 1024 fds per process. When backing up large datasets
    // with many small files across many directories, this limit is easily exceeded, causing
    // EMFILE ("Too many open files", os error 24) errors.
    //
    // A process is always allowed to raise its own soft limit up to the hard limit without
    // any special privileges. On typical Linux systems the hard limit is 524288 (512K).
    // We raise the soft limit to the hard limit here, at process startup, before any I/O
    // threads are created, so the full fd budget is available throughout the run.
    #[cfg(unix)]
    {
        use nix::sys::resource::{getrlimit, setrlimit, Resource};
        match getrlimit(Resource::RLIMIT_NOFILE) {
            Ok((soft, hard)) => {
                if soft < hard {
                    if let Err(e) = setrlimit(Resource::RLIMIT_NOFILE, hard, hard) {
                        eprintln!("Warning: failed to raise fd limit from {} to {}: {}", soft, hard, e);
                    }
                    // soft == hard after this point; no further action needed
                }
                // If soft == hard already, the limit is already maximized
            }
            Err(e) => {
                // Non-fatal: backup will proceed but may hit fd exhaustion on large datasets
                eprintln!("Warning: failed to query fd limit: {}", e);
            }
        }
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Backup {
            data,
            target,
            format,
            incremental_base,
            jobs,
            blob_size,
            threshold,
            hardlink,
            delete,
            mtime,
            workers,
            verbose,
        } => {
            // For backup, we need to initialize logger after creating copy structure
            // So we pass verbose to cmd_backup and let it initialize the logger
            cmd_backup(
                data,
                target,
                format,
                incremental_base,
                jobs,
                blob_size,
                threshold,
                hardlink,
                delete,
                mtime,
                workers,
                verbose,
            )
        }
        Commands::Restore {
            copy,
            target,
            policy,
            jobs,
            workers,
            hardlinks,
            mtime,
            verbose,
        } => {
            // Initialize global logger for restore (no file logging needed)
            init_global_logger(verbose)?;
            cmd_restore(
                copy,
                target,
                policy.into(),
                jobs,
                workers,
                hardlinks,
                mtime,
                verbose,
            )
        }
    }
}
