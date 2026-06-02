//! Shared utilities for scanner traversal (both bio and aio).
//!
//! Extracts duplicated patterns from `bio/traversal.rs`, `nfs/scanner.rs`,
//! and `smb/scanner.rs`:
//! - Failure recording

use crate::failure::{FailureItemType, FailureRecorder, FailureRecord, RetryPolicy};

// ---------------------------------------------------------------------------
// Async retry (thin wrapper over failure::retry_async)
// ---------------------------------------------------------------------------

/// Generic async retry wrapper that strips the attempt count from the error.
///
/// Delegates to [`crate::failure::retry_async`] which preserves attempt metadata.
/// This wrapper exists because scanner callers expect `Result<T, E>` while
/// the failure module returns `Result<T, (E, u32)>`.
#[allow(dead_code)] // used when nfs or smb feature is enabled
pub(crate) async fn retry_async<F, Fut, T, E>(
    retry_policy: RetryPolicy,
    op: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    crate::failure::retry_async(retry_policy, op)
        .await
        .map_err(|(err, _attempt)| err)
}

// ---------------------------------------------------------------------------
// Failure recording
// ---------------------------------------------------------------------------

/// Record a scan failure to the failure log (if configured).
///
/// Shared by bio, NFS, and SMB scanners.
pub(crate) fn record_scan_failure(
    recorder: Option<&FailureRecorder>,
    operation: &str,
    item_type: FailureItemType,
    path: &str,
    detail: impl Into<String>,
    attempts: u32,
) {
    if let Some(recorder) = recorder {
        recorder.record(FailureRecord::from_detail(
            "scan",
            operation,
            item_type,
            path.to_string(),
            detail.into(),
            attempts,
        ));
    }
}
