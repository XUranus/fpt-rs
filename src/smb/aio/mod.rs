//! Async SMB transport helpers shared by scanner, backup, and post-job flows.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::smb::SmbLocation;

pub mod delete;
pub mod hardlink;
pub mod mtime;

pub type DirCache = Arc<Mutex<HashSet<String>>>;

const SMB_WRITE_CHUNK: usize = 1024 * 1024;
const SMB_READ_CHUNK: usize = 1024 * 1024;

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
        let end = ((offset as usize) + SMB_WRITE_CHUNK).min(buf.len());
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

    file.flush()
        .await
        .map_err(|e| format!("flush {}: {e}", unc))?;
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
        let mut chunk = vec![0u8; SMB_READ_CHUNK];
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

    let mut stack = vec![(local_dir.to_path_buf(), normalize_relative_path(target_prefix))];
    while let Some((local_path, remote_path)) = stack.pop() {
        ensure_relative_directory(&client, location, &dir_cache, &remote_path).await?;

        for entry in std::fs::read_dir(&local_path)
            .map_err(|e| format!("read_dir {}: {e}", local_path.display()))?
        {
            let entry = entry.map_err(|e| format!("read_dir entry {}: {e}", local_path.display()))?;
            let child_name = entry.file_name().to_string_lossy().into_owned();
            let child_remote = join_relative(&remote_path, &child_name);
            let child_path = entry.path();

            if entry.file_type().map_err(|e| format!("file_type {}: {e}", child_path.display()))?.is_dir() {
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
    let data = std::fs::read(local_file)
        .map_err(|e| format!("read {}: {e}", local_file.display()))?;
    let client = connect_client(location).await?;
    let dir_cache = new_dir_cache();
    write_relative_file(&client, location, &dir_cache, remote_path, &data).await?;
    client.close().await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn relative_unc_path(location: &SmbLocation, relative_path: &str) -> Result<smb_client::UncPath, String> {
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
        .unwrap_or_else(|_| path.to_path_buf())
}

pub async fn close_resource(resource: smb_client::Resource) -> Result<(), String> {
    match resource {
        smb_client::Resource::File(file) => file.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Directory(dir) => dir.close().await.map_err(|e| e.to_string()),
        smb_client::Resource::Pipe(pipe) => pipe.close().await.map_err(|e| e.to_string()),
    }
}
