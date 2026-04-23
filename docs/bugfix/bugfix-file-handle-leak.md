# Bug Fix Report: File Handle Leak in Aggregate Backup

## Summary

Fixed a "Too many open files (os error 24)" error that occurred during aggregate backup operations when processing directories with many small files. The issue was caused by file handles not being released promptly enough under high concurrency, leading to exhaustion of the system's file descriptor limit.

## Affected Components

- **File**: `src/backup/aggregate_engine.rs`
- **Function**: `create_blob_from_local_files()`
- **Feature**: Aggregate backup format (small file aggregation)

## Root Cause Analysis

### Previous Aggregation Path Issue

The old aggregation path queued `FileControlBlock` values after reading small-file data into memory. Earlier versions also carried optional source/target file handles inside the FCB state machine. Under high concurrency this design made it easy to keep too many source descriptors and payload buffers alive at once.

### Why File Handles Accumulated

While Rust's drop mechanism should close files when variables go out of scope, several factors contributed to file descriptor exhaustion:

1. **High Concurrency**: Multiple workers could process small files simultaneously.
2. **Rapid File Opening**: The scanner quickly enumerated files, sending them to the reader pipeline.
3. **Queued Payloads**: Small-file payloads could accumulate in memory before blob flush.
4. **Queued Handles**: Older FCB state carried file handle slots that made ownership harder to reason about.
5. **Many Small Files**: Aggregate backups target small files, meaning many files are processed quickly.

When the system file descriptor limit (default 1024 on most Linux systems) is reached, subsequent `File::open()` calls fail with "Too many open files (os error 24)".

## The Fix

### Code Change

In `src/backup/bio/copy.rs`, function `spawn_reader_with_aggregation()`:

```rust
if should_aggregate(file_size) {
    // Current implementation: aggregation stores pending file descriptors as
    // paths and sizes only. Blob flush opens each source file, streams it into
    // the blob with a bounded scratch buffer, and immediately drops the handle.
    pending_local_files.push(PendingLocalFile {
        relative_path,
        source_path,
        size,
        // metadata omitted
    });
}
```

### Why This Fix Works

1. **No queued handles**: File handles are no longer stored in `FileControlBlock` or queued between stages.
2. **Bounded memory**: Aggregation avoids storing small-file payloads in memory; it stores path metadata and streams data during blob flush.
3. **Immediate release**: Each source file is opened only for the duration of its stream-copy into the aggregate blob.
4. **Targeted**: The fix affects aggregation without changing common backup semantics.

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
