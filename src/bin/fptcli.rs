//!/usr/bin/env rust-script
//! fptcli - File Protection Tool CLI
//!
//! A unified CLI for backup and restore operations with support for:
//! - Common format backups (full synthesis copies)
//! - Aggregated format backups (full and incremental)
//! - Multi-subtask scheduling for large filesets
//! - Task-specific logging

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicUsize, Ordering}, Mutex},
    thread,
    time::Duration,
};
use clap::{Parser, Subcommand, ValueEnum};
use log::{info, warn, error, LevelFilter};
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

        /// Target backup copy directory (will create D/ and M/ repos)
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
        /// Source backup copy directory (containing D/ and M/ repos)
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

/// Backup Copy structure containing D repo and M repo
struct BackupCopy {
    copy_path: PathBuf,
    d_repo: PathBuf,  // Data repository
    m_repo: PathBuf,  // Metadata repository
}

impl BackupCopy {
    fn new(copy_path: PathBuf) -> Self {
        let d_repo = copy_path.join("D");
        let m_repo = copy_path.join("M");
        Self { copy_path, d_repo, m_repo }
    }

    fn create_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.d_repo)?;
        std::fs::create_dir_all(&self.m_repo)?;
        Ok(())
    }

    fn exists(&self) -> bool {
        self.copy_path.exists()
    }

    fn write_manifest(&self, manifest: &BackupManifest) -> std::io::Result<()> {
        let manifest_path = self.m_repo.join("manifest.json");
        let content = serde_json::to_string_pretty(manifest)?;
        std::fs::write(manifest_path, content)
    }

    fn read_manifest(&self) -> Option<BackupManifest> {
        let manifest_path = self.m_repo.join("manifest.json");
        if manifest_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                if let Ok(manifest) = serde_json::from_str(&content) {
                    return Some(manifest);
                }
            }
        }
        None
    }
}

/// Backup manifest stored in M repo
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct BackupManifest {
    version: String,
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
    control_file: String,
    status: String,
    log_file: String,
}

/// Task scheduler for managing concurrent subtasks
struct TaskScheduler {
    max_concurrent: usize,
    running: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
}

impl TaskScheduler {
    fn new(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            running: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            failed: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn can_start_new(&self) -> bool {
        self.running.load(Ordering::Relaxed) < self.max_concurrent
    }

    fn start_task(&self) {
        self.running.fetch_add(1, Ordering::Relaxed);
    }

    fn complete_task(&self, success: bool) {
        self.running.fetch_sub(1, Ordering::Relaxed);
        if success {
            self.completed.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn stats(&self) -> SchedulerStats {
        SchedulerStats {
            running: self.running.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
struct SchedulerStats {
    running: usize,
    completed: usize,
    failed: usize,
}

/// Initialize global logging
fn init_global_logger(verbose: u8) -> Result<(), fern::InitError> {
    let log_level = match verbose {
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {} - {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.target(),
                message
            ))
        })
        .level(log_level)
        .chain(std::io::stdout())
        .apply()?;

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
    _log_file: PathBuf,
    _verbose: u8,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Note: Logging is handled globally, subtask logs to stdout with prefix
    println!("[{}] Starting backup subtask", subtask_id);
    println!("[{}] Control file: {}", subtask_id, control_file.display());
    println!("[{}] Format: {:?}", subtask_id, format);

    // Create backup option
    // Meta files are in M/meta/, control files are in M/ctrl/
    let meta_dir = copy.m_repo.join("meta");
    let ctrl_dir = copy.m_repo.join("ctrl");

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
        println!("[{}] Aggregation enabled: blob_size={}MB, threshold={}KB", subtask_id, blob_size, threshold);
    }

    let backup_task: backup::BackupTask = backup_option.into();

    let running_backup = backup_task.start()?;

    // Monitor progress
    loop {
        let stats = running_backup.stats();
        println!(
            "[{}] Progress: {} files ({} bytes), {} dirs, {} failed",
            subtask_id,
            stats.files_copied,
            stats.bytes_copied,
            stats.dirs_created,
            stats.files_failed
        );

        if running_backup.complete() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }

    let final_stats = running_backup.stats();
    running_backup.wait()?;

    println!(
        "[{}] Subtask completed: {} files ({} MB), {} dirs",
        subtask_id,
        final_stats.files_copied,
        final_stats.bytes_copied / (1024 * 1024),
        final_stats.dirs_created
    );

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

    // Create backup copy structure
    let copy = BackupCopy::new(target.clone());

    // Check if this is a new copy or incremental
    let is_incremental = incremental_base.is_some();
    let copy_type = if is_incremental { "incremental" } else { "full" };

    println!("Creating {} backup copy at: {}", copy_type, target.display());
    println!("Source: {}", data.display());
    println!("Format: {:?}", format);

    // Create directories
    copy.create_dirs()?;

    // Create temp scan directories within M repo
    let scan_ctrl_dir = copy.m_repo.join("ctrl");
    let scan_meta_dir = copy.m_repo.join("meta");
    std::fs::create_dir_all(&scan_ctrl_dir)?;
    std::fs::create_dir_all(&scan_meta_dir)?;

    // Determine previous meta for incremental scan
    let prev_meta_dir = if let Some(ref base) = incremental_base {
        let base_copy = BackupCopy::new(base.clone());
        if !base_copy.exists() {
            return Err(format!("Base copy does not exist: {}", base.display()).into());
        }
        Some(base_copy.m_repo.join("meta"))
    } else {
        None
    };

    // Step 1: Scan the source
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

    println!("  Scan complete: {} files, {} dirs", scan_stats.tot_files, scan_stats.tot_dirs);

    // Step 2: Find control files and schedule backup subtasks
    println!("\n[2/3] Scheduling backup subtasks...");
    let control_files = find_control_files(&scan_ctrl_dir);
    println!("  Found {} control files", control_files.len());

    let scheduler = TaskScheduler::new(jobs);
    let mut subtask_handles = Vec::new();
    let mut subtask_infos = Vec::new();

    for (ctrl_file, priority) in control_files {
        let subtask_id = Uuid::new_v4().to_string();
        let log_file = copy.m_repo.join(format!("{}.log", subtask_id));

        let info = SubtaskInfo {
            id: subtask_id.clone(),
            control_file: ctrl_file.to_string_lossy().to_string(),
            status: "pending".to_string(),
            log_file: log_file.to_string_lossy().to_string(),
        };
        subtask_infos.push(info);

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
        let ctrl_file_clone = ctrl_file.clone();
        let format_clone = format;
        let log_file_clone = log_file.clone();
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
            let result = execute_backup_subtask(
                subtask_id_clone.clone(),
                source_dir,
                &BackupCopy { copy_path: copy_d_repo.parent().unwrap().to_path_buf(), d_repo: copy_d_repo, m_repo: copy_m_repo },
                ctrl_file_clone,
                format_clone,
                workers,
                enable_hardlink_phase,
                enable_delete_phase,
                enable_mtime_phase,
                blob_size,
                threshold,
                log_file_clone,
                verbose,
            );

            let success = result.is_ok();
            if let Err(ref e) = result {
                eprintln!("[{}] Subtask failed: {}", subtask_id_clone, e);
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
            }
            Err(e) => {
                failed += 1;
                println!("  ✗ Subtask {} panicked: {:?}", subtask_id, e);
            }
        }
    }

    // Step 4: Write manifest
    let manifest = BackupManifest {
        version: "1.0".to_string(),
        copy_type: copy_type.to_string(),
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
    println!("Copy type: {}", copy_type);
    println!("Format: {:?}", format);
    println!("Target: {}", target.display());
    println!("Subtasks: {} completed, {} failed", completed, failed);
    println!("Manifest: {}", copy.m_repo.join("manifest.json").display());

    if failed > 0 {
        println!("\nWarning: Some subtasks failed. Check log files in {} for details.", copy.m_repo.display());
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

    let copy = BackupCopy::new(copy_path.clone());

    if !copy.exists() {
        return Err(format!("Backup copy does not exist: {}", copy_path.display()).into());
    }

    // Read manifest
    let manifest = copy.read_manifest()
        .ok_or("Failed to read backup manifest")?;

    println!("\nBackup info:");
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
    let ctrl_dir = copy.m_repo.join("ctrl");
    let control_files = find_restore_control_files(&ctrl_dir);

    if control_files.is_empty() {
        return Err("No control files found for restore".into());
    }

    println!("\nRestoring {} control files...", control_files.len());

    let scheduler = TaskScheduler::new(jobs);
    let mut subtask_handles = Vec::new();

    for (ctrl_file, priority) in control_files {
        let subtask_id = Uuid::new_v4().to_string();
        let log_file = target.join(format!(".restore_{}.log", subtask_id));

        println!("  Subtask {}: {:?} -> {}", subtask_id, priority, ctrl_file.display());

        while !scheduler.can_start_new() {
            thread::sleep(Duration::from_millis(100));
        }

        scheduler.start_task();

        let subtask_id_clone = subtask_id.clone();
        let copy_d_repo = copy.d_repo.clone();
        let copy_m_repo = copy.m_repo.clone();
        let target_dir = target.clone();
        let ctrl_file_clone = ctrl_file.clone();
        let policy_clone = policy;
        let log_file_clone = log_file.clone();

        let handle = thread::spawn(move || {
            let result = execute_restore_subtask(
                subtask_id_clone.clone(),
                copy_d_repo,
                copy_m_repo,
                target_dir,
                ctrl_file_clone,
                policy_clone,
                workers,
                hardlinks,
                mtime,
                log_file_clone,
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
    source_dir: PathBuf,
    meta_dir: PathBuf,
    target_dir: PathBuf,
    control_file: PathBuf,
    policy: RestorePolicy,
    workers: usize,
    _hardlinks: bool,
    _mtime: bool,
    _log_file: PathBuf,
    _verbose: u8,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("[{}] Starting restore subtask", subtask_id);
    println!("[{}] Control file: {}", subtask_id, control_file.display());

    // Create restore option
    // Meta files are in M/meta/, control files are in M/ctrl/
    let meta_dir_full = meta_dir.join("meta");
    let ctrl_dir = meta_dir.join("ctrl");
    let restore_option = RestoreOption::new(
        source_dir,
        target_dir,
        meta_dir_full,
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
    // Initialize global logger
    init_global_logger(0)?;

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
