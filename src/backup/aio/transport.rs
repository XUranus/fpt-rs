//! Transport adapters for the generic async backup pipeline.

use std::path::PathBuf;
#[cfg(any(feature = "nfs", feature = "smb"))]
use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::task;

use crate::backup::aio::local_fs::{read_local_file_chunk, write_local_file_chunk};
use crate::backup::copy_block::CopyBlock;
use crate::backup::fcb::FileControlBlock;

pub const DEFAULT_COPY_BUFFER_SIZE: usize = 1024 * 1024;

pub fn clamp_copy_buffer_size(size: usize) -> usize {
    size.clamp(256 * 1024, 4 * 1024 * 1024)
}

pub trait SourceReader: Clone + Send + Sync + 'static {
    fn read_block(
        &self,
        block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>>;

    fn finish(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

pub trait TargetWriter: Clone + Send + Sync + 'static {
    fn create_dir(&self, path: PathBuf) -> BoxFuture<'static, Result<(), String>>;

    fn write_block(
        &self,
        block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>>;

    fn write_file(
        &self,
        fcb: FileControlBlock,
    ) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            let block = CopyBlock::from_fcb(fcb);
            match this.write_block(block).await {
                Ok(block) => Ok(block.into_fcb()),
                Err((block, msg)) => Err((block.into_fcb(), msg)),
            }
        })
    }

    fn finish(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
pub struct LocalSource {
    pub buffer_size: usize,
}

impl SourceReader for LocalSource {
    fn read_block(
        &self,
        mut block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        let buffer_size = clamp_copy_buffer_size(self.buffer_size);
        Box::pin(async move {
            let src_path = block.src_path.clone();
            let meta_size = block.file_size;
            let offset = block.src_offset;
            let read_result: Result<Vec<u8>, String> = task::spawn_blocking(move || {
                read_local_file_chunk(&src_path, offset, meta_size, buffer_size)
            })
            .await
            .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")));

            match read_result {
                Ok(buf) => {
                    block.src_offset = block.src_offset.saturating_add(buf.len() as u64);
                    block.is_last = block.src_offset >= block.file_size;
                    block.data = buf;
                    Ok(block)
                }
                Err(msg) => Err((block, msg)),
            }
        })
    }
}

#[derive(Clone)]
pub struct LocalTarget {
    pub base: PathBuf,
}

impl TargetWriter for LocalTarget {
    fn create_dir(&self, path: PathBuf) -> BoxFuture<'static, Result<(), String>> {
        let full_path = self.base.join(path);
        Box::pin(async move {
            task::spawn_blocking(move || {
                std::fs::create_dir_all(&full_path)
                    .map_err(|e| format!("mkdir {:?}: {e}", full_path))
            })
            .await
            .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")))
        })
    }

    fn write_block(
        &self,
        mut block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        let dst_path = self.base.join(&block.dst_path);
        let buf = block.data.clone();
        let offset = block.dst_offset;
        let mark_sparse = block.meta.sparse_range.is_some();
        Box::pin(async move {
            let result =
                task::spawn_blocking(move || {
                    write_local_file_chunk(&dst_path, offset, &buf, mark_sparse)
                })
                    .await
                    .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")));

            match result {
                Ok(()) => {
                    block.dst_offset = block.dst_offset.saturating_add(block.data.len() as u64);
                    Ok(block)
                }
                Err(msg) => Err((block, msg)),
            }
        })
    }
}

#[cfg(feature = "nfs")]
#[derive(Clone)]
pub struct NfsSource {
    pub pool: Arc<crate::nfs::connection::NfsConnectionPool>,
    pub dir_cache: crate::nfs::aio::reader::FileHandleCache,
    pub root_fh: nfs3_client::nfs3_types::nfs3::nfs_fh3,
    pub read_chunk: u32,
    pub buffer_size: usize,
}

#[cfg(feature = "nfs")]
impl SourceReader for NfsSource {
    fn read_block(
        &self,
        block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        use crate::nfs::aio::reader::{nfs_read_task, NfsReaderResult};
        let this = self.clone();
        Box::pin(async move {
            let fcb = block.into_fcb();
            match nfs_read_task(
                fcb,
                Arc::clone(&this.pool),
                Arc::clone(&this.dir_cache),
                this.root_fh.clone(),
                this.read_chunk
                    .min(clamp_copy_buffer_size(this.buffer_size) as u32),
                clamp_copy_buffer_size(this.buffer_size),
            )
            .await
            {
                NfsReaderResult::Read(fcb) => Ok(CopyBlock::from_fcb(fcb)),
                NfsReaderResult::Failed(fcb, msg) => Err((CopyBlock::from_fcb(fcb), msg)),
            }
        })
    }
}

#[cfg(feature = "nfs")]
#[derive(Clone)]
pub struct NfsTarget {
    pub pool: Arc<crate::nfs::connection::NfsConnectionPool>,
    pub dir_cache: crate::nfs::aio::writer::DirHandleCache,
    pub root_fh: nfs3_client::nfs3_types::nfs3::nfs_fh3,
    pub write_chunk: u32,
    pub buffer_size: usize,
}

#[cfg(feature = "nfs")]
impl TargetWriter for NfsTarget {
    fn create_dir(&self, path: PathBuf) -> BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            crate::nfs::aio::writer::get_or_create_dir(
                &this.pool,
                &this.dir_cache,
                &path.to_string_lossy(),
                &this.root_fh,
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
    }

    fn write_block(
        &self,
        block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        use crate::nfs::aio::writer::{nfs_write_task, NfsWriterResult};
        let this = self.clone();
        Box::pin(async move {
            let fcb = block.into_fcb();
            match nfs_write_task(
                fcb,
                Arc::clone(&this.pool),
                Arc::clone(&this.dir_cache),
                this.root_fh.clone(),
                this.write_chunk
                    .min(clamp_copy_buffer_size(this.buffer_size) as u32),
            )
            .await
            {
                NfsWriterResult::Written(fcb) => Ok(CopyBlock::from_fcb(fcb)),
                NfsWriterResult::Failed(fcb, msg) => Err((CopyBlock::from_fcb(fcb), msg)),
            }
        })
    }
}

#[cfg(feature = "smb")]
#[derive(Clone)]
pub struct SmbTarget {
    pub location: crate::smb::SmbLocation,
    pub pool: Arc<crate::smb::aio::SmbClientPool>,
    pub dir_cache: crate::smb::aio::DirCache,
    pub buffer_size: usize,
}

#[cfg(feature = "smb")]
impl TargetWriter for SmbTarget {
    fn create_dir(&self, path: PathBuf) -> BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            let client = this.pool.client();
            crate::smb::aio::ensure_relative_directory(
                &client,
                &this.location,
                &this.dir_cache,
                &path.to_string_lossy().replace('\\', "/"),
            )
            .await
        })
    }

    fn write_block(
        &self,
        mut block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            let rel_path = block.dst_path.to_string_lossy().replace('\\', "/");
            let client = this.pool.client();
            match crate::smb::aio::write_relative_file_chunk(
                &client,
                &this.location,
                &this.dir_cache,
                &rel_path,
                &block.data,
                block.dst_offset,
                clamp_copy_buffer_size(this.buffer_size),
            )
            .await
            {
                Ok(()) => {
                    block.dst_offset = block.dst_offset.saturating_add(block.data.len() as u64);
                    Ok(block)
                }
                Err(msg) => Err((block, msg)),
            }
        })
    }

    fn finish(&self) -> BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move { this.pool.close().await })
    }
}
