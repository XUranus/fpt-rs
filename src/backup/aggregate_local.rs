use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::backup::aggregate_engine::{AggregateBackupEngine, PendingLocalFile};

struct PendingLocalBuffer {
    files: Vec<PendingLocalFile>,
    current_size: u64,
    max_size: u64,
}

impl PendingLocalBuffer {
    fn new(max_size: u64) -> Self {
        Self {
            files: Vec::new(),
            current_size: 0,
            max_size,
        }
    }

    fn add_file(&mut self, file: PendingLocalFile) -> bool {
        self.current_size += file.size;
        self.files.push(file);
        self.current_size >= self.max_size
    }

    fn flush(&mut self) -> Vec<PendingLocalFile> {
        self.current_size = 0;
        std::mem::take(&mut self.files)
    }
}

pub(crate) struct LocalAggregateState {
    buffers: Mutex<HashMap<String, PendingLocalBuffer>>,
    pub engine: Arc<AggregateBackupEngine>,
}

impl LocalAggregateState {
    pub fn new(engine: Arc<AggregateBackupEngine>) -> Self {
        Self {
            buffers: Mutex::new(HashMap::new()),
            engine,
        }
    }

    pub fn pending_file_for_source(
        &self,
        relative_path: String,
        source_path: PathBuf,
        meta: &crate::scanner::metadata::FileMeta,
    ) -> PendingLocalFile {
        PendingLocalFile {
            relative_path,
            source_path,
            size: meta.size,
            ctime: meta.common.ctime as u64,
            mtime: meta.common.mtime as u64,
            mode: meta.common.mode,
            xattrs: meta.common.xattributes.clone(),
            acl: meta.common.posix_access_acl.clone(),
        }
    }

    pub fn add_file(
        &self,
        relative_path: &str,
        file: PendingLocalFile,
    ) -> Option<(String, Vec<PendingLocalFile>)> {
        let bucket_key = self
            .engine
            .bucket_key_for_relative_path(relative_path, file.size);
        let mut buffers = self.buffers.lock().unwrap();
        let buffer = buffers
            .entry(bucket_key.clone())
            .or_insert_with(|| PendingLocalBuffer::new(self.engine.config.max_blob_size));
        if buffer.add_file(file) {
            Some((bucket_key, buffer.flush()))
        } else {
            None
        }
    }

    pub fn flush_all(&self) -> Vec<(String, Vec<PendingLocalFile>)> {
        let mut buffers = self.buffers.lock().unwrap();
        let mut result = Vec::new();
        for (key, buffer) in buffers.iter_mut() {
            if !buffer.files.is_empty() {
                result.push((key.clone(), buffer.flush()));
            }
        }
        result
    }
}
