//! NFS connection pool for Bifrost.
//!
//! Because `Nfs3Client` (and therefore [`Nfs3Connection`]) requires `&mut self` for
//! every RPC call, a single connection is inherently sequential.  To achieve
//! concurrency, the pool maintains `connection_count` independent connections and
//! hands them out via `tokio::sync::Mutex` guards.
//!
//! Callers acquire a guard with [`NfsConnectionPool::acquire`], use it as a
//! `&mut Nfs3Connection`, and release it by dropping the guard.  The pool uses a
//! simple round-robin index to distribute load across connections.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use nfs3_client::nfs3_types::nfs3::nfs_fh3;
use nfs3_client::tokio::TokioConnector;
use nfs3_client::{Nfs3Connection, Nfs3ConnectionBuilder};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, MutexGuard};

use crate::nfs::error::NfsError;
use crate::nfs::NfsLocation;

/// Type alias for the concrete connection type used by the pool.
pub type PooledConnection = Nfs3Connection<nfs3_client::tokio::TokioIo<TcpStream>>;

/// A round-robin pool of NFSv3 connections.
///
/// Each connection wraps a single TCP stream to the NFS server and can serve one
/// RPC at a time.  The pool enables concurrent I/O by providing up to
/// `location.connection_count` independent connections.
pub struct NfsConnectionPool {
    /// Locked connections.  The outer `Arc` allows the pool to be shared across
    /// tokio tasks; the inner `Mutex` provides exclusive access per connection.
    connections: Vec<Mutex<PooledConnection>>,
    /// Round-robin index for `acquire()`.
    next: AtomicUsize,
    /// Effective root file handle.
    ///
    /// If the `NfsLocation` has a non-empty `sub_path`, this is the handle of
    /// the subdirectory resolved via LOOKUP RPCs from the export root.
    /// Otherwise it is the export root handle itself.  All downstream code
    /// (AIO pipelines, post-job uploads) should use this as the starting point
    /// for path resolution.
    root_fh: nfs_fh3,
    /// Server-reported maximum read transfer size (from `fsinfo`).
    pub server_rtmax: u32,
    /// Server-reported maximum write transfer size (from `fsinfo`).
    pub server_wtmax: u32,
}

impl NfsConnectionPool {
    /// Create a new pool by establishing `location.connection_count` TCP connections
    /// to the NFS server and calling `fsinfo` to obtain transfer-size limits.
    pub async fn new(location: &NfsLocation) -> Result<Arc<Self>, NfsError> {
        if location.connection_count == 0 {
            return Err(NfsError::Connect(
                "connection_count must be at least 1".to_string(),
            ));
        }

        log::info!(
            "NFS connection pool: creating {} connections to {}, export={}, uid={}, gid={}",
            location.connection_count,
            location.host,
            location.export,
            location.uid,
            location.gid,
        );

        // Establish all connections.
        let mut connections = Vec::with_capacity(location.connection_count);
        let mut root_fh_opt: Option<nfs_fh3> = None;
        let mut server_rtmax = location.read_chunk_size;
        let mut server_wtmax = location.write_chunk_size;

        for _ in 0..location.connection_count {
            let conn = Self::connect_one(location).await?;
            log::debug!(
                "NFS connection established to {}:{}",
                location.host,
                location.export
            );

            // On the first connection, grab the root FH and query server limits.
            if root_fh_opt.is_none() {
                root_fh_opt = Some(conn.root_nfs_fh3());
            }

            connections.push(Mutex::new(conn));
        }

        // Query fsinfo on the first connection to get server transfer limits.
        {
            use nfs3_client::nfs3_types::nfs3::{FSINFO3args, Nfs3Result};
            let root_fh = root_fh_opt.as_ref().unwrap().clone();
            let mut guard = connections[0].lock().await;
            match guard.fsinfo(&FSINFO3args { fsroot: root_fh }).await {
                Ok(Nfs3Result::Ok(ok)) => {
                    log::debug!(
                        "NFS fsinfo: rtmax={} wtmax={} dtperf={}",
                        ok.rtmax,
                        ok.wtmax,
                        ok.dtpref
                    );
                    server_rtmax = ok.rtmax.min(location.read_chunk_size);
                    server_wtmax = ok.wtmax.min(location.write_chunk_size);
                }
                Ok(Nfs3Result::Err((stat, _))) => {
                    log::warn!("NFS fsinfo returned error {stat}, using configured chunk sizes");
                }
                Err(e) => {
                    log::warn!("NFS fsinfo call failed: {e}, using configured chunk sizes");
                }
            }
        }

        // Resolve sub_path: if the location specifies a sub_path, walk from
        // the export root to obtain the effective root file handle.
        let export_fh = root_fh_opt.unwrap();
        let root_fh = if location.sub_path.is_empty() {
            export_fh
        } else {
            let sub_path = location.sub_path.trim_start_matches('/');
            log::info!(
                "NFS connection pool: resolving sub_path '{}' from export root",
                sub_path
            );
            let mut current_fh = export_fh;
            for component in sub_path.split('/').filter(|s| !s.is_empty()) {
                let mut guard = connections[0].lock().await;
                let res = guard
                    .lookup(&nfs3_client::nfs3_types::nfs3::LOOKUP3args {
                        what: nfs3_client::nfs3_types::nfs3::diropargs3 {
                            dir: current_fh.clone(),
                            name: nfs3_client::nfs3_types::nfs3::filename3::from(
                                component.as_bytes(),
                            ),
                        },
                    })
                    .await;
                drop(guard);
                match res {
                    Ok(nfs3_client::nfs3_types::nfs3::Nfs3Result::Ok(ok)) => {
                        log::debug!("NFS LOOKUP: {} → FH resolved", component);
                        current_fh = ok.object;
                    }
                    Ok(nfs3_client::nfs3_types::nfs3::Nfs3Result::Err((stat, _))) => {
                        return Err(NfsError::Nfs(
                            stat,
                            format!("LOOKUP '{}' in sub_path '{}'", component, sub_path),
                        ));
                    }
                    Err(e) => {
                        return Err(NfsError::Transport(e));
                    }
                }
            }
            log::info!("NFS connection pool: sub_path resolved successfully");
            current_fh
        };

        Ok(Arc::new(Self {
            connections,
            next: AtomicUsize::new(0),
            root_fh,
            server_rtmax,
            server_wtmax,
        }))
    }

    /// Return the root file handle of the mounted export.
    pub fn root_fh(&self) -> nfs_fh3 {
        self.root_fh.clone()
    }

    /// Return the number of connections in the pool.
    pub fn worker_count(&self) -> usize {
        self.connections.len()
    }

    /// Acquire a connection from the pool using round-robin selection.
    ///
    /// This call **awaits** until the selected connection's mutex is available,
    /// providing implicit backpressure when all connections are busy.
    pub async fn acquire(&self) -> NfsConnGuard<'_> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.connections.len();
        let guard = self.connections[idx].lock().await;
        NfsConnGuard { guard }
    }

    /// Establish a single connection to the NFS server described by `location`.
    async fn connect_one(location: &NfsLocation) -> Result<PooledConnection, NfsError> {
        let mut builder =
            Nfs3ConnectionBuilder::new(TokioConnector, &location.host, &location.export)
                .connect_from_privileged_port(false);

        if let Some(port) = location.nfs_port {
            builder = builder.nfs3_port(port);
        }

        // Set up AUTH_UNIX credential so the server sees the configured uid/gid.
        if location.uid != 0 || location.gid != 0 {
            use nfs3_client::nfs3_types::rpc::{auth_unix, opaque_auth};
            use nfs3_client::nfs3_types::xdr_codec::Opaque;
            let auth = auth_unix {
                stamp: 0,
                machinename: Opaque::borrowed(b"bifrost"),
                uid: location.uid,
                gid: location.gid,
                gids: vec![],
            };
            builder = builder.credential(opaque_auth::auth_unix(&auth));
        }

        builder
            .mount()
            .await
            .map_err(|e| NfsError::Connect(format!("{}@{}: {e}", location.export, location.host)))
    }
}

/// RAII guard that wraps a locked [`PooledConnection`].
///
/// The underlying mutex is released when this guard is dropped.
pub struct NfsConnGuard<'a> {
    guard: MutexGuard<'a, PooledConnection>,
}

impl<'a> std::ops::Deref for NfsConnGuard<'a> {
    type Target = PooledConnection;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a> std::ops::DerefMut for NfsConnGuard<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
