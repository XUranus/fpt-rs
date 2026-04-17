//! Async I/O backup pipeline.
//!
//! This module contains the AIO copy pipeline used when the backup target
//! (or source) is an NFS server.  It mirrors the BIO pipeline structure but
//! uses Tokio tasks and the NFS connection pool instead of blocking threads
//! and `std::fs::File` handles.
//!
//! Sub-modules:
//! - [`copy`] — [`run_aio_copy_pipeline`]: reads a control file and writes
//!   files to an NFS target using [`crate::nfs::aio::writer::nfs_write_task`].

pub mod copy;
