//! # Filesystem Scanner
//!
//! This module provides a high-performance, parallel filesystem scanner for backup systems.
//! It recursively traverses directory trees, collects rich metadata (including xattrs,
//! ACLs, symlinks), and writes results to disk in batches for crash resilience.
//!
//! ## Architecture
//!
//! The scanner uses a **multi-threaded pipeline**:
//! - **Traversal workers**: Process directories from a spillable queue, collect file metadata.
//! - **Metadata writers**: Serialize and persist metadata to disk in background threads.
//! - **Shared queues**: Coordinate work between components with bounded memory usage.
//!
//! Key features:
//! - Resumable scanning via checkpointed batch writes.
//! - Configurable depth limits, hidden file handling, and symlink following.
//! - Real-time statistics tracking (file count, size, errors).
//! - Automatic spilling to disk when memory pressure is high.

use std::path::PathBuf;

use crate::frame::control_files::{
    classify_control_file_name, find_primary_control_file,
};

use crate::utility::{BlockingQueue, SpillQueue};
use models::{DirBatchScanResult, DirScanEntry, ScanStatistics};
use options::ControlPathOption;

pub(crate) mod engine;
pub mod filter;
pub mod metadata;
pub(crate) mod models;
pub mod options;

pub use filter::ScanPathFilterSet;
pub use models::ScanStatsSnapshot;
pub use options::ScanOption;

/// Shared context passed to all scanner worker threads.
///
/// Contains configuration, work queues, and statistics counters.
/// Cloning this struct only clones the `Arc` handles—no deep copying occurs.
#[derive(Clone)]
pub struct ScanWorkerContext {
    /// Immutable scan configuration.
    pub scan_option: std::sync::Arc<ScanOption>,
    /// Queue of directories pending traversal.
    pub dirent_queue: std::sync::Arc<SpillQueue<DirScanEntry>>,
    /// Queue of completed scan batches ready for serialization.
    pub output_queue: std::sync::Arc<BlockingQueue<DirBatchScanResult>>,
    /// Real-time scan statistics (atomically updated).
    pub stats: std::sync::Arc<ScanStatistics>,
    /// Optional failure recorder shared by scan workers.
    pub failure_recorder: Option<crate::failure::FailureRecorder>,
}

pub(crate) fn normalize_control_artifacts(scan_option: &ScanOption) -> Result<(), String> {
    normalize_copy_controls(scan_option)?;
    normalize_delete_control_file(scan_option)?;
    normalize_mtime_control_file(scan_option)?;
    normalize_hardlink_control_file(scan_option)?;
    Ok(())
}

fn normalize_copy_controls(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{
        ControlEntry, ControlFileHeader, ControlFileReader, ControlFileWriter,
    };

    let ctrl_dir = &scan_option.target_dir.ctrl_dir;
    let entries = std::fs::read_dir(ctrl_dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if classify_control_file_name(file_name) != Some("copy") {
            continue;
        }

        let reader = ControlFileReader::open(&path).map_err(|e| e.to_string())?;
        let rewritten: Vec<ControlEntry> = reader
            .map(|result| {
                result.map(|entry| match entry {
                    ControlEntry::Dir(mut dir) => {
                        dir.path = normalize_control_path(&scan_option.control_path, &dir.path);
                        ControlEntry::Dir(dir)
                    }
                    ControlEntry::File(file) => ControlEntry::File(file),
                })
            })
            .collect::<Result<_, _>>()
            .map_err(|e: std::io::Error| e.to_string())?;

        let tmp = path.with_extension("tmp");
        let header = ControlFileHeader {
            source_kind: scan_option.control_path.source_kind.clone(),
            source_root: scan_option.control_path.source_root.clone(),
            ..ControlFileHeader::default()
        };
        let mut writer =
            ControlFileWriter::new_with_header(&tmp, &header).map_err(|e| e.to_string())?;
        for entry in &rewritten {
            match entry {
                ControlEntry::Dir(dir) => writer.write_dir(dir).map_err(|e| e.to_string())?,
                ControlEntry::File(file) => writer.write_file(file).map_err(|e| e.to_string())?,
            }
        }
        writer.finish().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn normalize_delete_control_file(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{DeleteControlFileReader, DeleteControlFileWriter};

    let Some(path) = find_primary_control_file(&scan_option.target_dir.ctrl_dir, "delete") else {
        return Ok(());
    };

    let reader = DeleteControlFileReader::open(&path).map_err(|e| e.to_string())?;
    let entries: Vec<_> = reader
        .collect::<Result<_, _>>()
        .map_err(|e: std::io::Error| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut writer = DeleteControlFileWriter::new_with_source(
        &tmp,
        &scan_option.control_path.source_kind,
        &scan_option.control_path.source_root,
    )
    .map_err(|e| e.to_string())?;
    for entry in entries {
        writer
            .write_entry(&crate::scanner::metadata::DeleteEntry {
                entry_type: entry.entry_type,
                path: normalize_control_path(&scan_option.control_path, &entry.path),
            })
            .map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_mtime_control_file(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{MtimeControlFileReader, MtimeControlFileWriter};

    let Some(path) = find_primary_control_file(&scan_option.target_dir.ctrl_dir, "mtime") else {
        return Ok(());
    };

    let reader = MtimeControlFileReader::open(&path).map_err(|e| e.to_string())?;
    let entries: Vec<_> = reader
        .collect::<Result<_, _>>()
        .map_err(|e: std::io::Error| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut writer = MtimeControlFileWriter::new_with_source(
        &tmp,
        &scan_option.control_path.source_kind,
        &scan_option.control_path.source_root,
    )
    .map_err(|e| e.to_string())?;
    for mut entry in entries {
        entry.path = normalize_control_path(&scan_option.control_path, &entry.path);
        writer.write_dir(&entry).map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn normalize_hardlink_control_file(scan_option: &ScanOption) -> Result<(), String> {
    use crate::scanner::metadata::{
        HardlinkControlFileReader, HardlinkControlFileWriter, HardlinkEntry, HardlinkFileEntry,
    };

    let Some(path) = find_primary_control_file(&scan_option.target_dir.ctrl_dir, "hardlink") else {
        return Ok(());
    };
    let reader = HardlinkControlFileReader::open(&path).map_err(|e| e.to_string())?;
    let entries: Vec<_> = reader
        .collect::<Result<_, _>>()
        .map_err(|e: std::io::Error| e.to_string())?;
    let tmp = path.with_extension("tmp");
    let mut writer = HardlinkControlFileWriter::new_with_source(
        &tmp,
        &scan_option.control_path.source_kind,
        &scan_option.control_path.source_root,
    )
    .map_err(|e| e.to_string())?;
    for entry in entries {
        match entry {
            HardlinkEntry::Inode(inode) => writer.write_inode(&inode).map_err(|e| e.to_string())?,
            HardlinkEntry::File(file) => writer
                .write_file(&HardlinkFileEntry {
                    path: normalize_control_path(&scan_option.control_path, &file.path),
                    ..file
                })
                .map_err(|e| e.to_string())?,
        }
    }
    writer.finish().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_control_path(cfg: &ControlPathOption, path: &str) -> String {
    let physical = PathBuf::from(path);
    let logical_root = PathBuf::from(&cfg.source_root);
    if !physical.starts_with(&cfg.physical_base) && physical.starts_with(&logical_root) {
        return path.to_string();
    }
    let base = cfg.physical_base.clone();
    let rel = physical
        .strip_prefix(&base)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| physical.clone());

    format!("/{}", rel.to_string_lossy().trim_start_matches('/'))
}
