//! Async backup execution for remote-involved data paths.
//!
//! This module provides:
//! - **Source/Target abstractions** ([`source`], [`target`]) — connect to any
//!   transport (local, NFS, SMB) and run post-copy phases.
//! - **Orchestrator** ([`orchestrator`]) — composes source + target into a
//!   single generic backup pipeline.
//! - **Generic copy pipeline** ([`pipeline`], [`executor`]) — parameterized
//!   by [`transport::SourceReader`] / [`transport::TargetWriter`] traits.

#[cfg(feature = "smb")]
pub const DEFAULT_SMB_POOL_SIZE: usize = 2;

pub(crate) mod aggregation;
pub(crate) mod entry;
pub(crate) mod executor;
pub(crate) mod local_fs;
pub mod orchestrator;
pub(crate) mod phases;
pub(crate) mod pipeline;
pub mod source;
pub mod target;
pub(crate) mod transport;
