//! Async I/O operations for NFS backup/restore.
//!
//! Sub-modules:
//! - [`writer`] — NFS write task: create + write chunks + setattr, dir handle cache.
//! - [`reader`] — NFS read task: path lookup + read chunks, file handle cache.
//! - [`delete`] — NFS delete phase: `remove` / `rmdir` RPCs.
//! - [`hardlink`] — NFS hardlink phase: `link` RPC.
//! - [`mtime`] — NFS mtime phase: `setattr` with `SET_TO_CLIENT_TIME`.

pub mod delete;
pub mod hardlink;
pub mod mtime;
pub mod reader;
pub mod writer;
