//! Cross-transport backup direction orchestrators.
//!
//! NFS directions have moved to [`crate::nfs::backup`].
//! SMB directions have moved to [`crate::smb::backup`].

pub(crate) mod copy_pipelines;

#[cfg(all(feature = "nfs", feature = "smb"))]
pub mod nfs_to_smb;
#[cfg(all(feature = "nfs", feature = "smb"))]
pub mod smb_to_nfs;
