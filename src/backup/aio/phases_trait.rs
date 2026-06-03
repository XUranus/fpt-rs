//! Trait for transport-specific post-copy phases.
//!
//! Each transport (local, NFS, SMB) implements this trait to provide
//! hardlink, delete, and mtime restoration after the copy phase.

use std::path::Path;

use crate::backup::PhaseFlags;
use crate::failure::{FailureRecorder, RetryPolicy};

/// Post-copy phases that a backup target must support.
///
/// Implementors run hardlink, delete, and mtime phases as needed.
/// The default implementations are no-ops, so transports only override
/// the phases they support.
#[allow(async_fn_in_trait, dead_code)]
pub trait PostCopyPhases: Send + Sync {
    /// Run hardlink phase (create hard links from hardlink control files).
    async fn run_hardlink_phase(
        &self,
        _ctrl_dir: &Path,
        _source_dir_base: &Path,
        _target_prefix: &str,
        _phase_flags: PhaseFlags,
        _retry_policy: RetryPolicy,
        _failure_recorder: Option<&FailureRecorder>,
    ) {
        // Default: no-op
    }

    /// Run delete phase (delete files/dirs from delete control files).
    async fn run_delete_phase(
        &self,
        _ctrl_dir: &Path,
        _source_dir_base: &Path,
        _target_prefix: &str,
        _phase_flags: PhaseFlags,
        _retry_policy: RetryPolicy,
        _failure_recorder: Option<&FailureRecorder>,
    ) {
        // Default: no-op
    }

    /// Run mtime phase (restore modification times from mtime control files).
    async fn run_mtime_phase(
        &self,
        _ctrl_dir: &Path,
        _source_dir_base: &Path,
        _target_prefix: &str,
        _phase_flags: PhaseFlags,
        _retry_policy: RetryPolicy,
        _failure_recorder: Option<&FailureRecorder>,
    ) {
        // Default: no-op
    }

    /// Run all enabled post-copy phases in order: hardlink, delete, mtime.
    async fn run_all_phases(
        &self,
        ctrl_dir: &Path,
        source_dir_base: &Path,
        target_prefix: &str,
        phase_flags: PhaseFlags,
        retry_policy: RetryPolicy,
        failure_recorder: Option<&FailureRecorder>,
    ) {
        self.run_hardlink_phase(ctrl_dir, source_dir_base, target_prefix, phase_flags, retry_policy, failure_recorder).await;
        self.run_delete_phase(ctrl_dir, source_dir_base, target_prefix, phase_flags, retry_policy, failure_recorder).await;
        self.run_mtime_phase(ctrl_dir, source_dir_base, target_prefix, phase_flags, retry_policy, failure_recorder).await;
    }
}
