use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use clap::Parser;
use crossbeam::channel;

/// Parse human-readable size string (e.g., "1G", "10M", "512K") to bytes
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim().to_uppercase();
    
    // Find the numeric part and unit part
    let (num_part, unit_part) = s.chars()
        .enumerate()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
        .map(|(i, _)| s.split_at(i))
        .unwrap_or((&s, ""));
    
    let num: f64 = num_part.parse()
        .map_err(|_| format!("Invalid number: {}", num_part))?;
    
    let multiplier = match unit_part.trim() {
        "" | "B" => 1u64,
        "K" | "KB" => 1024u64,
        "M" | "MB" => 1024u64 * 1024,
        "G" | "GB" => 1024u64 * 1024 * 1024,
        "T" | "TB" => 1024u64 * 1024 * 1024 * 1024,
        _ => return Err(format!("Unknown unit: {}", unit_part)),
    };
    
    let result = (num * multiplier as f64) as u64;
    Ok(result)
}

#[derive(Parser, Clone)]
#[command(name = "vdbench", version = "1.0", author = "Bifrost")]
struct Opts {
    /// Directory depth (number of levels)
    #[arg(short, long, default_value_t = 1)]
    depth: u32,

    /// Number of files per directory
    #[arg(short, long, default_value_t = 10)]
    files: u32,

    /// Number of subdirectories per directory
    #[arg(short = 'r', long, default_value_t = 5)]
    dirs: u32,

    /// Size of each file (e.g., 1024, 1K, 10M, 1G)
    #[arg(short, long, default_value = "1024", value_parser = parse_size)]
    size: u64,

    /// Output root directory path
    #[arg(short, long, default_value = "./fileset")]
    output: String,

    /// Number of worker threads
    #[arg(short, long, default_value_t = 8)]
    threads: usize,

    /// Skip confirmation prompt
    #[arg(short, long)]
    yes: bool,
}

/// Calculate total files, directories, and size
fn estimate_total(opts: &Opts) -> (u64, u64, u64) {
    let mut total_dirs = 0u64;
    let mut total_files = 0u64;
    
    // At each level, we create dirs directories
    // Level 0: 1 directory (root)
    // Level 1: dirs directories
    // Level 2: dirs^2 directories
    // ... up to depth levels
    
    let dirs_per_level = opts.dirs as u64;
    let files_per_dir = opts.files as u64;
    let depth = opts.depth;
    
    for level in 0..depth {
        let dir_count_at_level = dirs_per_level.pow(level as u32);
        total_dirs += dir_count_at_level;
        total_files += dir_count_at_level * files_per_dir;
    }
    
    let total_size = total_files * opts.size;
    (total_dirs, total_files, total_size)
}

/// Format bytes to human-readable string
fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    
    format!("{:.2} {}", size, UNITS[unit_idx])
}

/// Ask for user confirmation
fn confirm_generation(total_dirs: u64, total_files: u64, total_size: u64) -> bool {
    println!("\n=== Generation Estimate ===");
    println!("Total directories: {}", total_dirs);
    println!("Total files:       {}", total_files);
    println!("Total size:        {} ({} bytes)", format_bytes(total_size), total_size);
    println!();
    
    print!("Do you want to proceed? [y/N]: ");
    io::stdout().flush().unwrap();
    
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    
    let input = input.trim().to_lowercase();
    input == "y" || input == "yes"
}

fn main() {
    let opts = Opts::parse();
    
    // Calculate and display estimate
    let (total_dirs, total_files, total_size) = estimate_total(&opts);
    
    // Ask for confirmation unless -y flag is set
    if !opts.yes {
        if !confirm_generation(total_dirs, total_files, total_size) {
            println!("Generation cancelled.");
            return;
        }
    } else {
        println!("=== Generation Estimate ===");
        println!("Total directories: {}", total_dirs);
        println!("Total files:       {}", total_files);
        println!("Total size:        {} ({} bytes)", format_bytes(total_size), total_size);
        println!("Skipping confirmation (yes flag set).\n");
    }
    
    let start = Instant::now();
    let last_report = Arc::new(AtomicU64::new(0));

    // Create root directory
    let root = PathBuf::from(&opts.output);
    fs::create_dir_all(&root).expect("Failed to create output directory");

    // Shared atomic counters
    let file_count = Arc::new(AtomicU64::new(0));
    let dir_count = Arc::new(AtomicU64::new(0));
    let total_size = Arc::new(AtomicU64::new(0));
    let pending_work = Arc::new(AtomicU64::new(1)); // Start with 1 for root

    // Pre-allocate file content buffer (zero-filled)
    let content = vec![0u8; opts.size as usize];

    // Work queue: (directory_path, current_depth)
    let (tx, rx) = channel::unbounded();
    tx.send((root, 0)).unwrap();

    // Spawn worker threads
    let mut handles = vec![];
    for _ in 0..opts.threads {
        let rx = rx.clone();
        let file_count = Arc::clone(&file_count);
        let dir_count = Arc::clone(&dir_count);
        let total_size = Arc::clone(&total_size);
        let pending = Arc::clone(&pending_work);
        let content = content.clone();
        let opts = opts.clone();
        let tx = tx.clone();

        let handle = thread::spawn(move || {
            loop {
                // Try to receive work item
                match rx.try_recv() {
                    Ok((path, current_depth)) => {
                        if current_depth >= opts.depth {
                            // Just count down and skip
                            pending.fetch_sub(1, Ordering::SeqCst);
                            continue;
                        }

                        // Create subdirectories
                        let mut new_dirs = Vec::with_capacity(opts.dirs as usize);
                        for i in 0..opts.dirs {
                            let subdir = path.join(format!("d{}", i));
                            fs::create_dir_all(&subdir).expect("Failed to create subdirectory");
                            dir_count.fetch_add(1, Ordering::Relaxed);
                            new_dirs.push(subdir);
                        }

                        // Create files
                        for i in 0..opts.files {
                            let file_path = path.join(format!("f{}", i));
                            fs::write(&file_path, &content).expect("Failed to write file");
                            file_count.fetch_add(1, Ordering::Relaxed);
                            total_size.fetch_add(opts.size, Ordering::Relaxed);
                        }

                        // Queue subdirectories for deeper levels
                        let new_work = new_dirs.len() as u64;
                        if new_work > 0 {
                            pending.fetch_add(new_work, Ordering::SeqCst);
                            for subdir in new_dirs {
                                tx.send((subdir, current_depth + 1)).unwrap();
                            }
                        }
                        
                        // Mark current work as done
                        pending.fetch_sub(1, Ordering::SeqCst);
                    }
                    Err(crossbeam::channel::TryRecvError::Empty) => {
                        // No work available, check if we're done
                        if pending.load(Ordering::SeqCst) == 0 {
                            break;
                        }
                        // Brief sleep to avoid busy-waiting
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(crossbeam::channel::TryRecvError::Disconnected) => {
                        break;
                    }
                }
            }
            // Drop tx when worker exits to allow channel to close
            drop(tx);
        });
        handles.push(handle);
    }

    // Progress reporter thread
    let file_count_report = Arc::clone(&file_count);
    let dir_count_report = Arc::clone(&dir_count);
    let total_size_report = Arc::clone(&total_size);
    let report_handle = thread::spawn(move || {
        let mut last_files = 0u64;
        let mut last_report_time = Instant::now();
        
        loop {
            thread::sleep(Duration::from_secs(1));
            
            let files = file_count_report.load(Ordering::Relaxed);
            let dirs = dir_count_report.load(Ordering::Relaxed);
            let size = total_size_report.load(Ordering::Relaxed);
            
            // Exit if we've generated all expected files
            if files >= total_files && files > 0 {
                let elapsed = start.elapsed().as_secs();
                println!(
                    "[Progress] Elapsed: {:3}s | Dirs: {:6} | Files: {:7} | Size: {}",
                    elapsed,
                    dirs,
                    files,
                    format_bytes(size)
                );
                break;
            }
            
            // Print progress every 5 seconds or when there's progress
            let now = Instant::now();
            let should_report = now.duration_since(last_report_time).as_secs() >= 5 
                || (files != last_files && files > 0);
            
            if should_report {
                let elapsed = start.elapsed().as_secs();
                println!(
                    "[Progress] Elapsed: {:3}s | Dirs: {:6} | Files: {:7} | Size: {}",
                    elapsed,
                    dirs,
                    files,
                    format_bytes(size)
                );
                last_report_time = now;
                last_files = files;
            }
            
            // Check if work is done (no progress in last 5 seconds and files > 0)
            if files == last_files && files > 0 && now.duration_since(last_report_time).as_secs() >= 5 {
                break;
            }
        }
    });

    // Close sender and wait for all workers to finish
    drop(tx);
    for handle in handles {
        handle.join().unwrap();
    }
    
    // Wait for reporter to finish
    let _ = report_handle.join();

    // Final stats
    let elapsed = start.elapsed();
    let final_dirs = dir_count.load(Ordering::Relaxed);
    let final_files = file_count.load(Ordering::Relaxed);
    let final_size = total_size.load(Ordering::Relaxed);
    
    println!("\n=== Generation Complete ===");
    println!("Directories created: {}", final_dirs);
    println!("Files created:       {}", final_files);
    println!("Total size:          {} ({} bytes)", format_bytes(final_size), final_size);
    println!("Time elapsed:        {:.2?}", elapsed);
    
    if final_files > 0 {
        let files_per_sec = final_files as f64 / elapsed.as_secs_f64();
        let bytes_per_sec = final_size as f64 / elapsed.as_secs_f64();
        println!("Throughput:          {:.0} files/sec, {}/sec", files_per_sec, format_bytes(bytes_per_sec as u64));
    }
}