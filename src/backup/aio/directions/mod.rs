//! Per-direction backup orchestrators.
//!
//! Each submodule contains a `spawn()` function (thread-spawning wrapper) and
//! a `run()` async function (the actual backup pipeline + post-copy phases)
//! for one source→target transport pair.
//!
//! The `copy_pipelines` submodule contains the generic copy pipeline functions
//! that each direction orchestrator delegates to.

pub(crate) mod copy_pipelines;

#[cfg(feature = "nfs")]
pub mod local_to_nfs;
#[cfg(feature = "smb")]
pub mod local_to_smb;
#[cfg(feature = "nfs")]
pub mod nfs_to_local;
#[cfg(feature = "nfs")]
pub mod nfs_to_nfs;
#[cfg(all(feature = "nfs", feature = "smb"))]
pub mod nfs_to_smb;
#[cfg(feature = "smb")]
pub mod smb_to_local;
#[cfg(all(feature = "nfs", feature = "smb"))]
pub mod smb_to_nfs;
#[cfg(feature = "smb")]
pub mod smb_to_smb;
