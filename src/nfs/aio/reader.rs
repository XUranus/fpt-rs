//! NFS async read task for the AIO copy pipeline.
//!
//! [`nfs_read_task`] resolves a source file path to an NFS file handle via a
//! series of `lookup` RPCs, then reads the entire file (or the next chunk for
//! large files) into `fcb.buffer`.
//!
//! A shared [`FileHandleCache`] caches directory handles to avoid redundant
//! `lookup` calls when reading many files from the same parent directory.

use std::collections::HashMap;
use std::sync::Arc;

use nfs3_client::nfs3_types::nfs3::{
    Nfs3Result, READ3args, diropargs3, filename3, nfs_fh3,
};
use tokio::sync::RwLock;

use crate::backup::fcb::{FileControlBlock, SourceHandleState};
use crate::nfs::connection::NfsConnectionPool;
use crate::nfs::error::NfsError;

/// Maximum buffer size for a single read pass (4 MiB, matching the BIO pipeline).
const MAX_FILE_BUFFER_SIZE: usize = 4 * 1024 * 1024;

/// Shared cache mapping NFS directory path strings to their file handles.
///
/// Reusing the same `Arc<RwLock<...>>` type as the writer's [`DirHandleCache`]
/// so callers can share one map for both reader and writer.
pub type FileHandleCache = Arc<RwLock<HashMap<String, nfs_fh3>>>;

/// Create a new, empty [`FileHandleCache`].
pub fn new_file_handle_cache() -> FileHandleCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Result of an NFS read task.
pub enum NfsReaderResult {
    /// Data was successfully read into `fcb.buffer`.
    Read(FileControlBlock),
    /// The read failed; the FCB is returned for stats/logging.
    Failed(FileControlBlock, String),
}

/// Read the source file described by `fcb.src_path` from the NFS server.
///
/// Steps:
/// 1. Walk `fcb.src_path` components via `lookup` RPCs from `root_fh`, using
///    `dir_cache` to skip already-resolved directory handles.
/// 2. Read up to `MAX_FILE_BUFFER_SIZE` bytes starting at `fcb.src_offset`.
/// 3. Update `fcb.buffer`, `fcb.buffer_len`, `fcb.src_offset`, and
///    `fcb.src_state` on success.
pub async fn nfs_read_task(
    mut fcb: FileControlBlock,
    pool: Arc<NfsConnectionPool>,
    dir_cache: FileHandleCache,
    root_fh: nfs_fh3,
    read_chunk: u32,
) -> NfsReaderResult {
    let src_path = fcb.src_path.clone();
    log::debug!("NFS read: path={:?} offset={} size={}", src_path, fcb.src_offset, fcb.meta.size);

    // ------------------------------------------------------------------ 1. resolve file handle
    let file_fh = match resolve_path(&pool, &dir_cache, &fcb.src_path.to_string_lossy(), &root_fh).await {
        Ok(fh) => fh,
        Err(e) => {
            return NfsReaderResult::Failed(
                fcb,
                format!("lookup {src_path:?}: {e}"),
            );
        }
    };

    // ------------------------------------------------------------------ 2. read data
    let start_offset = fcb.src_offset;
    let file_size = fcb.meta.size;
    let remaining = file_size.saturating_sub(start_offset) as usize;
    let buf_cap = remaining.min(MAX_FILE_BUFFER_SIZE);

    fcb.buffer.clear();
    fcb.buffer.reserve(buf_cap);

    let mut offset = start_offset;

    loop {
        let already_read = (offset - start_offset) as usize;

        // Stop if we have enough data for this pass.
        if already_read >= buf_cap {
            break;
        }

        let to_read = ((buf_cap - already_read) as u32).min(read_chunk);

        let read_res = {
            let mut conn = pool.acquire().await;
            conn.read(&READ3args {
                file: file_fh.clone(),
                offset,
                count: to_read,
            })
            .await
        };

        match read_res {
            Ok(Nfs3Result::Ok(ok)) => {
                let data: &[u8] = ok.data.0.as_ref();
                if data.is_empty() {
                    break; // EOF
                }
                fcb.buffer.extend_from_slice(data);
                offset += data.len() as u64;
                if ok.eof {
                    break;
                }
            }
            Ok(Nfs3Result::Err((stat, _))) => {
                return NfsReaderResult::Failed(
                    fcb,
                    format!("read {src_path:?} at offset {offset}: NFS error {stat}"),
                );
            }
            Err(e) => {
                return NfsReaderResult::Failed(
                    fcb,
                    format!("read {src_path:?} at offset {offset}: {e}"),
                );
            }
        }
    }

    let n = fcb.buffer.len();
    fcb.buffer_len = n;
    fcb.src_offset = offset;
    fcb.src_state = if fcb.src_offset >= file_size {
        SourceHandleState::Read
    } else {
        SourceHandleState::PartialRead
    };

    log::debug!("NFS read done: path={:?} bytes_read={} offset={}", src_path, n, offset);
    NfsReaderResult::Read(fcb)
}

// ---------------------------------------------------------------------------
// Path resolution helpers
// ---------------------------------------------------------------------------

/// Resolve a full NFS path string to its file handle by walking components via
/// `lookup` from `root_fh`.  Directory handles along the path are cached in
/// `dir_cache` for reuse.
pub async fn resolve_path(
    pool: &NfsConnectionPool,
    dir_cache: &FileHandleCache,
    path: &str,
    root_fh: &nfs_fh3,
) -> Result<nfs_fh3, NfsError> {
    let components: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    if components.is_empty() {
        return Ok(root_fh.clone());
    }

    let mut current_fh = root_fh.clone();
    let mut current_path = String::new();

    // Walk all components except the last, using the dir cache for directories.
    for (i, component) in components.iter().enumerate() {
        let is_last = i == components.len() - 1;

        if !current_path.is_empty() {
            current_path.push('/');
        }
        current_path.push_str(component);

        // For intermediate directories, try the cache first.
        if !is_last {
            let cached = {
                let cache = dir_cache.read().await;
                cache.get(&current_path).cloned()
            };
            if let Some(fh) = cached {
                current_fh = fh;
                continue;
            }
        }

        // Do a lookup RPC.
        log::debug!("NFS LOOKUP: component={component} path={current_path}");
        let child_fh = lookup_one(pool, &current_fh, component).await?;

        // Cache directory handles (not the final file handle).
        if !is_last {
            let mut cache = dir_cache.write().await;
            cache.insert(current_path.clone(), child_fh.clone());
        }

        current_fh = child_fh;
    }

    Ok(current_fh)
}

/// Perform a single `lookup` RPC for one path component.
async fn lookup_one(
    pool: &NfsConnectionPool,
    dir_fh: &nfs_fh3,
    name: &str,
) -> Result<nfs_fh3, NfsError> {
    use nfs3_client::nfs3_types::nfs3::LOOKUP3args;

    log::debug!("NFS LOOKUP RPC: name={name}");
    let mut conn = pool.acquire().await;
    let res = conn
        .lookup(&LOOKUP3args {
            what: diropargs3 {
                dir: dir_fh.clone(),
                name: filename3::from(name.as_bytes()),
            },
        })
        .await?;

    match res {
        Nfs3Result::Ok(ok) => Ok(ok.object),
        Nfs3Result::Err((stat, _)) => Err(NfsError::Nfs(stat, format!("lookup {name}"))),
    }
}
