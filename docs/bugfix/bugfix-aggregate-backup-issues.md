# Bug Fix Report: Aggregate Backup — File Count Discrepancy and SQLite Connection Exhaustion

**Date**: 2026-04-17  
**Affected Component**: Aggregate backup engine  
**Severity**: High — backup produces incorrect statistics and fails to index a large number of files  

---

## Summary

Two distinct but related bugs were identified during aggregate backup of a dataset with 3905 files across 781 directories:

1. **File count double-counting**: The backup subtask reported 5600 files backed up, when the actual count was 3905.
2. **SQLite connection exhaustion**: 442 out of 781 directories failed to create their aggregate blob index, logging `SQLite error: unable to open database file`.

Both bugs were triggered by the same workload profile: a large number of small files spread across many directories.

---

## Bug 1: File Count Double-Counting

### Symptom

```
Subtask completed: 5600 files (54 MB), 3906 dirs
```

Expected: `3905 files (54 MB), 3906 dirs`

### Root Cause

In `src/backup/bio/copy.rs`, inside `spawn_reader_with_aggregation()`, the stats counter `stats.files_copied` was incremented in two places:

1. When a file was **added to the aggregation buffer** (the `else` branch of `agg_state.add_file()`).
2. When a **blob was created** from a full buffer (inside `agg_state.engine.create_blob()`).

Because most files go through both paths (first buffered, then flushed into a blob), every file was counted twice. Additionally, the "final flush" at the end of the subtask created blobs for the remaining buffered files while those files had already been counted once.

### Fix

Removed the stats increment in the buffer-add branch. Files are now counted only once: when they are successfully written into a blob.

```rust
// Add to buffer
if let Some((dir, files)) = agg_state.add_file(&dir_path, pending) {
    // Buffer full — create blob and count files NOW
    let file_count = files.len() as u64;
    let bytes_in_blob: u64 = files.iter().map(|f| f.data.len() as u64).sum();

    match agg_state.engine.create_blob(&dir, files) {
        Ok(blob_meta) => {
            stats.files_copied.fetch_add(file_count, Ordering::Relaxed);
            stats.bytes_copied.fetch_add(bytes_in_blob, Ordering::Relaxed);
        }
        Err(e) => {
            stats.files_failed.fetch_add(file_count, Ordering::Relaxed);
        }
    }
}
// Note: files added to the buffer are NOT counted here.
// They are counted only when successfully written to a blob.
```

**File**: `src/backup/bio/copy.rs`

---

## Bug 2: SQLite "unable to open database file" — Connection Exhaustion

### Symptom

442 errors of the following form in `backup.log`:

```
[ERROR] fpt::backup::bio::copy - Failed to create final blob for dir /opt/dataset/ds2/d1/d2/d2/d2: Index error: SQLite error: unable to open database file
```

### Root Cause

`AggregateIndex` previously stored a **persistent `Mutex<rusqlite::Connection>`** as a struct field. One `AggregateIndex` instance is created per directory that contains small files. With 781 such directories, the process held 781 open SQLite connections simultaneously.

Each SQLite connection consumes at least one file descriptor (the `.sqlite` database file), plus typically two more for WAL mode (`-wal` and `-shm` files). The default per-process file descriptor limit on Linux is **1024** (`ulimit -n`). With 781 directories × ~3 fds each ≈ 2343 fds required, the limit was exceeded well before all directories were processed. Beyond the limit, `rusqlite::Connection::open()` fails with the OS error `EMFILE` ("too many open files"), which SQLite surfaces as "unable to open database file".

The per-process fd budget was also being consumed by:
- Open source file handles during reading (see related fix: `bugfix-file-handle-leak.md`)
- Standard stdio fds
- Log file handle
- Other library-internal fds

### Fix

Replaced the persistent connection field with a per-operation `open_connection()` helper. The connection is opened at the start of each database operation and dropped (closed) automatically at the end.

#### Struct change

Before:
```rust
pub struct AggregateIndex {
    db_path: PathBuf,
    conn: Mutex<rusqlite::Connection>,
    memory_index: Mutex<HashMap<String, AggregateRestoreInfo>>,
}
```

After:
```rust
pub struct AggregateIndex {
    db_path: PathBuf,
    memory_index: Mutex<HashMap<String, AggregateRestoreInfo>>,
}
```

#### New helper method

```rust
#[cfg(feature = "sqlite")]
fn open_connection(&self) -> Result<rusqlite::Connection, AggregateIndexError> {
    let conn = rusqlite::Connection::open(&self.db_path)?;
    Ok(conn)
}
```

#### Schema initialization

`open()` now opens a temporary connection, applies the schema and WAL pragmas, then drops it immediately:

```rust
pub fn open(db_path: &Path) -> Result<Self, AggregateIndexError> {
    // ...
    #[cfg(feature = "sqlite")]
    {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(INDEX_SCHEMA)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        // conn dropped here — fd released immediately
    }
    // ...
}
```

All six SQLite methods were updated to use `open_connection()`:
- `add_blob_sqlite()`
- `query_file_sqlite()`
- `get_blob_files_sqlite()`
- `is_aggregated()` SQLite branch
- `delete_blob_entries()`
- `get_stats()` SQLite branch

**File**: `src/backup/aggregate_index.rs`

### Trade-off

Opening a new SQLite connection per operation has higher latency than reusing a persistent connection. However, for the aggregate backup use case, each directory's index is written sequentially in a single `add_blob()` call, so the number of open/close cycles is low. The correctness gain (no fd exhaustion) outweighs the minor performance cost.

---

## Related Fix

A third issue — source file handles accumulating under high concurrency — was addressed separately. See `docs/bugfix/bugfix-file-handle-leak.md`.

---

## Verification

Run an aggregate backup over a dataset with many small files spread across hundreds of directories:

```bash
./target/release/fptcli backup \
  --data /path/to/source \
  --target /tmp/test_backup \
  --format aggregated \
  --blob-size 64 \
  --threshold 1024
```

Expected after fix:
- Reported file count matches actual file count in source.
- No `SQLite error: unable to open database file` in `backup.log`.
- No `Too many open files (os error 24)` errors.
