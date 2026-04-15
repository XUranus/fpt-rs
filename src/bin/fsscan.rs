// src/bin/bifrost-scan.rs

use std::{
    path::PathBuf,
    thread,
    time::Duration,
};
use clap::{Parser};
use log::{info, warn};

use bifrost::scanner::{Scanner, options::ScanOption};

fn setup_logger() -> Result<(), fern::InitError> {
    fern::Dispatch::new()
        // Perform allocation-free log formatting
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                message
            ))
        })
        // Log to stdout (optional)
        //
        //.chain(std::io::stdout())
        // Log to a file
        .chain(fern::log_file("output.log")?)
        // Set the global log level
        .apply()?;
    Ok(())
}

/// Bifrost Backup Scanner
///
/// Scans filesystem paths and generates metadata for backup operations.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source paths to scan (at least one required)
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Control file output directory
    #[arg(
        long,
        short = 'c',
        default_value = "/tmp/bifrost/ctrl",
        value_name = "DIR"
    )]
    ctrl_dir: PathBuf,

    /// Metadata output directory
    #[arg(
        long,
        short = 'm',
        default_value = "/tmp/bifrost/meta",
        value_name = "DIR"
    )]
    meta_dir: PathBuf,

    /// Follow symbolic links during scanning
    #[arg(long, action = clap::ArgAction::SetTrue)]
    follow_symlinks: bool,

    /// Include hidden files and directories
    #[arg(long, action = clap::ArgAction::SetTrue)]
    scan_hidden: bool,

    /// Maximum recursion depth (0 = only root, none = unlimited)
    #[arg(long, short = 'd', value_name = "DEPTH")]
    max_depth: Option<usize>,

    /// Number of traversal worker threads
    #[arg(long, short = 'w', default_value = "4", value_name = "COUNT")]
    workers: usize,

    /// Number of metadata writer threads
    #[arg(long, short = 'W', default_value = "1", value_name = "COUNT")]
    writers: usize,

    /// Temporary directory for spillable queues
    #[arg(long, short = 't', default_value = "/tmp/bifrost/cache", value_name = "DIR")]
    temp_dir: PathBuf,

    /// Verbose logging
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Scan ACLs (Access Control Lists)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    scan_acl: bool,

    /// Scan extended attributes (xattrs)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    scan_xattrs: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_logger().unwrap();
    let args = Args::parse();

    // Initialize logging
    // let log_level = match args.verbose {
    //     0 => log::LevelFilter::Info,
    //     1 => log::LevelFilter::Debug,
    //     _ => log::LevelFilter::Trace,
    // };

    // Validate source paths
    for path in &args.paths {
        if !path.exists() {
            return Err(format!("Source path does not exist: {}", path.display()).into());
        }
        if !path.is_dir() {
            return Err(format!("Source path is not a directory: {}", path.display()).into());
        }
    }

    // Build scanner configuration
    let scan_option = ScanOption::new(
        args.ctrl_dir.clone(),
        args.meta_dir.clone(),
    )
    .follow_symlinks(args.follow_symlinks)
    .scan_hidden(args.scan_hidden)
    .scan_acl(args.scan_acl)
    .scan_xattrs(args.scan_xattrs)
    .max_depth(args.max_depth)
    .worker_count(args.workers)
    .writer_count(args.writers)
    .temp_dir(args.temp_dir);

    println!("scan option : {:#?}", scan_option);

    // Create scanner and enqueue paths
    let mut scanner = Scanner::new(scan_option);
    for path in args.paths.iter() {
        scanner.enqueue_path(path.clone())?;
    }

    info!("Starting scan with {} source paths", args.paths.len());
    let running_scan = scanner.start()?;

    // Monitor progress until completion
    loop {
        let stats = running_scan.stats();
        println!(
            "Files: {}, Dirs: {}, Size: {:.2} MB, Errors: {}",
            stats.tot_files,
            stats.tot_dirs,
            stats.tot_size as f64 / (1024.0 * 1024.0),
            stats.failed_files + stats.failed_dirs
        );

        if running_scan.complete() {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }

    // Final stats
    let final_stats = running_scan.stats();
    info!(
        "Scan completed! Files: {}, Dirs: {}, Total size: {:.2} GB",
        final_stats.tot_files,
        final_stats.tot_dirs,
        final_stats.tot_size as f64 / (1024.0 * 1024.0 * 1024.0)
    );

    // Wait for cleanup
    running_scan.wait();

    Ok(())
}