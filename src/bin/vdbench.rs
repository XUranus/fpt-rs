use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use clap::Parser;
use crossbeam::channel;

#[derive(Parser, Clone)]
#[command(name = "filegen", version = "1.0", author = "Your Name")]
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

    /// Size of each file in bytes
    #[arg(short, long, default_value_t = 1024)]
    size: u64,

    /// Output root directory path
    #[arg(short, long, default_value = "./fileset")]
    output: String,

    /// Number of worker threads
    #[arg(short, long, default_value_t = 8)]
    threads: usize,
}

fn main() {
    let opts = Opts::parse();
    let start = Instant::now();

    // Create root directory
    let root = PathBuf::from(&opts.output);
    fs::create_dir_all(&root).expect("Failed to create output directory");

    // Shared atomic counters
    let file_count = Arc::new(AtomicU64::new(0));
    let dir_count = Arc::new(AtomicU64::new(0));
    let total_size = Arc::new(AtomicU64::new(0));

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
        let content = content.clone();
        let opts = opts.clone();
        let tx = tx.clone();

        let handle = thread::spawn(move || {
            while let Ok((path, current_depth)) = rx.recv() {
                if current_depth >= opts.depth {
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
                for subdir in new_dirs {
                    tx.send((subdir, current_depth + 1)).unwrap();
                }
            }
        });
        handles.push(handle);
    }

    // Close sender and wait for all workers to finish
    drop(tx);
    for handle in handles {
        handle.join().unwrap();
    }

    // Final stats
    let elapsed = start.elapsed();
    println!("Generated:");
    println!("  Directories: {}", dir_count.load(Ordering::Relaxed));
    println!("  Files:       {}", file_count.load(Ordering::Relaxed));
    println!("  Total Size:  {} bytes", total_size.load(Ordering::Relaxed));
    println!("Time elapsed: {:.2?}", elapsed);
}