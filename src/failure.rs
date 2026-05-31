use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File};
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureLogFormat {
    Csv,
    Json,
    Xml,
}

impl FailureLogFormat {
    pub fn extension(self) -> &'static str {
        match self {
            FailureLogFormat::Csv => "csv",
            FailureLogFormat::Json => "json",
            FailureLogFormat::Xml => "xml",
        }
    }
}

impl fmt::Display for FailureLogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FailureLogFormat::Csv => write!(f, "csv"),
            FailureLogFormat::Json => write!(f, "json"),
            FailureLogFormat::Xml => write!(f, "xml"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailureLogConfig {
    pub path: PathBuf,
    pub format: FailureLogFormat,
}

impl FailureLogConfig {
    pub fn new(path: impl Into<PathBuf>, format: FailureLogFormat) -> Self {
        Self {
            path: path.into(),
            format,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub retry_delay: Duration,
    pub backoff_multiplier: f64,
    pub max_retry_delay: Duration,
    pub jitter_ratio: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_delay: Duration::from_secs(1),
            backoff_multiplier: 1.0,
            max_retry_delay: Duration::from_secs(1),
            jitter_ratio: 0.0,
        }
    }
}

impl RetryPolicy {
    pub fn new(max_retries: u32, retry_delay: Duration) -> Self {
        Self {
            max_retries,
            retry_delay,
            max_retry_delay: retry_delay,
            ..Self::default()
        }
    }

    pub fn with_backoff(mut self, backoff_multiplier: f64, max_retry_delay: Duration) -> Self {
        self.backoff_multiplier = backoff_multiplier.max(1.0);
        self.max_retry_delay = max_retry_delay.max(self.retry_delay);
        self
    }

    pub fn with_jitter(mut self, jitter_ratio: f64) -> Self {
        self.jitter_ratio = jitter_ratio.clamp(0.0, 1.0);
        self
    }

    pub fn max_attempts(self) -> u32 {
        self.max_retries.saturating_add(1)
    }

    pub fn should_retry(self, attempt: u32) -> bool {
        attempt < self.max_attempts()
    }

    pub fn delay_for_attempt(self, failed_attempt: u32) -> Duration {
        let exponent = failed_attempt.saturating_sub(1) as i32;
        let factor = self.backoff_multiplier.powi(exponent);
        let base_delay = duration_mul(self.retry_delay, factor).min(self.max_retry_delay);
        apply_deterministic_jitter(base_delay, self.jitter_ratio, failed_attempt)
    }
}

fn duration_mul(duration: Duration, multiplier: f64) -> Duration {
    if duration.is_zero() || multiplier <= 0.0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64((duration.as_secs_f64() * multiplier).min(u64::MAX as f64))
}

fn apply_deterministic_jitter(delay: Duration, jitter_ratio: f64, attempt: u32) -> Duration {
    if delay.is_zero() || jitter_ratio <= 0.0 {
        return delay;
    }
    let seed = attempt.wrapping_mul(1_103_515_245).wrapping_add(12_345);
    let unit = (seed % 10_000) as f64 / 10_000.0;
    let centered = (unit * 2.0) - 1.0;
    let factor = (1.0 + centered * jitter_ratio).max(0.0);
    duration_mul(delay, factor)
}

struct RetryQueue<I> {
    queue: VecDeque<ScheduledRetry<I>>,
}

struct ScheduledRetry<I> {
    item: I,
    attempt: u32,
    ready_at: Instant,
}

impl<I> RetryQueue<I> {
    fn new(item: I) -> Self {
        let mut queue = VecDeque::new();
        queue.push_back(ScheduledRetry {
            item,
            attempt: 1,
            ready_at: Instant::now(),
        });
        Self { queue }
    }

    fn pop_ready_sync(&mut self) -> Option<(I, u32)> {
        let next = self.queue.pop_front()?;
        sleep_until_sync(next.ready_at);
        Some((next.item, next.attempt))
    }

    async fn pop_ready_async(&mut self) -> Option<(I, u32)> {
        let next = self.queue.pop_front()?;
        sleep_until_async(next.ready_at).await;
        Some((next.item, next.attempt))
    }

    fn retry_or_fail(
        &mut self,
        policy: RetryPolicy,
        item: I,
        failed_attempt: u32,
    ) -> Result<(), (I, u32)> {
        if policy.should_retry(failed_attempt) {
            self.queue.push_back(ScheduledRetry {
                item,
                attempt: failed_attempt + 1,
                ready_at: Instant::now() + policy.delay_for_attempt(failed_attempt),
            });
            Ok(())
        } else {
            Err((item, failed_attempt))
        }
    }
}

fn sleep_until_sync(ready_at: Instant) {
    let now = Instant::now();
    if ready_at > now {
        thread::sleep(ready_at - now);
    }
}

async fn sleep_until_async(ready_at: Instant) {
    let now = Instant::now();
    if ready_at > now {
        tokio::time::sleep(ready_at - now).await;
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureItemType {
    File,
    Directory,
    Symlink,
    Special,
    Block,
    Unknown,
}

impl FailureItemType {
    fn as_str(self) -> &'static str {
        match self {
            FailureItemType::File => "file",
            FailureItemType::Directory => "directory",
            FailureItemType::Symlink => "symlink",
            FailureItemType::Special => "special",
            FailureItemType::Block => "block",
            FailureItemType::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureRecord {
    pub time: String,
    pub phase: String,
    pub operation: String,
    pub item_type: FailureItemType,
    pub path: String,
    pub code: String,
    pub detail: String,
    pub attempts: u32,
}

impl FailureRecord {
    pub fn new(
        phase: impl Into<String>,
        operation: impl Into<String>,
        item_type: FailureItemType,
        path: impl Into<String>,
        code: impl Into<String>,
        detail: impl Into<String>,
        attempts: u32,
    ) -> Self {
        Self {
            time: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            phase: phase.into(),
            operation: operation.into(),
            item_type,
            path: path.into(),
            code: code.into(),
            detail: detail.into(),
            attempts,
        }
    }

    pub fn from_io_error(
        phase: impl Into<String>,
        operation: impl Into<String>,
        item_type: FailureItemType,
        path: impl Into<String>,
        err: &io::Error,
        attempts: u32,
    ) -> Self {
        let code = classify_io_error(err);
        Self::new(
            phase,
            operation,
            item_type,
            path,
            code,
            err.to_string(),
            attempts,
        )
    }

    pub fn from_detail(
        phase: impl Into<String>,
        operation: impl Into<String>,
        item_type: FailureItemType,
        path: impl Into<String>,
        detail: impl Into<String>,
        attempts: u32,
    ) -> Self {
        let detail = detail.into();
        let code = classify_error_detail(&detail);
        Self::new(phase, operation, item_type, path, code, detail, attempts)
    }
}

#[derive(Clone)]
pub struct FailureRecorder {
    inner: Arc<Mutex<FailureRecorderInner>>,
}

struct FailureRecorderInner {
    format: FailureLogFormat,
    file: File,
    wrote_any: bool,
}

impl FailureRecorder {
    pub fn create(config: &FailureLogConfig) -> io::Result<Self> {
        if let Some(parent) = config.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::create(&config.path)?;
        match config.format {
            FailureLogFormat::Csv => {
                writeln!(
                    file,
                    "time,phase,operation,item_type,path,code,detail,attempts"
                )?;
            }
            FailureLogFormat::Json => {
                writeln!(file, "[")?;
            }
            FailureLogFormat::Xml => {
                writeln!(file, "<failures>")?;
            }
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(FailureRecorderInner {
                format: config.format,
                file,
                wrote_any: false,
            })),
        })
    }

    pub fn record(&self, record: FailureRecord) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Err(e) = inner.write_record(&record) {
                log::warn!("Failed to write failure record: {e}");
            }
        }
    }
}

impl FailureRecorderInner {
    fn write_record(&mut self, record: &FailureRecord) -> io::Result<()> {
        match self.format {
            FailureLogFormat::Csv => {
                let mut writer = csv::WriterBuilder::new()
                    .has_headers(false)
                    .from_writer(Vec::new());
                writer.serialize(record)?;
                let data = writer.into_inner().map_err(csv_into_inner_err)?;
                self.file.write_all(&data)?;
            }
            FailureLogFormat::Json => {
                if self.wrote_any {
                    writeln!(self.file, ",")?;
                }
                serde_json::to_writer_pretty(&mut self.file, record)?;
            }
            FailureLogFormat::Xml => {
                write!(
                    self.file,
                    "  <failure><time>{}</time><phase>{}</phase><operation>{}</operation><item_type>{}</item_type><path>{}</path><code>{}</code><detail>{}</detail><attempts>{}</attempts></failure>\n",
                    xml_escape(&record.time),
                    xml_escape(&record.phase),
                    xml_escape(&record.operation),
                    record.item_type.as_str(),
                    xml_escape(&record.path),
                    xml_escape(&record.code),
                    xml_escape(&record.detail),
                    record.attempts,
                )?;
            }
        }
        self.wrote_any = true;
        self.file.flush()
    }
}

impl Drop for FailureRecorderInner {
    fn drop(&mut self) {
        let _ = match self.format {
            FailureLogFormat::Csv => Ok(()),
            FailureLogFormat::Json => writeln!(self.file, "\n]"),
            FailureLogFormat::Xml => writeln!(self.file, "</failures>"),
        };
        let _ = self.file.flush();
    }
}

fn csv_into_inner_err(err: csv::IntoInnerError<csv::Writer<Vec<u8>>>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, err.to_string())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn classify_io_error(err: &io::Error) -> String {
    if let Some(code) = err.raw_os_error() {
        classify_errno(code)
    } else {
        classify_error_detail(&err.to_string())
    }
}

pub fn classify_errno(code: i32) -> String {
    match code {
        libc::EPERM => "EPERM",
        libc::EACCES => "EACCES",
        libc::ENOENT => "ENOENT",
        libc::ENOSPC => "ENOSPC",
        libc::EEXIST => "EEXIST",
        libc::EIO => "EIO",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EBUSY => "EBUSY",
        libc::EROFS => "EROFS",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::ECONNRESET => "ECONNRESET",
        libc::ECONNREFUSED => "ECONNREFUSED",
        _ => return format!("ERRNO_{code}"),
    }
    .to_string()
}

pub fn classify_error_detail(detail: &str) -> String {
    let upper = detail.to_ascii_uppercase();
    for token in [
        "EPERM",
        "EACCES",
        "ENOENT",
        "ENOSPC",
        "EEXIST",
        "EIO",
        "ENOTDIR",
        "EISDIR",
        "EBUSY",
        "EROFS",
        "ETIMEDOUT",
        "ECONNRESET",
        "ECONNREFUSED",
    ] {
        if upper.contains(token) {
            return token.to_string();
        }
    }
    for token in [
        "NFS3ERR_JUKEBOX",
        "NFS3ERR_ACCES",
        "NFS3ERR_NOENT",
        "NFS3ERR_NOSPC",
        "NFS3ERR_IO",
        "OBJECT PATH NOT FOUND",
        "NETWORK NAME DELETED",
        "ACCESS DENIED",
        "PERMISSION DENIED",
        "NO SUCH FILE",
        "NO SPACE LEFT",
    ] {
        if upper.contains(token) {
            return token.replace(' ', "_");
        }
    }
    "UNKNOWN".to_string()
}

pub fn failure_file_path(dir: &Path, base_name: &str, format: FailureLogFormat) -> PathBuf {
    dir.join(format!("{base_name}.{}", format.extension()))
}

pub fn retry_sync<T, E, F>(policy: RetryPolicy, mut op: F) -> Result<T, (E, u32)>
where
    F: FnMut() -> Result<T, E>,
{
    retry_sync_item(policy, (), |_| op().map_err(|err| ((), err)))
        .map_err(|(_, err, attempts)| (err, attempts))
}

pub async fn retry_async<T, E, F, Fut>(policy: RetryPolicy, mut op: F) -> Result<T, (E, u32)>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut queue = RetryQueue::new(());
    while let Some(((), attempt)) = queue.pop_ready_async().await {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if let Err(((), attempts)) = queue.retry_or_fail(policy, (), attempt) {
                    return Err((err, attempts));
                }
            }
        }
    }
    unreachable!("retry queue always returns from the loop")
}

pub fn retry_sync_item<I, T, E, F>(
    policy: RetryPolicy,
    item: I,
    mut op: F,
) -> Result<T, (I, E, u32)>
where
    F: FnMut(I) -> Result<T, (I, E)>,
{
    let mut queue = RetryQueue::new(item);
    while let Some((item, attempt)) = queue.pop_ready_sync() {
        match op(item) {
            Ok(value) => return Ok(value),
            Err((item, err)) => {
                if let Err((item, attempts)) = queue.retry_or_fail(policy, item, attempt) {
                    return Err((item, err, attempts));
                }
            }
        }
    }
    unreachable!("retry queue always returns from the loop")
}

pub async fn retry_async_item<I, T, E, F, Fut>(
    policy: RetryPolicy,
    item: I,
    mut op: F,
) -> Result<T, (I, E, u32)>
where
    F: FnMut(I) -> Fut,
    Fut: Future<Output = Result<T, (I, E)>>,
{
    let mut queue = RetryQueue::new(item);
    while let Some((item, attempt)) = queue.pop_ready_async().await {
        match op(item).await {
            Ok(value) => return Ok(value),
            Err((item, err)) => {
                if let Err((item, attempts)) = queue.retry_or_fail(policy, item, attempt) {
                    return Err((item, err, attempts));
                }
            }
        }
    }
    unreachable!("retry queue always returns from the loop")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn retry_sync_item_preserves_item_and_attempt_count() {
        let policy = RetryPolicy::new(2, Duration::ZERO);
        let attempts = AtomicU32::new(0);

        let err = retry_sync_item(policy, String::from("block-1"), |item| {
            attempts.fetch_add(1, Ordering::Relaxed);
            Err::<(), _>((item, "failed"))
        })
        .unwrap_err();

        assert_eq!(err.0, "block-1");
        assert_eq!(err.1, "failed");
        assert_eq!(err.2, 3);
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn retry_sync_succeeds_after_delayed_retry() {
        let policy = RetryPolicy::new(2, Duration::ZERO);
        let attempts = AtomicU32::new(0);

        let value = retry_sync(policy, || {
            let attempt = attempts.fetch_add(1, Ordering::Relaxed) + 1;
            if attempt < 2 {
                Err("not yet")
            } else {
                Ok("done")
            }
        })
        .unwrap();

        assert_eq!(value, "done");
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn retry_policy_supports_exponential_backoff_cap() {
        let policy = RetryPolicy::new(5, Duration::from_millis(100))
            .with_backoff(2.0, Duration::from_millis(250));

        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(200));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(250));
        assert_eq!(policy.delay_for_attempt(4), Duration::from_millis(250));
    }

    #[test]
    fn retry_policy_jitter_is_bounded() {
        let policy = RetryPolicy::new(5, Duration::from_millis(100)).with_jitter(0.25);
        let delay = policy.delay_for_attempt(1);

        assert!(delay >= Duration::from_millis(75));
        assert!(delay <= Duration::from_millis(125));
    }
}
