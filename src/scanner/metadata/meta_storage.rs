//! # Metadata Storage System
//!
//! This module provides a high-performance, binary-based metadata storage system
//! for backup operations. It supports writing and reading serialized file and
//! directory metadata (`FileMeta` and `DirMeta`) in a structured, append-only format.
//!
//! The design consists of two layers:
//! - **`MetaFileWriter` / `MetaFileReader`**: Handle I/O for a single metadata file.
//! - **`MetaRepoWriter` / `MetaRepoReader`**: Manage a repository of multiple metadata
//!   files (e.g., `meta_0.dat`, `meta_1.dat`, ...) with automatic rollover based on size.
//!
//! All metadata records are stored in a **TLV (Tag-Length-Value)** format:
//! ```text
//! [tag: u8][length: u32 (LE)][payload: serialized struct]
//! ```
//! where `tag` distinguishes between directory (`TAG_DIR = 1`) and file (`TAG_FILE = 2`) entries.
//!
//! This system enables efficient random access to metadata via `MetaEntryLocator`,
//! which encodes the file ID and byte offset of a record within the repository.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use bincode::{serialize, deserialize};

use super::filemeta::{DirMeta, FileMeta};

/// Maximum size (in bytes) of a single metadata file before rollover (2GB).
const MAX_FILE_SIZE: u32 = 2 * 1024 * 1024 * 1024;

/// Tag value indicating a `DirMeta` record.
const TAG_DIR: u8 = 1;

/// Tag value indicating a `FileMeta` record.
const TAG_FILE: u8 = 2;

/// A locator that uniquely identifies a metadata entry in the repository.
///
/// Encodes `(file_id, offset)` where:
/// - `file_id` is the numeric ID of the metadata file (e.g., `0` for `meta_0.dat`)
/// - `offset` is the byte offset within that file where the record starts.
pub type MetaEntryLocator = (u32, u32);

/// Writer for a single metadata file in TLV (Tag-Length-Value) format.
///
/// Records are appended sequentially. Each record consists of:
/// - 1-byte tag (`TAG_DIR` or `TAG_FILE`)
/// - 4-byte little-endian payload length
/// - Variable-length serialized payload (via `bincode`)
///
/// Not thread-safe. Use external synchronization if shared across threads.
pub struct MetaFileWriter {
    /// Path to the metadata file.
    #[allow(dead_code)]
    path: PathBuf,
    /// Buffered writer to reduce syscalls.
    fwriter: BufWriter<File>,
    /// Current write offset (in bytes).
    offset: u32,
}

impl MetaFileWriter {
    /// Opens a new or existing metadata file for appending.
    ///
    /// If the file exists, writing continues at the end.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            fwriter: BufWriter::new(file),
            offset: 0,
        })
    }

    /// Returns the current size of the file (in bytes).
    pub fn size(&self) -> u32 {
        self.offset
    }

    /// Writes a `DirMeta` record and returns its starting offset.
    pub fn write_dirmeta(&mut self, dir: &DirMeta) -> io::Result<u32> {
        let payload = serialize(dir).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Serialization failed: {}", e))
        })?;
        let offset = self.offset;
        self.write_entry(TAG_DIR, &payload)?;
        Ok(offset)
    }

    /// Writes a `FileMeta` record and returns its starting offset.
    pub fn write_filemeta(&mut self, file: &FileMeta) -> io::Result<u32> {
        let payload = serialize(file).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Serialization failed: {}", e))
        })?;
        let offset = self.offset;
        self.write_entry(TAG_FILE, &payload)?;
        Ok(offset)
    }

    /// Writes a raw TLV record to the file.
    fn write_entry(&mut self, tag: u8, payload: &[u8]) -> io::Result<()> {
        let record_size = 1 + 4 + payload.len();
        assert!(record_size <= u32::MAX as usize, "Record too large");

        self.fwriter.write_all(&[tag])?;
        self.fwriter.write_all(&(payload.len() as u32).to_le_bytes())?;
        self.fwriter.write_all(payload)?;
        self.offset += record_size as u32;
        Ok(())
    }

    /// Flushes all buffered data to disk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.fwriter.flush()
    }
}

/// Writer for a repository of metadata files with automatic rollover.
///
/// Manages multiple `meta_<id>.dat` files in a base directory. When a file
/// reaches the configured maximum size (or the default `MAX_FILE_SIZE`),
/// it automatically rolls over to a new file.
///
/// Not thread-safe. Use external synchronization if needed.
pub struct MetaRepoWriter {
    /// Base directory containing metadata files.
    base_dir: PathBuf,
    /// Maximum size per metadata file (in bytes). Uses `MAX_FILE_SIZE` if `None`.
    max_size: Option<u32>,
    /// Current active writer.
    current_writer: MetaFileWriter,
    /// ID of the current metadata file (e.g., `0` → `meta_0.dat`).
    current_file_id: u32,
}

impl MetaRepoWriter {
    /// Initializes a new metadata repository.
    ///
    /// Creates the base directory if it doesn't exist and opens the first metadata file (`meta_0.dat`).
    pub fn new<P: AsRef<Path>>(base_dir: P) -> io::Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&base_dir)?;

        let current_file_id = 0;
        let dat_path = base_dir.join(format!("meta_{}.dat", current_file_id));
        let current_writer = MetaFileWriter::open(dat_path)?;

        Ok(Self {
            base_dir,
            max_size: None,
            current_writer,
            current_file_id,
        })
    }

    /// Sets the maximum size (in bytes) for each metadata file.
    pub fn max_size(mut self, max_size: u32) -> Self {
        self.max_size = Some(max_size);
        self
    }

    /// Returns the path of the currently active metadata file.
    fn current_file_path(&self) -> PathBuf {
        self.base_dir.join(format!("meta_{}.dat", self.current_file_id))
    }

    /// Checks if a new metadata file should be started based on available space.
    fn check_room(&mut self, needed: u32) -> io::Result<()> {
        let max_size = self.max_size.unwrap_or(MAX_FILE_SIZE);
        if self.current_writer.size() + needed > max_size {
            self.current_writer.flush()?;
            self.current_file_id += 1;
            let new_path = self.current_file_path();
            self.current_writer = MetaFileWriter::open(new_path)?;
        }
        Ok(())
    }

    /// Writes a `DirMeta` and returns its locator in the repository.
    pub fn write_dirmeta(&mut self, dirmeta: &DirMeta) -> io::Result<MetaEntryLocator> {
        // Estimate max possible serialized size (conservative)
        let needed = 1 + 4 + bincode::serialized_size(dirmeta).unwrap_or(4096) as u32;
        self.check_room(needed)?;
        let offset = self.current_writer.write_dirmeta(dirmeta)?;
        Ok((self.current_file_id, offset))
    }

    /// Writes a `FileMeta` and returns its locator in the repository.
    pub fn write_filemeta(&mut self, filemeta: &FileMeta) -> io::Result<MetaEntryLocator> {
        let needed = 1 + 4 + bincode::serialized_size(filemeta).unwrap_or(4096) as u32;
        self.check_room(needed)?;
        let offset = self.current_writer.write_filemeta(filemeta)?;
        Ok((self.current_file_id, offset))
    }
}

// --- Reader (separate from writer to allow concurrent read-only access) ---

/// Enum representing a deserialized metadata variant.
#[derive(Debug)]
pub enum MetaVariant {
    /// A directory metadata entry.
    Dir(DirMeta),
    /// A file metadata entry.
    File(FileMeta),
}

/// Reader for a single metadata file supporting random access by offset.
pub struct MetaFileReader {
    /// Path to the metadata file (for diagnostics).
    #[allow(dead_code)]
    path: PathBuf,
    /// Underlying file handle.
    file: File,
}

impl MetaFileReader {
    /// Opens a metadata file for reading.
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        Ok(Self { path, file })
    }

    /// Reads and deserializes a metadata record at the given byte offset.
    pub fn get_meta(&mut self, offset: u32) -> io::Result<MetaVariant> {
        self.file.seek(SeekFrom::Start(offset as u64))?;

        let mut tag = [0u8; 1];
        self.file.read_exact(&mut tag)?;

        let mut len_bytes = [0u8; 4];
        self.file.read_exact(&mut len_bytes)?;
        let payload_len = u32::from_le_bytes(len_bytes) as usize;

        let mut payload = vec![0u8; payload_len];
        self.file.read_exact(&mut payload)?;

        match tag[0] {
            TAG_DIR => {
                let dir: DirMeta = deserialize(&payload)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(MetaVariant::Dir(dir))
            }
            TAG_FILE => {
                let file_meta: FileMeta = deserialize(&payload)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(MetaVariant::File(file_meta))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid tag {} in meta file", tag[0]),
            )),
        }
    }
}

/// Reader for a metadata repository supporting lookup by `MetaEntryLocator`.
///
/// Caches open file handles to avoid repeated `open()` syscalls.
/// Not thread-safe due to interior mutability (`RefCell`).
pub struct MetaRepoReader {
    /// Base directory of the metadata repository.
    base_dir: PathBuf,
    /// Cache of open file handles: file_id → MetaFileReader.
    file_handle_map: std::cell::RefCell<HashMap<u32, MetaFileReader>>,
}

impl MetaRepoReader {
    /// Creates a new repository reader.
    pub fn new<P: AsRef<Path>>(base_dir: P) -> io::Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        Ok(Self {
            base_dir,
            file_handle_map: std::cell::RefCell::new(HashMap::new()),
        })
    }

    /// Retrieves a metadata entry by its locator.
    pub fn get_meta(&self, meta_loc: MetaEntryLocator) -> io::Result<MetaVariant> {
        let (file_id, offset) = meta_loc;
        let mut cache = self.file_handle_map.borrow_mut();

        let reader = cache.entry(file_id).or_insert_with(|| {
            let path = self.base_dir.join(format!("meta_{}.dat", file_id));
            MetaFileReader::new(path).expect("Failed to open metadata file")
        });

        reader.get_meta(offset)
    }

    /// Retrieves a `FileMeta` by locator.
    ///
    /// # Panics
    /// Panics if the locator does not point to a `FileMeta`.
    pub fn get_fmeta(&self, meta_loc: MetaEntryLocator) -> io::Result<FileMeta> {
        match self.get_meta(meta_loc)? {
            MetaVariant::File(f) => Ok(f),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Locator does not point to a FileMeta",
            )),
        }
    }

    /// Retrieves a `DirMeta` by locator.
    ///
    /// # Panics
    /// Panics if the locator does not point to a `DirMeta`.
    pub fn get_dmeta(&self, meta_loc: MetaEntryLocator) -> io::Result<DirMeta> {
        match self.get_meta(meta_loc)? {
            MetaVariant::Dir(d) => Ok(d),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Locator does not point to a DirMeta",
            )),
        }
    }
}