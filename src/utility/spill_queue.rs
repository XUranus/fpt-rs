//! A spillable, disk-backed FIFO queue for handling large volumes of serializable data.
//!
//! This module provides [`SpillQueue`], a thread-safe queue that holds items in memory up to a
//! configurable upper bound. When the memory limit is exceeded, the newest items are spilled to
//! temporary files on disk in fixed-size batches. When the in-memory portion falls below a lower
//! bound during consumption, items are loaded back from disk in batches.
//!
//! The queue maintains strict FIFO ordering across both memory and disk, and is designed for
//! high-throughput backup or scanning workloads where the total number of items (e.g., file paths
//! or metadata entries) may exceed available RAM—such as when processing over 100 million files.
//!
//! ## Key Features
//!
//! - **Memory-bounded**: Configurable upper and lower memory thresholds.
//! - **Disk spilling**: Automatically spills excess items to disk in batches.
//! - **FIFO semantics**: Preserves order across memory and disk.
//! - **Thread-safe**: Internally synchronized with `Arc<Mutex<...>>`.
//! - **Serializable items**: Requires items to implement `serde::Serialize` and
//!   `serde::DeserializeOwned`.
//! - **Crash-resilient design**: Uses isolated cache files per batch (though not transactional).
//!
//! ## Configuration
//!
//! - `memory_upper_bound`: Maximum number of items to keep in memory before spilling.
//! - `memory_lower_bound`: Minimum number of items to keep in memory; if below this during `pop`,
//!   more data is loaded from disk.
//! - `spill_load_batch_size`: Number of items per disk batch (must be ≤ `memory_upper_bound -
//!   memory_lower_bound`).
//!
//! ## File Format
//!
//! Each spilled batch is stored as a file named `{id}.qcache.bin` in the provided `cache_dir`,
//! with items serialized sequentially using `bincode`. Files are deleted once fully consumed.
//!
//! ## Limitations
//!
//! - The cache directory must be **empty** at initialization.
//! - Not suitable for long-term persistence—designed for transient, large-scale queuing.
//! - Disk I/O is synchronous and blocking; intended for use with blocking I/O threads (BIO model).
//!
//! ## Example
//!
//! ```rust,ignore
//! use tempfile::TempDir;
//! use fpt::utility::SpillQueue;
//!
//! let temp_dir = TempDir::new().unwrap();
//! let queue = SpillQueue::<i32>::new(temp_dir.path().to_path_buf(), 3, 1, 2).unwrap();
//!
//! for i in 0..6 {
//!     queue.push(i).unwrap();
//! }
//!
//! for i in 0..6 {
//!     assert_eq!(queue.pop().unwrap(), Some(i));
//! }
//! ```
//!
//! ## Error Handling
//!
//! Operations may fail due to I/O errors (e.g., disk full), serialization issues, or invalid
//! configuration. All errors are wrapped in [`SpillQueueError`].
//!
//! ---
//!
//! This implementation is optimized for **backup/scan engines** that generate massive metadata
//! streams and need to avoid out-of-memory conditions while preserving ordering and minimizing
//! disk seeks through batched I/O.

use std::{
    collections::VecDeque,
    fmt, fs,
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use bincode::{deserialize_from, serialize_into};
use log::debug;

/// Errors that can occur during spill queue operations.
#[derive(Debug)]
pub enum SpillQueueError {
    /// An I/O error occurred (e.g., disk full, permission denied).
    Io(std::io::Error),
    /// A serialization or deserialization error occurred.
    Serialization(bincode::Error),
    /// Invalid configuration parameters were provided.
    InvalidConfig,
}

impl From<std::io::Error> for SpillQueueError {
    fn from(err: std::io::Error) -> Self {
        SpillQueueError::Io(err)
    }
}

impl From<bincode::Error> for SpillQueueError {
    fn from(err: bincode::Error) -> Self {
        SpillQueueError::Serialization(err)
    }
}

impl fmt::Display for SpillQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpillQueueError::Io(e) => write!(f, "I/O Error: {}", e),
            SpillQueueError::Serialization(e) => write!(f, "Serialization failed: {}", e),
            SpillQueueError::InvalidConfig => write!(f, "Invalid config"),
        }
    }
}

/// A thread-safe, spillable FIFO queue that uses disk as overflow storage.
///
/// Items are held in memory up to `memory_upper_bound`. When exceeded, the newest items are
/// spilled to disk in batches of size `spill_load_batch_size`. During consumption, if the
/// in-memory queue drops below `memory_lower_bound` and disk batches exist, one batch is loaded
/// back into memory.
///
/// The queue guarantees FIFO ordering across memory and disk.
pub struct SpillQueue<T> {
    inner: Arc<Mutex<SpillQueueInner<T>>>,
}

struct SpillQueueInner<T> {
    /// In-memory portion of the queue (front = oldest, back = newest).
    memory_queue: VecDeque<T>,
    /// Number of items added since the last spill (only relevant when disk batches exist).
    unspilled_count: usize,
    /// Directory where spilled batches are stored.
    cache_dir: PathBuf,
    /// Number of batches currently on disk.
    in_disk_batch_count: usize,
    /// ID of the oldest (front) cache file on disk.
    front_cache_id: u64,
    /// Next ID to use for a new cache file.
    next_cache_id: u64,
    /// Max number of items to keep in memory before spilling.
    memory_upper_bound: usize,
    /// Min number of items to keep in memory; triggers load if below during pop.
    memory_lower_bound: usize,
    /// Number of items per spilled/loaded batch.
    spill_load_batch_size: usize,
    /// Total number of items in the queue (memory + disk).
    item_count: usize,
}

impl<T> SpillQueue<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Clone,
{
    /// Creates a new spillable queue with disk backing.
    ///
    /// # Arguments
    ///
    /// * `cache_dir` - Directory to store spilled batches. Must exist and be empty.
    /// * `memory_upper_bound` - Max items to keep in memory before spilling.
    /// * `memory_lower_bound` - Min items to keep in memory; if queue drops below this during pop,
    ///   data is loaded from disk.
    /// * `spill_load_batch_size` - Number of items per disk batch.
    ///
    /// # Errors
    ///
    /// Returns [`SpillQueueError::InvalidConfig`] if:
    /// - `memory_lower_bound >= memory_upper_bound`
    /// - `spill_load_batch_size` is 0 or greater than `memory_upper_bound - memory_lower_bound`
    ///
    /// Returns [`SpillQueueError::Io`] if the cache directory cannot be created or is not empty.
    pub fn new(
        cache_dir: PathBuf,
        memory_upper_bound: usize,
        memory_lower_bound: usize,
        spill_load_batch_size: usize,
    ) -> Result<Self, SpillQueueError> {
        if memory_lower_bound >= memory_upper_bound {
            return Err(SpillQueueError::InvalidConfig);
        }
        if spill_load_batch_size == 0
            || spill_load_batch_size > (memory_upper_bound - memory_lower_bound)
        {
            return Err(SpillQueueError::InvalidConfig);
        }

        fs::create_dir_all(&cache_dir)?;

        // Ensure cache dir is empty
        let cache_entries: Vec<_> = fs::read_dir(&cache_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                    && entry.file_name().to_string_lossy().ends_with(".qcache.bin")
            })
            .collect();
        if !cache_entries.is_empty() {
            panic!("Cache directory {:?} is not empty", cache_dir);
        }

        let inner = SpillQueueInner {
            memory_queue: VecDeque::new(),
            unspilled_count: 0,
            in_disk_batch_count: 0,
            cache_dir,
            front_cache_id: 0,
            next_cache_id: 0,
            memory_upper_bound,
            memory_lower_bound,
            spill_load_batch_size,
            item_count: 0,
        };

        Ok(SpillQueue {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    /// Pushes an item to the back of the queue.
    ///
    /// If the in-memory size exceeds `memory_upper_bound`, the newest items are spilled to disk.
    ///
    /// # Errors
    ///
    /// Returns [`SpillQueueError::Io`] or [`SpillQueueError::Serialization`] if spilling fails.
    pub fn push(&self, item: T) -> Result<(), SpillQueueError> {
        let mut inner = self.inner.lock().unwrap();
        inner.memory_queue.push_back(item);
        inner.item_count += 1;

        if inner.in_disk_batch_count > 0 {
            inner.unspilled_count += 1;
        }

        if inner.memory_queue.len() > inner.memory_upper_bound {
            inner.spill_to_disk()?;
        }

        Ok(())
    }

    /// Pops the oldest item from the front of the queue.
    ///
    /// If the in-memory queue is empty but disk batches exist, one batch is loaded.
    /// If the in-memory size drops below `memory_lower_bound` after popping, another batch may be
    /// loaded.
    ///
    /// # Errors
    ///
    /// Returns [`SpillQueueError::Io`] or [`SpillQueueError::Serialization`] if loading fails.
    pub fn pop(&self) -> Result<Option<T>, SpillQueueError> {
        let mut inner = self.inner.lock().unwrap();

        if inner.item_count == 0 {
            return Ok(None);
        }

        if inner.memory_queue.is_empty() && inner.in_disk_batch_count > 0 {
            inner.load_from_disk()?;
        }

        let item = inner.memory_queue.pop_front();
        inner.item_count -= 1;

        if inner.memory_queue.len() < inner.memory_lower_bound && inner.in_disk_batch_count > 0 {
            inner.load_from_disk()?;
        }

        Ok(item)
    }

    /// Returns the total number of items in the queue (memory + disk).
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.item_count
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of items currently held in memory.
    pub fn memory_usage(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.memory_queue.len()
    }

    /// Estimates disk usage by counting cache files and multiplying by batch size.
    ///
    /// Note: This is an approximation; actual disk usage may vary due to filesystem block size.
    pub fn disk_usage(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        let file_count = fs::read_dir(&inner.cache_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                            && e.file_name().to_string_lossy().ends_with(".qcache.bin")
                    })
                    .count()
            })
            .unwrap_or(0);
        file_count * inner.spill_load_batch_size
    }

    /// Updates the queue's runtime configuration.
    ///
    /// # Errors
    ///
    /// Returns [`SpillQueueError::InvalidConfig`] if the new parameters are invalid.
    pub fn set_config(
        &self,
        memory_upper_bound: usize,
        memory_lower_bound: usize,
        spill_load_batch_size: usize,
    ) -> Result<(), SpillQueueError> {
        if memory_lower_bound >= memory_upper_bound {
            return Err(SpillQueueError::InvalidConfig);
        }
        if spill_load_batch_size == 0
            || spill_load_batch_size > (memory_upper_bound - memory_lower_bound)
        {
            return Err(SpillQueueError::InvalidConfig);
        }

        let mut inner = self.inner.lock().unwrap();
        inner.memory_upper_bound = memory_upper_bound;
        inner.memory_lower_bound = memory_lower_bound;
        inner.spill_load_batch_size = spill_load_batch_size;
        Ok(())
    }
}

impl<T> SpillQueueInner<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Clone,
{
    /// Spills the newest `spill_load_batch_size` items from memory to a new disk batch.
    ///
    /// If there are already disk batches, the `unspilled_count` items (newest in memory) are
    /// preserved during the spill to maintain order.
    fn spill_to_disk(&mut self) -> Result<(), SpillQueueError> {
        assert!(self.memory_queue.len() >= self.memory_upper_bound);

        let spill_count = self.spill_load_batch_size;

        // Save unspilled items (newest in memory) to preserve order
        let repush_count = if self.in_disk_batch_count > 0 {
            assert!(self.unspilled_count >= spill_count);
            self.unspilled_count - spill_count
        } else {
            0
        };
        let mut repush = Vec::with_capacity(repush_count);
        for _ in 0..repush_count {
            if let Some(item) = self.memory_queue.pop_back() {
                repush.push(item);
            }
        }
        repush.reverse();
        if self.in_disk_batch_count > 0 {
            self.unspilled_count -= spill_count;
        }

        let cache_filename = format!("{}.qcache.bin", self.next_cache_id);
        let cache_path = self.cache_dir.join(cache_filename);
        let file = File::create(cache_path)?;
        let mut writer = BufWriter::new(file);

        // Spill the next-oldest `spill_count` items (now at the back after repush removal)
        let mut to_spill = Vec::with_capacity(spill_count);
        for _ in 0..spill_count {
            if let Some(item) = self.memory_queue.pop_back() {
                to_spill.push(item);
            }
        }
        to_spill.reverse(); // Restore FIFO order for storage

        for item in to_spill {
            serialize_into(&mut writer, &item)?;
        }
        writer.flush()?;

        // Restore unspilled items
        for item in repush {
            self.memory_queue.push_back(item);
        }

        self.next_cache_id += 1;
        self.in_disk_batch_count += 1;
        Ok(())
    }

    /// Loads one batch from disk into memory.
    ///
    /// Loads the oldest disk batch (identified by `front_cache_id`) and appends it to the memory
    /// queue. The unspilled items (if any) are temporarily removed and re-appended afterward to
    /// maintain correct order.
    fn load_from_disk(&mut self) -> Result<(), SpillQueueError> {
        assert!(self.memory_queue.len() < self.memory_lower_bound);
        assert!(self.in_disk_batch_count > 0);

        let cache_entries: Vec<_> = fs::read_dir(&self.cache_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                    && entry.file_name().to_string_lossy()
                        == format!("{}.qcache.bin", self.front_cache_id)
            })
            .collect();

        assert_eq!(
            cache_entries.len(),
            1,
            "Expected exactly one cache file for front ID {}",
            self.front_cache_id
        );

        let earliest = &cache_entries[0];
        debug!("Loading from cache file: {:?}", earliest.path());
        let file = File::open(earliest.path())?;
        let mut reader = BufReader::new(file);

        // Temporarily remove unspilled items (newest in memory)
        let mut unspilled = Vec::with_capacity(self.unspilled_count);
        for _ in 0..self.unspilled_count {
            if let Some(item) = self.memory_queue.pop_back() {
                unspilled.push(item);
            }
        }
        unspilled.reverse();

        // Load items from disk
        let mut loaded = 0;
        loop {
            if loaded >= self.spill_load_batch_size {
                break;
            }
            match deserialize_from(&mut reader) {
                Ok(item) => {
                    self.memory_queue.push_back(item);
                    loaded += 1;
                }
                Err(e) => {
                    if let bincode::ErrorKind::Io(io_err) = e.as_ref() {
                        if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                            break;
                        }
                    }
                    return Err(SpillQueueError::Serialization(e));
                }
            }
        }

        self.front_cache_id += 1;

        // Restore unspilled items
        for item in unspilled {
            self.memory_queue.push_back(item);
        }
        self.in_disk_batch_count -= 1;
        if self.in_disk_batch_count == 0 {
            self.unspilled_count = 0;
        }
        fs::remove_file(earliest.path())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_basic_spill_load() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().to_path_buf();

        let queue = SpillQueue::<i32>::new(cache_dir.clone(), 3, 1, 2).unwrap();

        for i in 0..6 {
            queue.push(i).unwrap();
        }

        assert_eq!(queue.len(), 6);
        // After spilling: when len exceeds upper_bound(3), spill batch_size(2) items
        // So memory ends up with upper_bound - batch_size = 1 item after each spill
        // But with multiple spills, the unspilled_count mechanism preserves some items
        // Actual behavior: memory holds 2 items after spills
        assert_eq!(queue.memory_usage(), 2);
        // Disk usage: items are spilled in batches of 2
        assert_eq!(queue.disk_usage(), 4); // 2 files × 2 items

        for i in 0..6 {
            assert_eq!(queue.pop().unwrap(), Some(i));
        }

        assert_eq!(queue.len(), 0);
        assert_eq!(queue.disk_usage(), 0);
    }

    #[test]
    fn test_invalid_config() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().to_path_buf();

        assert!(SpillQueue::<String>::new(cache_dir.clone(), 3, 3, 1).is_err()); // lower >= upper
        assert!(SpillQueue::<String>::new(cache_dir.clone(), 5, 2, 4).is_err()); // batch too big (4 > 3)
        assert!(SpillQueue::<String>::new(cache_dir.clone(), 5, 2, 0).is_err());
        // batch = 0
    }
}
