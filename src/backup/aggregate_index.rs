//! Aggregate index for shard-based aggregated backups.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::debug;
use serde::{Deserialize, Serialize};

use crate::backup::aggregate::{AggregateBlobMeta, AggregateFileEntry, AggregateRestoreInfo};

pub const BINARY_INDEX_FILE_NAME: &str = "AGGREGATE_INDEX.bidx";
const BINARY_INDEX_MAGIC: &[u8; 8] = b"BAGG0002";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BinaryIndexRecord {
    relative_path: String,
    blob_path: String,
    offset: u64,
    size: u64,
    mtime: u64,
    mode: u32,
    xattrs: Option<String>,
    acl: Option<String>,
}

pub struct AggregateIndex {
    index_path: PathBuf,
    records: Mutex<HashMap<String, AggregateRestoreInfo>>,
}

impl AggregateIndex {
    pub fn open(index_path: &Path) -> Result<Self, AggregateIndexError> {
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let records = if index_path.exists() {
            load_binary_index(index_path)?
        } else {
            HashMap::new()
        };

        debug!(
            "Aggregate index opened at {:?} with {} records",
            index_path,
            records.len()
        );
        Ok(Self {
            index_path: index_path.to_path_buf(),
            records: Mutex::new(records),
        })
    }

    pub fn add_blob(&self, blob_meta: &AggregateBlobMeta) -> Result<(), AggregateIndexError> {
        let mut records = self.records.lock().unwrap();
        for entry in &blob_meta.files {
            let info = AggregateRestoreInfo {
                blob_path: blob_meta.blob_path.clone(),
                offset: entry.offset,
                size: entry.size,
                mtime: entry.mtime,
                mode: entry.mode,
                xattrs: entry.xattrs.clone(),
                acl: entry.acl.clone(),
            };
            records.insert(entry.relative_path.clone(), info);
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<(), AggregateIndexError> {
        let records = self.records.lock().unwrap();
        persist_binary_index(&self.index_path, &records)
    }

    pub fn query_file(
        &self,
        relative_path: &str,
    ) -> Result<Option<AggregateRestoreInfo>, AggregateIndexError> {
        Ok(self.records.lock().unwrap().get(relative_path).cloned())
    }

    pub fn is_aggregated(&self, relative_path: &str) -> Result<bool, AggregateIndexError> {
        Ok(self.records.lock().unwrap().contains_key(relative_path))
    }

    pub fn get_blob_files(
        &self,
        blob_path: &str,
    ) -> Result<Vec<AggregateFileEntry>, AggregateIndexError> {
        let records = self.records.lock().unwrap();
        Ok(records
            .iter()
            .filter(|(_, info)| info.blob_path == blob_path)
            .map(|(relative_path, info)| AggregateFileEntry {
                relative_path: relative_path.clone(),
                offset: info.offset,
                size: info.size,
                ctime: 0,
                mtime: info.mtime,
                mode: info.mode,
                xattrs: info.xattrs.clone(),
                acl: info.acl.clone(),
            })
            .collect())
    }

    pub fn delete_blob_entries(&self, blob_path: &str) -> Result<usize, AggregateIndexError> {
        let mut records = self.records.lock().unwrap();
        let before = records.len();
        records.retain(|_, info| info.blob_path != blob_path);
        Ok(before - records.len())
    }

    pub fn get_stats(&self) -> Result<IndexStats, AggregateIndexError> {
        let records = self.records.lock().unwrap();
        Ok(IndexStats {
            total_files: records.len() as u64,
            total_blobs: records
                .values()
                .map(|info| info.blob_path.as_str())
                .collect::<HashSet<_>>()
                .len() as u64,
            total_size: records.values().map(|info| info.size).sum(),
        })
    }
}

fn load_binary_index(
    path: &Path,
) -> Result<HashMap<String, AggregateRestoreInfo>, AggregateIndexError> {
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0_u8; BINARY_INDEX_MAGIC.len()];
    file.read_exact(&mut magic)?;
    if &magic != BINARY_INDEX_MAGIC {
        return Err(AggregateIndexError::Other(format!(
            "invalid aggregate index magic: {}",
            path.display()
        )));
    }

    let entries: Vec<BinaryIndexRecord> = bincode::deserialize_from(file)
        .map_err(|e| AggregateIndexError::Other(format!("decode aggregate index: {e}")))?;
    let mut records = HashMap::with_capacity(entries.len());
    for entry in entries {
        records.insert(
            entry.relative_path,
            AggregateRestoreInfo {
                blob_path: entry.blob_path,
                offset: entry.offset,
                size: entry.size,
                mtime: entry.mtime,
                mode: entry.mode,
                xattrs: entry.xattrs,
                acl: entry.acl,
            },
        );
    }
    Ok(records)
}

fn persist_binary_index(
    path: &Path,
    records: &HashMap<String, AggregateRestoreInfo>,
) -> Result<(), AggregateIndexError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let temp_path = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temp_path)?;
    file.write_all(BINARY_INDEX_MAGIC)?;

    let mut entries = Vec::with_capacity(records.len());
    for (relative_path, info) in records {
        entries.push(BinaryIndexRecord {
            relative_path: relative_path.clone(),
            blob_path: info.blob_path.clone(),
            offset: info.offset,
            size: info.size,
            mtime: info.mtime,
            mode: info.mode,
            xattrs: info.xattrs.clone(),
            acl: info.acl.clone(),
        });
    }
    bincode::serialize_into(&mut file, &entries)
        .map_err(|e| AggregateIndexError::Other(format!("encode aggregate index: {e}")))?;
    file.flush()?;
    drop(file);
    std::fs::rename(temp_path, path)?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct IndexStats {
    pub total_files: u64,
    pub total_blobs: u64,
    pub total_size: u64,
}

#[derive(Debug)]
pub enum AggregateIndexError {
    Io(std::io::Error),
    Other(String),
}

impl std::fmt::Display for AggregateIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregateIndexError::Io(e) => write!(f, "IO error: {}", e),
            AggregateIndexError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for AggregateIndexError {}

impl From<std::io::Error> for AggregateIndexError {
    fn from(e: std::io::Error) -> Self {
        AggregateIndexError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::aggregate::{AggregateBlobMeta, AggregateFileEntry};

    #[test]
    fn binary_index_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(BINARY_INDEX_FILE_NAME);
        let index = AggregateIndex::open(&path).unwrap();
        index
            .add_blob(&AggregateBlobMeta {
                blob_path: ".AGGR/shard-000/blob-1.blob".to_string(),
                blob_size: 7,
                file_count: 1,
                shard_id: 0,
                files: vec![AggregateFileEntry {
                    relative_path: "a/b/f1".to_string(),
                    offset: 3,
                    size: 4,
                    ctime: 1,
                    mtime: 2,
                    mode: 0o644,
                    xattrs: None,
                    acl: None,
                }],
            })
            .unwrap();
        index.flush().unwrap();
        drop(index);

        let reopened = AggregateIndex::open(&path).unwrap();
        let info = reopened.query_file("a/b/f1").unwrap().unwrap();
        assert_eq!(info.blob_path, ".AGGR/shard-000/blob-1.blob");
        assert_eq!(info.offset, 3);
        assert_eq!(info.size, 4);
        assert!(reopened.is_aggregated("a/b/f1").unwrap());
    }
}
