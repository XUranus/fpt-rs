# Bug Fix Report: "Too Many Open Files" — EMFILE at File Open Time

**Date**: 2026-04-17  
**Affected Component**: Aggregate backup engine, `fptcli` entry point  
**Severity**: High — backup fails for large datasets with many small files across many directories  

---

## Symptom

When running an aggregate backup over a large dataset (e.g. 3905 files across 781 directories), many files fail with:

```
[ERROR] bifrost::backup::bio::copy - Failed to open source file "/opt/dataset/.../f3": Too many open files (os error 24)
```

The errors appear in bursts, all at approximately the same timestamp, indicating the fd limit was crossed suddenly during a period of high concurrency.

---

## Root Cause

### Linux Per-Process File Descriptor Limits

Linux enforces two fd limits per process:

| Limit | Default | Description |
|-------|---------|-------------|
| Soft limit (`RLIMIT_NOFILE` soft) | **1024** | Enforced at `open()` time — `EMFILE` is returned when this is exceeded |
| Hard limit (`RLIMIT_NOFILE` hard) | **524288** (512K) | Ceiling that an unprivileged process can raise its soft limit to |

The soft limit is what `ulimit -n` shows in a shell. Most shells and system services inherit the default of 1024.

### How the Old Aggregate Backup Pipeline Accumulated File Descriptors

The backup engine runs a multi-threaded pipeline:

```
FCB producer thread
    │
    ▼
reader_rx channel ──► reader control thread
                           │
                           ▼
                      reader_io_pool_tx ──► N reader I/O threads
                                                │  (each calls open() + read())
                                                ▼
                                          FCB / result with source state
                                                │
                                                ▼
                                      result channel back to reader control thread
                                                │
                                                ▼
                                         source handle released here
```

At any moment, the following fds are open simultaneously:

- **N reader I/O threads × in-flight files**: each in-flight open/read operation can hold one source file fd. With 4+ reader threads and many small files, dozens of descriptors can overlap.
- **Writer threads**: target blob files being written.
- **SQLite connections**: one per `add_blob()` call (short-lived but overlapping).
- **Infrastructure**: stdin (0), stdout (1), stderr (2), the backup log file, metadata reader files, control file reader.

With the default soft limit of 1024, this total is easily exceeded when many directories are processed in parallel, causing every subsequent `File::open()` call to fail with `EMFILE`.

### Why Raising `RLIMIT_NOFILE` Was Not Sufficient By Itself

Raising the process fd limit made the failure less likely, but it did not address the underlying resource model. The current aggregation path stores only path metadata for pending small files, then opens each file during blob flush, streams it into the blob with a bounded scratch buffer, and immediately drops the handle. `FileControlBlock` no longer stores source/target file handles.

---

## Fix

### Approach: Raise the Soft Limit at Process Startup

A process is always permitted to raise its own soft `RLIMIT_NOFILE` up to the hard limit, without any special privileges (`CAP_SYS_RESOURCE`). This is the standard approach used by production daemons (e.g. nginx, PostgreSQL, systemd services) to work around the low default.

The fix is applied in `src/bin/fptcli.rs`, at the very start of `main()`, before any threads or file operations are started:

```rust
#[cfg(unix)]
{
    use nix::sys::resource::{getrlimit, setrlimit, Resource};
    match getrlimit(Resource::RLIMIT_NOFILE) {
        Ok((soft, hard)) => {
            if soft < hard {
                if let Err(e) = setrlimit(Resource::RLIMIT_NOFILE, hard, hard) {
                    eprintln!("Warning: failed to raise fd limit from {} to {}: {}", soft, hard, e);
                }
            }
        }
        Err(e) => {
            eprintln!("Warning: failed to query fd limit: {}", e);
        }
    }
}
```

**Result on a typical Linux system**: soft limit raised from 1024 → 524288 at startup.

### Dependency Addition (`Cargo.toml`)

The `resource` feature was added to the `nix` crate dependency (which was already present for filesystem operations):

```toml
[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["fs", "resource"] }
```

### Why Not Reduce Concurrency Instead?

Reducing the number of reader I/O threads would lower fd usage but also significantly reduce backup throughput. Raising the fd limit is the correct solution: it removes an arbitrary OS default constraint without changing the backup engine's behavior or performance characteristics.

### Why Not Use `ulimit -n` Before Running?

Relying on the caller's shell environment to set `ulimit -n` before invoking `fptcli` is fragile — the tool may be invoked from scripts, cron jobs, or systemd units that inherit the default. Raising the limit programmatically at startup makes the tool self-sufficient.

---

## Verification

Run a full aggregate backup on a dataset with many small files:

```bash
./target/release/fptcli backup \
  --data /path/to/source \
  --target /tmp/test_backup \
  --format aggregated \
  --blob-size 64 \
  --threshold 1024
```

Expected after fix:
- No `"Too many open files (os error 24)"` errors in `backup.log`.
- All source files successfully opened, read, and aggregated into blobs.

To confirm the limit was raised, you can check `/proc/<pid>/limits` while the process is running:

```bash
grep "Max open files" /proc/$(pgrep fptcli)/limits
```

Expected output:
```
Max open files            524288               524288               files
```

---

## Related Reports

- `docs/bugfix/bugfix-file-handle-leak.md` — file handles held open longer than necessary after reading (addressed premature accumulation)
- `docs/bugfix/bugfix-aggregate-backup-issues.md` — SQLite connection exhaustion and file count double-counting
