//!/usr/bin/env rust-script
//! fptcli - File Protection Tool CLI
//!
//! A unified CLI for backup and restore operations with support for:
//! - Common format backups (full synthesis copies)
//! - Aggregated format backups (full and incremental)
//! - Multi-subtask scheduling for large filesets
//! - Task-specific logging

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Instant;

use bifrost::backup::aggregate::{AggregateConfig, AggregateLayout};
use bifrost::backup::RestorePolicy;
use bifrost::failure::{FailureLogFormat, RetryPolicy};
use bifrost::frame::{
    scan::ScanConfig, traits::BackupRestoreJob, BackupJob, BackupJobConfig, DataLocation,
    RestoreJob, RestoreJobConfig,
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
        /// Source data path. Local paths look like `/opt/dataset`; NFS paths
        /// look like `nfs://127.0.0.1/opt/dataset?sub=/ds1`; SMB paths look
        /// like `smb://127.0.0.1/share/root?username=u&password=p`.
        #[arg(long, short = 'd', value_name = "PATH_OR_URL", required = true)]
        data: String,

        /// Target path where the copy will be created. Local paths look like
        /// `/backup`; NFS paths look like `nfs://127.0.0.1/opt/backup?sub=/out`;
        /// SMB paths look like `smb://127.0.0.1/share/root?username=u&password=p`.
        #[arg(long, short = 't', value_name = "PATH_OR_URL", required = true)]
        target: String,

        /// Backup format: common or aggregated
        #[arg(long, short = 'f', value_enum, default_value = "common")]
        format: BackupFormat,

        /// Shortcut for `--format aggregated`
        #[arg(long, action = clap::ArgAction::SetTrue)]
        aggregate: bool,

        /// Previous backup copy for incremental (only valid with aggregated format)
        #[arg(long, short = 'i', value_name = "DIR")]
        incremental_base: Option<PathBuf>,

        /// Maximum concurrent subtasks
        #[arg(long, short = 'j', default_value = "4", value_name = "COUNT")]
        jobs: usize,

        /// Aggregate blob size in MB (only for aggregated format)
        #[arg(long, default_value = "4", value_name = "MB")]
        blob_size: u64,

        /// Aggregate file threshold in KB (only for aggregated format)
        #[arg(long, default_value = "1024", value_name = "KB")]
        threshold: u64,

        /// Aggregate layout/version: `dir-level` or `shard`
        #[arg(long, value_enum, default_value = "shard")]
        aggregate_layout: AggregateLayoutArg,

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
        #[arg(long, short = 'w', default_value = "8", value_name = "COUNT")]
        workers: usize,

        /// Number of parallel NFS connections (used when source or target is an NFS URL)
        #[arg(long, default_value = "32", value_name = "COUNT")]
        nfs_connections: usize,

        /// Number of SMB client connections per SMB endpoint
        #[arg(long, default_value = "4", value_name = "COUNT")]
        smb_connections: usize,

        /// Maximum concurrent SMB file copy tasks. 0 = auto (2 per SMB connection, capped at 16).
        #[arg(long, default_value = "0", value_name = "COUNT")]
        smb_copy_tasks: usize,

        /// Maximum per-file copy buffer size in KB [default: 1024, recommended: 256..4096].
        /// SMB source reads are capped at 2048 KiB; SMB writes stay capped at 256 KiB.
        #[arg(long, default_value = "1024", value_name = "SIZE_KB")]
        buffer_size: usize,

        /// Structured failure log format written under C_REPO/logs.
        #[arg(long, value_enum, value_name = "FMT")]
        failure_log_format: Option<FailureLogFormatArg>,

        /// Number of retries for scan/copy operations before recording failure.
        #[arg(long, default_value = "3", value_name = "COUNT")]
        operation_retries: u32,

        /// Delay in milliseconds between retries.
        #[arg(long, default_value = "1000", value_name = "MS")]
        retry_delay_ms: u64,

        /// Exponential retry backoff multiplier. 1.0 keeps fixed delay.
        #[arg(long, default_value = "1.0", value_name = "N")]
        retry_backoff: f64,

        /// Maximum retry delay in milliseconds when backoff is enabled.
        #[arg(long, default_value = "1000", value_name = "MS")]
        retry_max_delay_ms: u64,

        /// Deterministic jitter ratio for retry delays, range 0.0..1.0.
        #[arg(long, default_value = "0.0", value_name = "RATIO")]
        retry_jitter: f64,

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
        /// Source backup copy path (local or NFS URL).
        #[arg(long, short = 'c', required = true, value_name = "PATH_OR_URL")]
        copy: String,

        /// Target restore path (local or NFS URL).
        #[arg(long, short = 't', value_name = "PATH_OR_URL", required = true)]
        target: String,

        /// Restore policy: replace, skip, or keep-newer
        #[arg(long, short = 'p', value_enum, default_value = "replace")]
        policy: RestorePolicyArg,

        /// Maximum concurrent subtasks
        #[arg(long, short = 'j', default_value = "4", value_name = "COUNT")]
        jobs: usize,

        /// Number of worker threads per subtask
        #[arg(long, short = 'w', default_value = "8", value_name = "COUNT")]
        workers: usize,

        /// Restore hardlinks
        #[arg(long, action = clap::ArgAction::SetTrue)]
        hardlinks: bool,

        /// Restore modification times
        #[arg(long, action = clap::ArgAction::SetTrue, default_value = "true")]
        mtime: bool,

        /// Fine-grained restore path. Repeat to restore multiple files/directories.
        /// Files are exact matches; directories restore the full subtree.
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,

        /// Number of parallel NFS connections (used when copy or target is an NFS URL)
        #[arg(long, default_value = "32", value_name = "COUNT")]
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

#[derive(ValueEnum, Debug, Clone, Copy)]
enum AggregateLayoutArg {
    DirLevel,
    Shard,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum FailureLogFormatArg {
    Csv,
    Json,
    Xml,
}

impl From<AggregateLayoutArg> for AggregateLayout {
    fn from(value: AggregateLayoutArg) -> Self {
        match value {
            AggregateLayoutArg::DirLevel => AggregateLayout::DirLevel,
            AggregateLayoutArg::Shard => AggregateLayout::Shard,
        }
    }
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

impl From<FailureLogFormatArg> for FailureLogFormat {
    fn from(value: FailureLogFormatArg) -> Self {
        match value {
            FailureLogFormatArg::Csv => FailureLogFormat::Csv,
            FailureLogFormatArg::Json => FailureLogFormat::Json,
            FailureLogFormatArg::Xml => FailureLogFormat::Xml,
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
fn parse_nfs_location(
    url: &str,
    connections: usize,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<DataLocation, Box<dyn std::error::Error>> {
    let mut loc = bifrost::nfs::NfsLocation::from_url(url)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
        .connection_count(connections);
    // CLI flags override any uid/gid already set via URL query params
    let final_uid = uid.unwrap_or(loc.uid);
    let final_gid = gid.unwrap_or(loc.gid);
    loc = loc.credentials(final_uid, final_gid);
    Ok(DataLocation::nfs(loc))
}

fn parse_data_location(
    spec: &str,
    connections: usize,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<DataLocation, Box<dyn std::error::Error>> {
    if spec.starts_with("nfs://") {
        #[cfg(feature = "nfs")]
        {
            parse_nfs_location(spec, connections, uid, gid)
        }
        #[cfg(not(feature = "nfs"))]
        {
            let _ = (connections, uid, gid);
            Err("NFS support not compiled in. Rebuild with --features nfs".into())
        }
    } else if spec.starts_with("smb://") || spec.starts_with(r"smb:\\") {
        #[cfg(feature = "smb")]
        {
            let _ = (connections, uid, gid);
            Ok(DataLocation::smb(
                bifrost::smb::SmbLocation::from_url(spec)
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
            ))
        }
        #[cfg(not(feature = "smb"))]
        {
            let _ = (connections, uid, gid);
            Err("SMB support not compiled in. Rebuild with --features smb".into())
        }
    } else {
        let _ = (connections, uid, gid);
        Ok(DataLocation::local(PathBuf::from(spec)))
    }
}

/// Execute backup command using the `frame::BackupJob` orchestrator.
fn cmd_backup(
    data: String,
    target: String,
    mut format: BackupFormat,
    aggregate: bool,
    incremental_base: Option<PathBuf>,
    jobs: usize,
    blob_size: u64,
    threshold: u64,
    aggregate_layout: AggregateLayoutArg,
    hardlink: bool,
    delete: bool,
    mtime: bool,
    workers: usize,
    nfs_connections: usize,
    smb_connections: usize,
    smb_copy_tasks: usize,
    buffer_size: usize,
    failure_log_format: Option<FailureLogFormatArg>,
    operation_retries: u32,
    retry_delay_ms: u64,
    retry_backoff: f64,
    retry_max_delay_ms: u64,
    retry_jitter: f64,
    nfs_uid: Option<u32>,
    nfs_gid: Option<u32>,
    temp_dir: Option<PathBuf>,
    verbose: u8,
    log_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if aggregate {
        format = BackupFormat::Aggregated;
    }

    if incremental_base.is_some() && matches!(format, BackupFormat::Common) {
        return Err("Incremental backup is only supported with aggregated format".into());
    }

    let source = parse_data_location(&data, nfs_connections, nfs_uid, nfs_gid)?;
    let target_loc = parse_data_location(&target, nfs_connections, nfs_uid, nfs_gid)?;

    let is_incremental = incremental_base.is_some();
    let type_tag = if is_incremental { "INC" } else { "FULL" }.to_string();
    let format_tag = format_abbr(format).to_string();

    // Build aggregate config
    let aggregate_config = if matches!(format, BackupFormat::Aggregated) {
        AggregateConfig::enabled()
            .layout(aggregate_layout.into())
            .max_blob_size(blob_size * 1024 * 1024)
            .file_threshold(threshold * 1024)
    } else {
        AggregateConfig::default()
    };

    let retry_policy = RetryPolicy::new(
        operation_retries,
        std::time::Duration::from_millis(retry_delay_ms),
    )
    .with_backoff(
        retry_backoff,
        std::time::Duration::from_millis(retry_max_delay_ms),
    )
    .with_jitter(retry_jitter);

    let scan_config = ScanConfig {
        worker_count: workers,
        writer_count: 1,
        prev_meta_dir: None, // set by BackupJob for incremental
        enable_aggregation: matches!(format, BackupFormat::Aggregated),
        max_aggregate_blob_size: blob_size * 1024 * 1024,
        aggregate_file_threshold: threshold * 1024,
        failure_log: None,
        retry_policy,
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
        enable_delete: delete && !matches!(format, BackupFormat::Aggregated),
        enable_mtime: mtime && !matches!(format, BackupFormat::Aggregated),
        max_concurrent_subtasks: jobs,
        smb_connection_count: smb_connections.max(1),
        smb_copy_task_count: smb_copy_tasks,
        copy_buffer_size: (buffer_size * 1024).clamp(256 * 1024, 4 * 1024 * 1024),
        failure_log_format: failure_log_format.map(Into::into),
        retry_policy,
        incremental_base,
        verbose,
    };

    println!(
        "Starting {} {} backup...",
        config.format_tag, config.type_tag
    );
    println!("Source : {}", config.source);
    println!("Target : {}", config.target);
    let summary_format_tag = config.format_tag.clone();

    // Initialize logger.  BackupJob will add module→file routes after prereq.
    bifrost::logging::init(verbose);
    if let Some(ref p) = log_file {
        bifrost::logging::add_file(p);
    }

    let started_at = Instant::now();
    let job = BackupJob::new(config);
    let result = job
        .run()
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    let elapsed = started_at.elapsed();

    println!("\n{}", "=".repeat(60));
    println!("Backup Summary");
    println!("{}", "=".repeat(60));
    println!("Source type : {}", location_kind(&data));
    println!("Target type : {}", location_kind(&target));
    println!("Format      : {}", summary_format_tag);
    if matches!(format, BackupFormat::Aggregated) {
        println!("Aggregation : enabled");
        println!(
            "Layout      : {}",
            AggregateLayout::from(aggregate_layout).as_str()
        );
        println!("Blob size   : {} MiB", blob_size);
        println!("Threshold   : {} KiB", threshold);
    } else {
        println!("Aggregation : disabled");
    }
    println!("Source path : {}", data);
    println!("Target path : {}", target);
    println!("Copy UUID   : {}", result.copy_uuid);
    println!("Copy root   : {}", result.copy_root.display());
    println!(
        "Subtasks    : {} ok, {} failed",
        result.subtasks_ok, result.subtasks_failed
    );
    println!("Total files : {}", result.total_files);
    println!("Total dirs  : {}", result.total_dirs);
    println!("Total bytes : {}", result.total_bytes);
    println!("Elapsed     : {}", format_duration(elapsed));
    println!(
        "File rate   : {:.2} files/s",
        rate(result.total_files as f64, elapsed)
    );
    println!(
        "Data rate   : {}/s",
        format_bytes(rate(result.total_bytes as f64, elapsed) as u64)
    );

    if result.subtasks_failed > 0 {
        return Err(format!("{} subtask(s) failed", result.subtasks_failed).into());
    }

    println!("\nBackup completed successfully!");
    Ok(())
}

/// Execute restore command using the `frame::RestoreJob` orchestrator.
fn cmd_restore(
    copy_path: String,
    target: String,
    policy: RestorePolicy,
    jobs: usize,
    _workers: usize,
    _hardlinks: bool,
    _mtime: bool,
    paths: Vec<String>,
    nfs_connections: usize,
    nfs_uid: Option<u32>,
    nfs_gid: Option<u32>,
    temp_dir: Option<PathBuf>,
    verbose: u8,
    log_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let restore_target = parse_data_location(&target, nfs_connections, nfs_uid, nfs_gid)?;
    let copy_source = parse_data_location(&copy_path, nfs_connections, nfs_uid, nfs_gid)?;

    println!("Restoring from : {}", copy_path);
    println!("Restore target : {}", restore_target);
    println!("Policy         : {:?}", policy);
    if !paths.is_empty() {
        println!("Mode           : fine-grained ({} path(s))", paths.len());
    }

    bifrost::logging::init(verbose);
    if let Some(ref p) = log_file {
        bifrost::logging::add_file(p);
    }

    let started_at = Instant::now();
    let config = RestoreJobConfig {
        copy_source,
        restore_target,
        policy,
        temp_config: match temp_dir {
            Some(p) => bifrost::frame::repo::TempRepoConfig::new(p),
            None => bifrost::frame::repo::TempRepoConfig::default(),
        },
        max_concurrent_subtasks: jobs,
        fine_grain_paths: paths,
    };

    let job = RestoreJob::new(config);
    let result = job
        .run()
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    let elapsed = started_at.elapsed();

    println!("\n{}", "=".repeat(60));
    println!("Restore Summary");
    println!("{}", "=".repeat(60));
    println!("Source type : {}", location_kind(&copy_path));
    println!("Target type : {}", location_kind(&target));
    println!("Source path : {}", copy_path);
    println!("Target path : {}", target);
    println!(
        "Subtasks    : {} ok, {} failed",
        result.subtasks_ok, result.subtasks_failed
    );
    println!("Total files : {}", result.total_files);
    println!("Elapsed     : {}", format_duration(elapsed));
    println!(
        "File rate   : {:.2} files/s",
        rate(result.total_files as f64, elapsed)
    );

    if result.subtasks_failed > 0 {
        return Err(format!("{} subtask(s) failed", result.subtasks_failed).into());
    }

    println!("\nRestore completed successfully!");
    Ok(())
}

fn location_kind(spec: &str) -> &'static str {
    if spec.starts_with("nfs://") {
        "NFS"
    } else if spec.starts_with("smb://") || spec.starts_with(r"smb:\\") {
        "SMB"
    } else {
        "Local"
    }
}

fn rate(value: f64, elapsed: std::time::Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs > 0.0 {
        value / secs
    } else {
        value
    }
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    if secs < 60 {
        format!("{}.{:03}s", secs, millis)
    } else {
        format!("{}m {}.{:03}s", secs / 60, secs % 60, millis)
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.2} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.2} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.2} KiB", value / KIB)
    } else {
        format!("{} B", bytes)
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
                        eprintln!(
                            "Warning: failed to raise fd limit from {} to {}: {}",
                            soft, hard, e
                        );
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
            aggregate,
            incremental_base,
            jobs,
            blob_size,
            threshold,
            aggregate_layout,
            hardlink,
            delete,
            mtime,
            workers,
            nfs_connections,
            smb_connections,
            smb_copy_tasks,
            buffer_size,
            failure_log_format,
            operation_retries,
            retry_delay_ms,
            retry_backoff,
            retry_max_delay_ms,
            retry_jitter,
            nfs_uid,
            nfs_gid,
            temp_dir,
            verbose,
            log_file,
        } => cmd_backup(
            data,
            target,
            format,
            aggregate,
            incremental_base,
            jobs,
            blob_size,
            threshold,
            aggregate_layout,
            hardlink,
            delete,
            mtime,
            workers,
            nfs_connections,
            smb_connections,
            smb_copy_tasks,
            buffer_size,
            failure_log_format,
            operation_retries,
            retry_delay_ms,
            retry_backoff,
            retry_max_delay_ms,
            retry_jitter,
            nfs_uid,
            nfs_gid,
            temp_dir,
            verbose,
            log_file,
        ),
        Commands::Restore {
            copy,
            target,
            policy,
            jobs,
            workers,
            hardlinks,
            mtime,
            paths,
            nfs_connections,
            nfs_uid,
            nfs_gid,
            temp_dir,
            verbose,
            log_file,
        } => cmd_restore(
            copy,
            target,
            policy.into(),
            jobs,
            workers,
            hardlinks,
            mtime,
            paths,
            nfs_connections,
            nfs_uid,
            nfs_gid,
            temp_dir,
            verbose,
            log_file,
        ),
    }
}
