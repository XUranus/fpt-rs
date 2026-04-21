# Bug Fix Report: SMB Large-File Write Stall During `local -> SMB` Backup

## Summary

Fixed a `local -> SMB` backup stall where `fptcli backup` stopped making progress on large files even though the process remained alive.

Observed behavior:

- the backup subtask thread never exited
- `fptcli` blocked in `JoinHandle::join()`
- the last SMB log line was typically a `write_block()` on the second or later chunk of the same file
- `smb-rs` logged `STATUS_PENDING` (`259`) for the write, but no completion followed

This was reproduced with files around `4 MiB` and a dataset size around `20 GiB`.

## Affected Components

- [src/smb/aio.rs](/home/xuranus/workspace/bifrost/src/smb/aio.rs)
- `fptcli backup` on `local -> SMB`
- potentially any SMB copy path that reused the same fixed-size write loop

## Root Cause Analysis

### What Was Not Broken

The backup job framework was not deadlocked.

`gdb` showed the main thread waiting here:

- `src/frame/backup_job.rs:287`

That means the top-level job was only waiting for the backup subtask thread to finish. The real stall was inside the SMB copy path.

### The Real Failure Pattern

Before the fix, Bifrost used hard-coded SMB I/O chunk sizes:

- read chunk: `1 MiB`
- write chunk: `1 MiB`

Those values were chosen locally and were not derived from the SMB session's negotiated properties.

At runtime, the failing pattern was:

1. open target file on SMB
2. write first `1 MiB` chunk successfully
3. send second `1 MiB` write
4. `smb-rs` receives `STATUS_PENDING`
5. no completion ever arrives back to the caller
6. backup appears stuck forever

This strongly indicated that the fixed `1 MiB` write size was too aggressive for the server / session behavior being exercised, even though the connection itself remained alive.

### Why Lowering Connection Count Alone Was Not Enough

An earlier investigation reduced SMB connection count and SMB file-task concurrency. That improved throughput and removed some scheduling noise, but did **not** fix this stall.

The stall still reproduced with:

- `--smb-connections 1`
- one effective SMB copy task at a time

That ruled out Bifrost-side task fan-out as the primary cause for this bug.

## The Fix

### 1. Stop Using Fixed `1 MiB` SMB Chunks

The fixed constants were replaced with conservative defaults:

- default read chunk: `256 KiB`
- default write chunk: `256 KiB`

### 2. Query Negotiated SMB Limits

The SMB client already exposes negotiated connection information through `smb-rs`:

- `max_read_size`
- `max_write_size`

The Bifrost SMB I/O helpers now query those values from the active client connection.

### 3. Clamp To A Conservative Safe Ceiling

Even after querying negotiated limits, Bifrost does **not** drive the write loop at the maximum advertised size.

Instead it clamps to:

- `256 KiB` max safe read chunk
- `256 KiB` max safe write chunk

This keeps the code simple and avoids replaying the stalled large-write path.

## Implementation Notes

The change was applied in:

- [src/smb/aio.rs](/home/xuranus/workspace/bifrost/src/smb/aio.rs)

Key behavior changes:

- `write_relative_file()` now uses a negotiated-and-clamped write chunk
- `read_relative_file()` now uses a negotiated-and-clamped read chunk
- `copy_relative_file_streaming()` now:
  - reads with a negotiated-and-clamped read chunk
  - splits each write into negotiated-and-clamped write sub-chunks

## Why This Fix Works

This fix removes the stalled request shape rather than trying to recover after the stall occurs.

That is the correct first response because:

- the failure is deterministic at the SMB write operation boundary
- the process is not panicking or returning an error
- the request simply never completes

Reducing per-request size is safer than trying to bolt timeouts or retries on top of a request shape that is already known to wedge.

## Verification

The failing command pattern now completes:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/ds3 \
  --target "smb://127.0.0.1/dataset/out?username=xuranus&password=123456789" \
  --temp-dir /opt/target/work \
  --format common \
  -v
```

Validation completed with:

```bash
cargo build --release --bin fptcli --bin fsbackup --features smb --features nfs
cargo test --lib --features smb --features nfs
```

## Remaining Follow-Up

If a future server still stalls even with `256 KiB` chunks, the next step should be inside `smb-rs` instrumentation:

- log negotiated `max_read_size` / `max_write_size`
- add write-completion timeout diagnostics around pending async writes
- fall back to even smaller writes, such as `64 KiB`, after a pending write exceeds a threshold

At that point the remaining bug would be in SMB client request/completion behavior rather than in Bifrost's backup orchestration.
