//! Async SMB directory scanner for Fpt.
//!
//! This scanner walks an SMB share root using `smb-rs`, emitting the same
//! `DirBatchScanResult` batches as the local and NFS scanners so the existing
//! metadata/control-file pipeline can be reused unchanged.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt;
use smb_client::{
    CreateDisposition, CreateOptions, DirAccessMask, Directory, FileAccessMask, FileAllInformation,
    FileCreateArgs, FileIdBothDirectoryInformation, FileStandardInformation, Resource, UncPath,
};
use tokio::sync::mpsc;

use crate::failure::{FailureItemType, FailureRecorder, RetryPolicy};
use crate::scanner::filter::logical_path_from_physical;
use crate::scanner::models::DirBatchScanResult;
use crate::scanner::options::ScanOption;
use crate::smb::fstat::{
    share_devno, smb_all_info_to_dir_meta, smb_dir_info_to_file_meta, smb_dir_seed_from_entry,
    smb_seed_to_dir_meta, SmbDirSeed,
};
use crate::smb::SmbLocation;

#[derive(Clone)]
pub struct SmbScanner {
    client: Arc<smb_client::Client>,
    location: SmbLocation,
    devno: u64,
    metrics: Arc<SmbScanMetrics>,
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
}

#[derive(Default)]
struct SmbScanMetrics {
    dir_open_calls: AtomicU64,
    dir_open_ns: AtomicU64,
    dir_query_info_calls: AtomicU64,
    dir_query_info_ns: AtomicU64,
    dir_query_calls: AtomicU64,
    dir_query_ns: AtomicU64,
    link_count_calls: AtomicU64,
    link_count_ns: AtomicU64,
    close_calls: AtomicU64,
    close_ns: AtomicU64,
}

impl SmbScanMetrics {
    fn add_dir_open(&self, started: Instant) {
        self.dir_open_calls.fetch_add(1, Ordering::Relaxed);
        self.dir_open_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    fn add_dir_query_info(&self, started: Instant) {
        self.dir_query_info_calls.fetch_add(1, Ordering::Relaxed);
        self.dir_query_info_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    fn add_dir_query(&self, started: Instant) {
        self.dir_query_calls.fetch_add(1, Ordering::Relaxed);
        self.dir_query_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    fn add_link_count(&self, started: Instant) {
        self.link_count_calls.fetch_add(1, Ordering::Relaxed);
        self.link_count_ns.fetch_add(
            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    fn log_summary(&self) {
        fn fmt_ms(total_ns: u64) -> String {
            format!("{:.3} ms", total_ns as f64 / 1_000_000.0)
        }
        fn fmt_avg(total_ns: u64, calls: u64) -> String {
            if calls == 0 {
                "0.000 ms".to_string()
            } else {
                format!("{:.3} ms", total_ns as f64 / calls as f64 / 1_000_000.0)
            }
        }

        let dir_open_calls = self.dir_open_calls.load(Ordering::Relaxed);
        let dir_open_ns = self.dir_open_ns.load(Ordering::Relaxed);
        let dir_query_info_calls = self.dir_query_info_calls.load(Ordering::Relaxed);
        let dir_query_info_ns = self.dir_query_info_ns.load(Ordering::Relaxed);
        let dir_query_calls = self.dir_query_calls.load(Ordering::Relaxed);
        let dir_query_ns = self.dir_query_ns.load(Ordering::Relaxed);
        let link_count_calls = self.link_count_calls.load(Ordering::Relaxed);
        let link_count_ns = self.link_count_ns.load(Ordering::Relaxed);
        let close_calls = self.close_calls.load(Ordering::Relaxed);
        let close_ns = self.close_ns.load(Ordering::Relaxed);

        log::info!(
            "SMB scan timing: dir_open={} total={} avg={}, dir_query_info={} total={} avg={}, dir_query={} total={} avg={}, link_count={} total={} avg={}, close={} total={} avg={}",
            dir_open_calls,
            fmt_ms(dir_open_ns),
            fmt_avg(dir_open_ns, dir_open_calls),
            dir_query_info_calls,
            fmt_ms(dir_query_info_ns),
            fmt_avg(dir_query_info_ns, dir_query_info_calls),
            dir_query_calls,
            fmt_ms(dir_query_ns),
            fmt_avg(dir_query_ns, dir_query_calls),
            link_count_calls,
            fmt_ms(link_count_ns),
            fmt_avg(link_count_ns, link_count_calls),
            close_calls,
            fmt_ms(close_ns),
            fmt_avg(close_ns, close_calls),
        );
    }
}

struct DirTask {
    unc: UncPath,
    path: String,
    depth: usize,
    seed: Option<SmbDirSeed>,
}

struct DirScanOutput {
    batch: Option<DirBatchScanResult>,
    children: Vec<DirTask>,
}

impl SmbScanner {
    pub async fn new(
        location: &SmbLocation,
        retry_policy: RetryPolicy,
        failure_recorder: Option<FailureRecorder>,
    ) -> Result<Self, String> {
        let client = crate::smb::aio::connect_client(location).await?;

        Ok(Self {
            client,
            location: location.clone(),
            devno: share_devno(&location.host, &location.share),
            metrics: Arc::new(SmbScanMetrics::default()),
            retry_policy,
            failure_recorder,
        })
    }

    pub async fn scan(
        &self,
        scan_option: &ScanOption,
        tx: mpsc::Sender<DirBatchScanResult>,
    ) -> Result<(), String> {
        let root_unc = self.location.root_unc_path()?;
        let root_path = self
            .location
            .synthetic_root()
            .to_string_lossy()
            .into_owned();
        let mut pending = vec![DirTask {
            unc: root_unc,
            path: root_path,
            depth: 0,
            seed: None,
        }];
        let max_concurrent = scan_option.worker_count.max(1);
        let mut active = tokio::task::JoinSet::<DirScanOutput>::new();
        let scan_option = Arc::new(scan_option.clone());

        while !pending.is_empty() || !active.is_empty() {
            while active.len() < max_concurrent && !pending.is_empty() {
                let task = pending.pop().expect("pending non-empty");
                let scanner = self.clone();
                let scan_option = Arc::clone(&scan_option);
                active.spawn(async move { scanner.scan_one_dir(task, &scan_option).await });
            }

            match active.join_next().await {
                Some(Ok(output)) => {
                    if let Some(batch) = output.batch {
                        let _ = tx.send(batch).await;
                    }
                    pending.extend(output.children);
                }
                Some(Err(e)) => return Err(format!("SMB scan task panicked: {e}")),
                None => break,
            }
        }

        self.client.close().await.map_err(|e| e.to_string())?;
        self.metrics.log_summary();
        Ok(())
    }

    async fn scan_one_dir(&self, task: DirTask, scan_option: &ScanOption) -> DirScanOutput {
        let path_filters = scan_option.meta_option.path_filters.as_ref();
        let current_logical_path = path_filters
            .map(|_| logical_path_from_physical(&scan_option.control_path, task.path.as_ref()));
        if let (Some(filters), Some(logical_path)) = (path_filters, current_logical_path.as_deref())
        {
            if !filters.should_descend_dir(logical_path) {
                return DirScanOutput {
                    batch: None,
                    children: Vec::new(),
                };
            }
        }

        let dir_access = if task.seed.is_some() {
            DirAccessMask::new().with_list_directory(true)
        } else {
            DirAccessMask::new()
                .with_list_directory(true)
                .with_read_attributes(true)
        };
        let open_args = FileCreateArgs {
            disposition: CreateDisposition::Open,
            attributes: smb_client::FileAttributes::new().with_directory(true),
            options: CreateOptions::new().with_directory_file(true),
            desired_access: dir_access.into(),
        };

        let open_started = Instant::now();
        let resource = match crate::scanner::engine::common::retry_async(self.retry_policy, || {
            self.client.create_file(&task.unc, &open_args)
        })
        .await
        .map_err(|e| e.to_string())
        {
            Ok(r) => r,
            Err(e) => {
                self.metrics.add_dir_open(open_started);
                log::error!("SMB open dir {} failed: {}", task.path, e);
                crate::scanner::engine::common::record_scan_failure(self.failure_recorder.as_ref(),
                    "open_dir",
                    FailureItemType::Directory,
                    &task.path,
                    e,
                    self.retry_policy.max_retries + 1,
                );
                if let Some(seed) = task.seed {
                    return DirScanOutput {
                        batch: Some(DirBatchScanResult {
                            dir: smb_seed_to_dir_meta(&seed, &task.path, self.devno),
                            files: Vec::new(),
                            partial: false,
                            complete: true,
                        }),
                        children: Vec::new(),
                    };
                }
                return DirScanOutput {
                    batch: None,
                    children: Vec::new(),
                };
            }
        };
        self.metrics.add_dir_open(open_started);

        let dir = match resource {
            Resource::Directory(dir) => dir,
            other => {
                log::warn!("SMB path {} did not resolve to a directory", task.path);
                let _ = close_resource(other).await;
                return DirScanOutput {
                    batch: None,
                    children: Vec::new(),
                };
            }
        };

        let batch_dir = if let Some(seed) = &task.seed {
            smb_seed_to_dir_meta(seed, &task.path, self.devno)
        } else {
            let query_info_started = Instant::now();
            match crate::scanner::engine::common::retry_async(self.retry_policy, || dir.query_info::<FileAllInformation>()).await.map_err(|e| e.to_string()) {
                Ok(info) => {
                    self.metrics.add_dir_query_info(query_info_started);
                    smb_all_info_to_dir_meta(&info, &task.path, self.devno)
                }
                Err(e) => {
                    self.metrics.add_dir_query_info(query_info_started);
                    log::warn!("SMB query_info failed for {} (continuing with defaults): {}", task.path, e);
                    // Fallback: create a minimal DirMeta from the path.
                    // query_info can fail on some SMB servers (e.g. Windows local shares)
                    // due to deserialization issues, but directory enumeration still works.
                    use std::path::Path;
                    let dir_name = Path::new(&task.path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    crate::scanner::metadata::DirMeta {
                        common: crate::scanner::metadata::MetaCommon {
                            name: dir_name,
                            ..Default::default()
                        },
                        path: task.path.clone(),
                    }
                }
            }
        };
        let mut batch = DirBatchScanResult {
            dir: batch_dir,
            files: Vec::new(),
            partial: false,
            complete: true,
        };
        let mut children = Vec::new();

        let dir = Arc::new(dir);
        {
            let query_started = Instant::now();
            let query_result = crate::scanner::engine::common::retry_async(self.retry_policy, || {
                Directory::query_with_options::<FileIdBothDirectoryInformation>(
                    &dir,
                    "*",
                    scan_option.smb_query_buffer_size,
                )
            })
            .await
            .map_err(|e| e.to_string());
            self.metrics.add_dir_query(query_started);
            let mut stream = match query_result {
                Ok(stream) => stream,
                Err(e) => {
                    log::error!("SMB query dir failed for {}: {}", task.path, e);
                    crate::scanner::engine::common::record_scan_failure(self.failure_recorder.as_ref(),
                        "query_directory",
                        FailureItemType::Directory,
                        &task.path,
                        e,
                        self.retry_policy.max_retries + 1,
                    );
                    return DirScanOutput {
                        batch: Some(batch),
                        children: Vec::new(),
                    };
                }
            };

            while let Some(entry_result) = stream.next().await {
                let entry = match entry_result {
                    Ok(entry) => entry,
                    Err(e) => {
                        log::warn!("SMB dir entry read failed in {}: {}", task.path, e);
                        crate::scanner::engine::common::record_scan_failure(self.failure_recorder.as_ref(),
                            "read_dir_entry",
                            FailureItemType::Unknown,
                            &task.path,
                            e.to_string(),
                            1,
                        );
                        continue;
                    }
                };

                let name = entry.file_name.to_string();
                if name == "." || name == ".." {
                    continue;
                }

                if should_skip(&name, &entry, scan_option) {
                    continue;
                }

                let child_path = format!("{}/{}", task.path, name);
                let child_unc = task.unc.clone().with_add_path(&name);
                let child_logical = path_filters.map(|_| {
                    logical_path_from_physical(&scan_option.control_path, child_path.as_ref())
                });

                if entry.file_attributes.directory() && !entry.file_attributes.reparse_point() {
                    if let (Some(filters), Some(logical)) = (path_filters, child_logical.as_deref())
                    {
                        if !filters.should_descend_dir(logical) {
                            continue;
                        }
                    }
                    if scan_option.max_depth.is_some_and(|max| task.depth >= max) {
                        continue;
                    }
                    children.push(DirTask {
                        unc: child_unc,
                        path: child_path,
                        depth: task.depth + 1,
                        seed: Some(smb_dir_seed_from_entry(&entry)),
                    });
                    continue;
                }

                if let (Some(filters), Some(logical)) = (path_filters, child_logical.as_deref()) {
                    if !filters.should_emit_file(logical) {
                        continue;
                    }
                }

                let links = if scan_option.meta_option.scan_hardlinks {
                    self.query_link_count(&child_unc).await.unwrap_or(1)
                } else {
                    1
                };

                batch
                    .files
                    .push(smb_dir_info_to_file_meta(&entry, self.devno, None, links));
            }
        }

        drop(dir);
        if let (Some(filters), Some(logical_path)) = (path_filters, current_logical_path.as_deref())
        {
            if !filters.should_emit_dir(logical_path) && batch.files.is_empty() {
                return DirScanOutput {
                    batch: None,
                    children,
                };
            }
        }
        DirScanOutput {
            batch: Some(batch),
            children,
        }
    }

    async fn query_link_count(&self, path: &UncPath) -> Result<u64, String> {
        let started = Instant::now();
        let open_args =
            FileCreateArgs::make_open_existing(FileAccessMask::new().with_generic_read(true));
        let resource = crate::scanner::engine::common::retry_async(self.retry_policy, || {
            self.client.create_file(path, &open_args)
        })
        .await
        .map_err(|e| e.to_string())?;
        let links = match &resource {
            Resource::File(file) => {
                let standard = crate::scanner::engine::common::retry_async(self.retry_policy, || {
                    file.query_info::<FileStandardInformation>()
                })
                .await
                .map_err(|e| e.to_string())?;
                u64::from(standard.number_of_links)
            }
            Resource::Directory(dir) => {
                let standard = crate::scanner::engine::common::retry_async(self.retry_policy, || {
                    dir.query_info::<FileStandardInformation>()
                })
                .await
                .map_err(|e| e.to_string())?;
                u64::from(standard.number_of_links)
            }
            Resource::Pipe(_) => 1,
        };
        close_resource(resource).await?;
        self.metrics.add_link_count(started);
        Ok(links)
    }

}

fn should_skip(
    name: &str,
    entry: &FileIdBothDirectoryInformation,
    scan_option: &ScanOption,
) -> bool {
    if !scan_option.meta_option.scan_hidden
        && (name.starts_with('.') || entry.file_attributes.hidden())
    {
        return true;
    }

    scan_option
        .meta_option
        .skip_entries
        .contains(&name.to_string())
}

async fn close_resource(resource: Resource) -> Result<(), String> {
    match resource {
        Resource::File(file) => file.close().await.map_err(|e| e.to_string()),
        Resource::Directory(dir) => dir.close().await.map_err(|e| e.to_string()),
        Resource::Pipe(pipe) => pipe.close().await.map_err(|e| e.to_string()),
    }
}
