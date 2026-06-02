//! SMB write operations: directory creation, file writing, and streaming copy.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use log::warn;

use crate::smb::aio::metrics::SmbCopyMetrics;
use crate::smb::aio::path_util::{
    close_resource, join_relative, normalize_relative_path, relative_unc_path,
    SMB_DEFAULT_READ_CHUNK, SMB_DEFAULT_WRITE_CHUNK, SMB_MAX_SAFE_READ_CHUNK, SMB_MAX_SAFE_WRITE_CHUNK,
};
use crate::smb::connection::{connect_client, same_share, DirCache, SmbClientPool};
use crate::smb::SmbLocation;

/// Ensure a directory exists on the SMB share, creating it (and parents) if needed.
/// Uses a shared cache to skip redundant mkdir calls.
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

/// Write a complete file to the SMB share at the given relative path.
pub async fn write_relative_file(
    client: &smb_client::Client,
    location: &SmbLocation,
    dir_cache: &DirCache,
    relative_path: &str,
    buf: &[u8],
) -> Result<(), String> {
    write_relative_file_chunk(
        client,
        location,
        dir_cache,
        relative_path,
        buf,
        0,
        SMB_MAX_SAFE_WRITE_CHUNK,
    )
    .await
}

pub async fn write_relative_file_chunk(
    client: &smb_client::Client,
    location: &SmbLocation,
    dir_cache: &DirCache,
    relative_path: &str,
    buf: &[u8],
    file_offset: u64,
    buffer_cap: usize,
) -> Result<(), String> {
    let write_chunk = negotiated_write_chunk(client, location)
        .await
        .min(buffer_cap.max(1));
    let relative_path = normalize_relative_path(relative_path);
    if let Some((parent, _)) = relative_path.rsplit_once('/') {
        ensure_relative_directory(client, location, dir_cache, parent).await?;
    }

    let unc = relative_unc_path(location, &relative_path)?;
    let file_args = if file_offset == 0 {
        smb_client::FileCreateArgs::make_overwrite(
            smb_client::FileAttributes::new(),
            smb_client::CreateOptions::new().with_non_directory_file(true),
        )
    } else {
        smb_client::FileCreateArgs::make_open_existing(
            smb_client::FileAccessMask::new().with_generic_write(true),
        )
    };
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
            .write_block(&buf[offset as usize..end], file_offset + offset, None)
            .await
            .map_err(|e| format!("write {} @{}: {e}", unc, file_offset + offset))?;
        if written == 0 {
            let _ = file.close().await;
            return Err(format!(
                "short write to {} at offset {}",
                unc,
                file_offset + offset
            ));
        }
        offset += written as u64;
    }

    file.close()
        .await
        .map_err(|e| format!("close {}: {e}", unc))?;
    Ok(())
}

/// Copy a file between two SMB locations using streaming read/write.
///
/// Attempts server-side copy (`srv_copy`) first; falls back to client-side
/// read-then-write if the server doesn't support it.
pub async fn copy_relative_file_streaming(
    source_pool: &Arc<SmbClientPool>,
    source_location: &SmbLocation,
    source_relative_path: &str,
    expected_size: u64,
    target_pool: &Arc<SmbClientPool>,
    target_location: &SmbLocation,
    dir_cache: &DirCache,
    target_relative_path: &str,
    ensure_parent_dir: bool,
    metrics: Option<Arc<SmbCopyMetrics>>,
    buffer_cap: usize,
) -> Result<(), String> {
    let _copy_guard = metrics
        .as_ref()
        .map(|m| SmbCopyMetrics::active_guard(&m.copy_active, &m.copy_active_max));
    let buffer_cap = buffer_cap.max(1);
    let read_chunk = negotiated_read_chunk(&source_pool.client(), source_location)
        .await
        .min(buffer_cap);
    let write_chunk = negotiated_write_chunk(&target_pool.client(), target_location)
        .await
        .min(buffer_cap);
    let source_relative_path = normalize_relative_path(source_relative_path);
    let target_relative_path = normalize_relative_path(target_relative_path);

    if ensure_parent_dir {
        if let Some((parent, _)) = target_relative_path.rsplit_once('/') {
            let started = Instant::now();
            ensure_relative_directory(&target_pool.client(), target_location, dir_cache, parent)
                .await?;
            if let Some(metrics) = &metrics {
                SmbCopyMetrics::add_with_max(
                    &metrics.ensure_dir_count,
                    &metrics.ensure_dir_ns,
                    &metrics.ensure_dir_max_ns,
                    started,
                );
            }
        }
    }

    let source_unc = relative_unc_path(source_location, &source_relative_path)?;
    let target_unc = relative_unc_path(target_location, &target_relative_path)?;

    let share_local_copy = same_share(source_location, target_location);
    let source_client = source_pool.client();
    let target_client = if share_local_copy {
        Arc::clone(&source_client)
    } else {
        target_pool.client()
    };

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
        SmbCopyMetrics::add_with_max(
            &metrics.source_open_count,
            &metrics.source_open_ns,
            &metrics.source_open_max_ns,
            started,
        );
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
        SmbCopyMetrics::add_with_max(
            &metrics.target_open_count,
            &metrics.target_open_ns,
            &metrics.target_open_max_ns,
            started,
        );
    }
    let target_file = match target_resource {
        smb_client::Resource::File(file) => file,
        other => {
            let _ = source_file.close().await;
            close_resource(other).await?;
            return Err(format!("{} did not resolve to a file handle", target_unc));
        }
    };

    if share_local_copy {
        let started = Instant::now();
        match target_file.srv_copy(&source_file).await {
            Ok(()) => {
                if let Some(metrics) = &metrics {
                    SmbCopyMetrics::add_io(
                        &metrics.srv_copy_count,
                        &metrics.srv_copy_ns,
                        &metrics.srv_copy_max_ns,
                        &metrics.srv_copy_bytes,
                        expected_size,
                        started,
                    );
                    metrics
                        .source_close_deferred
                        .fetch_add(1, Ordering::Relaxed);
                    metrics
                        .target_close_deferred
                        .fetch_add(1, Ordering::Relaxed);
                }
                drop(source_file);
                drop(target_file);
                return Ok(());
            }
            Err(e) => {
                if let Some(metrics) = &metrics {
                    metrics
                        .srv_copy_fallback_count
                        .fetch_add(1, Ordering::Relaxed);
                }
                warn!(
                    "SMB srv_copy fallback for {} -> {}: {}",
                    source_unc, target_unc, e
                );
            }
        }
    }

    let read_smb_chunk = |offset: u64| {
        let remaining = expected_size.saturating_sub(offset);
        let request_len = read_chunk.min(remaining as usize);
        let mut read_buf = vec![0u8; request_len];
        let metrics = metrics.clone();
        let source_unc = source_unc.clone();
        let source_file = &source_file;
        async move {
            let started = Instant::now();
            let _read_guard = metrics
                .as_ref()
                .map(|m| SmbCopyMetrics::active_guard(&m.read_active, &m.read_active_max));
            let read_len = source_file
                .read_block(&mut read_buf, offset, None, false)
                .await
                .map_err(|e| format!("read {} @{}: {e}", source_unc, offset))?;
            drop(_read_guard);
            if let Some(metrics) = &metrics {
                SmbCopyMetrics::add_io(
                    &metrics.read_count,
                    &metrics.read_ns,
                    &metrics.read_max_ns,
                    &metrics.read_bytes,
                    read_len as u64,
                    started,
                );
            }
            read_buf.truncate(read_len);
            Ok::<Vec<u8>, String>(read_buf)
        }
    };

    let mut chunk = if expected_size == 0 {
        Vec::new()
    } else {
        read_smb_chunk(0).await?
    };
    let mut next_src_offset = chunk.len() as u64;
    let mut dst_offset = 0u64;

    while !chunk.is_empty() {
        let next_read = if next_src_offset < expected_size {
            Some(read_smb_chunk(next_src_offset))
        } else {
            None
        };
        let read_len = chunk.len();

        let mut chunk_offset = 0usize;
        while chunk_offset < read_len {
            let chunk_end = (chunk_offset + write_chunk).min(read_len);
            let started = Instant::now();
            let _write_guard = metrics
                .as_ref()
                .map(|m| SmbCopyMetrics::active_guard(&m.write_active, &m.write_active_max));
            let written = target_file
                .write_block(&chunk[chunk_offset..chunk_end], dst_offset, None)
                .await
                .map_err(|e| format!("write {} @{}: {e}", target_unc, dst_offset))?;
            drop(_write_guard);
            if let Some(metrics) = &metrics {
                SmbCopyMetrics::add_io(
                    &metrics.write_count,
                    &metrics.write_ns,
                    &metrics.write_max_ns,
                    &metrics.write_bytes,
                    written as u64,
                    started,
                );
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

        chunk = match next_read {
            Some(next_read) => next_read.await?,
            None => Vec::new(),
        };
        next_src_offset = next_src_offset.saturating_add(chunk.len() as u64);
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

/// Upload a local directory tree to an SMB share.
pub async fn upload_local_dir_to_smb(
    local_dir: &Path,
    location: &SmbLocation,
    target_prefix: &str,
) -> Result<(), String> {
    if !local_dir.exists() {
        return Ok(());
    }

    let client = connect_client(location).await?;
    let dir_cache = super::new_dir_cache();
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

/// Upload a single local file to an SMB share.
pub async fn upload_local_file_to_smb(
    local_file: &Path,
    location: &SmbLocation,
    remote_path: &str,
) -> Result<(), String> {
    let data =
        std::fs::read(local_file).map_err(|e| format!("read {}: {e}", local_file.display()))?;
    let client = connect_client(location).await?;
    let dir_cache = super::new_dir_cache();
    write_relative_file(&client, location, &dir_cache, remote_path, &data).await?;
    client.close().await.map_err(|e| e.to_string())?;
    Ok(())
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
