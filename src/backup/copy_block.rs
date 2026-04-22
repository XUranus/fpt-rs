//! Explicit block units for copy pipelines.
//!
//! `CopyBlock` is the common transfer unit used by transport adapters and
//! aggregation code. It keeps bounded payload data separate from file-level
//! metadata so future pipelines can apply backpressure by block count/bytes.

use std::path::PathBuf;
use std::sync::Arc;

use crate::backup::fcb::{FileControlBlock, SourceHandleState, TargetHandleState};
use crate::scanner::metadata::FileMeta;

#[derive(Debug, Clone)]
pub struct CopyBlock {
    pub meta: Arc<FileMeta>,
    pub src_path: PathBuf,
    pub dst_path: PathBuf,
    pub src_offset: u64,
    pub dst_offset: u64,
    pub file_size: u64,
    pub data: Vec<u8>,
    pub is_last: bool,
}

impl CopyBlock {
    pub fn from_fcb(fcb: FileControlBlock) -> Self {
        let meta = Arc::new((*fcb.meta).clone());
        let file_size = meta.size;
        Self {
            meta,
            src_path: fcb.src_path,
            dst_path: fcb.dst_path,
            src_offset: fcb.src_offset,
            dst_offset: fcb.dst_offset,
            file_size,
            data: fcb.buffer,
            is_last: fcb.src_offset >= file_size,
        }
    }

    pub fn into_fcb(self) -> FileControlBlock {
        let mut fcb = FileControlBlock::from((*self.meta).clone());
        fcb.src_path = self.src_path;
        fcb.dst_path = self.dst_path;
        fcb.src_offset = self.src_offset;
        fcb.dst_offset = self.dst_offset;
        fcb.buffer_len = self.data.len();
        fcb.buffer = self.data;
        fcb.src_state = if self.src_offset >= self.file_size {
            SourceHandleState::Read
        } else {
            SourceHandleState::PartialRead
        };
        fcb.dst_state = if self.dst_offset >= self.file_size {
            TargetHandleState::Written
        } else {
            TargetHandleState::PartialWritten
        };
        fcb
    }

    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    pub fn clear_data(&mut self) {
        self.data.clear();
    }

    pub fn read_complete(&self) -> bool {
        self.src_offset >= self.file_size
    }

    pub fn write_complete(&self) -> bool {
        self.dst_offset >= self.file_size
    }
}
