//! Aggregate restore helper for shard-based aggregated repositories.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use log::debug;

use crate::backup::aggregate::AggregateRestoreInfo;

pub struct AggregateRestoreEngine {
    source_base: PathBuf,
    blob_cache: Mutex<HashMap<String, Vec<u8>>>,
    stats: Arc<Mutex<AggregateRestoreStats>>,
}

#[derive(Debug, Default, Clone)]
pub struct AggregateRestoreStats {
    pub files_from_blobs: u64,
    pub bytes_from_blobs: u64,
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

    pub fn restore_file(
        &self,
        info: &AggregateRestoreInfo,
        target_path: &Path,
    ) -> Result<(), AggregateRestoreError> {
        let data = self.read_from_blob(&info.blob_path, info.offset, info.size)?;

        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = File::create(target_path)?;
        file.write_all(&data)?;
        file.flush()?;

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;

            let permissions = std::fs::Permissions::from_mode(info.mode);
            std::fs::set_permissions(target_path, permissions)?;

            let mtime =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(info.mtime);
            let times = std::fs::FileTimes::new()
                .set_modified(mtime)
                .set_accessed(mtime);
            File::open(target_path)?.set_times(times)?;

            if let Some(ref xattrs) = info.xattrs {
                restore_xattrs(target_path, xattrs);
            }
            if let Some(ref acl) = info.acl {
                restore_acl(target_path, acl);
            }
        }

        let mut stats = self.stats.lock().unwrap();
        stats.files_from_blobs += 1;
        stats.bytes_from_blobs += info.size;
        debug!("Restored {} from {}", target_path.display(), info.blob_path);
        Ok(())
    }

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

#[cfg(target_os = "linux")]
fn restore_xattrs(path: &Path, xattrs: &str) {
    for line in xattrs.lines() {
        if let Some((name, b64_value)) = line.split_once('=') {
            if let Ok(value) = base64::engine::general_purpose::STANDARD.decode(b64_value) {
                let _ = xattr::set(path, name, &value);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_xattrs(_path: &Path, _xattrs: &str) {}

#[cfg(target_os = "linux")]
fn restore_acl(path: &Path, acl: &str) {
    use exacl::{setfacl, AclEntry};
    let mut acl_entries = Vec::new();
    for line in acl.lines() {
        if let Ok(entry) = line.parse::<AclEntry>() {
            acl_entries.push(entry);
        }
    }
    if !acl_entries.is_empty() {
        let _ = setfacl(&[path], &acl_entries, None);
    }
}

#[cfg(not(target_os = "linux"))]
fn restore_acl(_path: &Path, _acl: &str) {}
