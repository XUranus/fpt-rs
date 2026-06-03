//! SMB backup pipeline, transport, and I/O primitives.

pub(crate) mod delete;
pub(crate) mod executor;
pub(crate) mod hardlink;
pub(crate) mod metrics;
pub(crate) mod mtime;
pub(crate) mod phases_impl;
pub(crate) mod pipeline;
pub(crate) mod transport;
pub(crate) mod writer;
