//! Async SMB directory scanner for Bifrost.
//!
//! This scanner walks an SMB share root using `smb-rs`, emitting the same
//! `DirBatchScanResult` batches as the local and NFS scanners so the existing
//! metadata/control-file pipeline can be reused unchanged.

use std::sync::Arc;

use futures_util::StreamExt;
use smb_client::{CreateDisposition, CreateOptions, DirAccessMask, Directory, FileAccessMask, FileAllInformation, FileCreateArgs, FileIdBothDirectoryInformation, FileStandardInformation, Resource, UncPath};
use tokio::sync::mpsc;

use crate::scanner::models::DirBatchScanResult;
use crate::scanner::options::ScanOption;
use crate::smb::SmbLocation;
use crate::smb::fstat::{
    SmbDirSeed, share_devno, smb_all_info_to_dir_meta, smb_dir_info_to_file_meta,
    smb_dir_seed_from_entry, smb_seed_to_dir_meta,
};

#[derive(Clone)]
pub struct SmbScanner {
    client: Arc<smb_client::Client>,
    location: SmbLocation,
    devno: u64,
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
    pub async fn new(location: &SmbLocation) -> Result<Self, String> {
        let client = crate::smb::aio::connect_client(location).await?;

        Ok(Self {
            client,
            location: location.clone(),
            devno: share_devno(&location.host, &location.share),
        })
    }

    pub async fn scan(
        &self,
        scan_option: &ScanOption,
        tx: mpsc::Sender<DirBatchScanResult>,
    ) -> Result<(), String> {
        let root_unc = self.location.root_unc_path()?;
        let root_path = self.location.synthetic_root().to_string_lossy().into_owned();
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
        Ok(())
    }

    async fn scan_one_dir(
        &self,
        task: DirTask,
        scan_option: &ScanOption,
    ) -> DirScanOutput {
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

        let resource = match self.client.create_file(&task.unc, &open_args).await {
            Ok(r) => r,
            Err(e) => {
                log::error!("SMB open dir {} failed: {}", task.path, e);
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
                return DirScanOutput { batch: None, children: Vec::new() };
            }
        };

        let dir = match resource {
            Resource::Directory(dir) => dir,
            other => {
                log::warn!("SMB path {} did not resolve to a directory", task.path);
                let _ = close_resource(other).await;
                return DirScanOutput { batch: None, children: Vec::new() };
            }
        };

        let batch_dir = if let Some(seed) = &task.seed {
            smb_seed_to_dir_meta(seed, &task.path, self.devno)
        } else {
            match dir.query_info::<FileAllInformation>().await {
                Ok(info) => smb_all_info_to_dir_meta(&info, &task.path, self.devno),
                Err(e) => {
                    log::error!("SMB query_info failed for {}: {}", task.path, e);
                    let _ = dir.close().await;
                    return DirScanOutput { batch: None, children: Vec::new() };
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
        let query_result = Directory::query_with_options::<FileIdBothDirectoryInformation>(
            &dir,
            "*",
            scan_option.smb_query_buffer_size,
        )
        .await;
        let mut stream = match query_result {
            Ok(stream) => stream,
            Err(e) => {
                log::error!("SMB query dir failed for {}: {}", task.path, e);
                let _ = dir.close().await;
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

            if entry.file_attributes.directory() && !entry.file_attributes.reparse_point() {
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

            let links = if scan_option.meta_option.scan_hardlinks {
                self.query_link_count(&child_unc).await.unwrap_or(1)
            } else {
                1
            };

            batch.files.push(smb_dir_info_to_file_meta(
                &entry,
                self.devno,
                None,
                links,
            ));
        }

        let _ = dir.close().await;
        DirScanOutput {
            batch: Some(batch),
            children,
        }
    }

    async fn query_link_count(&self, path: &UncPath) -> Result<u64, String> {
        let open_args = FileCreateArgs::make_open_existing(
            FileAccessMask::new().with_generic_read(true),
        );
        let resource = self.client.create_file(path, &open_args).await.map_err(|e| e.to_string())?;
        let links = match &resource {
            Resource::File(file) => {
                let standard = file
                    .query_info::<FileStandardInformation>()
                    .await
                    .map_err(|e| e.to_string())?;
                u64::from(standard.number_of_links)
            }
            Resource::Directory(dir) => {
                let standard = dir
                    .query_info::<FileStandardInformation>()
                    .await
                    .map_err(|e| e.to_string())?;
                u64::from(standard.number_of_links)
            }
            Resource::Pipe(_) => 1,
        };
        close_resource(resource).await?;
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

    scan_option.meta_option.skip_entries.contains(&name.to_string())
}

async fn close_resource(resource: Resource) -> Result<(), String> {
    match resource {
        Resource::File(file) => file.close().await.map_err(|e| e.to_string()),
        Resource::Directory(dir) => dir.close().await.map_err(|e| e.to_string()),
        Resource::Pipe(pipe) => pipe.close().await.map_err(|e| e.to_string()),
    }
}
