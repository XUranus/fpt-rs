//! SMB backup pipeline and direction orchestrators.

pub(crate) mod local_to_smb;
pub(crate) mod pipeline;
pub(crate) mod smb_to_local;
pub(crate) mod smb_to_smb;
pub(crate) mod transport;
