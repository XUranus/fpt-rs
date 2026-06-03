//! SMB copy performance metrics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Detailed timing and throughput metrics for SMB copy operations.
#[derive(Debug, Default)]
pub struct SmbCopyMetrics {
    pub ensure_dir_count: AtomicU64,
    pub ensure_dir_ns: AtomicU64,
    pub ensure_dir_max_ns: AtomicU64,
    pub source_open_count: AtomicU64,
    pub source_open_ns: AtomicU64,
    pub source_open_max_ns: AtomicU64,
    pub target_open_count: AtomicU64,
    pub target_open_ns: AtomicU64,
    pub target_open_max_ns: AtomicU64,
    pub srv_copy_count: AtomicU64,
    pub srv_copy_ns: AtomicU64,
    pub srv_copy_max_ns: AtomicU64,
    pub srv_copy_bytes: AtomicU64,
    pub srv_copy_fallback_count: AtomicU64,
    pub read_count: AtomicU64,
    pub read_ns: AtomicU64,
    pub read_max_ns: AtomicU64,
    pub read_bytes: AtomicU64,
    pub write_count: AtomicU64,
    pub write_ns: AtomicU64,
    pub write_max_ns: AtomicU64,
    pub write_bytes: AtomicU64,
    pub source_close_count: AtomicU64,
    pub source_close_ns: AtomicU64,
    pub source_close_max_ns: AtomicU64,
    pub source_close_deferred: AtomicU64,
    pub target_close_count: AtomicU64,
    pub target_close_ns: AtomicU64,
    pub target_close_max_ns: AtomicU64,
    pub target_close_deferred: AtomicU64,
    pub copy_active: AtomicU64,
    pub copy_active_max: AtomicU64,
    pub read_active: AtomicU64,
    pub read_active_max: AtomicU64,
    pub write_active: AtomicU64,
    pub write_active_max: AtomicU64,
}

impl SmbCopyMetrics {
    pub(crate) fn add_with_max(counter: &AtomicU64, nanos: &AtomicU64, max_ns: &AtomicU64, started: Instant) {
        let elapsed = duration_ns(started.elapsed());
        counter.fetch_add(1, Ordering::Relaxed);
        nanos.fetch_add(elapsed, Ordering::Relaxed);
        update_atomic_max(max_ns, elapsed);
    }

    pub(crate) fn add_io(
        counter: &AtomicU64,
        nanos: &AtomicU64,
        max_ns: &AtomicU64,
        bytes: &AtomicU64,
        byte_count: u64,
        started: Instant,
    ) {
        let elapsed = duration_ns(started.elapsed());
        counter.fetch_add(1, Ordering::Relaxed);
        nanos.fetch_add(elapsed, Ordering::Relaxed);
        bytes.fetch_add(byte_count, Ordering::Relaxed);
        update_atomic_max(max_ns, elapsed);
    }

    pub(crate) fn active_guard<'a>(active: &'a AtomicU64, active_max: &'a AtomicU64) -> ActiveMetricGuard<'a> {
        let current = active.fetch_add(1, Ordering::Relaxed) + 1;
        update_atomic_max(active_max, current);
        ActiveMetricGuard { active }
    }

    pub fn timing_summary(&self) -> String {
        let read_count = self.read_count.load(Ordering::Relaxed);
        let read_ns = self.read_ns.load(Ordering::Relaxed);
        let read_bytes = self.read_bytes.load(Ordering::Relaxed);
        let srv_copy_count = self.srv_copy_count.load(Ordering::Relaxed);
        let srv_copy_ns = self.srv_copy_ns.load(Ordering::Relaxed);
        let srv_copy_bytes = self.srv_copy_bytes.load(Ordering::Relaxed);
        let write_count = self.write_count.load(Ordering::Relaxed);
        let write_ns = self.write_ns.load(Ordering::Relaxed);
        let write_bytes = self.write_bytes.load(Ordering::Relaxed);
        format!(
            "ensure_dir={} total={} avg={} max={}, source_open={} total={} avg={} max={}, target_open={} total={} avg={} max={}, srv_copy={} bytes={} avg_bytes={} total={} avg={} max={} rate={} fallback={}, read={} bytes={} avg_bytes={} total={} avg={} max={} rate={}, write={} bytes={} avg_bytes={} total={} avg={} max={} rate={}, active_max: copy={} read={} write={}, source_close={} total={} avg={} max={} deferred={}, target_close={} total={} avg={} max={} deferred={}",
            self.ensure_dir_count.load(Ordering::Relaxed),
            format_duration_ns(self.ensure_dir_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.ensure_dir_ns.load(Ordering::Relaxed), self.ensure_dir_count.load(Ordering::Relaxed)),
            format_duration_ns(self.ensure_dir_max_ns.load(Ordering::Relaxed)),
            self.source_open_count.load(Ordering::Relaxed),
            format_duration_ns(self.source_open_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.source_open_ns.load(Ordering::Relaxed), self.source_open_count.load(Ordering::Relaxed)),
            format_duration_ns(self.source_open_max_ns.load(Ordering::Relaxed)),
            self.target_open_count.load(Ordering::Relaxed),
            format_duration_ns(self.target_open_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.target_open_ns.load(Ordering::Relaxed), self.target_open_count.load(Ordering::Relaxed)),
            format_duration_ns(self.target_open_max_ns.load(Ordering::Relaxed)),
            srv_copy_count,
            format_bytes(srv_copy_bytes),
            format_bytes(avg_u64(srv_copy_bytes, srv_copy_count)),
            format_duration_ns(srv_copy_ns),
            avg_duration_ns(srv_copy_ns, srv_copy_count),
            format_duration_ns(self.srv_copy_max_ns.load(Ordering::Relaxed)),
            format_rate(srv_copy_bytes, srv_copy_ns),
            self.srv_copy_fallback_count.load(Ordering::Relaxed),
            read_count,
            format_bytes(read_bytes),
            format_bytes(avg_u64(read_bytes, read_count)),
            format_duration_ns(read_ns),
            avg_duration_ns(read_ns, read_count),
            format_duration_ns(self.read_max_ns.load(Ordering::Relaxed)),
            format_rate(read_bytes, read_ns),
            write_count,
            format_bytes(write_bytes),
            format_bytes(avg_u64(write_bytes, write_count)),
            format_duration_ns(write_ns),
            avg_duration_ns(write_ns, write_count),
            format_duration_ns(self.write_max_ns.load(Ordering::Relaxed)),
            format_rate(write_bytes, write_ns),
            self.copy_active_max.load(Ordering::Relaxed),
            self.read_active_max.load(Ordering::Relaxed),
            self.write_active_max.load(Ordering::Relaxed),
            self.source_close_count.load(Ordering::Relaxed),
            format_duration_ns(self.source_close_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.source_close_ns.load(Ordering::Relaxed), self.source_close_count.load(Ordering::Relaxed)),
            format_duration_ns(self.source_close_max_ns.load(Ordering::Relaxed)),
            self.source_close_deferred.load(Ordering::Relaxed),
            self.target_close_count.load(Ordering::Relaxed),
            format_duration_ns(self.target_close_ns.load(Ordering::Relaxed)),
            avg_duration_ns(self.target_close_ns.load(Ordering::Relaxed), self.target_close_count.load(Ordering::Relaxed)),
            format_duration_ns(self.target_close_max_ns.load(Ordering::Relaxed)),
            self.target_close_deferred.load(Ordering::Relaxed),
        )
    }
}

pub(crate) struct ActiveMetricGuard<'a> {
    active: &'a AtomicU64,
}

impl Drop for ActiveMetricGuard<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) fn update_atomic_max(max: &AtomicU64, value: u64) {
    let mut current = max.load(Ordering::Relaxed);
    while value > current {
        match max.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

pub(crate) fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

pub(crate) fn format_duration_ns(ns: u64) -> String {
    let ms = ns as f64 / 1_000_000.0;
    format!("{ms:.3}ms")
}

pub(crate) fn avg_duration_ns(total_ns: u64, count: u64) -> String {
    if count == 0 {
        "0.000ms".to_string()
    } else {
        format_duration_ns(total_ns / count)
    }
}

pub(crate) fn avg_u64(total: u64, count: u64) -> u64 {
    if count == 0 {
        0
    } else {
        total / count
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.2}GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.2}MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.2}KiB", bytes_f / KIB)
    } else {
        format!("{bytes}B")
    }
}

pub(crate) fn format_rate(bytes: u64, ns: u64) -> String {
    if bytes == 0 || ns == 0 {
        return "0.00B/s".to_string();
    }
    let seconds = ns as f64 / 1_000_000_000.0;
    format!("{}/s", format_bytes((bytes as f64 / seconds) as u64))
}
