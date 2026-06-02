//! Shared utilities for scanner traversal (both bio and aio).
//!
//! Extracts duplicated patterns from `bio/traversal.rs`, `nfs/scanner.rs`,
//! and `smb/scanner.rs`:
//! - Async retry wrapper
//! - Failure recording

use crate::failure::{FailureItemType, FailureRecorder, FailureRecord, RetryPolicy};

// ---------------------------------------------------------------------------
// Async retry
// ---------------------------------------------------------------------------

/// Generic async retry wrapper used by NFS and SMB scanners.
///
/// Retries `op` up to `retry_policy.max_retries` times with exponential backoff.
#[allow(dead_code)]
pub(crate) async fn retry_async<F, Fut, T, E>(
    retry_policy: RetryPolicy,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut attempts = 0;
    loop {
        attempts += 1;
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) if retry_policy.should_retry(attempts) => {
                tokio::time::sleep(retry_policy.delay_for_attempt(attempts)).await;
                let _ = &err;
            }
            Err(err) => return Err(err),
        }
    }
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
