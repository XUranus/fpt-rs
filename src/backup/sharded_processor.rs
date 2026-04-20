//! # Sharded Control File Processor
//!
//! This module provides concurrent processing of sharded control files
//! for the backup engine. It enables parallel execution of backup phases
//! across multiple shards.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                 ShardedControlProcessor                     │
//! └───────────────────────┬─────────────────────────────────────┘
//!                         │
//!         ┌───────────────┼───────────────┐
//!         ▼               ▼               ▼
//! ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
//! │   Shard 0    │ │   Shard 1    │ │   Shard N    │
//! │  Processor   │ │  Processor   │ │  Processor   │
//! └──────────────┘ └──────────────┘ └──────────────┘
//!         │               │               │
//!         └───────────────┼───────────────┘
//!                         ▼
//!              ┌─────────────────────┐
//!              │   Result Aggregator │
//!              └─────────────────────┘
//! ```
//!
//! Each shard processor handles one control file independently,
//! enabling parallel I/O and CPU utilization.

use std::path::{Path, PathBuf};
use std::thread;

use crate::scanner::metadata::ShardedControlInfo;

/// Configuration for sharded control file processing.
#[derive(Debug, Clone)]
pub struct ShardedProcessorConfig {
    /// Number of concurrent shard processors
    pub concurrency: usize,
    /// Source directory (for copy phase)
    pub source_dir: Option<PathBuf>,
    /// Target/backup directory
    pub target_dir: PathBuf,
    /// Work directory for intermediate files
    pub work_dir: PathBuf,
    /// Whether to enable hardlink phase
    pub enable_hardlink: bool,
    /// Whether to enable delete phase
    pub enable_delete: bool,
    /// Whether to enable mtime phase
    pub enable_mtime: bool,
}

impl Default for ShardedProcessorConfig {
    fn default() -> Self {
        Self {
            concurrency: std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(4),
            source_dir: None,
            target_dir: PathBuf::new(),
            work_dir: PathBuf::new(),
            enable_hardlink: true,
            enable_delete: true,
            enable_mtime: true,
        }
    }
}

/// Result from processing a single shard.
#[derive(Debug, Default)]
pub struct ShardResult {
    /// Shard identifier
    pub shard_id: usize,
    /// Number of files processed
    pub files_processed: u64,
    /// Number of directories processed
    pub dirs_processed: u64,
    /// Number of bytes transferred
    pub bytes_transferred: u64,
    /// Number of errors
    pub errors: u64,
}

/// Aggregated results from all shards.
#[derive(Debug, Default)]
pub struct ShardedResults {
    /// Results per shard
    pub shard_results: Vec<ShardResult>,
    /// Total files processed
    pub total_files: u64,
    /// Total directories processed
    pub total_dirs: u64,
    /// Total bytes transferred
    pub total_bytes: u64,
    /// Total errors
    pub total_errors: u64,
}

/// Processes sharded control files concurrently.
pub struct ShardedControlProcessor {
    config: ShardedProcessorConfig,
}

impl ShardedControlProcessor {
    /// Creates a new sharded control processor.
    pub fn new(config: ShardedProcessorConfig) -> Self {
        Self { config }
    }

    /// Processes copy phase sharded control files.
    ///
    /// This method discovers all copy_*.txt files in the control directory
    /// and processes them concurrently.
    pub fn process_copy_phase(
        &self,
        ctrl_info: &ShardedControlInfo,
    ) -> anyhow::Result<ShardedResults> {
        let shard_files = &ctrl_info.shard_files;
        let concurrency = self.config.concurrency.min(shard_files.len());
        
        log::info!(
            "Processing copy phase with {} shards using {} workers",
            shard_files.len(),
            concurrency
        );

        // Process shards concurrently using thread pool
        let results: Vec<ShardResult> = thread::scope(|s| {
            let mut handles = Vec::new();
            
            // Create worker threads
            for chunk in shard_files.chunks((shard_files.len() + concurrency - 1) / concurrency) {
                let _config = &self.config;
                let handle = s.spawn(move || {
                    let mut chunk_results = Vec::new();
                    
                    for shard_path in chunk {
                        match self.process_copy_shard(shard_path) {
                            Ok(result) => chunk_results.push(result),
                            Err(e) => {
                                log::error!("Failed to process shard {:?}: {}", shard_path, e);
                                chunk_results.push(ShardResult {
                                    errors: 1,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    
                    chunk_results
                });
                handles.push(handle);
            }
            
            // Collect results
            let mut all_results = Vec::new();
            for handle in handles {
                if let Ok(results) = handle.join() {
                    all_results.extend(results);
                }
            }
            
            all_results
        });

        Ok(self.aggregate_results(results))
    }

    /// Processes a single copy shard.
    fn process_copy_shard(&self, shard_path: &Path) -> anyhow::Result<ShardResult> {
        let shard_id = self.extract_shard_id(shard_path)?;
        
        log::debug!("Processing copy shard {}: {:?}", shard_id, shard_path);
        
        // TODO: Implement actual copy processing using existing CopyEngine
        // For now, return placeholder result
        Ok(ShardResult {
            shard_id,
            files_processed: 0,
            dirs_processed: 0,
            bytes_transferred: 0,
            errors: 0,
        })
    }

    /// Processes delete phase sharded control files.
    pub fn process_delete_phase(
        &self,
        ctrl_info: &ShardedControlInfo,
    ) -> anyhow::Result<ShardedResults> {
        log::info!(
            "Processing delete phase with {} shards",
            ctrl_info.shard_files.len()
        );

        // Similar to copy phase but for deletes
        let mut results = Vec::new();
        
        for shard_path in &ctrl_info.shard_files {
            match self.process_delete_shard(shard_path) {
                Ok(result) => results.push(result),
                Err(e) => {
                    log::error!("Failed to process delete shard {:?}: {}", shard_path, e);
                }
            }
        }

        Ok(self.aggregate_results(results))
    }

    /// Processes a single delete shard.
    fn process_delete_shard(&self, shard_path: &Path) -> anyhow::Result<ShardResult> {
        let shard_id = self.extract_shard_id(shard_path)?;
        
        log::debug!("Processing delete shard {}: {:?}", shard_id, shard_path);
        
        // TODO: Implement actual delete processing
        Ok(ShardResult {
            shard_id,
            files_processed: 0,
            dirs_processed: 0,
            bytes_transferred: 0,
            errors: 0,
        })
    }

    /// Processes hardlink phase sharded control files.
    pub fn process_hardlink_phase(
        &self,
        ctrl_info: &ShardedControlInfo,
    ) -> anyhow::Result<ShardedResults> {
        if !self.config.enable_hardlink {
            log::info!("Hardlink phase disabled");
            return Ok(ShardedResults::default());
        }

        log::info!(
            "Processing hardlink phase with {} shards",
            ctrl_info.shard_files.len()
        );

        // TODO: Implement hardlink phase processing
        Ok(ShardedResults::default())
    }

    /// Processes mtime phase sharded control files.
    pub fn process_mtime_phase(
        &self,
        ctrl_info: &ShardedControlInfo,
    ) -> anyhow::Result<ShardedResults> {
        if !self.config.enable_mtime {
            log::info!("Mtime phase disabled");
            return Ok(ShardedResults::default());
        }

        log::info!(
            "Processing mtime phase with {} shards",
            ctrl_info.shard_files.len()
        );

        // TODO: Implement mtime phase processing
        Ok(ShardedResults::default())
    }

    /// Extracts shard ID from shard file path.
    ///
    /// Path format: {base_name}_{shard_id}_{file_index}.txt
    fn extract_shard_id(&self, path: &Path) -> anyhow::Result<usize> {
        let file_name = path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid shard path"))?;
        
        let parts: Vec<_> = file_name.split('_').collect();
        if parts.len() < 2 {
            return Err(anyhow::anyhow!("Invalid shard file name format"));
        }
        
        usize::from_str_radix(parts[1], 16)
            .map_err(|e| anyhow::anyhow!("Invalid shard ID: {}", e))
    }

    /// Aggregates results from all shards.
    fn aggregate_results(&self, shard_results: Vec<ShardResult>) -> ShardedResults {
        let mut total = ShardedResults {
            shard_results,
            ..Default::default()
        };
        
        for result in &total.shard_results {
            total.total_files += result.files_processed;
            total.total_dirs += result.dirs_processed;
            total.total_bytes += result.bytes_transferred;
            total.total_errors += result.errors;
        }
        
        total
    }
}

/// Builder for sharded backup operations.
pub struct ShardedBackupBuilder {
    config: ShardedProcessorConfig,
}

impl ShardedBackupBuilder {
    /// Creates a new sharded backup builder.
    pub fn new(target_dir: PathBuf) -> Self {
        Self {
            config: ShardedProcessorConfig {
                target_dir,
                ..Default::default()
            },
        }
    }

    /// Sets the source directory.
    pub fn source_dir(mut self, dir: PathBuf) -> Self {
        self.config.source_dir = Some(dir);
        self
    }

    /// Sets the work directory.
    pub fn work_dir(mut self, dir: PathBuf) -> Self {
        self.config.work_dir = dir;
        self
    }

    /// Sets the concurrency level.
    pub fn concurrency(mut self, n: usize) -> Self {
        self.config.concurrency = n;
        self
    }

    /// Disables hardlink phase.
    pub fn disable_hardlink(mut self) -> Self {
        self.config.enable_hardlink = false;
        self
    }

    /// Disables delete phase.
    pub fn disable_delete(mut self) -> Self {
        self.config.enable_delete = false;
        self
    }

    /// Disables mtime phase.
    pub fn disable_mtime(mut self) -> Self {
        self.config.enable_mtime = false;
        self
    }

    /// Builds and returns the processor.
    pub fn build(self) -> ShardedControlProcessor {
        ShardedControlProcessor::new(self.config)
    }
}

/// Discovers and processes all sharded control files for a complete backup.
pub fn process_sharded_backup(
    ctrl_dir: &Path,
    config: ShardedProcessorConfig,
) -> anyhow::Result<ShardedResults> {
    let processor = ShardedControlProcessor::new(config);
    
    // Discover sharded control files
    let copy_info = crate::scanner::metadata::discover_sharded_controls(ctrl_dir, "copy")?;
    let delete_info = crate::scanner::metadata::discover_sharded_controls(ctrl_dir, "delete")?;
    let hardlink_info = crate::scanner::metadata::discover_sharded_controls(ctrl_dir, "hardlink")?;
    let mtime_info = crate::scanner::metadata::discover_sharded_controls(ctrl_dir, "mtime")?;
    
    // Process each phase
    let mut combined_results = ShardedResults::default();
    
    // Copy phase
    let copy_results = processor.process_copy_phase(&copy_info)?;
    combined_results.total_files += copy_results.total_files;
    combined_results.total_dirs += copy_results.total_dirs;
    combined_results.total_bytes += copy_results.total_bytes;
    combined_results.total_errors += copy_results.total_errors;
    
    // Hardlink phase
    let hardlink_results = processor.process_hardlink_phase(&hardlink_info)?;
    combined_results.total_files += hardlink_results.total_files;
    combined_results.total_errors += hardlink_results.total_errors;
    
    // Delete phase
    let delete_results = processor.process_delete_phase(&delete_info)?;
    combined_results.total_files += delete_results.total_files;
    combined_results.total_dirs += delete_results.total_dirs;
    combined_results.total_errors += delete_results.total_errors;
    
    // Mtime phase
    let mtime_results = processor.process_mtime_phase(&mtime_info)?;
    combined_results.total_files += mtime_results.total_files;
    combined_results.total_dirs += mtime_results.total_dirs;
    combined_results.total_errors += mtime_results.total_errors;
    
    Ok(combined_results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_shard_id() {
        let config = ShardedProcessorConfig::default();
        let processor = ShardedControlProcessor::new(config);
        
        let path = Path::new("/tmp/copy_0000000A_0001.txt");
        assert_eq!(processor.extract_shard_id(path).unwrap(), 10);
        
        let path = Path::new("/tmp/copy_000000FF_0000.txt");
        assert_eq!(processor.extract_shard_id(path).unwrap(), 255);
    }

    #[test]
    fn test_aggregate_results() {
        let config = ShardedProcessorConfig::default();
        let processor = ShardedControlProcessor::new(config);
        
        let shard_results = vec![
            ShardResult {
                shard_id: 0,
                files_processed: 100,
                dirs_processed: 10,
                bytes_transferred: 1000000,
                errors: 0,
            },
            ShardResult {
                shard_id: 1,
                files_processed: 200,
                dirs_processed: 20,
                bytes_transferred: 2000000,
                errors: 1,
            },
        ];
        
        let aggregated = processor.aggregate_results(shard_results);
        
        assert_eq!(aggregated.total_files, 300);
        assert_eq!(aggregated.total_dirs, 30);
        assert_eq!(aggregated.total_bytes, 3000000);
        assert_eq!(aggregated.total_errors, 1);
    }
}
