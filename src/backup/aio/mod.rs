//! Async I/O backup pipeline.
//!
//! This module contains the AIO copy pipeline used when the backup target
//! (or source) is an NFS server.  It mirrors the BIO pipeline structure but
//! uses Tokio tasks and the NFS connection pool instead of blocking threads
//! and `std::fs::File` handles.
//!
//! Sub-modules:
//! - [`copy`] — [`run_aio_copy_pipeline`]: local source → NFS target.
//! - [`nfs_to_local`] — [`run_aio_nfs_to_local_pipeline`]: NFS source → local target.
//! - [`nfs_to_nfs`] — [`run_aio_nfs_to_nfs_pipeline`]: NFS source → NFS target.

pub mod copy;
pub mod nfs_to_local;
pub mod nfs_to_nfs;
