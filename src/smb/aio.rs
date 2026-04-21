//! Async SMB transport helpers shared by scanner, backup, and post-job flows.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::smb::SmbLocation;

pub mod delete;
pub mod hardlink;
pub mod mtime;

pub type DirCache = Arc<Mutex<HashSet<String>>>;

pub struct SmbClientPool {
    clients: Vec<Arc<smb_client::Client>>,
    next: AtomicUsize,
}

const SMB_DEFAULT_WRITE_CHUNK: usize = 256 * 1024;
const SMB_DEFAULT_READ_CHUNK: usize = 256 * 1024;
const SMB_MAX_SAFE_WRITE_CHUNK: usize = 256 * 1024;
const SMB_MAX_SAFE_READ_CHUNK: usize = 256 * 1024;

#[derive(Debug, Default)]
pub struct SmbCopyMetrics {
    pub ensure_dir_count: AtomicU64,
    pub ensure_dir_ns: AtomicU64,
    pub source_open_count: AtomicU64,
    pub source_open_ns: AtomicU64,
    pub target_open_count: AtomicU64,
    pub target_open_ns: AtomicU64,
    pub read_count: AtomicU64,
    pub read_ns: AtomicU64,
    pub write_count: AtomicU64,
    pub write_ns: AtomicU64,
    pub source_close_count: AtomicU64,
    pub source_close_ns: AtomicU64,
    pub source_close_deferred: AtomicU64,
    pub target_close_count: AtomicU64,
    pub target_close_ns: AtomicU64,
    pub target_close_deferred: AtomicU64,
}

impl SmbCopyMetrics {
    fn add(counter: &AtomicU64, nanos: &AtomicU64, started: Instant) {
        counter.fetch_add(1, Ordering::Relaxed);
        nanos.fetch_add(duration_ns(started.elapsed()), Ordering::Relaxed);
    }

    pub fn timing_summary(&self) -> String {
        format!(
            "ensure_dir={} total={} avg={}, source_open={} total={} avg={}, target_open={} total={} avg={}, read={} total={} avg={}, write={} total={} avg={}, source_close={} total={} avg={} deferred={}, target_close={} total={} avg={} deferred={}",
            self.ensure_dir_count.load(Ordering::Relaxed),
            format_duration_ns(self.ensure_dir_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.ensure_dir_ns.load(Ordering::Relaxed), self.ensure_dir_count.load(Ordering::Relaxed)),
            self.source_open_count.load(Ordering::Relaxed),
            format_duration_ns(self.source_open_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.source_open_ns.load(Ordering::Relaxed), self.source_open_count.load(Ordering::Relaxed)),
            self.target_open_count.load(Ordering::Relaxed),
            format_duration_ns(self.target_open_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.target_open_ns.load(Ordering::Relaxed), self.target_open_count.load(Ordering::Relaxed)),
            self.read_count.load(Ordering::Relaxed),
            format_duration_ns(self.read_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.read_ns.load(Ordering::Relaxed), self.read_count.load(Ordering::Relaxed)),
            self.write_count.load(Ordering::Relaxed),
            format_duration_ns(self.write_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.write_ns.load(Ordering::Relaxed), self.write_count.load(Ordering::Relaxed)),
            self.source_close_count.load(Ordering::Relaxed),
            format_duration_ns(self.source_close_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.source_close_ns.load(Ordering::Relaxed), self.source_close_count.load(Ordering::Relaxed)),
            self.source_close_deferred.load(Ordering::Relaxed),
            self.target_close_count.load(Ordering::Relaxed),
            format_duration_ns(self.target_close_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.target_close_ns.load(Ordering::Relaxed), self.target_close_count.load(Ordering::Relaxed)),
            self.target_close_deferred.load(Ordering::Relaxed),
        )
    }
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn format_duration_ns(ns: u64) -> String {
    let ms = ns as f64 / 1_000_000.0;
    format!("{ms:.3}ms")
}

fn avg_duration_ns(total_ns: u64, count: u64) -> String {
    if count == 0 {
        "0.000ms".to_string()
    } else {
        format_duration_ns(total_ns / count)
    }
}

pub fn new_dir_cache() -> DirCache {
    Arc::new(Mutex::new(HashSet::new()))
}

pub async fn connect_client(location: &SmbLocation) -> Result<Arc<smb_client::Client>, String> {
    let client = Arc::new(smb_client::Client::new(crate::smb::client_config(location)));
    let share_root = location.share_unc_path()?;
    let username = location.username.as_deref().unwrap_or("");
    let password = location.password.clone().unwrap_or_default();

    client
        .share_connect(&share_root, username, password)
        .await
        .map_err(|e| format!("share connect {}: {e}", location.display_string()))?;

    Ok(client)
}

impl SmbClientPool {
    pub fn with_client(client: Arc<smb_client::Client>) -> Self {
        Self {
            clients: vec![client],
            next: AtomicUsize::new(0),
        }
    }

    pub async fn connect(location: &SmbLocation, size: usize) -> Result<Arc<Self>, String> {
        let pool_size = size.max(1);
        let mut clients = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            clients.push(connect_client(location).await?);
        }
        Ok(Arc::new(Self {
            clients,
            next: AtomicUsize::new(0),
        }))
    }

    pub fn client(&self) -> Arc<smb_client::Client> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        Arc::clone(&self.clients[idx])
    }

    pub fn size(&self) -> usize {
        self.clients.len().max(1)
    }

    pub async fn close(&self) -> Result<(), String> {
        for client in &self.clients {
            client.close().await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

pub async fn ensure_relative_directory(
    client: &smb_client::Client,
    location: &SmbLocation,
    dir_cache: &DirCache,
    relative_dir: &str,
) -> Result<(), String> {
    let relative_dir = normalize_relative_path(relative_dir);
    let base_rel = normalize_relative_path(&location.sub_path);
    let full_rel = join_relative(&base_rel, &relative_dir);
    if full_rel.is_empty() {
        return Ok(());
    }

    {
        let cache = dir_cache.lock().await;
        if cache.contains(&full_rel) {
            return Ok(());
        }
    }

    let mut current_unc = location.share_unc_path()?;
    let mut current_rel = String::new();
    let dir_args = smb_client::FileCreateArgs {
        disposition: smb_client::CreateDisposition::OpenIf,
        attributes: smb_client::FileAttributes::new().with_directory(true),
        options: smb_client::CreateOptions::new().with_directory_file(true),
        desired_access: smb_client::FileAccessMask::new().with_generic_all(true),
    };

    for segment in full_rel.split('/').filter(|s| !s.is_empty()) {
        current_unc = current_unc.with_add_path(segment);
        if current_rel.is_empty() {
            current_rel.push_str(segment);
        } else {
            current_rel.push('/');
            current_rel.push_str(segment);
        }

        let already_known = {
            let cache = dir_cache.lock().await;
            cache.contains(&current_rel)
        };
        if already_known {
            continue;
        }

        let resource = client
            .create_file(&current_unc, &dir_args)
            .await
            .map_err(|e| format!("mkdir {}: {e}", current_unc))?;
        close_resource(resource).await?;

        let mut cache = dir_cache.lock().await;
        cache.insert(current_rel.clone());
    }

    Ok(())
}

pub async fn write_relative_file(
    client: &smb_client::Client,
    location: &SmbLocation,
    dir_cache: &DirCache,
    relative_path: &str,
    buf: &[u8],
) -> Result<(), String> {
    let write_chunk = negotiated_write_chunk(client, location).await;
    let relative_path = normalize_relative_path(relative_path);
    if let Some((parent, _)) = relative_path.rsplit_once('/') {
        ensure_relative_directory(client, location, dir_cache, parent).await?;
    }

    let unc = relative_unc_path(location, &relative_path)?;
    let file_args = smb_client::FileCreateArgs::make_overwrite(
        smb_client::FileAttributes::new(),
        smb_client::CreateOptions::new().with_non_directory_file(true),
    );
    let resource = client
        .create_file(&unc, &file_args)
        .await
        .map_err(|e| format!("create {}: {e}", unc))?;

    let file = match resource {
        smb_client::Resource::File(file) => file,
        other => {
            close_resource(other).await?;
            return Err(format!("{} did not resolve to a file handle", unc));
        }
    };

    let mut offset = 0u64;
    while (offset as usize) < buf.len() {
        let end = ((offset as usize) + write_chunk).min(buf.len());
        let written = file
            .write_block(&buf[offset as usize..end], offset, None)
            .await
            .map_err(|e| format!("write {} @{}: {e}", unc, offset))?;
        if written == 0 {
            let _ = file.close().await;
            return Err(format!("short write to {} at offset {}", unc, offset));
        }
        offset += written as u64;
    }

    file.close()
        .await
        .map_err(|e| format!("close {}: {e}", unc))?;
    Ok(())
}

pub async fn read_relative_file(
    client: &smb_client::Client,
    location: &SmbLocation,
    relative_path: &str,
    expected_size: u64,
) -> Result<Vec<u8>, String> {
    let read_chunk = negotiated_read_chunk(client, location).await;
    let relative_path = normalize_relative_path(relative_path);
    let unc = relative_unc_path(location, &relative_path)?;
    let open_args = smb_client::FileCreateArgs::make_open_existing(
        smb_client::FileAccessMask::new().with_generic_read(true),
    );
    let resource = client
        .create_file(&unc, &open_args)
        .await
        .map_err(|e| format!("open {}: {e}", unc))?;

    let file = match resource {
        smb_client::Resource::File(file) => file,
        other => {
            close_resource(other).await?;
            return Err(format!("{} did not resolve to a file handle", unc));
        }
    };

    let mut data = Vec::with_capacity(expected_size.min(usize::MAX as u64) as usize);
    let mut offset = 0u64;
    loop {
        let mut chunk = vec![0u8; read_chunk];
        let read_len = file
            .read_block(&mut chunk, offset, None, false)
            .await
            .map_err(|e| format!("read {} @{}: {e}", unc, offset))?;
        if read_len == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read_len]);
        offset += read_len as u64;
    }

    file.close()
        .await
        .map_err(|e| format!("close {}: {e}", unc))?;
    Ok(data)
}

pub async fn copy_relative_file_streaming(
    source_pool: &Arc<SmbClientPool>,
    source_location: &SmbLocation,
    source_relative_path: &str,
    target_pool: &Arc<SmbClientPool>,
    target_location: &SmbLocation,
    dir_cache: &DirCache,
    target_relative_path: &str,
    ensure_parent_dir: bool,
    metrics: Option<Arc<SmbCopyMetrics>>,
) -> Result<(), String> {
    let read_chunk = negotiated_read_chunk(&source_pool.client(), source_location).await;
    let write_chunk = negotiated_write_chunk(&target_pool.client(), target_location).await;
    let source_relative_path = normalize_relative_path(source_relative_path);
    let target_relative_path = normalize_relative_path(target_relative_path);

    if ensure_parent_dir {
        if let Some((parent, _)) = target_relative_path.rsplit_once('/') {
            let started = Instant::now();
            ensure_relative_directory(&target_pool.client(), target_location, dir_cache, parent)
                .await?;
            if let Some(metrics) = &metrics {
                SmbCopyMetrics::add(&metrics.ensure_dir_count, &metrics.ensure_dir_ns, started);
            }
        }
    }

    let source_unc = relative_unc_path(source_location, &source_relative_path)?;
    let target_unc = relative_unc_path(target_location, &target_relative_path)?;

    let source_client = source_pool.client();
    let target_client = target_pool.client();

    let source_open_args = smb_client::FileCreateArgs::make_open_existing(
        smb_client::FileAccessMask::new().with_generic_read(true),
    );
    let target_open_args = smb_client::FileCreateArgs::make_overwrite(
        smb_client::FileAttributes::new(),
        smb_client::CreateOptions::new().with_non_directory_file(true),
    );

    let started = Instant::now();
    let source_resource = source_client
        .create_file(&source_unc, &source_open_args)
        .await
        .map_err(|e| format!("open {}: {e}", source_unc))?;
    if let Some(metrics) = &metrics {
        SmbCopyMetrics::add(&metrics.source_open_count, &metrics.source_open_ns, started);
    }
    let source_file = match source_resource {
        smb_client::Resource::File(file) => file,
        other => {
            close_resource(other).await?;
            return Err(format!("{} did not resolve to a file handle", source_unc));
        }
    };

    let started = Instant::now();
    let target_resource = match target_client
        .create_file(&target_unc, &target_open_args)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = source_file.close().await;
            return Err(format!("create {}: {e}", target_unc));
        }
    };
    if let Some(metrics) = &metrics {
        SmbCopyMetrics::add(&metrics.target_open_count, &metrics.target_open_ns, started);
    }
    let target_file = match target_resource {
        smb_client::Resource::File(file) => file,
        other => {
            let _ = source_file.close().await;
            close_resource(other).await?;
            return Err(format!("{} did not resolve to a file handle", target_unc));
        }
    };

    let mut chunk = vec![0u8; read_chunk];
    let mut src_offset = 0u64;
    let mut dst_offset = 0u64;

    loop {
        let started = Instant::now();
        let read_len = source_file
            .read_block(&mut chunk, src_offset, None, false)
            .await
            .map_err(|e| format!("read {} @{}: {e}", source_unc, src_offset))?;
        if let Some(metrics) = &metrics {
            SmbCopyMetrics::add(&metrics.read_count, &metrics.read_ns, started);
        }
        if read_len == 0 {
            break;
        }

        let mut chunk_offset = 0usize;
        while chunk_offset < read_len {
            let chunk_end = (chunk_offset + write_chunk).min(read_len);
            let started = Instant::now();
            let written = target_file
                .write_block(&chunk[chunk_offset..chunk_end], dst_offset, None)
                .await
                .map_err(|e| format!("write {} @{}: {e}", target_unc, dst_offset))?;
            if let Some(metrics) = &metrics {
                SmbCopyMetrics::add(&metrics.write_count, &metrics.write_ns, started);
            }
            if written == 0 {
                let _ = source_file.close().await;
                let _ = target_file.close().await;
                return Err(format!(
                    "short write to {} at offset {}",
                    target_unc, dst_offset
                ));
            }
            chunk_offset += written;
            dst_offset += written as u64;
        }

        src_offset += read_len as u64;
    }

    if let Some(metrics) = &metrics {
        metrics
            .source_close_deferred
            .fetch_add(1, Ordering::Relaxed);
    }
    if let Some(metrics) = &metrics {
        metrics
            .target_close_deferred
            .fetch_add(1, Ordering::Relaxed);
    }
    drop(source_file);
    drop(target_file);
    Ok(())
}

pub async fn upload_local_dir_to_smb(
    local_dir: &std::path::Path,
    location: &SmbLocation,
    target_prefix: &str,
) -> Result<(), String> {
    if !local_dir.exists() {
        return Ok(());
    }

    let client = connect_client(location).await?;
    let dir_cache = new_dir_cache();
    ensure_relative_directory(&client, location, &dir_cache, target_prefix).await?;

    let mut stack = vec![(
        local_dir.to_path_buf(),
        normalize_relative_path(target_prefix),
    )];
    while let Some((local_path, remote_path)) = stack.pop() {
        ensure_relative_directory(&client, location, &dir_cache, &remote_path).await?;

        for entry in std::fs::read_dir(&local_path)
            .map_err(|e| format!("read_dir {}: {e}", local_path.display()))?
        {
            let entry =
                entry.map_err(|e| format!("read_dir entry {}: {e}", local_path.display()))?;
            let child_name = entry.file_name().to_string_lossy().into_owned();
            let child_remote = join_relative(&remote_path, &child_name);
            let child_path = entry.path();

            if entry
                .file_type()
                .map_err(|e| format!("file_type {}: {e}", child_path.display()))?
                .is_dir()
            {
                stack.push((child_path, child_remote));
            } else {
                let data = std::fs::read(&child_path)
                    .map_err(|e| format!("read {}: {e}", child_path.display()))?;
                write_relative_file(&client, location, &dir_cache, &child_remote, &data).await?;
            }
        }
    }

    client.close().await.map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn upload_local_file_to_smb(
    local_file: &std::path::Path,
    location: &SmbLocation,
    remote_path: &str,
) -> Result<(), String> {
    let data =
        std::fs::read(local_file).map_err(|e| format!("read {}: {e}", local_file.display()))?;
    let client = connect_client(location).await?;
    let dir_cache = new_dir_cache();
    write_relative_file(&client, location, &dir_cache, remote_path, &data).await?;
    client.close().await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn relative_unc_path(
    location: &SmbLocation,
    relative_path: &str,
) -> Result<smb_client::UncPath, String> {
    let relative_path = normalize_relative_path(relative_path);
    let root = location.root_unc_path()?;
    if relative_path.is_empty() {
        Ok(root)
    } else {
        Ok(root.with_add_path(&relative_path))
    }
}

pub fn normalize_relative_path(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

pub fn target_relative_path(source_dir_base: &Path, target_prefix: &str, path: &str) -> String {
    let rel = relative_path_buf(source_dir_base, Path::new(path));
    let prefixed = if target_prefix.is_empty() {
        rel
    } else {
        Path::new(target_prefix).join(rel)
    };
    normalize_relative_path(&prefixed.to_string_lossy())
}

pub fn relative_path_from_base(source_dir_base: &Path, path: &Path) -> String {
    normalize_relative_path(&relative_path_buf(source_dir_base, path).to_string_lossy())
}

pub fn share_relative_path(location: &SmbLocation, relative_path: &str) -> String {
    let relative_path = normalize_relative_path(relative_path);
    if location.sub_path.is_empty() {
        relative_path.replace('/', "\\")
    } else if relative_path.is_empty() {
        location.sub_path.replace('/', "\\")
    } else {
        format!(
            "{}\\{}",
            location.sub_path.replace('/', "\\"),
            relative_path.replace('/', "\\")
        )
    }
}

fn join_relative(base: &str, child: &str) -> String {
    let child = normalize_relative_path(child);
    if base.is_empty() {
        child
    } else if child.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{child}")
    }
}

fn relative_path_buf(source_dir_base: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(source_dir_base)
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| {
            if path.is_absolute() {
                let logical_root_name = source_dir_base.file_name().and_then(|n| n.to_str());
                let first_segment = path
                    .strip_prefix("/")
                    .ok()
                    .and_then(|p| p.iter().next())
                    .and_then(|s| s.to_str());
                if logical_root_name.is_some() && logical_root_name == first_segment {
                    return path
                        .strip_prefix("/")
                        .map(|r| r.to_path_buf())
                        .unwrap_or_else(|_| path.to_path_buf());
                }
            }
            path.file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| path.to_path_buf())
        })
}

pub async fn close_resource(resource: smb_client::Resource) -> Result<(), String> {
    match resource {
        smb_client::Resource::File(file) => file.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Directory(dir) => dir.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Pipe(pipe) => pipe.close().await.map_err(|e| e.to_string()),
    }
}

async fn negotiated_write_chunk(client: &smb_client::Client, location: &SmbLocation) -> usize {
    negotiated_io_chunk(
        client,
        &location.host,
        SMB_DEFAULT_WRITE_CHUNK,
        SMB_MAX_SAFE_WRITE_CHUNK,
        |info| info.negotiation.max_write_size as usize,
    )
    .await
}

async fn negotiated_read_chunk(client: &smb_client::Client, location: &SmbLocation) -> usize {
    negotiated_io_chunk(
        client,
        &location.host,
        SMB_DEFAULT_READ_CHUNK,
        SMB_MAX_SAFE_READ_CHUNK,
        |info| info.negotiation.max_read_size as usize,
    )
    .await
}

async fn negotiated_io_chunk<F>(
    client: &smb_client::Client,
    server: &str,
    fallback: usize,
    safe_cap: usize,
    select: F,
) -> usize
where
    F: Fn(&smb_client::connection::connection_info::ConnectionInfo) -> usize,
{
    let negotiated = client
        .get_connection(server)
        .await
        .ok()
        .and_then(|conn| conn.conn_info().map(|info| select(info.as_ref())));

    negotiated.unwrap_or(fallback).max(1).min(safe_cap.max(1))
}
