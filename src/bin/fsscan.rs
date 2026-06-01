use std::path::PathBuf;
use std::time::{Duration, Instant};

use fpt::failure::{failure_file_path, FailureLogConfig, FailureLogFormat, RetryPolicy};
use fpt::frame::DataLocation;
use fpt::scanner::{ScanOption, ScanPathFilterSet, Scanner};
use clap::Parser;

/// Fpt Filesystem Scanner
///
/// Scans a local path, NFS export URL, or SMB share URL and generates
/// metadata + control files for use by `fsbackup` or `fptcli`.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source path(s) to scan. Local paths look like `/opt/dataset/ds2`;
    /// NFS paths look like `nfs://127.0.0.1/opt/dataset?sub=/out`;
    /// SMB paths look like `smb://127.0.0.1/share/root?username=u&password=p`.
    #[arg(value_name = "PATH_OR_URL", required = true)]
    sources: Vec<String>,

    /// Control file output directory
    #[arg(
        long,
        short = 'c',
        default_value = "/tmp/fpt/ctrl",
        value_name = "DIR"
    )]
    ctrl_dir: PathBuf,

    /// Metadata output directory
    #[arg(
        long,
        short = 'm',
        default_value = "/tmp/fpt/meta",
        value_name = "DIR"
    )]
    meta_dir: PathBuf,

    /// Number of traversal worker threads (local) / concurrent RPC tasks (NFS/SMB)
    #[arg(long, short = 'w', default_value = "8", value_name = "COUNT")]
    workers: usize,

    /// Number of metadata writer threads
    #[arg(long, short = 'W', default_value = "1", value_name = "COUNT")]
    writers: usize,

    /// Temporary directory for spillable queues
    #[arg(
        long,
        short = 't',
        default_value = "/tmp/fpt/cache",
        value_name = "DIR"
    )]
    temp_dir: PathBuf,

    /// Follow symbolic links during scanning
    #[arg(long, action = clap::ArgAction::SetTrue)]
    follow_symlinks: bool,

    /// Include hidden files and directories
    #[arg(long, action = clap::ArgAction::SetTrue)]
    scan_hidden: bool,

    /// Maximum recursion depth (none = unlimited)
    #[arg(long, short = 'd', value_name = "DEPTH")]
    max_depth: Option<usize>,

    /// Scan ACLs
    #[arg(long, action = clap::ArgAction::SetTrue)]
    scan_acl: bool,

    /// Scan extended attributes (xattrs)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    scan_xattrs: bool,

    /// Scan and track hardlinks
    #[arg(long, action = clap::ArgAction::SetTrue)]
    scan_hardlinks: bool,

    /// Skip block devices during scanning
    #[arg(long, action = clap::ArgAction::SetTrue, default_value = "true")]
    skip_block_devices: bool,

    /// Entry names to skip (repeatable)
    #[arg(long, value_name = "NAME")]
    skip: Vec<String>,

    #[command(flatten)]
    path_filters: PathFilterArgs,

    #[command(flatten)]
    retry: RetryArgs,

    /// Previous metadata directory for incremental scan
    #[arg(long, value_name = "DIR")]
    prev_meta_dir: Option<PathBuf>,

    /// Enable sharded control files
    #[arg(long, action = clap::ArgAction::SetTrue)]
    shard: bool,

    /// Number of shards [default: 16]
    #[arg(long, default_value = "16", value_name = "COUNT")]
    shard_num: usize,

    /// Max entries per shard for copy phase
    #[arg(long, value_name = "COUNT")]
    shard_max_entries_copy: Option<usize>,

    /// Max entries per shard for other phases
    #[arg(long, value_name = "COUNT")]
    shard_max_entries_other: Option<usize>,

    /// Max shard file size in bytes
    #[arg(long, value_name = "BYTES")]
    shard_max_size: Option<u64>,

    /// SMB query-directory buffer size in MiB.
    #[arg(long, default_value = "8", value_name = "MIB")]
    smb_query_buffer_mb: u32,

    /// Number of parallel NFS connections (used when a source is an NFS URL)
    #[arg(long, default_value = "32", value_name = "COUNT")]
    nfs_connections: usize,

    /// AUTH_UNIX uid to present to the NFS server (overrides uid= in URL)
    #[arg(long, value_name = "UID")]
    nfs_uid: Option<u32>,

    /// AUTH_UNIX gid to present to the NFS server (overrides gid= in URL)
    #[arg(long, value_name = "GID")]
    nfs_gid: Option<u32>,

    /// Verbose logging (-v=INFO, -vv=DEBUG, -vvv=TRACE)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Log file path (append mode; logs also go to stdout)
    #[arg(long, value_name = "FILE")]
    log_file: Option<PathBuf>,

    /// Scan entries and print summary stats only; skip metadata/cache/control-file generation.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    stats_only: bool,

    /// Structured failure log output path. If omitted and --failure-log-format is set,
    /// defaults to <ctrl-dir>/SCAN_FAILURE.<ext>.
    #[arg(long, value_name = "FILE")]
    failure_log: Option<PathBuf>,

    /// Structured failure log format.
    #[arg(long, value_enum, value_name = "FMT")]
    failure_log_format: Option<FailureLogFormatArg>,
}

#[derive(clap::Args, Debug, Clone, Default)]
struct PathFilterArgs {
    /// Include directories whose logical path matches this pattern. Repeatable.
    #[arg(long, value_name = "PATTERN")]
    include_dir_pattern: Vec<String>,

    /// Include files whose logical path matches this pattern. Repeatable.
    #[arg(long, value_name = "PATTERN")]
    include_file_pattern: Vec<String>,

    /// Exclude directories whose logical path matches this pattern. Repeatable.
    #[arg(long, value_name = "PATTERN")]
    exclude_dir_pattern: Vec<String>,

    /// Exclude files whose logical path matches this pattern. Repeatable.
    #[arg(long, value_name = "PATTERN")]
    exclude_file_pattern: Vec<String>,
}

impl PathFilterArgs {
    fn compile(&self) -> Result<Option<ScanPathFilterSet>, std::io::Error> {
        ScanPathFilterSet::compile(
            self.include_dir_pattern.clone(),
            self.include_file_pattern.clone(),
            self.exclude_dir_pattern.clone(),
            self.exclude_file_pattern.clone(),
        )
        .map_err(std::io::Error::other)
    }
}

#[derive(clap::Args, Debug, Clone, Copy)]
struct RetryArgs {
    /// Number of retries for scan operations before recording failure.
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
}

impl RetryArgs {
    fn policy(self) -> RetryPolicy {
        RetryPolicy::new(
            self.operation_retries,
            Duration::from_millis(self.retry_delay_ms),
        )
        .with_backoff(
            self.retry_backoff,
            Duration::from_millis(self.retry_max_delay_ms),
        )
        .with_jitter(self.retry_jitter)
    }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum FailureLogFormatArg {
    Csv,
    Json,
    Xml,
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

#[derive(Debug, Clone, Copy, Default)]
struct ScanSummary {
    total_files: u64,
    total_dirs: u64,
    total_size_bytes: u64,
    failed_files: u64,
    failed_dirs: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    fpt::logging::init(args.verbose);
    if let Some(ref p) = args.log_file {
        fpt::logging::add_file(p);
    }

    let mut totals = ScanSummary::default();
    let total_start = Instant::now();

    for source in &args.sources {
        let location =
            parse_data_location(source, args.nfs_connections, args.nfs_uid, args.nfs_gid)?;
        let scan_option = build_scan_option(&args, &location)?;

        let started = Instant::now();
        let summary = run_scan(&location, scan_option.clone())?;
        print_summary(&location, summary, started.elapsed());

        totals.total_files += summary.total_files;
        totals.total_dirs += summary.total_dirs;
        totals.total_size_bytes += summary.total_size_bytes;
        totals.failed_files += summary.failed_files;
        totals.failed_dirs += summary.failed_dirs;
    }

    if args.sources.len() > 1 {
        print_total_summary(totals, total_start.elapsed());
    }

    Ok(())
}

fn build_scan_option(
    args: &Args,
    location: &DataLocation,
) -> Result<ScanOption, Box<dyn std::error::Error>> {
    let mut opt = ScanOption::new(args.ctrl_dir.clone(), args.meta_dir.clone())
        .worker_count(args.workers)
        .writer_count(args.writers)
        .temp_dir(args.temp_dir.clone())
        .follow_symlinks(args.follow_symlinks)
        .scan_hidden(args.scan_hidden)
        .max_depth(args.max_depth)
        .scan_acl(args.scan_acl)
        .scan_xattrs(args.scan_xattrs)
        .scan_hardlinks(args.scan_hardlinks)
        .skip_block_devices(args.skip_block_devices)
        .skip_entries(args.skip.clone())
        .prev_meta_dir(args.prev_meta_dir.clone())
        .enable_sharding(args.shard)
        .shard_num(args.shard_num)
        .control_path(
            location.control_path_base(),
            location.logical_source_root(),
            location.kind_name(),
        )
        .stats_only(args.stats_only)
        .retry_policy(args.retry.policy());

    opt = opt.path_filters(args.path_filters.compile()?);

    if let Some(format) = args.failure_log_format {
        let format: FailureLogFormat = format.into();
        let path = args
            .failure_log
            .clone()
            .unwrap_or_else(|| failure_file_path(&args.ctrl_dir, "SCAN_FAILURE", format));
        opt = opt.failure_log(Some(FailureLogConfig::new(path, format)));
    }

    if let Some(max) = args.shard_max_entries_copy {
        opt = opt.shard_max_entries_copy(max);
    }
    if let Some(max) = args.shard_max_entries_other {
        opt = opt.shard_max_entries_other(max);
    }
    if let Some(size) = args.shard_max_size {
        opt = opt.shard_max_size(size);
    }

    Ok(opt)
}

fn run_scan(
    location: &DataLocation,
    scan_option: ScanOption,
) -> Result<ScanSummary, Box<dyn std::error::Error>> {
    match location {
        DataLocation::Local(path) => {
            if !path.exists() {
                return Err(format!("Source path does not exist: {}", path.display()).into());
            }
            if !path.is_dir() {
                return Err(format!("Source path is not a directory: {}", path.display()).into());
            }

            let mut scanner = Scanner::new(scan_option);
            scanner.enqueue_path(path.clone())?;
            let running = scanner.start()?;
            while !running.complete() {
                std::thread::sleep(Duration::from_millis(200));
            }
            let snap = running.stats();
            running.wait();

            Ok(ScanSummary {
                total_files: snap.tot_files,
                total_dirs: snap.tot_dirs,
                total_size_bytes: snap.tot_size,
                failed_files: snap.failed_files,
                failed_dirs: snap.failed_dirs,
            })
        }
        #[cfg(feature = "nfs")]
        DataLocation::Nfs(loc) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("fpt-fsscan-nfs")
                .build()?;
            let (total_files, total_dirs, total_size_bytes, failed_files, failed_dirs) = rt
                .block_on(fpt::scanner::run_nfs_scan(loc, scan_option))
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            Ok(ScanSummary {
                total_files,
                total_dirs,
                total_size_bytes,
                failed_files,
                failed_dirs,
            })
        }
        #[cfg(feature = "smb")]
        DataLocation::Smb(loc) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("fpt-fsscan-smb")
                .build()?;
            let (total_files, total_dirs, total_size_bytes, failed_files, failed_dirs) = rt
                .block_on(fpt::scanner::run_smb_scan(loc, scan_option))
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            Ok(ScanSummary {
                total_files,
                total_dirs,
                total_size_bytes,
                failed_files,
                failed_dirs,
            })
        }
    }
}

#[cfg(feature = "nfs")]
fn parse_nfs_location(
    url: &str,
    connections: usize,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<DataLocation, Box<dyn std::error::Error>> {
    let mut loc = fpt::nfs::NfsLocation::from_url(url)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
        .connection_count(connections);
    let default_uid = if loc.uid == 0 {
        #[cfg(unix)] { unsafe { libc::geteuid() as u32 } }
        #[cfg(not(unix))] { 0 }
    } else {
        loc.uid
    };
    let default_gid = if loc.gid == 0 {
        #[cfg(unix)] { unsafe { libc::getegid() as u32 } }
        #[cfg(not(unix))] { 0 }
    } else {
        loc.gid
    };
    let final_uid = uid.unwrap_or(default_uid);
    let final_gid = gid.unwrap_or(default_gid);
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
                fpt::smb::SmbLocation::from_url(spec)
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

fn print_summary(location: &DataLocation, stats: ScanSummary, elapsed: Duration) {
    println!("Scanning: {}", location);
    println!(
        "Scan complete: {} files, {} dirs, {:.2} MB, {} failed files, {} failed dirs, elapsed {}",
        stats.total_files,
        stats.total_dirs,
        stats.total_size_bytes as f64 / (1024.0 * 1024.0),
        stats.failed_files,
        stats.failed_dirs,
        format_elapsed(elapsed),
    );
}

fn print_total_summary(stats: ScanSummary, elapsed: Duration) {
    println!(
        "\nTotal: {} files, {} dirs, {:.2} MB, {} failed files, {} failed dirs, elapsed {}",
        stats.total_files,
        stats.total_dirs,
        stats.total_size_bytes as f64 / (1024.0 * 1024.0),
        stats.failed_files,
        stats.failed_dirs,
        format_elapsed(elapsed),
    );
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let millis = elapsed.subsec_millis();
    if secs >= 60 {
        format!("{}m {}.{:03}s", secs / 60, secs % 60, millis)
    } else {
        format!("{}.{:03}s", secs, millis)
    }
}
