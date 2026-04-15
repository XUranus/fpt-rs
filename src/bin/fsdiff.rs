use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use clap::Parser;
use sha2::{Sha256, Digest};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(name = "fsdiff", version = "1.0", author = "Bifrost")]
#[command(about = "Compare two directories and report differences")]
struct Args {
    /// Source directory path
    #[arg(short, long, value_name = "DIR")]
    source: PathBuf,

    /// Target directory path
    #[arg(short, long, value_name = "DIR")]
    target: PathBuf,

    /// Strip prefix from source paths when comparing (e.g., "/opt/dataset")
    #[arg(long, value_name = "PREFIX")]
    strip_source_prefix: Option<PathBuf>,

    /// Strip prefix from target paths when comparing (e.g., "/opt/dataset")
    #[arg(long, value_name = "PREFIX")]
    strip_target_prefix: Option<PathBuf>,

    /// Follow symbolic links
    #[arg(short, long)]
    follow_links: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    size: u64,
    checksum: Option<String>,
    is_symlink: bool,
    symlink_target: Option<PathBuf>,
}

#[derive(Debug)]
struct DiffReport {
    source_only: Vec<PathBuf>,
    target_only: Vec<PathBuf>,
    size_mismatch: Vec<(PathBuf, u64, u64)>,
    checksum_mismatch: Vec<PathBuf>,
    symlink_mismatch: Vec<(PathBuf, Option<PathBuf>, Option<PathBuf>)>,
    identical: Vec<PathBuf>,
}

impl DiffReport {
    fn new() -> Self {
        Self {
            source_only: Vec::new(),
            target_only: Vec::new(),
            size_mismatch: Vec::new(),
            checksum_mismatch: Vec::new(),
            symlink_mismatch: Vec::new(),
            identical: Vec::new(),
        }
    }

    fn has_differences(&self) -> bool {
        !self.source_only.is_empty()
            || !self.target_only.is_empty()
            || !self.size_mismatch.is_empty()
            || !self.checksum_mismatch.is_empty()
            || !self.symlink_mismatch.is_empty()
    }

    fn print_summary(&self) {
        println!("\n=== Diff Report Summary ===");
        println!("Files only in source:      {}", self.source_only.len());
        println!("Files only in target:      {}", self.target_only.len());
        println!("Files with size mismatch:  {}", self.size_mismatch.len());
        println!("Files with checksum diff:  {}", self.checksum_mismatch.len());
        println!("Files with symlink diff:   {}", self.symlink_mismatch.len());
        println!("Identical files:           {}", self.identical.len());
        
        if self.has_differences() {
            println!("\nResult: DIFFERENCES FOUND");
        } else {
            println!("\nResult: DIRECTORIES ARE IDENTICAL");
        }
    }

    fn print_details(&self) {
        if !self.source_only.is_empty() {
            println!("\n--- Files only in source ---");
            for path in &self.source_only {
                println!("  + {}", path.display());
            }
        }

        if !self.target_only.is_empty() {
            println!("\n--- Files only in target ---");
            for path in &self.target_only {
                println!("  - {}", path.display());
            }
        }

        if !self.size_mismatch.is_empty() {
            println!("\n--- Files with size mismatch ---");
            for (path, src_size, tgt_size) in &self.size_mismatch {
                println!("  ! {} (source: {} bytes, target: {} bytes)", 
                    path.display(), src_size, tgt_size);
            }
        }

        if !self.checksum_mismatch.is_empty() {
            println!("\n--- Files with checksum mismatch ---");
            for path in &self.checksum_mismatch {
                println!("  ! {}", path.display());
            }
        }

        if !self.symlink_mismatch.is_empty() {
            println!("\n--- Files with symlink mismatch ---");
            for (path, src_target, tgt_target) in &self.symlink_mismatch {
                println!("  ! {}", path.display());
                println!("      source -> {:?}", src_target);
                println!("      target -> {:?}", tgt_target);
            }
        }

        if !self.identical.is_empty() {
            println!("\n--- Identical files ({}) ---", self.identical.len());
            for path in &self.identical {
                println!("  = {}", path.display());
            }
        }
    }
}

/// Calculate SHA256 checksum of a file
fn calculate_checksum(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    
    Ok(format!("{:x}", hasher.finalize()))
}

/// Collect file information from a directory
fn collect_files(
    base_path: &Path,
    follow_links: bool,
    strip_prefix: Option<&Path>,
) -> io::Result<HashMap<PathBuf, FileInfo>> {
    let mut files = HashMap::new();
    
    let walker = WalkDir::new(base_path)
        .follow_links(follow_links)
        .into_iter();
    
    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        let metadata = if follow_links {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        };
        
        let metadata = match metadata {
            Ok(m) => m,
            Err(_) => continue,
        };
        
        // Get relative path from base
        let relative_path = path.strip_prefix(base_path)
            .unwrap_or(path)
            .to_path_buf();
        
        // Apply additional prefix stripping if requested
        let relative_path = match strip_prefix {
            Some(prefix) => {
                // Try to strip the prefix from the beginning of the relative path
                let path_str = relative_path.to_string_lossy();
                let prefix_str = prefix.to_string_lossy();
                if path_str.starts_with(prefix_str.as_ref()) {
                    PathBuf::from(&path_str[prefix_str.len()..])
                        .strip_prefix("/")
                        .unwrap_or(&PathBuf::from(&path_str[prefix_str.len()..]))
                        .to_path_buf()
                } else {
                    relative_path
                }
            }
            None => relative_path,
        };
        
        let is_symlink = entry.file_type().is_symlink();
        let symlink_target = if is_symlink {
            fs::read_link(path).ok()
        } else {
            None
        };
        
        // Calculate checksum for regular files
        let checksum = if metadata.is_file() && !is_symlink {
            calculate_checksum(path).ok()
        } else {
            None
        };
        
        files.insert(
            relative_path,
            FileInfo {
                path: path.to_path_buf(),
                size: metadata.len(),
                checksum,
                is_symlink,
                symlink_target,
            },
        );
    }
    
    Ok(files)
}

/// Compare two directory structures
fn compare_directories(
    source_path: &Path,
    target_path: &Path,
    source_strip_prefix: Option<&Path>,
    target_strip_prefix: Option<&Path>,
    follow_links: bool,
    verbose: bool,
) -> io::Result<DiffReport> {
    println!("Scanning source directory: {}", source_path.display());
    if let Some(prefix) = source_strip_prefix {
        println!("  Stripping prefix: {}", prefix.display());
    }
    let source_files = collect_files(source_path, follow_links, source_strip_prefix)?;
    println!("  Found {} entries", source_files.len());
    
    println!("Scanning target directory: {}", target_path.display());
    if let Some(prefix) = target_strip_prefix {
        println!("  Stripping prefix: {}", prefix.display());
    }
    let target_files = collect_files(target_path, follow_links, target_strip_prefix)?;
    println!("  Found {} entries", target_files.len());
    
    let mut report = DiffReport::new();
    
    // Check for files in source but not in target, and compare existing files
    for (rel_path, src_info) in &source_files {
        if verbose {
            println!("Comparing: {}", rel_path.display());
        }
        
        match target_files.get(rel_path) {
            None => {
                report.source_only.push(rel_path.clone());
            }
            Some(tgt_info) => {
                // Check if symlink status matches
                if src_info.is_symlink != tgt_info.is_symlink {
                    report.symlink_mismatch.push((
                        rel_path.clone(),
                        src_info.symlink_target.clone(),
                        tgt_info.symlink_target.clone(),
                    ));
                } else if src_info.is_symlink {
                    // Both are symlinks, check targets
                    if src_info.symlink_target != tgt_info.symlink_target {
                        report.symlink_mismatch.push((
                            rel_path.clone(),
                            src_info.symlink_target.clone(),
                            tgt_info.symlink_target.clone(),
                        ));
                    } else {
                        report.identical.push(rel_path.clone());
                    }
                } else {
                    // Both are regular files, compare size and checksum
                    if src_info.size != tgt_info.size {
                        report.size_mismatch.push((
                            rel_path.clone(),
                            src_info.size,
                            tgt_info.size,
                        ));
                    } else if src_info.checksum != tgt_info.checksum {
                        report.checksum_mismatch.push(rel_path.clone());
                    } else {
                        report.identical.push(rel_path.clone());
                    }
                }
            }
        }
    }
    
    // Check for files in target but not in source
    for rel_path in target_files.keys() {
        if !source_files.contains_key(rel_path) {
            report.target_only.push(rel_path.clone());
        }
    }
    
    Ok(report)
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    
    // Validate source directory
    if !args.source.exists() {
        eprintln!("Error: Source directory does not exist: {}", args.source.display());
        std::process::exit(1);
    }
    
    if !args.source.is_dir() {
        eprintln!("Error: Source path is not a directory: {}", args.source.display());
        std::process::exit(1);
    }
    
    // Validate target directory
    if !args.target.exists() {
        eprintln!("Error: Target directory does not exist: {}", args.target.display());
        std::process::exit(1);
    }
    
    if !args.target.is_dir() {
        eprintln!("Error: Target path is not a directory: {}", args.target.display());
        std::process::exit(1);
    }
    
    println!("=== Directory Comparison ===");
    println!("Source: {}", args.source.display());
    println!("Target: {}", args.target.display());
    println!();
    
    let report = compare_directories(
        &args.source,
        &args.target,
        args.strip_source_prefix.as_deref(),
        args.strip_target_prefix.as_deref(),
        args.follow_links,
        args.verbose,
    )?;
    
    report.print_details();
    report.print_summary();
    
    // Exit with error code if differences found
    if report.has_differences() {
        std::process::exit(1);
    }
    
    Ok(())
}
