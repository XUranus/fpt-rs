//! NFS backup/restore support for Fpt.
//!
//! This top-level module exposes the [`Location`] enum, [`NfsLocation`]
//! configuration, and re-exports the key types from sub-modules.
//!
//! # Feature flag
//!
//! All items in this module are gated behind the `nfs` Cargo feature.  Add
//! `--features nfs` (or `features = ["nfs"]` in your dependency) to enable NFS
//! support.  Without the feature, the `nfs` module is not compiled and existing
//! local-FS backup code is unaffected.
//!
//! # Architecture overview
//!
//! See `docs/nfs.md` for the full design document.  In brief:
//!
//! - [`NfsConnectionPool`] manages multiple concurrent TCP connections to a
//!   single NFS server.
//! - [`NfsScanner`] enumerates an NFS export and emits `DirBatchScanResult`
//!   items that feed the existing metadata writers unchanged.
//! - The AIO copy pipeline (Milestone 3+) reads/writes files via `nfs3_client`
//!   RPCs instead of `std::fs::File` handles.

use std::path::PathBuf;

pub(crate) mod aio;
pub mod connection;
pub mod error;
pub(crate) mod fstat;
pub mod scanner;

pub use connection::NfsConnectionPool;
pub use error::NfsError;
pub use scanner::NfsScanner;

// ---------------------------------------------------------------------------
// Location abstraction
// ---------------------------------------------------------------------------

/// Identifies where data lives: local filesystem or an NFSv3 export.
///
/// Pass `Location::Local` to keep the existing BIO pipeline.  Pass
/// `Location::Nfs` to use the async NFS pipeline for either source or target
/// (or both).
#[derive(Clone, Debug)]
pub enum Location {
    /// Standard local filesystem path.
    Local(PathBuf),
    /// NFSv3 export accessed via `nfs3_client`.
    Nfs(NfsLocation),
}

impl Location {
    /// Return `true` if this location is a local filesystem path.
    pub fn is_local(&self) -> bool {
        matches!(self, Location::Local(_))
    }

    /// Return `true` if this location is an NFS export.
    pub fn is_nfs(&self) -> bool {
        matches!(self, Location::Nfs(_))
    }

    /// Return the local `PathBuf`, panicking if this is an NFS location.
    pub fn local_path(&self) -> &PathBuf {
        match self {
            Location::Local(p) => p,
            Location::Nfs(_) => panic!("called local_path() on an NFS Location"),
        }
    }

    /// Return the [`NfsLocation`], panicking if this is a local location.
    pub fn nfs_location(&self) -> &NfsLocation {
        match self {
            Location::Nfs(l) => l,
            Location::Local(_) => panic!("called nfs_location() on a Local Location"),
        }
    }
}

impl Default for Location {
    fn default() -> Self {
        Location::Local(PathBuf::new())
    }
}

// ---------------------------------------------------------------------------
// NFS location configuration
// ---------------------------------------------------------------------------

/// Configuration for a single NFSv3 mount point.
///
/// Used within [`Location::Nfs`] to describe how Fpt should connect to an
/// NFS server and which part of the exported tree to use.
#[derive(Clone, Debug)]
pub struct NfsLocation {
    /// NFS server IP address or hostname (e.g. `"192.168.1.10"`).
    pub host: String,

    /// Export path on the server (e.g. `"/export/data"`).
    /// This is the path passed to `mount`.
    pub export: String,

    /// Sub-path within the export to use as the working root.
    ///
    /// An empty string means the export root itself.  For example, if the
    /// export is `/export/data` and `sub_path` is `"project/backup"`, Fpt
    /// will work under `/export/data/project/backup`.
    pub sub_path: String,

    /// UID presented in AUTH_UNIX credentials.  Use `0` for root.
    pub uid: u32,

    /// GID presented in AUTH_UNIX credentials.  Use `0` for root.
    pub gid: u32,

    /// If set, connect to the NFS service on this port instead of querying
    /// the portmapper.
    pub nfs_port: Option<u16>,

    /// Maximum bytes per `READ` RPC.  Will be capped to the server's `rtmax`
    /// value from `fsinfo`.  Default: 131072 (128 KiB).
    pub read_chunk_size: u32,

    /// Maximum bytes per `WRITE` RPC.  Will be capped to the server's `wtmax`
    /// value from `fsinfo`.  Default: 131072 (128 KiB).
    pub write_chunk_size: u32,

    /// Number of independent TCP connections to maintain to the NFS server.
    ///
    /// Because `Nfs3Client` allows only one in-flight RPC per connection,
    /// concurrency scales linearly with this value.  Default: 4.
    pub connection_count: usize,
}

impl Default for NfsLocation {
    fn default() -> Self {
        Self {
            host: String::new(),
            export: String::new(),
            sub_path: String::new(),
            uid: 0,
            gid: 0,
            nfs_port: None,
            read_chunk_size: 128 * 1024,
            write_chunk_size: 128 * 1024,
            connection_count: 4,
        }
    }
}

impl NfsLocation {
    /// Create an `NfsLocation` with the given host and export.
    /// All other fields are set to their defaults.
    pub fn new(host: impl Into<String>, export: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            export: export.into(),
            ..Default::default()
        }
    }

    /// Set the sub-path within the export.
    pub fn sub_path(mut self, sub_path: impl Into<String>) -> Self {
        self.sub_path = sub_path.into();
        self
    }

    /// Set the AUTH_UNIX uid/gid presented to the server.
    pub fn credentials(mut self, uid: u32, gid: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self
    }

    /// Override the NFS service port (skip portmapper).
    pub fn nfs_port(mut self, port: u16) -> Self {
        self.nfs_port = Some(port);
        self
    }

    /// Set the number of parallel connections.
    pub fn connection_count(mut self, count: usize) -> Self {
        self.connection_count = count;
        self
    }

    /// Set the read chunk size (bytes per READ RPC).
    pub fn read_chunk_size(mut self, size: u32) -> Self {
        self.read_chunk_size = size;
        self
    }

    /// Set the write chunk size (bytes per WRITE RPC).
    pub fn write_chunk_size(mut self, size: u32) -> Self {
        self.write_chunk_size = size;
        self
    }

    /// Parse an NFS URL of the form `nfs://HOST/EXPORT_PATH`.
    ///
    /// The full path component of the URL becomes the export.  Optional query
    /// parameters:
    /// - `sub=VALUE` — sub-path within the export
    /// - `uid=VALUE` — AUTH_UNIX uid to present to the server (default: 0)
    /// - `gid=VALUE` — AUTH_UNIX gid to present to the server (default: 0)
    ///
    /// # Examples
    ///
    /// ```
    /// # use fpt::nfs::NfsLocation;
    /// let loc = NfsLocation::from_url("nfs://127.0.0.1/opt/dataset").unwrap();
    /// assert_eq!(loc.host, "127.0.0.1");
    /// assert_eq!(loc.export, "/opt/dataset");
    /// assert_eq!(loc.sub_path, "");
    ///
    /// let loc = NfsLocation::from_url("nfs://192.168.1.10/export/data?sub=project/backup&uid=1000&gid=1000").unwrap();
    /// assert_eq!(loc.host, "192.168.1.10");
    /// assert_eq!(loc.export, "/export/data");
    /// assert_eq!(loc.sub_path, "project/backup");
    /// assert_eq!(loc.uid, 1000);
    /// assert_eq!(loc.gid, 1000);
    /// ```
    pub fn from_url(url: &str) -> Result<Self, String> {
        // Must start with "nfs://"
        let rest = url
            .strip_prefix("nfs://")
            .ok_or_else(|| format!("NFS URL must start with 'nfs://', got: {url}"))?;

        // Split authority (host[:port]) from path[?query]
        let (authority, path_and_query) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => {
                return Err(format!(
                    "NFS URL must include an export path (e.g. nfs://host/export), got: {url}"
                ))
            }
        };

        if authority.is_empty() {
            return Err(format!("NFS URL missing host: {url}"));
        }

        // Split optional port from host
        let (host, nfs_port) = if let Some(colon) = authority.rfind(':') {
            let port_str = &authority[colon + 1..];
            match port_str.parse::<u16>() {
                Ok(p) => (&authority[..colon], Some(p)),
                Err(_) => (authority, None), // colon is part of an IPv6 address literal
            }
        } else {
            (authority, None)
        };

        // Split path from optional query string
        let (export, sub_path, uid, gid) = match path_and_query.find('?') {
            Some(idx) => {
                let export = &path_and_query[..idx];
                let query = &path_and_query[idx + 1..];

                let mut sub = String::new();
                let mut uid: u32 = 0;
                let mut gid: u32 = 0;

                for kv in query.split('&') {
                    if let Some(v) = kv.strip_prefix("sub=") {
                        sub = v.to_string();
                    } else if let Some(v) = kv.strip_prefix("uid=") {
                        uid = v
                            .parse::<u32>()
                            .map_err(|_| format!("invalid uid in NFS URL: '{v}'"))?;
                    } else if let Some(v) = kv.strip_prefix("gid=") {
                        gid = v
                            .parse::<u32>()
                            .map_err(|_| format!("invalid gid in NFS URL: '{v}'"))?;
                    }
                }

                (export, sub, uid, gid)
            }
            None => (path_and_query, String::new(), 0u32, 0u32),
        };

        if export == "/" || export.is_empty() {
            return Err(format!(
                "NFS URL export path must be non-empty (e.g. nfs://host/export), got: {url}"
            ));
        }

        let mut loc = NfsLocation::new(host, export)
            .sub_path(sub_path)
            .credentials(uid, gid);
        if let Some(port) = nfs_port {
            loc = loc.nfs_port(port);
        }
        Ok(loc)
    }
}
