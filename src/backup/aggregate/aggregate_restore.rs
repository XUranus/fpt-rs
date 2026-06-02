//! Aggregate restore helper for shard-based aggregated repositories.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct AggregateRestoreEngine {
    source_base: PathBuf,
    blob_cache: Mutex<HashMap<String, Vec<u8>>>,
    stats: Arc<Mutex<AggregateRestoreStats>>,
}

#[derive(Debug, Default, Clone)]
pub struct AggregateRestoreStats {
    pub blobs_read: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

impl AggregateRestoreEngine {
    pub fn new(source_base: PathBuf) -> Result<Self, AggregateRestoreError> {
        Ok(Self {
            source_base,
            blob_cache: Mutex::new(HashMap::new()),
            stats: Arc::new(Mutex::new(AggregateRestoreStats::default())),
        })
    }

    pub fn read_from_blob(
        &self,
        blob_rel_path: &str,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, AggregateRestoreError> {
        let blob_path = self.source_base.join(blob_rel_path);
        let cache_key = blob_path.to_string_lossy().to_string();

        {
            let cache = self.blob_cache.lock().unwrap();
            if let Some(blob_data) = cache.get(&cache_key) {
                let mut stats = self.stats.lock().unwrap();
                stats.cache_hits += 1;
                return slice_blob(blob_data, offset, size);
            }
        }

        let mut blob_file = File::open(&blob_path)?;
        let mut blob_data = Vec::new();
        blob_file.read_to_end(&mut blob_data)?;

        {
            let mut cache = self.blob_cache.lock().unwrap();
            cache.insert(cache_key, blob_data.clone());
        }
        {
            let mut stats = self.stats.lock().unwrap();
            stats.cache_misses += 1;
            stats.blobs_read += 1;
        }

        slice_blob(&blob_data, offset, size)
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> AggregateRestoreStats {
        self.stats.lock().unwrap().clone()
    }
}

fn slice_blob(blob_data: &[u8], offset: u64, size: u64) -> Result<Vec<u8>, AggregateRestoreError> {
    let start = offset as usize;
    let end = (offset + size) as usize;
    if end <= blob_data.len() {
        Ok(blob_data[start..end].to_vec())
    } else {
        Err(AggregateRestoreError::Other(format!(
            "offset {offset} + size {size} exceeds blob size {}",
            blob_data.len()
        )))
    }
}

#[derive(Debug)]
pub enum AggregateRestoreError {
    Io(io::Error),
    Other(String),
}

impl std::fmt::Display for AggregateRestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregateRestoreError::Io(e) => write!(f, "IO error: {}", e),
            AggregateRestoreError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for AggregateRestoreError {}

impl From<io::Error> for AggregateRestoreError {
    fn from(e: io::Error) -> Self {
        AggregateRestoreError::Io(e)
    }
}
