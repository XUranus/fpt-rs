# Task Lifecycle API

Bifrost exposes a common lifecycle trait for scanner, backup, and restore
orchestration:

```rust
pub trait TaskLifecycle {
    type Stats: Clone + Default + Send + 'static;

    fn start(&mut self) -> Result<(), TaskLifecycleError>;
    fn stop(&mut self) -> Result<(), TaskLifecycleError>;
    fn get_stats(&self) -> Self::Stats;
    fn is_complete(&self) -> bool;
}
```

## Adapters

The trait is implemented by adapter types in `frame::lifecycle`:

- `ScannerLifecycleTask` wraps direct local `scanner::Scanner` tasks.
- `FileScannerLifecycleTask<S>` wraps frame-level scanner implementations such
  as local, NFS, and SMB scanners.
- `BackupLifecycleTask` wraps `backup::BackupTask`.
- `RestoreLifecycleTask` wraps `backup::RestoreTask`.

Backup and restore coverage is transport- and format-independent because the
wrapped `BackupTask` / `RestoreTask` already dispatches internally for:

- local, NFS, and SMB
- common and aggregate copy formats
- backup and restore phase control

## Stop Semantics

`stop()` is currently a common control hook, not hard cancellation. Existing
scan/backup/restore engines do not support safe interruption in every path, so
callers should treat `stop()` as best-effort and continue polling
`is_complete()` for actual termination.

Future cancellation work should make engines cooperatively observe a shared
stop token in traversal, read, write, and metadata-writer loops.
