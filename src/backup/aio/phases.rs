//! Post-copy phase dispatchers.
//!
//! This module previously contained thin wrappers that delegated to
//! transport-specific phase runners. Those wrappers have been replaced
//! by the [`PostCopyPhases`](super::phases_trait::PostCopyPhases) trait,
//! which each transport implements directly.
//!
//! This file is kept as a module placeholder. It can be removed once
//! all references to `crate::backup::aio::phases` are eliminated.
