# Bifrost Aggregate Backup/Restore

## Overview

The aggregate backup feature in Bifrost is inspired by the DPA (Data Protection Agent) project. It combines multiple small files into larger "blob" files to improve backup and restore performance when dealing with millions of small files.

## Key Concepts

### Blob File

A large file (default max 64MB) containing multiple small files concatenated together. Blob files are stored in `.AGGR_DIR/` subdirectories and named with unique Snowflake IDs.

Example: `086a1642cb88e000.bifrost.blob`

### Aggregate Index

An SQLite database that maps original filenames to their locations within blob files. Each directory has its own index (0 or 1 per directory) for better scalability:

- **Per-directory index**: Each directory has its own `AGGREGATE_IDX.sqlite` inside `.AGGR_DIR/`
- **Query performance**: Small indexes provide fast lookups even with millions of files
- **Parallel processing**: Different directories can be processed independently

The index stores:
- File name and directory path
- Blob file name
- Offset within the blob
- File size
- Timestamps and metadata

### Aggregation Threshold

Files smaller than the threshold (default 1MB) are candidates for aggregation. Larger files are backed up normally to their original locations.

## Architecture

### Data Structures

```rust
// Configuration
pub struct AggregateConfig {
    pub enabled: bool,              // Enable aggregation
    pub max_blob_size: u64,         // Max blob size in bytes (default: 64MB)
    pub file_threshold: u64,        // Files smaller than this are aggregated (default: 1MB)
}

// File entry within a blob
pub struct AggregateFileEntry {
    pub file_name: String,          // Original filename
    pub offset: u64,                // Offset within blob
    pub size: u64,                  // File size
    pub ctime: u64,                 // Creation time
    pub mtime: u64,                 // Modification time
    pub mode: u32,                  // File permissions
    pub xattrs: Option<String>,     // Extended attributes
    pub acl: Option<String>,        // ACL
}

// Blob metadata
pub struct AggregateBlobMeta {
    pub blob_name: String,          // Blob filename (Snowflake ID)
    pub blob_size: u64,             // Total blob size
    pub file_count: u32,            // Number of files in blob
    pub files: Vec<AggregateFileEntry>,
    pub dir_path: String,           // Source directory
}
```

### Snowflake ID Generator

Blob filenames are generated using a Snowflake-like algorithm to ensure uniqueness across multiple processes:

**ID Structure (64 bits):**
```
| 41 bits timestamp | 10 bits process | 12 bits sequence | 1 bit reserved |
```

- **Timestamp**: Milliseconds since epoch (41 bits = ~69 years)
- **Process ID**: Unique per process (10 bits = 0-1023)
- **Sequence**: Per-millisecond counter (12 bits = 0-4095)
- **Reserved**: Always 0

This ensures:
- Unique IDs even with multiple backup processes
- Time-ordered IDs for better indexing
- Up to 4096 IDs per millisecond per process

### SQLite Index Schema

```sql
CREATE TABLE aggregate_index (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_name TEXT NOT NULL,
    dir_path TEXT NOT NULL,
    blob_name TEXT NOT NULL,
    offset INTEGER NOT NULL,
    size INTEGER NOT NULL,
    ctime INTEGER,
    mtime INTEGER,
    mode INTEGER,
    xattrs TEXT,
    acl TEXT,
    UNIQUE(file_name, dir_path)
);

CREATE INDEX idx_file_name ON aggregate_index(file_name);
CREATE INDEX idx_blob_name ON aggregate_index(blob_name);
CREATE INDEX idx_dir_path ON aggregate_index(dir_path);
```

## Backup Process

1. **File Collection**: Small files are collected and buffered in memory, grouped by directory
2. **Buffer Management**: When buffer reaches `max_blob_size` or directory is complete, a blob is created
3. **Blob Creation**: Files are concatenated into a blob file in `.AGGR_DIR/` subdirectory
4. **Index Creation**: SQLite index entries are created in `AGGREGATE_IDX.sqlite` inside `.AGGR_DIR/`
5. **Large Files**: Files exceeding the threshold are backed up normally to their original path

### Backup Phases

Aggregate backup uses **only the copy phase**. The hardlink, delete, and mtime phases are skipped because:
- Aggregation handles file grouping internally within blob files
- Hardlinks are tracked via the aggregate index
- File metadata (including mtime) is stored in the blob index

If you enable `--hardlink`, `--delete`, or `--mtime` flags with aggregated format, they will be ignored with a notice.

### Directory Buffering

Files are aggregated within the same directory to maintain locality. Each directory has its own buffer:

```rust
pub struct DirAggregateBuffer {
    pub dir_path: String,
    pub pending_files: Vec<PendingFile>,
    pub current_size: u64,
    pub max_size: u64,
}
```

## Restore Process

1. **Index Query**: For each file to restore, query the per-directory SQLite index
2. **Blob Reading**: Read the blob file from `.AGGR_DIR/` subdirectory (with caching)
3. **Extraction**: Extract the specific byte range for each file
4. **Metadata Restoration**: Restore timestamps, permissions, xattrs, and ACLs

### Restore Phases

Unlike aggregate backup (which only uses the copy phase), aggregate restore executes **all 4 phases** to properly reconstruct the filesystem:

| Phase | Description |
|-------|-------------|
| **Copy** | Extract files from blobs and copy large files |
| **Hardlink** | Create hardlinks between files that share the same inode |
| **Delete** | Remove files that should not exist in the restored state |
| **Mtime** | Restore modification times for all files and directories |

This ensures the restored filesystem matches the original state, including hardlink relationships and directory timestamps.

### Blob Caching

To avoid reading the same blob multiple times when restoring many files from it, a blob cache is maintained:

```rust
pub struct AggregateRestoreEngine {
    blob_cache: Mutex<HashMap<String, Vec<u8>>>,
    // ...
}
```

## Usage

### Using fptcli (Recommended)

The `fptcli` tool provides the easiest way to use aggregate backup:

```bash
# Full backup with aggregation (default: 64MB blobs, 1MB threshold)
./target/release/fptcli backup \
    --data /path/to/source \
    --target /path/to/backup/copy \
    --format aggregated

# Custom blob size and threshold
./target/release/fptcli backup \
    --data /path/to/source \
    --target /path/to/backup/copy \
    --format aggregated \
    --blob-size 128 \
    --threshold 512

# Incremental aggregate backup
./target/release/fptcli backup \
    --data /path/to/source \
    --target /path/to/backup/incremental \
    --format aggregated \
    --incremental-base /path/to/backup/full
```

### Using fsbackup Directly

For advanced use cases, you can use `fsbackup` directly:

```bash
# Enable aggregation with default settings (64MB blobs, 1MB threshold)
./target/release/fsbackup \
    --source /path/to/source \
    --target /path/to/backup \
    --meta-dir ./backup/meta \
    --control-file ./backup/ctrl/copy.txt \
    --aggregate

# Custom blob size and threshold
./target/release/fsbackup \
    --source /path/to/source \
    --target /path/to/backup \
    --meta-dir ./backup/meta \
    --control-file ./backup/ctrl/copy.txt \
    --aggregate \
    --max-blob-size 128 \
    --aggregate-threshold 512
```

### Programmatic API

```rust
use bifrost::backup::{BackupOption, BackupTask};
use bifrost::backup::aggregate::AggregateConfig;

// Create backup option with aggregation
let backup_option = BackupOption::new(
    source_dir,
    target_dir,
    meta_dir,
    ctrl_dir,
    control_file,
)
.enable_aggregation(true)
.aggregate_max_blob_size(64 * 1024 * 1024)      // 64MB
.aggregate_file_threshold(1024 * 1024);         // 1MB

let backup_task: BackupTask = backup_option.into();
let running_backup = backup_task.start()?;
```

## File Layout

### Backup Copy Structure (fptcli)

Each backup copy is created with standardized naming: `COPY_{FORMAT}_{TYPE}_{UUID}/`

```
COPY_AGGR_FULL_999931d2-acc7-477c-8fdb-80c48524f5ed/
├── manifest.json               # Backup manifest at copy root
├── D_REPO/                     # Data repository
│   ├── .AGGR_DIR/              # Aggregation directory for root
│   │   ├── AGGREGATE_IDX.sqlite  # Index for root directory
│   │   └── 086a1642cb88e000.bifrost.blob
│   ├── d0/                     # Same structure as source
│   │   ├── .AGGR_DIR/
│   │   │   ├── AGGREGATE_IDX.sqlite
│   │   │   └── 086a16441a08e000.bifrost.blob
│   │   ├── d0/
│   │   │   ├── .AGGR_DIR/
│   │   │   │   ├── AGGREGATE_IDX.sqlite
│   │   │   │   └── ...
│   │   │   └── ...
│   │   └── ...
│   ├── d1/
│   └── ...
├── M_REPO/                     # Metadata repository
│   └── meta/
│       ├── meta_00000000.dat
│       ├── fcache_00000000.dat
│       └── dcache_00000000.dat
└── C_REPO/                     # Control and logs repository
    ├── ctrl/                   # Control files
    │   ├── copy.txt
    │   ├── hardlink.txt
    │   ├── delete.txt
    │   └── mtime.txt
    ├── logs/                   # Log files
    │   ├── scan.log            # Scanner log
    │   └── {subtask_uuid}.log  # Per-subtask logs
    └── status/                 # Status tracking for crash recovery
        ├── SCAN_{copy_uuid}.DONE
        └── SUBTASK_{uuid}.DONE
```

### Key Points

- **Copy naming**: `COPY_{FORMAT}_{TYPE}_{UUID}/` (e.g., `COPY_AGGR_FULL_xxx`)
- **Three repos**: `D_REPO/` (data), `M_REPO/` (metadata), `C_REPO/` (control/logs)
- **Unified structure**: Large files are stored directly; small files go to `.AGGR_DIR/`
- **Per-directory indexes**: Each directory has 0 or 1 `AGGREGATE_IDX.sqlite`
- **Snowflake IDs**: Blob filenames are unique across processes
- **Hidden directories**: `.AGGR_DIR/` and its contents are hidden
- **Per-subtask logs**: Each subtask has its own log file in `C_REPO/logs/{subtask_uuid}.log`
- **Scan log**: Scanner output goes to `C_REPO/logs/scan.log`
- **Status tracking**: Magic files in `C_REPO/status/` track task state for crash recovery:
  - `SCAN_{copy_uuid}.RUNNING|DONE` - Scan task status
  - `SUBTASK_{uuid}.RUNNING|DONE|FAILED` - Subtask status

## Performance Considerations

### Benefits

- **Reduced I/O overhead**: Fewer file operations when backing up millions of small files
- **Better throughput**: Sequential writes to blob files are more efficient than random writes
- **Faster restore**: Reading large blobs sequentially is faster than many small random reads
- **Scalable indexing**: Per-directory indexes prevent SQLite performance degradation

### Trade-offs

- **Memory usage**: Blob caching increases memory usage during restore
- **Granularity**: Individual files cannot be accessed without reading the entire blob
- **Complexity**: Additional SQLite index management overhead

### Tuning Recommendations

| Scenario | Blob Size | Threshold | Notes |
|----------|-----------|-----------|-------|
| Many tiny files (< 4KB) | 128MB | 64KB | Maximize sequential I/O |
| Mixed file sizes | 64MB | 1MB | Balanced approach |
| Large files mostly | Disable | N/A | Aggregation adds little value |
| Network storage | 256MB | 256KB | Reduce network round trips |

## Implementation Details

### Thread Safety

- The `AggregateBackupEngine` uses `Arc<Mutex<>>` for thread-safe directory info access
- `ThreadSafeSnowflake` provides thread-safe unique ID generation
- Directory buffers are protected by mutexes

### Error Handling

- Failed blob creations are logged and counted in stats
- Index errors don't fail the entire backup; files fall back to normal backup
- Restore operations fall back to normal restore if index query fails

### Platform Support

- Aggregation works on all platforms supported by Bifrost
- SQLite index requires the `sqlite` feature (enabled by default)
- In-memory index fallback available when SQLite is not available

## Comparison with DPA

| Feature | DPA | Bifrost |
|---------|-----|---------|
| Blob naming | Snowflake ID | Snowflake ID |
| Index storage | SQLite per directory | SQLite per directory |
| Max blob size | Configurable (default 64MB) | Configurable (default 64MB) |
| File threshold | Configurable | Configurable |
| Blob caching | Yes | Yes |
| Metadata in index | Full metadata | Full metadata |
| Multi-process safe | Yes | Yes (Snowflake IDs) |

## Future Enhancements

1. **Incremental Aggregation**: Only aggregate changed files in incremental backups
2. **Compression**: Optional compression for blob files
3. **Encryption**: Encryption support for blob files
4. **Distributed Index**: Sharded index for very large backup sets
5. **Streaming Restore**: Stream blob data without full cache

## Testing

See `scripts/test/test_aggregate.py` for comprehensive test coverage including:
- Basic aggregate backup and restore
- Mixed aggregated and non-aggregated files
- Large fileset aggregation
- Metadata preservation
- Edge cases (empty files, exact threshold sizes)
