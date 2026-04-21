//! NFS async write task for the AIO copy pipeline.
//!
//! [`nfs_write_task`] creates a file on an NFS target, writes the buffered data
//! in chunks, then calls `setattr` to restore the file's metadata (mode, uid,
//! gid, mtime).
//!
//! A shared [`DirHandleCache`] prevents redundant `mkdir` / `lookup` RPCs for
//! directories that have already been seen in this pipeline run.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nfs3_client::nfs3_types::nfs3::{
    createhow3, diropargs3, filename3, nfs_fh3, nfsstat3, sattr3, sattrguard3, set_gid3, set_mode3,
    set_mtime, set_uid3, stable_how, CREATE3args, MKDIR3args, Nfs3Option, Nfs3Result, SETATTR3args,
    WRITE3args,
};
use nfs3_client::nfs3_types::xdr_codec::Opaque;
use tokio::sync::RwLock;
use tokio::time::sleep;

use crate::backup::fcb::{FileControlBlock, TargetHandleState};
use crate::nfs::connection::NfsConnectionPool;
use crate::nfs::error::NfsError;

/// Shared cache mapping NFS path strings to their directory file handles.
///
/// Wrapped in `Arc<RwLock<...>>` so it can be shared across concurrent write tasks.
pub type DirHandleCache = Arc<RwLock<HashMap<String, nfs_fh3>>>;

/// Create a new, empty [`DirHandleCache`].
pub fn new_dir_handle_cache() -> DirHandleCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Result of an NFS write task.
pub enum NfsWriterResult {
    /// File was successfully written to the NFS target.
    Written(FileControlBlock),
    /// The write failed; the FCB is returned for stats/logging.
    Failed(FileControlBlock, String),
}

/// Write `fcb.buffer[..fcb.buffer_len]` to the NFS target at `fcb.dst_path`.
///
/// Steps:
/// 1. Walk `fcb.dst_path` to ensure parent directories exist (using `dir_cache`).
/// 2. Create the file with `CREATE UNCHECKED`.
/// 3. Write the buffer in `write_chunk` byte segments.
/// 4. `setattr` to restore mode, uid, gid, mtime.
///
/// On success sends [`NfsWriterResult::Written`]; on any error sends
/// [`NfsWriterResult::Failed`].
pub async fn nfs_write_task(
    mut fcb: FileControlBlock,
    pool: Arc<NfsConnectionPool>,
    dir_cache: DirHandleCache,
    root_fh: nfs_fh3,
    write_chunk: u32,
) -> NfsWriterResult {
    // ------------------------------------------------------------------ 1. parent dir
    let dst_path = fcb.dst_path.clone();
    let parent = match dst_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => String::new(),
    };

    let dir_fh = match get_or_create_dir(&pool, &dir_cache, &parent, &root_fh).await {
        Ok(fh) => fh,
        Err(e) => {
            return NfsWriterResult::Failed(fcb, format!("mkdir ancestors of {parent}: {e}"));
        }
    };

    // ------------------------------------------------------------------ 2. create file
    let file_name: String = dst_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    log::debug!("NFS CREATE: name={file_name} in dir for {dst_path:?}");
    let create_res = retry_nfs_op(
        || async {
            let mut conn = pool.acquire().await;
            conn.create(&CREATE3args {
                where_: diropargs3 {
                    dir: dir_fh.clone(),
                    name: filename3::from(file_name.as_bytes()),
                },
                how: createhow3::UNCHECKED(sattr3 {
                    mode: set_mode3::Some(0o644),
                    uid: set_uid3::None,
                    gid: set_gid3::None,
                    size: nfs3_client::nfs3_types::nfs3::set_size3::None,
                    atime: nfs3_client::nfs3_types::nfs3::set_atime::SET_TO_SERVER_TIME,
                    mtime: set_mtime::SET_TO_SERVER_TIME,
                }),
            })
            .await
        },
        |res| matches!(res, Ok(Nfs3Result::Err((nfsstat3::NFS3ERR_JUKEBOX, _)))),
        "create",
        &dst_path.to_string_lossy(),
    )
    .await;

    let file_fh = match create_res {
        Ok(Nfs3Result::Ok(ok)) => match ok.obj {
            Nfs3Option::Some(fh) => fh,
            Nfs3Option::None => {
                // Server didn't return a handle; resolve via lookup.
                match lookup_child(&pool, &dir_fh, &file_name).await {
                    Ok(fh) => fh,
                    Err(e) => {
                        return NfsWriterResult::Failed(
                            fcb,
                            format!("lookup after create for {:?}: {e}", dst_path),
                        );
                    }
                }
            }
        },
        Ok(Nfs3Result::Err((stat, _))) => {
            return NfsWriterResult::Failed(fcb, format!("create {dst_path:?}: NFS error {stat}"));
        }
        Err(e) => {
            return NfsWriterResult::Failed(fcb, format!("create {dst_path:?}: {e}"));
        }
    };

    // ------------------------------------------------------------------ 3. write data
    let data = &fcb.buffer[..fcb.buffer_len];
    let total = data.len();
    let mut written = 0usize;
    let start_offset = fcb.dst_offset;

    log::debug!("NFS WRITE: path={dst_path:?} total_size={total}");

    while written < total {
        let end = (written + write_chunk as usize).min(total);
        let chunk = &data[written..end];

        let write_res = retry_nfs_op(
            || async {
                let mut conn = pool.acquire().await;
                log::debug!(
                    "NFS WRITE RPC: path={dst_path:?} offset={written} len={}",
                    chunk.len()
                );
                conn.write(&WRITE3args {
                    file: file_fh.clone(),
                    offset: start_offset + written as u64,
                    count: chunk.len() as u32,
                    stable: stable_how::DATA_SYNC,
                    data: Opaque::borrowed(chunk),
                })
                .await
            },
            |res| matches!(res, Ok(Nfs3Result::Err((nfsstat3::NFS3ERR_JUKEBOX, _)))),
            "write",
            &dst_path.to_string_lossy(),
        )
        .await;

        match write_res {
            Ok(Nfs3Result::Ok(ok)) => {
                written += ok.count as usize;
            }
            Ok(Nfs3Result::Err((stat, _))) => {
                return NfsWriterResult::Failed(
                    fcb,
                    format!(
                        "write {dst_path:?} at offset {}: NFS error {stat}",
                        start_offset + written as u64
                    ),
                );
            }
            Err(e) => {
                return NfsWriterResult::Failed(
                    fcb,
                    format!(
                        "write {dst_path:?} at offset {}: {e}",
                        start_offset + written as u64
                    ),
                );
            }
        }
    }

    // ------------------------------------------------------------------ 4. setattr
    let new_attrs = file_meta_to_sattr3(&fcb);
    let setattr_res = {
        let mut conn = pool.acquire().await;
        conn.setattr(&SETATTR3args {
            object: file_fh.clone(),
            new_attributes: new_attrs,
            guard: sattrguard3::None,
        })
        .await
    };
    if let Err(e) = setattr_res {
        log::warn!("setattr failed for {:?}: {e}", dst_path);
    }

    fcb.dst_state = if fcb.dst_offset >= fcb.meta.size {
        TargetHandleState::Written
    } else {
        TargetHandleState::PartialWritten
    };
    fcb.dst_offset = start_offset + total as u64;
    NfsWriterResult::Written(fcb)
}

const NFS_RETRY_ATTEMPTS: usize = 5;
const NFS_RETRY_BASE_DELAY_MS: u64 = 50;

async fn retry_nfs_op<F, Fut, T, P>(
    mut op: F,
    should_retry: P,
    op_name: &str,
    path: &str,
) -> Result<T, nfs3_client::error::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, nfs3_client::error::Error>>,
    P: Fn(&Result<T, nfs3_client::error::Error>) -> bool,
{
    let mut attempt = 0usize;
    loop {
        let result = op().await;
        if !should_retry(&result) || attempt >= NFS_RETRY_ATTEMPTS {
            return result;
        }

        let delay_ms = NFS_RETRY_BASE_DELAY_MS * (1u64 << attempt);
        log::warn!(
            "NFS {} {} got transient JUKEBOX response, retry {}/{} after {}ms",
            op_name,
            path,
            attempt + 1,
            NFS_RETRY_ATTEMPTS,
            delay_ms,
        );
        sleep(Duration::from_millis(delay_ms)).await;
        attempt += 1;
    }
}

// ---------------------------------------------------------------------------
// Directory handle helpers
// ---------------------------------------------------------------------------

/// Return the NFS file handle for `path`, creating missing ancestor directories
/// via `mkdir` as needed.  Results are cached in `dir_cache`.
pub async fn get_or_create_dir(
    pool: &NfsConnectionPool,
    dir_cache: &DirHandleCache,
    path: &str,
    root_fh: &nfs_fh3,
) -> Result<nfs_fh3, NfsError> {
    // Root/empty path → return the root handle directly.
    if path.is_empty() || path == "/" {
        return Ok(root_fh.clone());
    }

    // Fast path: already cached.
    {
        let cache = dir_cache.read().await;
        if let Some(fh) = cache.get(path) {
            return Ok(fh.clone());
        }
    }

    // Slow path: walk path components from root, creating missing dirs.
    let components: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let mut current_fh = root_fh.clone();
    let mut current_path = String::new();

    for component in &components {
        if !current_path.is_empty() {
            current_path.push('/');
        }
        current_path.push_str(component);

        // Check cache for this intermediate path.
        {
            let cache = dir_cache.read().await;
            if let Some(fh) = cache.get(&current_path) {
                current_fh = fh.clone();
                continue;
            }
        }

        // Try lookup first (directory may already exist).
        let child_fh = match try_lookup(pool, &current_fh, component).await {
            Ok(Some(fh)) => fh,
            Ok(None) | Err(_) => {
                // Doesn't exist or lookup error — try mkdir.
                mkdir_one(pool, &current_fh, component).await?
            }
        };

        // Cache the result.
        {
            let mut cache = dir_cache.write().await;
            cache.insert(current_path.clone(), child_fh.clone());
        }
        current_fh = child_fh;
    }

    Ok(current_fh)
}

/// Attempt to look up a single path component.  Returns `None` if not found.
async fn try_lookup(
    pool: &NfsConnectionPool,
    dir_fh: &nfs_fh3,
    name: &str,
) -> Result<Option<nfs_fh3>, NfsError> {
    use nfs3_client::nfs3_types::nfs3::LOOKUP3args;

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
        Nfs3Result::Ok(ok) => Ok(Some(ok.object)),
        Nfs3Result::Err(_) => Ok(None),
    }
}

/// Create a single directory on the NFS server.  Returns its file handle.
async fn mkdir_one(
    pool: &NfsConnectionPool,
    parent_fh: &nfs_fh3,
    name: &str,
) -> Result<nfs_fh3, NfsError> {
    log::debug!("NFS MKDIR RPC: name={name}");
    let mut conn = pool.acquire().await;
    let res = conn
        .mkdir(&MKDIR3args {
            where_: diropargs3 {
                dir: parent_fh.clone(),
                name: filename3::from(name.as_bytes()),
            },
            attributes: sattr3 {
                mode: set_mode3::Some(0o755),
                uid: set_uid3::None,
                gid: set_gid3::None,
                size: nfs3_client::nfs3_types::nfs3::set_size3::None,
                atime: nfs3_client::nfs3_types::nfs3::set_atime::SET_TO_SERVER_TIME,
                mtime: set_mtime::SET_TO_SERVER_TIME,
            },
        })
        .await?;

    match res {
        Nfs3Result::Ok(ok) => {
            match ok.obj {
                Nfs3Option::Some(fh) => Ok(fh),
                Nfs3Option::None => {
                    // Server didn't return a handle; resolve via lookup.
                    drop(conn);
                    match try_lookup(pool, parent_fh, name).await {
                        Ok(Some(fh)) => Ok(fh),
                        Ok(None) => Err(NfsError::Path(format!(
                            "mkdir {name} succeeded but lookup returned nothing"
                        ))),
                        Err(e) => Err(e),
                    }
                }
            }
        }
        Nfs3Result::Err((stat, _)) => {
            // NFS3ERR_EXIST means it already exists — do a lookup instead.
            use nfs3_client::nfs3_types::nfs3::nfsstat3;
            if stat == nfsstat3::NFS3ERR_EXIST {
                drop(conn);
                match try_lookup(pool, parent_fh, name).await {
                    Ok(Some(fh)) => Ok(fh),
                    Ok(None) => Err(NfsError::Nfs(
                        stat,
                        format!("mkdir {name}: exist but lookup found nothing"),
                    )),
                    Err(e) => Err(e),
                }
            } else {
                Err(NfsError::Nfs(stat, format!("mkdir {name}")))
            }
        }
    }
}

/// Resolve a child name to a file handle via `lookup`.
async fn lookup_child(
    pool: &NfsConnectionPool,
    dir_fh: &nfs_fh3,
    name: &str,
) -> Result<nfs_fh3, NfsError> {
    use nfs3_client::nfs3_types::nfs3::LOOKUP3args;

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

// ---------------------------------------------------------------------------
// Metadata helpers
// ---------------------------------------------------------------------------

/// Build an [`sattr3`] from an FCB's [`FileMeta`], setting mode, uid, gid, and
/// mtime.  Size and atime are left at `SET_TO_SERVER_TIME` / unchanged.
fn file_meta_to_sattr3(fcb: &FileControlBlock) -> sattr3 {
    let meta = &fcb.meta.common;

    sattr3 {
        mode: set_mode3::Some(meta.mode),
        uid: set_uid3::Some(0), // NFS clients typically use uid 0 (root)
        gid: set_gid3::Some(0),
        size: nfs3_client::nfs3_types::nfs3::set_size3::None,
        atime: nfs3_client::nfs3_types::nfs3::set_atime::SET_TO_SERVER_TIME,
        mtime: set_mtime::SET_TO_CLIENT_TIME(nfs3_client::nfs3_types::nfs3::nfstime3 {
            seconds: meta.mtime,
            nseconds: 0,
        }),
    }
}

/// Build an [`sattr3`] that sets mode and mtime for a directory.
pub fn dir_meta_sattr3(mode: u32, mtime_secs: u32) -> sattr3 {
    sattr3 {
        mode: set_mode3::Some(mode),
        uid: set_uid3::None,
        gid: set_gid3::None,
        size: nfs3_client::nfs3_types::nfs3::set_size3::None,
        atime: nfs3_client::nfs3_types::nfs3::set_atime::SET_TO_SERVER_TIME,
        mtime: set_mtime::SET_TO_CLIENT_TIME(nfs3_client::nfs3_types::nfs3::nfstime3 {
            seconds: mtime_secs,
            nseconds: 0,
        }),
    }
}

/// Upload raw bytes to a file at `nfs_path` on the NFS server.
///
/// Used by the post-job phase to upload M\_REPO / C\_REPO files after all
/// data subtasks have completed.  The caller provides an already-connected
/// [`NfsConnectionPool`].  Parent directories are created on demand.
pub async fn nfs_create_and_write(
    pool: Arc<NfsConnectionPool>,
    nfs_path: std::path::PathBuf,
    data: Vec<u8>,
) -> Result<(), NfsError> {
    log::debug!(
        "NFS create_and_write: path={nfs_path:?} size={}",
        data.len()
    );

    let dir_cache = new_dir_handle_cache();
    let root_fh = pool.root_fh();
    let write_chunk = pool.server_wtmax;

    // Ensure parent directories exist.
    let parent = nfs_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let dir_fh = get_or_create_dir(&pool, &dir_cache, &parent, &root_fh).await?;

    let file_name: String = nfs_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());

    // CREATE (UNCHECKED — overwrite if it exists).
    // File mode will be set via setattr after the write completes.
    log::debug!("NFS CREATE RPC: name={file_name} path={nfs_path:?}");
    let create_res = {
        let mut conn = pool.acquire().await;
        conn.create(&CREATE3args {
            where_: diropargs3 {
                dir: dir_fh.clone(),
                name: filename3::from(file_name.as_bytes()),
            },
            how: createhow3::UNCHECKED(sattr3 {
                mode: set_mode3::Some(0o644),
                uid: set_uid3::None,
                gid: set_gid3::None,
                size: nfs3_client::nfs3_types::nfs3::set_size3::None,
                atime: nfs3_client::nfs3_types::nfs3::set_atime::SET_TO_SERVER_TIME,
                mtime: set_mtime::SET_TO_SERVER_TIME,
            }),
        })
        .await
        .map_err(NfsError::Transport)?
    };

    let file_fh = match create_res {
        Nfs3Result::Ok(ok) => match ok.obj {
            Nfs3Option::Some(fh) => fh,
            Nfs3Option::None => {
                return Err(NfsError::Path(format!(
                    "CREATE returned no FH for {nfs_path:?}"
                )));
            }
        },
        Nfs3Result::Err((stat, _)) => {
            return Err(NfsError::Nfs(stat, format!("CREATE {nfs_path:?}")));
        }
    };

    // WRITE in chunks.
    let chunk_size = write_chunk as usize;
    let mut offset: u64 = 0;
    for slice in data.chunks(chunk_size.max(1)) {
        log::debug!(
            "NFS WRITE RPC: path={nfs_path:?} offset={offset} len={}",
            slice.len()
        );
        let write_res = {
            let mut conn = pool.acquire().await;
            conn.write(&WRITE3args {
                file: file_fh.clone(),
                offset,
                count: slice.len() as u32,
                stable: stable_how::FILE_SYNC,
                data: Opaque::owned(slice.to_vec()),
            })
            .await
        };
        match write_res {
            Ok(Nfs3Result::Ok(_)) => {}
            Ok(Nfs3Result::Err((stat, _))) => {
                return Err(NfsError::Nfs(
                    stat,
                    format!("WRITE {nfs_path:?} at {offset}"),
                ));
            }
            Err(e) => return Err(NfsError::Transport(e)),
        }
        offset += slice.len() as u64;
    }

    Ok(())
}
