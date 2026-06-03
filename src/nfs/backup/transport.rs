//! NFS transport implementations for the generic async backup pipeline.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::backup::aio::transport::{clamp_copy_buffer_size, CopyBlock, SourceReader, TargetWriter};

#[derive(Clone)]
pub struct NfsSource {
    pub pool: Arc<crate::nfs::connection::NfsConnectionPool>,
    pub dir_cache: crate::nfs::backup::reader::FileHandleCache,
    pub root_fh: nfs3_client::nfs3_types::nfs3::nfs_fh3,
    pub read_chunk: u32,
    pub buffer_size: usize,
}

impl SourceReader for NfsSource {
    fn read_block(
        &self,
        block: CopyBlock,
    ) -> BoxFuture<'static, Result<CopyBlock, (CopyBlock, String)>> {
        use crate::nfs::backup::reader::{nfs_read_task, NfsReaderResult};
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

#[derive(Clone)]
pub struct NfsTarget {
    pub pool: Arc<crate::nfs::connection::NfsConnectionPool>,
    pub dir_cache: crate::nfs::backup::writer::DirHandleCache,
    pub root_fh: nfs3_client::nfs3_types::nfs3::nfs_fh3,
    pub write_chunk: u32,
    pub buffer_size: usize,
}

impl TargetWriter for NfsTarget {
    fn create_dir(&self, path: PathBuf) -> BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            crate::nfs::backup::writer::get_or_create_dir(
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
        use crate::nfs::backup::writer::{nfs_write_task, NfsWriterResult};
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
