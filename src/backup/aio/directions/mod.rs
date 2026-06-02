//! SMB and cross-transport backup direction orchestrators.
//!
//! NFS directions have moved to [`crate::nfs::backup`].

pub(crate) mod copy_pipelines;

#[cfg(feature = "smb")]
pub mod local_to_smb;
#[cfg(all(feature = "nfs", feature = "smb"))]
pub mod nfs_to_smb;
#[cfg(feature = "smb")]
pub mod smb_to_local;
#[cfg(all(feature = "nfs", feature = "smb"))]
pub mod smb_to_nfs;
#[cfg(feature = "smb")]
pub mod smb_to_smb;
