//! Transport adapters for the generic async backup pipeline.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use tokio::task;

use crate::backup::aio::local_fs::{read_local_file, write_local_file};
use crate::backup::fcb::{FileControlBlock, SourceHandleState};

pub trait SourceReader: Clone + Send + Sync + 'static {
    fn read_file(&self, fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>>;

    fn finish(&self) -> BoxFuture<'static, Result<(), String>> { Box::pin(async { Ok(()) }) }
}

pub trait TargetWriter: Clone + Send + Sync + 'static {
    fn create_dir(&self, path: PathBuf) -> BoxFuture<'static, Result<(), String>>;

    fn write_file(&self, fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>>;

    fn finish(&self) -> BoxFuture<'static, Result<(), String>> { Box::pin(async { Ok(()) }) }
}

#[derive(Clone, Default)]
pub struct LocalSource;

impl SourceReader for LocalSource {
    fn read_file(&self, mut fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        Box::pin(async move {
            let src_path = fcb.src_path.clone();
            let meta_size = fcb.meta.size;
            let read_result: Result<Vec<u8>, String> =
                task::spawn_blocking(move || read_local_file(&src_path, meta_size))
                    .await
                    .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")));

            match read_result {
                Ok(buf) => {
                    fcb.buffer_len = buf.len();
                    fcb.buffer = buf;
                    fcb.src_state = SourceHandleState::Read;
                    Ok(fcb)
                }
                Err(msg) => Err((fcb, msg)),
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
            task::spawn_blocking(move || std::fs::create_dir_all(&full_path).map_err(|e| format!("mkdir {:?}: {e}", full_path)))
                .await
                .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")))
        })
    }

    fn write_file(&self, fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        let dst_path = self.base.join(&fcb.dst_path);
        let buf = fcb.buffer.clone();
        Box::pin(async move {
            let result = task::spawn_blocking(move || write_local_file(&dst_path, &buf))
                .await
                .unwrap_or_else(|e| Err(format!("blocking task panicked: {e}")));

            match result {
                Ok(()) => Ok(fcb),
                Err(msg) => Err((fcb, msg)),
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
}

#[cfg(feature = "nfs")]
impl SourceReader for NfsSource {
    fn read_file(&self, fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        use crate::nfs::aio::reader::{NfsReaderResult, nfs_read_task};
        let this = self.clone();
        Box::pin(async move {
            match nfs_read_task(
                fcb,
                Arc::clone(&this.pool),
                Arc::clone(&this.dir_cache),
                this.root_fh.clone(),
                this.read_chunk,
            )
            .await
            {
                NfsReaderResult::Read(fcb) => Ok(fcb),
                NfsReaderResult::Failed(fcb, msg) => Err((fcb, msg)),
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

    fn write_file(&self, fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        use crate::nfs::aio::writer::{NfsWriterResult, nfs_write_task};
        let this = self.clone();
        Box::pin(async move {
            match nfs_write_task(
                fcb,
                Arc::clone(&this.pool),
                Arc::clone(&this.dir_cache),
                this.root_fh.clone(),
                this.write_chunk,
            )
            .await
            {
                NfsWriterResult::Written(fcb) => Ok(fcb),
                NfsWriterResult::Failed(fcb, msg) => Err((fcb, msg)),
            }
        })
    }
}

#[cfg(feature = "smb")]
#[derive(Clone)]
pub struct SmbSource {
    pub location: crate::smb::SmbLocation,
    pub pool: Arc<crate::smb::aio::SmbClientPool>,
}

#[cfg(feature = "smb")]
impl SourceReader for SmbSource {
    fn read_file(&self, mut fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            let rel_path = fcb.src_path.to_string_lossy().replace('\\', "/");
            let client = this.pool.client();
            match crate::smb::aio::read_relative_file(&client, &this.location, &rel_path, fcb.meta.size).await {
                Ok(buf) => {
                    fcb.buffer_len = buf.len();
                    fcb.buffer = buf;
                    fcb.src_state = SourceHandleState::Read;
                    Ok(fcb)
                }
                Err(msg) => Err((fcb, msg)),
            }
        })
    }

    fn finish(&self) -> BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move { this.pool.close().await })
    }
}

#[cfg(feature = "smb")]
#[derive(Clone)]
pub struct SmbTarget {
    pub location: crate::smb::SmbLocation,
    pub pool: Arc<crate::smb::aio::SmbClientPool>,
    pub dir_cache: crate::smb::aio::DirCache,
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

    fn write_file(&self, fcb: FileControlBlock) -> BoxFuture<'static, Result<FileControlBlock, (FileControlBlock, String)>> {
        let this = self.clone();
        Box::pin(async move {
            let rel_path = fcb.dst_path.to_string_lossy().replace('\\', "/");
            let client = this.pool.client();
            match crate::smb::aio::write_relative_file(
                &client,
                &this.location,
                &this.dir_cache,
                &rel_path,
                &fcb.buffer,
            )
            .await
            {
                Ok(()) => Ok(fcb),
                Err(msg) => Err((fcb, msg)),
            }
        })
    }

    fn finish(&self) -> BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move { this.pool.close().await })
    }
}
