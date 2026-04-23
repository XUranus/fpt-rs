//! # File Control Block for Backup Operations
//!
//! This module defines the [`FileControlBlock`] (FCB), a central data structure that
//! encapsulates all state required to perform a single file's backup or restore operation.
//!
//! The FCB acts as a **state machine** that tracks progress through the I/O pipeline:
//! - Source file handling (open → read → close)
//! - Target file handling (create → write → close)
//! - Buffer management for efficient data transfer
//!
//! It is designed to be **moved by value** between threads in a message-passing architecture,
//! ensuring thread safety without shared mutable state. Open file handles are only held
//! temporarily during I/O operations and are never stored when the FCB resides in a queue.

use std::path::PathBuf;

use crate::scanner::metadata::{DirMeta, FileMeta};

/// Maximum buffer size for file data (4 MiB).
///
/// Files larger than this are processed in chunks to limit memory usage.
#[allow(dead_code)]
pub(crate) const MAX_FILE_BUFFER_SIZE: usize = 4 * 1024 * 1024;

/// State of the source (input) file during backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceHandleState {
    /// Initial state: file not yet opened.
    Inited,
    /// Entire file content has been read into the buffer.
    Read,
    /// Partial data has been read (used for large files processed in chunks).
    PartialRead,
}

/// State of the target (output) file during backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetHandleState {
    /// Initial state: file not yet created/opened.
    Inited,
    /// Partial data has been written (used for large files processed in chunks).
    PartialWritten,
    /// Entire file content has been written.
    Written,
}

/// File Control Block: orchestrates backup/restore of a single file.
///
/// This struct carries all necessary metadata, paths, buffers, and state to
/// manage a file copy operation across multiple stages (read, write, finalize).
/// It is **not thread-safe** for shared access, but is safe to **move between threads**.
#[derive(Debug)]
pub struct FileControlBlock {
    /// Full metadata of the source file.
    pub meta: Box<FileMeta>,
    /// Buffer holding file data (size ≤ `MAX_FILE_BUFFER_SIZE`).
    pub buffer: Vec<u8>,
    /// Number of valid bytes currently in `buffer` (≤ `buffer.len()`).
    pub buffer_len: usize,
    /// Current state of source file processing.
    pub src_state: SourceHandleState,
    /// Current state of target file processing.
    pub dst_state: TargetHandleState,
    /// Absolute path to the source file.
    pub src_path: PathBuf,
    /// Absolute path to the target file.
    pub dst_path: PathBuf,
    /// Current read offset in the source file (in bytes).
    pub src_offset: u64,
    /// Current write offset in the target file (in bytes).
    pub dst_offset: u64,
}

/// Dir Control Block: orchestrates backup/restore of a single dir.
///
/// This struct carries all necessary metadata, paths to manage a dir creation operation
/// It is **not thread-safe** for shared access, but is safe to **move between threads**.
#[derive(Debug)]
pub struct DirControlBlock {
    /// Full metadata of the source dir.
    #[allow(dead_code)]
    pub meta: Box<DirMeta>,
    /// Absolute path to the source file.
    pub src_path: PathBuf,
    /// Absolute path to the target file.
    pub dst_path: PathBuf,
}

#[derive(Debug)]
pub enum ControlBlockVarient {
    FileControlBlock(FileControlBlock),
    DirControlBlock(DirControlBlock),
}

impl From<FileMeta> for FileControlBlock {
    /// Creates a new `FileControlBlock` from file metadata.
    ///
    /// Initializes without allocating file payload storage up front.
    ///
    /// Buffer memory is allocated lazily when an active read operation starts.
    /// This avoids reserving up to 4 MiB for every queued file control block,
    /// which would otherwise explode memory usage on large file sets.
    ///
    /// Note: `src_path` and `dst_path` must be set externally before use.
    fn from(fmeta: FileMeta) -> Self {
        Self {
            meta: Box::new(fmeta),
            buffer: Vec::new(),
            buffer_len: 0,
            src_state: SourceHandleState::Inited,
            dst_state: TargetHandleState::Inited,
            src_path: PathBuf::new(),
            dst_path: PathBuf::new(),
            src_offset: 0,
            dst_offset: 0,
        }
    }
}

impl From<DirMeta> for DirControlBlock {
    /// Creates a new `DirControlBlock` from dir metadata.
    ///
    /// Note: `src_path` and `dst_path` must be set externally before use.
    fn from(dmeta: DirMeta) -> Self {
        Self {
            meta: Box::new(dmeta),
            src_path: PathBuf::new(),
            dst_path: PathBuf::new(),
        }
    }
}
