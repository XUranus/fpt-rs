//! NFS error types for Fpt.
//!
//! [`NfsError`] wraps transport errors from `nfs3_client` and NFS-level status
//! codes from `nfs3_types`, providing a unified error type for the NFS module.

use nfs3_client::nfs3_types::nfs3::nfsstat3;

/// Errors that can occur during NFS operations.
#[derive(Debug)]
pub enum NfsError {
    /// Transport-level error (TCP, XDR, RPC, mount, etc.)
    Transport(nfs3_client::error::Error),

    /// NFS server returned a non-OK status code.
    Nfs(nfsstat3, String),

    /// A path component could not be resolved or is invalid.
    Path(String),

    /// An NFS server connection could not be established.
    Connect(String),
}

impl std::fmt::Display for NfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NfsError::Transport(e) => write!(f, "NFS transport error: {e}"),
            NfsError::Nfs(stat, msg) => write!(f, "NFS error {stat}: {msg}"),
            NfsError::Path(p) => write!(f, "NFS path error: {p}"),
            NfsError::Connect(msg) => write!(f, "NFS connect error: {msg}"),
        }
    }
}

impl std::error::Error for NfsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NfsError::Transport(e) => Some(e),
            _ => None,
        }
    }
}

impl From<nfs3_client::error::Error> for NfsError {
    fn from(e: nfs3_client::error::Error) -> Self {
        NfsError::Transport(e)
    }
}
