//! Async NFS directory scanner for Fpt.
//!
//! [`NfsScanner`] recursively traverses an NFS export starting from a root file
//! handle, collecting file and directory metadata via `readdirplus` RPCs.  It
//! produces [`DirBatchScanResult`] items — the same type emitted by the local
//! filesystem scanner — so the rest of the scan pipeline (metadata writers,
//! control file generation) requires no changes.
//!
//! ## Concurrency model
//!
//! A work-queue of `(dir_fh, dir_path)` pairs is shared across `N` async tasks
//! (one per connection in the pool).  Each task picks a directory, reads it
//! completely via `readdirplus` with cookie pagination, and pushes child
//! subdirectories back into the queue.  Tasks terminate when the queue is empty
//! **and** no other task is still working (detected via an in-flight counter).
//!
//! A `tokio::sync::Semaphore` caps the total number of concurrent `readdirplus`
//! RPCs in flight to avoid overwhelming the server.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use nfs3_client::nfs3_types::nfs3::{
    self, ftype3, nfs_fh3, GETATTR3args, Nfs3Option, Nfs3Result, READDIRPLUS3args, READLINK3args,
};
use tokio::sync::{mpsc, Semaphore};

use crate::failure::{FailureItemType, FailureRecord, FailureRecorder, RetryPolicy};
use crate::nfs::connection::NfsConnectionPool;
use crate::nfs::error::NfsError;
use crate::nfs::fstat::{nfs_fattr3_to_dir_meta, nfs_fattr3_to_file_meta};
use crate::nfs::NfsLocation;
use crate::scanner::filter::{logical_path_from_physical, ScanPathFilterSet};
use crate::scanner::models::DirBatchScanResult;
use crate::scanner::options::{ControlPathOption, ScanOption};

/// Maximum number of concurrent `readdirplus` RPCs in flight across all tasks.
const MAX_CONCURRENT_SCAN_RPCS: usize = 16;

/// Byte budget passed to `readdirplus` per call (128 KiB).
const READDIRPLUS_MAXCOUNT: u32 = 128 * 1024;

/// Async NFS directory scanner.
///
/// Construct with [`NfsScanner::new`], then call [`NfsScanner::scan`] to start
/// traversal.  All results are sent on the provided `mpsc::Sender`.
pub struct NfsScanner {
    pool: Arc<NfsConnectionPool>,
    sem: Arc<Semaphore>,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
}

/// Immutable scan settings shared by every NFS traversal worker.
#[derive(Clone)]
struct NfsScanShared {
    control_path: ControlPathOption,
    path_filters: Option<ScanPathFilterSet>,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
}

/// Channel and progress handles used by one NFS worker.
#[derive(Clone)]
struct NfsWorkerChannels {
    work_rx: async_channel::Receiver<(nfs_fh3, String)>,
    work_tx: async_channel::Sender<(nfs_fh3, String)>,
    result_tx: mpsc::Sender<DirBatchScanResult>,
    in_flight: Arc<AtomicUsize>,
}

/// Complete runtime context for one NFS worker task.
struct NfsWorkerRuntime {
    pool: Arc<NfsConnectionPool>,
    sem: Arc<Semaphore>,
    channels: NfsWorkerChannels,
    shared: NfsScanShared,
}

/// Parameters needed to scan one NFS directory.
struct NfsDirScan<'a> {
    pool: &'a NfsConnectionPool,
    sem: &'a Semaphore,
    dir_fh: nfs_fh3,
    dir_path: &'a str,
    work_tx: &'a async_channel::Sender<(nfs_fh3, String)>,
    result_tx: &'a mpsc::Sender<DirBatchScanResult>,
    shared: &'a NfsScanShared,
}

impl NfsScanner {
    /// Create a new scanner backed by a fresh [`NfsConnectionPool`] for `location`.
    pub async fn new(
        location: &NfsLocation,
        retry_policy: RetryPolicy,
        failure_recorder: Option<FailureRecorder>,
    ) -> Result<Self, NfsError> {
        let pool = NfsConnectionPool::new(location).await?;
        Ok(Self {
            pool,
            sem: Arc::new(Semaphore::new(MAX_CONCURRENT_SCAN_RPCS)),
            retry_policy,
            failure_recorder,
        })
    }

    /// Create a scanner that reuses an existing pool.
    pub fn with_pool(
        pool: Arc<NfsConnectionPool>,
        retry_policy: RetryPolicy,
        failure_recorder: Option<FailureRecorder>,
    ) -> Self {
        Self {
            pool,
            sem: Arc::new(Semaphore::new(MAX_CONCURRENT_SCAN_RPCS)),
            retry_policy,
            failure_recorder,
        }
    }

    /// Recursively scan the directory rooted at `root_fh`.
    ///
    /// # Arguments
    /// * `root_fh`   – File handle of the top-level directory to scan.
    /// * `root_path` – Absolute path string that corresponds to `root_fh` (used
    ///                 for [`DirMeta::path`] fields in the output).
    /// * `tx`        – Channel on which [`DirBatchScanResult`] items are sent.
    ///                 The channel is **not** closed by this function.
    ///
    /// Returns when all reachable directories have been fully enumerated or when
    /// a fatal error is encountered.
    pub async fn scan(
        &self,
        root_fh: nfs_fh3,
        root_path: String,
        scan_option: &ScanOption,
        tx: mpsc::Sender<DirBatchScanResult>,
    ) -> Result<(), NfsError> {
        let shared = NfsScanShared {
            control_path: scan_option.control_path.clone(),
            path_filters: scan_option.meta_option.path_filters.clone(),
            retry_policy: self.retry_policy,
            failure_recorder: self.failure_recorder.clone(),
        };
        // Shared work queue: (dir_fh, dir_path)
        let (work_tx, work_rx) = async_channel::unbounded::<(nfs_fh3, String)>();
        let in_flight = Arc::new(AtomicUsize::new(0));

        work_tx
            .send((root_fh, root_path))
            .await
            .map_err(|_| NfsError::Path("work channel closed".to_string()))?;

        let worker_count = self.pool.worker_count().min(MAX_CONCURRENT_SCAN_RPCS);

        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let pool = Arc::clone(&self.pool);
            let sem = Arc::clone(&self.sem);
            let tx = tx.clone();
            let work_tx = work_tx.clone();
            let work_rx = work_rx.clone();
            let in_flight = Arc::clone(&in_flight);
            let shared = shared.clone();

            handles.push(tokio::spawn(async move {
                scan_worker(NfsWorkerRuntime {
                    pool,
                    sem,
                    channels: NfsWorkerChannels {
                        work_rx,
                        work_tx,
                        result_tx: tx,
                        in_flight,
                    },
                    shared,
                })
                .await
            }));
        }

        // Drop our producer clone so workers can detect a drained queue.
        drop(work_tx);

        // Collect errors from workers.
        let mut first_err: Option<NfsError> = None;
        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(join_err) => {
                    if first_err.is_none() {
                        first_err =
                            Some(NfsError::Path(format!("scanner task panicked: {join_err}")));
                    }
                }
            }
        }

        first_err.map_or(Ok(()), Err)
    }
}

/// One scan worker task.  Repeatedly receives `(dir_fh, dir_path)` from the
/// queue, reads the directory with `readdirplus`, emits a [`DirBatchScanResult`],
/// and pushes subdirectories back into the queue.
async fn scan_worker(runtime: NfsWorkerRuntime) -> Result<(), NfsError> {
    let NfsWorkerRuntime {
        pool,
        sem,
        channels,
        shared,
    } = runtime;

    loop {
        // Try to get a work item; exit cleanly when queue is empty and drained.
        let (dir_fh, dir_path) = match channels.work_rx.try_recv() {
            Ok(item) => item,
            Err(async_channel::TryRecvError::Empty) => {
                // If nothing is in-flight, we're done.
                if channels.in_flight.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                    break;
                }
                // Otherwise yield and retry.
                tokio::task::yield_now().await;
                continue;
            }
            Err(async_channel::TryRecvError::Closed) => break,
        };

        channels
            .in_flight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let result = scan_one_dir(NfsDirScan {
            pool: &pool,
            sem: &sem,
            dir_fh,
            dir_path: &dir_path,
            work_tx: &channels.work_tx,
            result_tx: &channels.result_tx,
            shared: &shared,
        })
        .await;

        channels
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

        if let Err(e) = result {
            log::error!("NFS scan error for {dir_path}: {e}");
            // Continue scanning other directories rather than aborting.
        }
    }

    Ok(())
}

/// Resolve file attributes for a readdirplus entry, falling back to getattr if needed.
/// Returns `None` (with failure logged) if attributes cannot be resolved.
async fn resolve_entry_attrs(
    pool: &NfsConnectionPool,
    sem: &Semaphore,
    shared: &NfsScanShared,
    entry: &nfs3::entryplus3<'_>,
    child_path: &str,
) -> Option<nfs3::fattr3> {
    match &entry.name_attributes {
        Nfs3Option::Some(a) => Some(a.clone()),
        Nfs3Option::None => match entry.name_handle.clone() {
            Nfs3Option::Some(fh) => {
                match retry_nfs(shared.retry_policy, || async {
                    let _permit = sem
                        .acquire()
                        .await
                        .map_err(|_| NfsError::Path("semaphore closed".to_string()))?;
                    let mut conn = pool.acquire().await;
                    conn.getattr(&GETATTR3args { object: fh.clone() })
                        .await
                        .map_err(NfsError::from)
                })
                .await
                {
                    Ok(Nfs3Result::Ok(ok)) => Some(ok.obj_attributes),
                    Ok(Nfs3Result::Err((stat, _))) => {
                        log::warn!("NFS getattr failed for {child_path}: {stat}");
                        record_nfs_failure(
                            shared.failure_recorder.as_ref(),
                            "getattr_entry",
                            FailureItemType::Unknown,
                            child_path,
                            format!("NFS error {stat:?}"),
                            shared.retry_policy.max_retries + 1,
                        );
                        None
                    }
                    Err(e) => {
                        log::warn!("NFS getattr error for {child_path}: {e}");
                        None
                    }
                }
            }
            Nfs3Option::None => {
                log::warn!(
                    "NFS readdirplus returned no attrs and no fh for {child_path}; skipping"
                );
                None
            }
        },
    }
}

/// Read a single directory and send a [`DirBatchScanResult`] for it.
/// Subdirectories are pushed into `work_tx` for recursive processing.
async fn scan_one_dir(request: NfsDirScan<'_>) -> Result<(), NfsError> {
    let NfsDirScan {
        pool,
        sem,
        dir_fh,
        dir_path,
        work_tx,
        result_tx,
        shared,
    } = request;
    let path_filters = shared.path_filters.as_ref();

    if let Some(filters) = path_filters {
        let logical_dir = logical_path_from_physical(&shared.control_path, dir_path.as_ref());
        if !filters.should_descend_dir(&logical_dir) {
            return Ok(());
        }
    }
    // Get directory attributes via getattr.
    let dir_attrs = {
        let res = retry_nfs(shared.retry_policy, || async {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| NfsError::Path("semaphore closed".to_string()))?;
            let mut conn = pool.acquire().await;
            conn.getattr(&GETATTR3args {
                object: dir_fh.clone(),
            })
            .await
            .map_err(NfsError::from)
        })
        .await?;
        match res {
            Nfs3Result::Ok(ok) => ok.obj_attributes,
            Nfs3Result::Err((stat, _)) => {
                record_nfs_failure(
                    shared.failure_recorder.as_ref(),
                    "getattr_dir",
                    FailureItemType::Directory,
                    dir_path,
                    format!("NFS error {stat:?}"),
                    shared.retry_policy.max_retries + 1,
                );
                return Err(NfsError::Nfs(stat, format!("getattr on {dir_path}")));
            }
        }
    };

    let dir_name = dir_path.rsplit('/').next().unwrap_or(dir_path);
    let dir_meta = nfs_fattr3_to_dir_meta(&dir_attrs, dir_path, dir_name);

    // Enumerate directory entries via readdirplus with cookie pagination.
    let mut files = Vec::new();
    let mut cookie = nfs3::cookie3::default();
    let mut cookieverf = nfs3::cookieverf3::default();

    loop {
        log::debug!("NFS READDIRPLUS: dir={dir_path} cookie={cookie:?}");
        let res = retry_nfs(shared.retry_policy, || async {
            let _permit = sem
                .acquire()
                .await
                .map_err(|_| NfsError::Path("semaphore closed".to_string()))?;
            let mut conn = pool.acquire().await;
            conn.readdirplus(&READDIRPLUS3args {
                dir: dir_fh.clone(),
                cookie,
                cookieverf,
                dircount: READDIRPLUS_MAXCOUNT,
                maxcount: READDIRPLUS_MAXCOUNT,
            })
            .await
            .map_err(NfsError::from)
        })
        .await?;

        let ok = match res {
            Nfs3Result::Ok(ok) => ok,
            Nfs3Result::Err((stat, _)) => {
                record_nfs_failure(
                    shared.failure_recorder.as_ref(),
                    "readdirplus",
                    FailureItemType::Directory,
                    dir_path,
                    format!("NFS error {stat:?}"),
                    shared.retry_policy.max_retries + 1,
                );
                return Err(NfsError::Nfs(stat, format!("readdirplus on {dir_path}")));
            }
        };

        let entries = ok.reply.entries.into_inner();
        let eof = ok.reply.eof;
        let new_cookieverf = ok.cookieverf;

        for entry in &entries {
            let name = String::from_utf8_lossy(&entry.name.0).into_owned();

            // Skip "." and ".."
            if name == "." || name == ".." {
                continue;
            }

            let child_path = format!("{dir_path}/{name}");
            let child_logical = path_filters
                .map(|_| logical_path_from_physical(&shared.control_path, child_path.as_ref()));

            // Get fattr3 for this entry (present in readdirplus, may be absent).
            let attrs = match resolve_entry_attrs(pool, sem, shared, entry, &child_path).await {
                Some(a) => a,
                None => continue,
            };

            match attrs.type_ {
                ftype3::NF3DIR => {
                    if let (Some(filters), Some(ref logical)) =
                        (path_filters, child_logical.as_ref())
                    {
                        if !filters.should_descend_dir(logical) {
                            continue;
                        }
                    }
                    log::debug!("NFS scan: dir entry {child_path}");
                    if let Nfs3Option::Some(fh) = entry.name_handle.clone() {
                        work_tx
                            .send((fh, child_path))
                            .await
                            .map_err(|_| NfsError::Path("work channel closed".to_string()))?;
                    } else {
                        match lookup_child(pool, &dir_fh, &name).await {
                            Ok(Some(fh)) => {
                                work_tx.send((fh, child_path)).await.map_err(|_| {
                                    NfsError::Path("work channel closed".to_string())
                                })?;
                            }
                            Ok(None) => {
                                log::warn!(
                                    "NFS lookup found no handle for dir {child_path}; skipping"
                                );
                            }
                            Err(e) => {
                                log::warn!("NFS lookup error for dir {child_path}: {e}");
                            }
                        }
                    }
                }
                ftype3::NF3REG => {
                    if let (Some(filters), Some(ref logical)) =
                        (path_filters, child_logical.as_ref())
                    {
                        if !filters.should_emit_file(logical) {
                            continue;
                        }
                    }
                    log::debug!("NFS scan: file entry {child_path} size={}", attrs.size);
                    files.push(nfs_fattr3_to_file_meta(&attrs, &name, None));
                }
                ftype3::NF3LNK => {
                    if let (Some(filters), Some(ref logical)) =
                        (path_filters, child_logical.as_ref())
                    {
                        if !filters.should_emit_file(logical) {
                            continue;
                        }
                    }
                    log::debug!("NFS scan: symlink entry {child_path}");
                    let target = if let Nfs3Option::Some(fh) = entry.name_handle.clone() {
                        match readlink_target(pool, &fh, shared.retry_policy).await {
                            Ok(t) => Some(t),
                            Err(e) => {
                                log::warn!("NFS readlink failed for {child_path}: {e}");
                                record_nfs_failure(
                                    shared.failure_recorder.as_ref(),
                                    "readlink",
                                    FailureItemType::Symlink,
                                    &child_path,
                                    e.to_string(),
                                    shared.retry_policy.max_retries + 1,
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };
                    files.push(nfs_fattr3_to_file_meta(&attrs, &name, target));
                }
                other => {
                    log::debug!("NFS skipping special file {child_path} (type {other:?})");
                }
            }
        }

        if eof {
            break;
        }

        // Advance cookie for the next page.
        if let Some(last) = entries.last() {
            cookie = last.cookie;
        }
        cookieverf = new_cookieverf;
    }

    // Emit the result for this directory.
    let result = DirBatchScanResult {
        dir: dir_meta,
        files,
        partial: false,
        complete: true,
    };
    if let Some(filters) = path_filters {
        let logical_dir = logical_path_from_physical(&shared.control_path, dir_path.as_ref());
        if !filters.should_emit_dir(&logical_dir) && result.files.is_empty() {
            return Ok(());
        }
    }
    if result_tx.send(result).await.is_err() {
        return Err(NfsError::Path("result channel closed".to_string()));
    }

    Ok(())
}

/// Resolve a child name to its file handle using `lookup`.
async fn lookup_child(
    pool: &NfsConnectionPool,
    dir_fh: &nfs_fh3,
    name: &str,
) -> Result<Option<nfs_fh3>, NfsError> {
    use nfs3_client::nfs3_types::nfs3::{diropargs3, filename3, LOOKUP3args};

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
        Nfs3Result::Err((stat, _)) => Err(NfsError::Nfs(stat, format!("lookup {name}"))),
    }
}

/// Read the target of a symbolic link.
async fn readlink_target(
    pool: &NfsConnectionPool,
    fh: &nfs_fh3,
    retry_policy: RetryPolicy,
) -> Result<String, NfsError> {
    let res = retry_nfs(retry_policy, || async {
        let mut conn = pool.acquire().await;
        conn.readlink(&READLINK3args {
            symlink: fh.clone(),
        })
        .await
        .map_err(NfsError::from)
    })
    .await?;

    match res {
        Nfs3Result::Ok(ok) => {
            let bytes = ok.data.0.as_ref().to_vec();
            String::from_utf8(bytes)
                .map_err(|e| NfsError::Path(format!("readlink returned non-UTF-8 path: {e}")))
        }
        Nfs3Result::Err((stat, _)) => Err(NfsError::Nfs(stat, "readlink".to_string())),
    }
}

async fn retry_nfs<F, Fut, T>(retry_policy: RetryPolicy, mut op: F) -> Result<T, NfsError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, NfsError>>,
{
    let mut attempts = 0;
    loop {
        attempts += 1;
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) if retry_policy.should_retry(attempts) => {
                tokio::time::sleep(retry_policy.delay_for_attempt(attempts)).await;
                let _ = &err;
            }
            Err(err) => return Err(err),
        }
    }
}

fn record_nfs_failure(
    recorder: Option<&FailureRecorder>,
    operation: &str,
    item_type: FailureItemType,
    path: &str,
    detail: String,
    attempts: u32,
) {
    if let Some(recorder) = recorder {
        recorder.record(FailureRecord::from_detail(
            "scan",
            operation,
            item_type,
            path.to_string(),
            detail,
            attempts,
        ));
    }
}
