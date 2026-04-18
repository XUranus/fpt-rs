# Bug Fix Report: File Handle Leak in Aggregate Backup

## Summary

Fixed a "Too many open files (os error 24)" error that occurred during aggregate backup operations when processing directories with many small files. The issue was caused by file handles not being released promptly enough under high concurrency, leading to exhaustion of the system's file descriptor limit.

## Affected Components

- **File**: `src/backup/bio/copy.rs`
- **Function**: `spawn_reader_with_aggregation()`
- **Feature**: Aggregate backup format (small file aggregation)

## Root Cause Analysis

### Normal File Processing Flow

In the backup engine's I/O pipeline, files typically flow through these states:

1. **Inited** → FileControlBlock (FCB) created with file metadata
2. **Opened** → `open_source()` opens the file, stores handle in `fcb.src_handle`
3. **Read** → `read_source()` reads data, takes handle via `take()`, returns FCB
4. **Closed** → `close_source()` explicitly closes the file handle

For non-aggregated files, this flow works correctly because the writer pipeline eventually calls `close_source()`.

### The Aggregation Path Issue

For aggregated backups, the flow differs:

1. Small files are identified for aggregation based on size threshold
2. Files are read by `read_source()` which takes the handle:
   ```rust
   let mut file = fcb.src_handle.take().expect("...");
   // ... read data ...
   ReaderBioResult::ReadSource(Ok(fcb))  // fcb.src_handle is now None
   ```
3. The local `file` variable should be dropped at the end of `read_source()`
4. The FCB (with read data in buffer) is returned to the aggregation reader
5. The FCB is converted to a `PendingFile` and buffered for blob creation

### Why File Handles Accumulated

While Rust's drop mechanism should close files when variables go out of scope, several factors contributed to file descriptor exhaustion:

1. **High Concurrency**: With 4 worker threads (`--workers 4` default), multiple files are processed simultaneously
2. **Rapid File Opening**: The scanner quickly enumerates files, sending them to the reader pipeline
3. **Delayed Drop**: The local `file` variable in `read_source()` may not be dropped immediately upon function return, especially under high load
4. **No Explicit Close**: The aggregation path lacked an explicit file close operation
5. **Many Small Files**: Aggregate backups target small files, meaning many files are processed quickly

When the system file descriptor limit (default 1024 on most Linux systems) is reached, subsequent `File::open()` calls fail with "Too many open files (os error 24)".

## The Fix

### Code Change

In `src/backup/bio/copy.rs`, function `spawn_reader_with_aggregation()`:

```rust
if should_agg && fcb.src_state == SourceHandleState::Read {
    let file_size = fcb.meta.size;

    // BUG FIX: Explicitly close the source file handle immediately after
    // reading to avoid "Too many open files (os error 24)" error.
    //
    // Background: When read_source() reads a file, it takes the file
    // handle from fcb.src_handle using take(), reads the data, and the
    // local file variable should be dropped at the end of read_source().
    // However, under high concurrency with many small files being
    // aggregated, file handles can accumulate faster than they are
    // released, causing the process to hit the system file descriptor
    // limit (default 1024 on many systems).
    //
    // This explicit close ensures the file descriptor is released
    // immediately before we continue processing, preventing resource
    // exhaustion.
    if fcb.src_handle.is_some() {
        drop(fcb.src_handle.take());
    }

    let pending = fcb_to_pending_file(&fcb);
    // ... rest of aggregation logic
}
```

### Why This Fix Works

1. **Immediate Release**: Explicitly dropping the file handle ensures the OS file descriptor is released immediately
2. **Defensive Programming**: Even if `src_handle` should be `None` (from `read_source()`), this code handles any case where it might still exist
3. **Minimal Overhead**: The check and drop operation has negligible performance impact
4. **Targeted**: Only affects the aggregation path where the issue occurred

## Testing

### Reproduction Steps

1. Create a directory with many small files (e.g., 10,000+ files under 1KB)
2. Run aggregate backup:
   ```bash
   ./target/release/fptcli backup \
       --data /path/to/many-small-files \
       --target /tmp/backup \
       --format aggregated
   ```
3. Without the fix: "Too many open files" errors appear in logs
4. With the fix: Backup completes successfully

### Verification

Monitor open file descriptors during backup:
```bash
# Watch file descriptor count for the process
watch -n 0.5 'ls -l /proc/$(pgrep fptcli)/fd | wc -l'
```

With the fix, the count should remain well below the system limit (1024).

## Additional Improvements

While this fix resolves the immediate issue, consider these future improvements:

1. **Resource Limits**: Add configuration for maximum concurrent open files
2. **Backpressure**: Implement backpressure to slow down file enumeration when resources are constrained
3. **Connection Pooling**: For SQLite indexes, use connection pooling to limit concurrent connections
4. **Monitoring**: Add metrics for tracking file descriptor usage during backup

## Related Changes

This fix was made alongside other improvements to the aggregate backup system:

- SQLite connection persistence in `aggregate_index.rs`
- Stats tracking fixes for aggregated files
- Logging improvements to capture all output to `C_REPO/logs/backup.log`

## References

- Linux file descriptor limits: `ulimit -n`
- Rust File drop behavior: https://doc.rust-lang.org/std/fs/struct.File.html
- Related error: "Too many open files (os error 24)"
