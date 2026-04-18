//!/usr/bin/env rust-script
//! fptcli - File Protection Tool CLI
//!
//! A unified CLI for backup and restore operations with support for:
//! - Common format backups (full synthesis copies)
//! - Aggregated format backups (full and incremental)
//! - Multi-subtask scheduling for large filesets
//! - Task-specific logging

use std::path::PathBuf;
use clap::{Parser, Subcommand, ValueEnum};

use bifrost::backup::RestorePolicy;
use bifrost::backup::aggregate::AggregateConfig;
use bifrost::frame::{
    BackupJob, BackupJobConfig,
    RestoreJob, RestoreJobConfig,
    DataLocation,
    scan::ScanConfig,
    traits::BackupRestoreJob,
};

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
        /// Source data directory to backup (local path).
        /// Mutually exclusive with --data-nfs.
        #[arg(long, short = 'd', value_name = "DIR",
              conflicts_with = "data_nfs")]
        data: Option<PathBuf>,

        /// Source NFS export to backup (NFS URL, e.g. nfs://127.0.0.1/opt/dataset).
        /// Mutually exclusive with --data.
        #[arg(long, value_name = "NFS_URL",
              conflicts_with = "data")]
        data_nfs: Option<String>,

        /// Target directory where the copy will be created (local path, creates COPY_* folder).
        /// Mutually exclusive with --target-nfs.
        #[arg(long, short = 't', value_name = "DIR",
              conflicts_with = "target_nfs")]
        target: Option<PathBuf>,

        /// Target NFS export where the copy will be created (NFS URL, e.g. nfs://127.0.0.1/opt/backup).
        /// Mutually exclusive with --target.
        #[arg(long, value_name = "NFS_URL",
              conflicts_with = "target")]
        target_nfs: Option<String>,

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

        /// Number of parallel NFS connections (used when --data-nfs or --target-nfs is set)
        #[arg(long, default_value = "4", value_name = "COUNT")]
        nfs_connections: usize,

        /// AUTH_UNIX uid to present to the NFS server (overrides uid= in URL)
        #[arg(long, value_name = "UID")]
        nfs_uid: Option<u32>,

        /// AUTH_UNIX gid to present to the NFS server (overrides gid= in URL)
        #[arg(long, value_name = "GID")]
        nfs_gid: Option<u32>,

        /// Temporary working directory for staging metadata/control files (default: /tmp/bifrost)
        #[arg(long, value_name = "DIR")]
        temp_dir: Option<PathBuf>,

        /// Verbose logging (-v=INFO, -vv=DEBUG, -vvv=TRACE)
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,

        /// Log file path (append mode; logs also go to stdout and C_REPO/logs/backup.log)
        #[arg(long, value_name = "FILE")]
        log_file: Option<PathBuf>,
    },

    /// Restore from a backup copy
    Restore {
        /// Source backup copy directory (containing manifest.json and D_REPO, M_REPO, C_REPO)
        #[arg(long, short = 'c', required = true, value_name = "DIR")]
        copy: PathBuf,

        /// Target restore directory (local path).
        /// Mutually exclusive with --target-nfs.
        #[arg(long, short = 't', value_name = "DIR",
              conflicts_with = "target_nfs")]
        target: Option<PathBuf>,

        /// Target NFS export for restore (NFS URL, e.g. nfs://127.0.0.1/opt/restore).
        /// Mutually exclusive with --target.
        #[arg(long, value_name = "NFS_URL",
              conflicts_with = "target")]
        target_nfs: Option<String>,

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

        /// Number of parallel NFS connections (used when --target-nfs is set)
        #[arg(long, default_value = "4", value_name = "COUNT")]
        nfs_connections: usize,

        /// AUTH_UNIX uid to present to the NFS server (overrides uid= in URL)
        #[arg(long, value_name = "UID")]
        nfs_uid: Option<u32>,

        /// AUTH_UNIX gid to present to the NFS server (overrides gid= in URL)
        #[arg(long, value_name = "GID")]
        nfs_gid: Option<u32>,

        /// Temporary working directory for staging metadata/control files (default: /tmp/bifrost)
        #[arg(long, value_name = "DIR")]
        temp_dir: Option<PathBuf>,

        /// Verbose logging (-v=INFO, -vv=DEBUG, -vvv=TRACE)
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,

        /// Log file path (append mode; logs also go to stdout)
        #[arg(long, value_name = "FILE")]
        log_file: Option<PathBuf>,
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

/// Format abbreviation for naming
fn format_abbr(fmt: BackupFormat) -> &'static str {
    match fmt {
        BackupFormat::Common => "COMMON",
        BackupFormat::Aggregated => "AGGR",
    }
}

/// Resolve and validate an NFS URL into a [`DataLocation`].
#[cfg(feature = "nfs")]
fn parse_nfs_location(url: &str, connections: usize, uid: Option<u32>, gid: Option<u32>) -> Result<DataLocation, Box<dyn std::error::Error>> {
    let mut loc = bifrost::nfs::NfsLocation::from_url(url)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
        .connection_count(connections);
    // CLI flags override any uid/gid already set via URL query params
    let final_uid = uid.unwrap_or(loc.uid);
    let final_gid = gid.unwrap_or(loc.gid);
    loc = loc.credentials(final_uid, final_gid);
    Ok(DataLocation::nfs(loc))
}

/// Execute backup command using the `frame::BackupJob` orchestrator.
fn cmd_backup(
    data: Option<PathBuf>,
    data_nfs: Option<String>,
    target: Option<PathBuf>,
    target_nfs: Option<String>,
    format: BackupFormat,
    incremental_base: Option<PathBuf>,
    jobs: usize,
    blob_size: u64,
    threshold: u64,
    hardlink: bool,
    delete: bool,
    mtime: bool,
    workers: usize,
    nfs_connections: usize,
    nfs_uid: Option<u32>,
    nfs_gid: Option<u32>,
    temp_dir: Option<PathBuf>,
    verbose: u8,
    log_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate: exactly one source and one target
    if data.is_none() && data_nfs.is_none() {
        return Err("Either --data (local path) or --data-nfs (NFS URL) must be provided".into());
    }
    if target.is_none() && target_nfs.is_none() {
        return Err("Either --target (local path) or --target-nfs (NFS URL) must be provided".into());
    }
    if incremental_base.is_some() && matches!(format, BackupFormat::Common) {
        return Err("Incremental backup is only supported with aggregated format".into());
    }

    // Build source DataLocation
    let source: DataLocation = if let Some(path) = data {
        DataLocation::local(path)
    } else {
        let url = data_nfs.as_deref().unwrap();
        #[cfg(feature = "nfs")]
        { parse_nfs_location(url, nfs_connections, nfs_uid, nfs_gid)? }
        #[cfg(not(feature = "nfs"))]
        { return Err("NFS support not compiled in. Rebuild with --features nfs".into()); }
    };

    // Build target DataLocation
    let target_loc: DataLocation = if let Some(path) = target {
        DataLocation::local(path)
    } else {
        let url = target_nfs.as_deref().unwrap();
        #[cfg(feature = "nfs")]
        { parse_nfs_location(url, nfs_connections, nfs_uid, nfs_gid)? }
        #[cfg(not(feature = "nfs"))]
        { return Err("NFS support not compiled in. Rebuild with --features nfs".into()); }
    };

    let is_incremental = incremental_base.is_some();
    let type_tag = if is_incremental { "INC" } else { "FULL" }.to_string();
    let format_tag = format_abbr(format).to_string();

    // Build aggregate config
    let aggregate_config = if matches!(format, BackupFormat::Aggregated) {
        AggregateConfig::enabled()
            .max_blob_size(blob_size * 1024 * 1024)
            .file_threshold(threshold * 1024)
    } else {
        AggregateConfig::default()
    };

    let scan_config = ScanConfig {
        worker_count: workers,
        writer_count: 1,
        prev_meta_dir: None, // set by BackupJob for incremental
    };

    let config = BackupJobConfig {
        source,
        target: target_loc,
        format_tag,
        type_tag,
        temp_config: match temp_dir {
            Some(p) => bifrost::frame::repo::TempRepoConfig::new(p),
            None => bifrost::frame::repo::TempRepoConfig::default(),
        },
        scan_config,
        aggregate_config,
        enable_hardlink: hardlink && !matches!(format, BackupFormat::Aggregated),
        enable_delete:   delete   && !matches!(format, BackupFormat::Aggregated),
        enable_mtime:    mtime    && !matches!(format, BackupFormat::Aggregated),
        max_concurrent_subtasks: jobs,
        incremental_base,
        verbose,
    };

    println!("Starting {} {} backup...", config.format_tag, config.type_tag);
    println!("Source : {}", config.source);
    println!("Target : {}", config.target);

    // Initialize logger.  BackupJob will add module→file routes after prereq.
    bifrost::logging::init(verbose);
    if let Some(ref p) = log_file {
        bifrost::logging::add_file(p);
    }

    let job = BackupJob::new(config);
    let result = job.run()
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    println!("\n{}", "=".repeat(60));
    println!("Backup Summary");
    println!("{}", "=".repeat(60));
    println!("Copy UUID  : {}", result.copy_uuid);
    println!("Copy root  : {}", result.copy_root.display());
    println!("Subtasks   : {} ok, {} failed", result.subtasks_ok, result.subtasks_failed);
    println!("Total files: {}", result.total_files);
    println!("Total bytes: {}", result.total_bytes);

    if result.subtasks_failed > 0 {
        return Err(format!("{} subtask(s) failed", result.subtasks_failed).into());
    }

    println!("\nBackup completed successfully!");
    Ok(())
}

/// Execute restore command using the `frame::RestoreJob` orchestrator.
fn cmd_restore(
    copy_path: PathBuf,
    target: Option<PathBuf>,
    target_nfs: Option<String>,
    policy: RestorePolicy,
    jobs: usize,
    _workers: usize,
    _hardlinks: bool,
    _mtime: bool,
    nfs_connections: usize,
    nfs_uid: Option<u32>,
    nfs_gid: Option<u32>,
    temp_dir: Option<PathBuf>,
    verbose: u8,
    log_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if target.is_none() && target_nfs.is_none() {
        return Err("Either --target (local path) or --target-nfs (NFS URL) must be provided".into());
    }

    // Build restore target DataLocation
    let restore_target: DataLocation = if let Some(path) = target {
        DataLocation::local(path)
    } else {
        let url = target_nfs.as_deref().unwrap();
        #[cfg(feature = "nfs")]
        { parse_nfs_location(url, nfs_connections, nfs_uid, nfs_gid)? }
        #[cfg(not(feature = "nfs"))]
        { return Err("NFS support not compiled in. Rebuild with --features nfs".into()); }
    };

    let copy_source = DataLocation::local(copy_path.clone());

    println!("Restoring from : {}", copy_path.display());
    println!("Restore target : {}", restore_target);
    println!("Policy         : {:?}", policy);

    bifrost::logging::init(verbose);
    if let Some(ref p) = log_file {
        bifrost::logging::add_file(p);
    }

    let config = RestoreJobConfig {
        copy_source,
        restore_target,
        policy,
        temp_config: match temp_dir {
            Some(p) => bifrost::frame::repo::TempRepoConfig::new(p),
            None => bifrost::frame::repo::TempRepoConfig::default(),
        },
        max_concurrent_subtasks: jobs,
    };

    let job = RestoreJob::new(config);
    let result = job.run()
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    println!("\n{}", "=".repeat(60));
    println!("Restore Summary");
    println!("{}", "=".repeat(60));
    println!("Subtasks   : {} ok, {} failed", result.subtasks_ok, result.subtasks_failed);
    println!("Total files: {}", result.total_files);

    if result.subtasks_failed > 0 {
        return Err(format!("{} subtask(s) failed", result.subtasks_failed).into());
    }

    println!("\nRestore completed successfully!");
    Ok(())
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
            data_nfs,
            target,
            target_nfs,
            format,
            incremental_base,
            jobs,
            blob_size,
            threshold,
            hardlink,
            delete,
            mtime,
            workers,
            nfs_connections,
            nfs_uid,
            nfs_gid,
            temp_dir,
            verbose,
            log_file,
        } => {
            cmd_backup(
                data,
                data_nfs,
                target,
                target_nfs,
                format,
                incremental_base,
                jobs,
                blob_size,
                threshold,
                hardlink,
                delete,
                mtime,
                workers,
                nfs_connections,
                nfs_uid,
                nfs_gid,
                temp_dir,
                verbose,
                log_file,
            )
        }
        Commands::Restore {
            copy,
            target,
            target_nfs,
            policy,
            jobs,
            workers,
            hardlinks,
            mtime,
            nfs_connections,
            nfs_uid,
            nfs_gid,
            temp_dir,
            verbose,
            log_file,
        } => {
            cmd_restore(
                copy,
                target,
                target_nfs,
                policy.into(),
                jobs,
                workers,
                hardlinks,
                mtime,
                nfs_connections,
                nfs_uid,
                nfs_gid,
                temp_dir,
                verbose,
                log_file,
            )
        }
    }
}
