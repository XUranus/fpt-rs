//! Local filesystem backup pipeline (BIO — blocking I/O).
//!
//! This module contains the local-to-local backup implementation using
//! OS threads and `std::fs`. It is the BIO counterpart to the AIO pipelines
//! in [`crate::backup::aio`] which handle NFS/SMB transports.

pub(crate) mod bio;
pub(crate) mod local_block;
pub(crate) mod local_executor;
pub(crate) mod local_metadata;
pub(crate) mod phases;
