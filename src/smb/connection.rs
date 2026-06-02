//! SMB connection pool and directory cache.
//!
//! Provides [`SmbClientPool`] for round-robin client reuse and [`DirCache`]
//! for deduplicating mkdir calls during backup.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::smb::SmbLocation;

/// Shared directory existence cache for SMB mkdir deduplication.
pub type DirCache = Arc<Mutex<HashSet<String>>>;

/// Create a new shared directory existence cache.
pub fn new_dir_cache() -> DirCache {
    Arc::new(Mutex::new(HashSet::new()))
}

/// Round-robin connection pool for SMB clients.
pub struct SmbClientPool {
    clients: Vec<Arc<smb_client::Client>>,
    next: AtomicUsize,
}

impl SmbClientPool {
    /// Connect `size` clients to the given SMB location and return a shared pool.
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

    /// Get the next client from the pool (round-robin).
    pub fn client(&self) -> Arc<smb_client::Client> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        Arc::clone(&self.clients[idx])
    }

    /// Number of clients in the pool.
    pub fn size(&self) -> usize {
        self.clients.len().max(1)
    }

    /// Close all clients in the pool.
    pub async fn close(&self) -> Result<(), String> {
        for client in &self.clients {
            client.close().await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Connect and authenticate an SMB client to the given share location.
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

/// Check if two SMB locations point to the same share (same host, share, port, credentials).
pub fn same_share(source: &SmbLocation, target: &SmbLocation) -> bool {
    source.host.eq_ignore_ascii_case(&target.host)
        && source.share.eq_ignore_ascii_case(&target.share)
        && source.port == target.port
        && source.username == target.username
        && source.password == target.password
}
