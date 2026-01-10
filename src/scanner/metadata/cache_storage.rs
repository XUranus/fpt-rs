//! # Fixed-Size Binary Cache Storage
//!
//! This module provides efficient storage and retrieval of fixed-size index entries
//! (`FileCacheEntry` and `DirCacheEntry`) in compact binary files. Entries are stored
//! **sequentially and densely** (no padding), enabling:
//! - High-throughput writing during scanning.
//! - Fast random access by index (O(1) per lookup).
//! - Memory-mapped or streaming iteration for diffing.
//!
//! Two types of cache files are managed:
//! - **File cache (`fcache_*.dat`)**: Contains `FileCacheEntry` records sorted by file ID.
//! - **Directory cache (`dcache_*.dat`)**: Contains `DirCacheEntry` records sorted by directory ID.
//!
//! Each cache type supports:
//! - **Writer**: Appends entries sequentially to a file.
//! - **Random reader**: Reads any entry by its 0-based index.
//! - **Iterator**: Streams all entries in order.
//! - **Repository reader**: Manages multiple files and routes requests by file ID.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::cell::RefCell;
use bincode::{serialize, deserialize};
use serde::Serialize;
use serde::de::DeserializeOwned;
use super::{DirCacheEntry, FileCacheEntry, FixedSize};

// TODO:: slicing
/// Maximum size (in bytes) of a single cache file before rollover (512 MB).
// const MAX_FILE_SIZE: usize = 128 * 1024 * 1024; // ~= 5,000,000 fcache

/// Filename prefix for file cache files (e.g., `fcache_0.dat`).
const FILE_CACHE_PREFIX: &str = "fcache";

/// Filename prefix for directory cache files (e.g., `dcache_0.dat`).
const DIR_CACHE_PREFIX: &str = "dcache";

/// Writer for a sequence of fixed-size, serializable objects in a binary file.
///
/// Objects are appended **densely and sequentially** without delimiters.
/// The file layout is simply: `[obj0][obj1][obj2]...`
///
/// Not thread-safe. Use external synchronization if shared across threads.
pub struct BinObjectSeqWriter<T: Serialize + FixedSize> {
    /// Path to the output file.
    path: PathBuf,
    /// Buffered writer to reduce syscalls.
    fwriter: BufWriter<File>,
    /// Number of objects written so far.
    index: u32,
    /// Current write offset (in bytes).
    offset: u32,
    /// Phantom marker for the generic type.
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Serialize + FixedSize> BinObjectSeqWriter<T> {
    /// Opens a file for appending new objects.
    ///
    /// If the file exists, writing continues at the end. The initial `index` and `offset`
    /// are derived from the existing file size.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let metadata = file.metadata()?;
        let file_size = metadata.len() as usize;

        // Validate that file size is a multiple of object size
        if file_size % T::SIZE != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "File size is not a multiple of object size",
            ));
        }

        let index = (file_size / T::SIZE) as u32;
        let offset = file_size as u32;

        Ok(Self {
            path,
            fwriter: BufWriter::new(file),
            index,
            offset,
            _phantom: std::marker::PhantomData,
        })
    }

    /// Appends an object to the file and returns its 0-based index.
    pub fn write(&mut self, item: &T) -> io::Result<u32> {
        let buffer = serialize(item).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Serialization failed: {}", e))
        })?;

        // Ensure serialized size matches expected fixed size
        if buffer.len() != T::SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Serialized size {} != expected {}", buffer.len(), T::SIZE),
            ));
        }

        let index = self.index;
        self.fwriter.write_all(&buffer)?;
        self.offset += T::SIZE as u32;
        self.index += 1;
        Ok(index)
    }

    /// Flushes all buffered data to disk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.fwriter.flush()
    }

    /// Returns the current state: `(next_index, next_offset)`.
    pub fn current(&self) -> (u32, u32) {
        (self.index, self.offset)
    }
}

/// Writer for file cache entries (`FileCacheEntry`).
pub type FileCacheWriter = BinObjectSeqWriter<FileCacheEntry>;

/// Writer for directory cache entries (`DirCacheEntry`).
pub type DirCacheWriter = BinObjectSeqWriter<DirCacheEntry>;

impl FileCacheWriter {
    /// Creates a new file cache writer for file ID `fid`.
    pub fn new<P: AsRef<Path>>(base_dir: P, fid: u32) -> io::Result<Self> {
        let path = base_dir.as_ref().join(format!("{}_{}.dat", FILE_CACHE_PREFIX, fid));
        Self::open(path)
    }
}

impl DirCacheWriter {
    /// Creates a new directory cache writer for file ID `fid`.
    pub fn new<P: AsRef<Path>>(base_dir: P, fid: u32) -> io::Result<Self> {
        let path = base_dir.as_ref().join(format!("{}_{}.dat", DIR_CACHE_PREFIX, fid));
        Self::open(path)
    }
}

/// Reader for random access to fixed-size objects in a binary file.
pub struct BinObjectRandomReader<T: DeserializeOwned + FixedSize> {
    /// Path to the input file (for diagnostics).
    path: PathBuf,
    /// Underlying file handle.
    file: File,
    /// file size
    size : u64,
    /// phantom data
    _phantom : std::marker::PhantomData<T>
}

impl<T: DeserializeOwned + FixedSize> BinObjectRandomReader<T> {
    /// Opens a file for random-access reading.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let size = fs::metadata(path.clone())?.size();
        let file = File::open(&path)?;
        Ok(Self { path, file , size, _phantom : std::marker::PhantomData})
    }

    /// Reads the object at the given 0-based index.
    pub fn read_object(&mut self, index: u32) -> io::Result<T> {
        let offset = (index as usize) * T::SIZE;
        if offset >= self.size as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidData, 
                format!("invalid offset: {}, size: {}", offset, self.size)))
        }
        self.file.seek(SeekFrom::Start(offset as u64))?;

        let mut payload = vec![0u8; T::SIZE];
        self.file.read_exact(&mut payload)?;

        let object = deserialize(&payload).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("Deserialization failed: {}", e))
        })?;
        Ok(object)
    }

    /// File size
    pub fn size(&self) -> u64 {
        self.size
    }

    // total num of objects
    pub fn total_count(&self) -> u32 {
        (self.size() / T::SIZE as u64) as u32
    }
}

/// Iterator over all objects in a binary cache file.
pub struct BinaryObjectSeqIterator<T: DeserializeOwned + FixedSize> {
    index: u32,
    total_count: Option<u32>,
    freader: BinObjectRandomReader<T>,
}

impl<T: DeserializeOwned + FixedSize> BinaryObjectSeqIterator<T> {
    /// Creates an iterator from a reader and total object count.
    pub fn new(freader: BinObjectRandomReader<T>, total_count: u32) -> Self {
        Self {
            index: 0,
            total_count: Some(total_count),
            freader,
        }
    }
}

impl<T: DeserializeOwned + FixedSize> Iterator for BinaryObjectSeqIterator<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let total = self.total_count?;
        if self.index >= total {
            return None;
        }

        match self.freader.read_object(self.index) {
            Ok(item) => {
                self.index += 1;
                Some(item)
            }
            Err(_) => None,
        }
    }
}

/// Random reader for file cache entries.
pub type FileCacheRandomReader = BinObjectRandomReader<FileCacheEntry>;

/// Random reader for directory cache entries.
pub type DirCacheRandomReader = BinObjectRandomReader<DirCacheEntry>;

pub type DirCacheIterator = BinaryObjectSeqIterator<DirCacheEntry>;

pub type FileCacheIterator = BinaryObjectSeqIterator<FileCacheEntry>;

impl FileCacheRandomReader {
    /// Opens a file cache reader for file ID `fid`.
    pub fn new<P: AsRef<Path>>(base_dir: P, fid: u32) -> io::Result<Self> {
        let path = base_dir.as_ref().join(format!("{}_{}.dat", FILE_CACHE_PREFIX, fid));
        Self::open(path)
    }
}

impl DirCacheRandomReader {
    /// Opens a directory cache reader for file ID `fid`.
    pub fn new<P: AsRef<Path>>(base_dir: P, fid: u32) -> io::Result<Self> {
        let path = base_dir.as_ref().join(format!("{}_{}.dat", DIR_CACHE_PREFIX, fid));
        Self::open(path)
    }
}

/// Repository reader for accessing cache entries across multiple files.
///
/// Caches open file handles to avoid repeated `open()` syscalls.
/// Not thread-safe due to interior mutability (`RefCell`).
pub struct CacheRepoReader {
    /// Base directory containing cache files.
    base_dir: PathBuf,
    /// Cache of open file cache readers: file_id → reader.
    fcache_reader_map: RefCell<HashMap<u32, FileCacheRandomReader>>,
    /// Cache of open directory cache readers: file_id → reader.
    dcache_reader_map: RefCell<HashMap<u32, DirCacheRandomReader>>,
}

impl CacheRepoReader {
    /// Creates a new repository reader.
    pub fn new<P: AsRef<Path>>(base_dir: P) -> io::Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        Ok(Self {
            base_dir,
            fcache_reader_map: RefCell::new(HashMap::new()),
            dcache_reader_map: RefCell::new(HashMap::new()),
        })
    }

    /// Reads a `FileCacheEntry` by file ID and entry index.
    pub fn read_fcache(&self, fid: u32, index: u32) -> io::Result<FileCacheEntry> {
        let mut map = self.fcache_reader_map.borrow_mut();
        let reader = map.entry(fid).or_insert_with(|| {
            let path = self.base_dir.join(format!("{}_{}.dat", FILE_CACHE_PREFIX, fid));
            FileCacheRandomReader::new(path, fid).expect("Failed to open file cache")
        });
        reader.read_object(index)
    }

    /// Reads a `DirCacheEntry` by file ID and entry index.
    pub fn read_dcache(&self, fid: u32, index: u32) -> io::Result<DirCacheEntry> {
        let mut map = self.dcache_reader_map.borrow_mut();
        let reader = map.entry(fid).or_insert_with(|| {
            let path = self.base_dir.join(format!("{}_{}.dat", DIR_CACHE_PREFIX, fid));
            DirCacheRandomReader::new(path, fid).expect("Failed to open directory cache")
        });
        reader.read_object(index)
    }
}


impl DirCacheIterator {
    pub fn from(freader : DirCacheRandomReader) -> Self {
        let total_count = freader.total_count();
        // visit `DirCacheEntry` from beginning
        Self { index : 0, total_count: Some(total_count), freader }
    }
}

impl FileCacheIterator {
    pub fn from(freader : FileCacheRandomReader, total_count : u32, index : u32) -> Self {
        Self { index, total_count: Some(total_count), freader }
    }
}