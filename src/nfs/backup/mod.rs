//! NFS backup pipeline, transport, and I/O primitives.

pub(crate) mod delete;
pub(crate) mod hardlink;
pub(crate) mod mtime;
pub(crate) mod phases_impl;
pub(crate) mod pipeline;
pub(crate) mod reader;
pub(crate) mod transport;
pub(crate) mod writer;
