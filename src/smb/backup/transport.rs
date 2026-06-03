//! SMB transport implementations for the generic async backup pipeline.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::backup::aio::transport::{clamp_copy_buffer_size, CopyBlock, TargetWriter};

#[derive(Clone)]
pub struct SmbTarget {
    pub location: crate::smb::SmbLocation,
    pub pool: Arc<crate::smb::SmbClientPool>,
    pub dir_cache: crate::smb::DirCache,
    pub buffer_size: usize,
}

impl TargetWriter for SmbTarget {
    fn create_dir(&self, path: PathBuf) -> BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            let client = this.pool.client();
            crate::smb::backup::writer::ensure_relative_directory(
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
            match crate::smb::backup::writer::write_relative_file_chunk(
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
