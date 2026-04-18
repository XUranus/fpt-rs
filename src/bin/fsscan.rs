// src/bin/fsscan.rs
//
// Low-level scanner CLI — demonstrates LocalFileScanner and NfsFileScanner
// from the frame::traits layer directly.
//
// This tool exercises the scanning phase in isolation (no backup/restore).
// For the full integrated workflow, use `fptcli backup`.

use std::{path::PathBuf, thread, time::Duration};
use clap::Parser;

use bifrost::frame::{
    FileScanner,
    LocalFileScanner, ScannerConfig,
};
#[cfg(feature = "nfs")]
use bifrost::frame::NfsFileScanner;
#[cfg(feature = "nfs")]
use bifrost::nfs::NfsLocation;

/// Bifrost Filesystem Scanner
///
/// Scans a local filesystem path (or NFS export with --features nfs) and
/// generates metadata + control files for use by `fsbackup` or `fptcli`.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Local source paths to scan (at least one required; omit when --nfs-host is set)
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Control file output directory
    #[arg(long, short = 'c', default_value = "/tmp/bifrost/ctrl", value_name = "DIR")]
    ctrl_dir: PathBuf,

    /// Metadata output directory
    #[arg(long, short = 'm', default_value = "/tmp/bifrost/meta", value_name = "DIR")]
    meta_dir: PathBuf,

    /// Number of traversal worker threads (local) / concurrent RPC tasks (NFS)
    #[arg(long, short = 'w', default_value = "4", value_name = "COUNT")]
    workers: usize,

    /// Number of metadata writer threads
    #[arg(long, short = 'W', default_value = "1", value_name = "COUNT")]
    writers: usize,

    /// Temporary directory for spillable queues
    #[arg(long, short = 't', default_value = "/tmp/bifrost/cache", value_name = "DIR")]
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

    // ── NFS source (requires --features nfs) ─────────────────────────────────
    /// NFS source: server IP or hostname.
    /// When set, the NFS export is scanned instead of local <PATH> arguments.
    #[arg(long, value_name = "HOST", requires = "nfs_export")]
    nfs_host: Option<String>,

    /// NFS source: export path on the server (e.g. /export/data).
    #[arg(long, value_name = "PATH", requires = "nfs_host")]
    nfs_export: Option<String>,

    /// NFS source: sub-path within the export to use as the scan root.
    #[arg(long, value_name = "PATH")]
    nfs_sub_path: Option<String>,

    /// NFS source: number of parallel TCP connections [default: 4]
    #[arg(long, value_name = "N", default_value = "4")]
    nfs_connections: usize,

    /// Verbose logging (-v=INFO, -vv=DEBUG, -vvv=TRACE)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Log file path (append mode; logs also go to stdout)
    #[arg(long, value_name = "FILE")]
    log_file: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Initialise logger: stdout + optional file.
    bifrost::logging::init(args.verbose);
    if let Some(ref p) = args.log_file {
        bifrost::logging::add_file(p);
    }

    // ── Build shared ScannerConfig ────────────────────────────────────────────
    // Apply additional options through the underlying ScanOption builder.
    // ScannerConfig covers the common fields; low-level extras (sharding, acl,
    // xattrs, etc.) are applied by building a ScanOption manually and using
    // LocalFileScanner with it, or NfsFileScanner for NFS.
    let scanner_config = ScannerConfig::new(args.ctrl_dir.clone(), args.meta_dir.clone())
        .worker_count(args.workers)
        .writer_count(args.writers)
        .prev_meta_dir(args.prev_meta_dir.clone());

    // ── NFS scan branch ───────────────────────────────────────────────────────
    #[cfg(feature = "nfs")]
    if let Some(ref host) = args.nfs_host {
        let export = args.nfs_export.as_deref().unwrap_or("");
        let mut loc = NfsLocation::new(host, export)
            .connection_count(args.nfs_connections);
        if let Some(ref sub) = args.nfs_sub_path {
            loc = loc.sub_path(sub);
        }

        println!("Scanning NFS export: {}:{}", host, export);
        let scanner = NfsFileScanner::new(loc, scanner_config);
        let stats = scanner.scan()?;
        print_summary(&stats);
        return Ok(());
    }

    #[cfg(not(feature = "nfs"))]
    if args.nfs_host.is_some() {
        return Err("NFS source requested but binary was built without the `nfs` feature.\n\
                    Rebuild with: cargo build --features nfs".into());
    }

    // ── Local scan branch ─────────────────────────────────────────────────────
    if args.paths.is_empty() {
        return Err("At least one local <PATH> or --nfs-host must be provided".into());
    }

    for path in &args.paths {
        if !path.exists() {
            return Err(format!("Source path does not exist: {}", path.display()).into());
        }
        if !path.is_dir() {
            return Err(format!("Source path is not a directory: {}", path.display()).into());
        }
    }

    // For multiple local paths we run one scanner per root.
    // (The low-level Scanner supports multiple enqueued roots, but
    // LocalFileScanner is intentionally simple — one root per instance.)
    let mut total_files = 0u64;
    let mut total_dirs  = 0u64;
    let mut total_bytes = 0u64;

    for path in &args.paths {
        println!("Scanning: {}", path.display());
        let scanner = LocalFileScanner::new(path.clone(), scanner_config.clone());
        let stats = scanner.scan()?;
        println!(
            "  {} files, {} dirs, {:.2} MB",
            stats.total_files,
            stats.total_dirs,
            stats.total_size_bytes as f64 / (1024.0 * 1024.0),
        );
        total_files += stats.total_files;
        total_dirs  += stats.total_dirs;
        total_bytes += stats.total_size_bytes;
    }

    println!("\nTotal: {} files, {} dirs, {:.2} MB",
        total_files, total_dirs, total_bytes as f64 / (1024.0 * 1024.0));

    Ok(())
}

fn print_summary(stats: &bifrost::frame::ScanStats) {
    println!(
        "Scan complete: {} files, {} dirs, {:.2} MB",
        stats.total_files,
        stats.total_dirs,
        stats.total_size_bytes as f64 / (1024.0 * 1024.0),
    );
}
