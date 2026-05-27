# AGENTS.md

This file provides guidance to Qoder (qoder.com) when working with code in this repository.

## Project Overview

Fpt is a high-performance, cross-platform file backup and recovery system written in Rust. It uses native OS snapshot technologies (LVM on Linux, VSS on Windows) for crash-consistent backups.

## Build Commands

```bash
# Build release binaries
cargo build --release

# Build debug binaries
cargo build

# Run tests
cargo test

# Run a single test
cargo test <test_name>

# Format code
cargo fmt

# Run clippy lints
cargo clippy
```

## Available Binaries

After building, the following binaries are available in `target/release/`:

- `fptcli` - **Main CLI tool** for integrated backup/restore operations
- `fsscan` - Filesystem scanner that generates metadata and control files
- `fsbackup` - Backup executor that performs file copy operations
- `fsdiff` - Verification tool that compares source and backup
- `cacheinspect` - Tool to inspect cache files
- `metainspect` - Tool to inspect metadata files
- `vdbench` - Benchmarking tool

## Key Workflows

### Using fptcli (Recommended)

The `fptcli` tool provides an integrated workflow that combines scanning and backup into a single operation. Each backup copy is created with a unique UUID and standardized naming.

#### Copy Structure

Each backup copy is created as a folder named `COPY_{FORMAT}_{TYPE}_{UUID}` containing:
- `manifest.json` - Copy metadata at root level
- `D_REPO/` - Data repository (actual file contents)
- `M_REPO/` - Metadata repository (meta files, caches)
- `C_REPO/` - Control repository (control files and per-subtask logs)

#### Full Backup (Common Format)

```bash
./target/release/fptcli backup \
  --data /path/to/source \
  --target /path/to/backup \
  --format common
# Creates: /path/to/backup/COPY_COMMON_FULL_{uuid}/
```

#### Full Backup (Aggregated Format)

```bash
./target/release/fptcli backup \
  --data /path/to/source \
  --target /path/to/backup \
  --format aggregated \
  --blob-size 64 \
  --threshold 1024
# Creates: /path/to/backup/COPY_AGGR_FULL_{uuid}/
```

#### Incremental Backup (Aggregated Format Only)

```bash
./target/release/fptcli backup \
  --data /path/to/source \
  --target /path/to/backup \
  --format aggregated \
  --incremental-base /path/to/backup/COPY_AGGR_FULL_xxx
# Creates: /path/to/backup/COPY_AGGR_INC_{uuid}/
```

#### Restore

```bash
./target/release/fptcli restore \
  --copy /path/to/backup/COPY_AGGR_FULL_xxx \
  --target /path/to/restore \
  --policy replace
```

### Legacy Workflow: Scan + Backup + Verify

For advanced use cases, you can still use the individual tools:

```bash
# 1. Scan source directory
./target/release/fsscan \
  --ctrl-dir ./backup/ctrl \
  --meta-dir ./backup/meta \
  /path/to/source

# 2. Run backup using generated control file
./target/release/fsbackup \
  --source /path/to/source \
  --target /path/to/target \
  --meta-dir ./backup/meta \
  --ctrl-dir ./backup/ctrl \
  --control-file ./backup/ctrl/copy.txt

# 3. Verify backup integrity
./target/release/fsdiff \
  --source /path/to/source \
  --target /path/to/target
```

### Aggregate Backup (for many small files)

```bash
# Scan source directory
./target/release/fsscan \
  --ctrl-dir ./backup/ctrl \
  --meta-dir ./backup/meta \
  /path/to/source

# Run backup with aggregation enabled
./target/release/fsbackup \
  --source /path/to/source \
  --target /path/to/target \
  --meta-dir ./backup/meta \
  --ctrl-dir ./backup/ctrl \
  --control-file ./backup/ctrl/copy.txt \
  --aggregate \
  --max-blob-size 64 \
  --aggregate-threshold 1024
```

### Integrated Test

Use the pytest-based test suite for end-to-end testing:

```bash
python -m pytest tests/smoke/ -v   # smoke tests
python -m pytest tests/perf/ -v    # performance tests
python -m pytest tests/ -v         # all tests
```

## Architecture Overview

### Core Components

1. **Scanner Engine** (`src/scanner/`)
   - Multi-threaded filesystem traversal with work-stealing queues
   - Generates metadata files (`meta_*.dat`) and control files (`copy.txt`)
   - Supports incremental scanning when given previous metadata
   - Uses spillable in-memory queues to handle massive directory trees

2. **Backup Engine** (`src/backup/`)
   - Multi-phase pipeline: copy → hardlink → delete → mtime
   - Sharded processor for parallel file operations
   - Reads control files to determine what to backup
   - **Aggregate backup**: Combines small files into `.AGGR_DIR/` subdirectories with per-directory SQLite indexes
   - **Copy structure**: Each copy has UUID, stored in `COPY_{format}_{type}_{uuid}/` with `D_REPO/`, `M_REPO/`, `C_REPO/`

3. **Metadata Storage** (`src/scanner/metadata/`)
   - Binary format with tag-length-value encoding
   - Files: `meta_*.dat` contain serialized `FileMeta`/`DirMeta` structs
   - Fixed-size indexes (`fcache_*.dat`, `dcache_*.dat`) for fast lookups

4. **Control Files** (`src/backup/fcb.rs`)
   - Text-based line format describing file operations
   - Located in ctrl directory (e.g., `copy.txt`, `delete.txt`, `hardlink.txt`)
   - Reference metadata via `(meta_file_id, offset)` pairs

### Module Structure

```
src/
├── bin/           # CLI binaries (fptcli.rs, fsscan.rs, fsbackup.rs, fsdiff.rs, etc.)
│   └── fptcli.rs  # Main integrated CLI tool
├── scanner/       # Scanner engine
│   ├── engine/    # Traversal and queue logic
│   └── metadata/  # Metadata serialization
├── backup/        # Backup engine
│   ├── bio/       # I/O operations
│   ├── aggregate.rs           # Aggregate backup data structures
│   ├── aggregate_index.rs     # SQLite index for aggregates
│   ├── aggregate_engine.rs    # Aggregate backup engine
│   └── aggregate_restore.rs   # Aggregate restore engine
├── native/        # Platform-specific code (Linux/Windows)
├── agent/         # RPC agent for remote control
└── utility/       # Shared utilities
```

### Key Data Flow

1. Scanner traverses filesystem → writes metadata to `meta_*.dat` → writes control file entries
2. Backup engine reads control file → looks up metadata → performs file operations
3. Diff tool compares source and target using metadata

## Configuration

Scanner options use builder pattern in `ScanOption`:
- `worker_count` - Traversal threads (default: 4)
- `writer_count` - Metadata serialization threads (default: 1)
- `follow_symlinks` - Whether to follow symlinks (default: false)
- `scan_hidden` - Include dotfiles (default: false)
- `scan_acl` / `scan_xattrs` / `scan_hardlinks` - Extended metadata

Backup options in `BackupOption`:
- Enable phases: `enable_hardlink_phase()`, `enable_delete_phase()`, `enable_mtime_phase()`
- Aggregation: `enable_aggregation()`, `aggregate_max_blob_size()`, `aggregate_file_threshold()`

**Note on Aggregate Phases:**
- **Aggregate Backup**: Only copy phase is executed (hardlink/delete/mtime are ignored)
- **Aggregate Restore**: All 4 phases are executed (copy, hardlink, delete, mtime)

**Copy Structure (fptcli):**
- Copy folder: `COPY_{FORMAT}_{TYPE}_{UUID}/` (e.g., `COPY_AGGR_FULL_xxx`)
- `manifest.json` - Copy metadata at copy root (written once at start)
- `D_REPO/` - Data repository (large files + `.AGGR_DIR/` for aggregated)
- `M_REPO/` - Metadata repository (meta files, caches)
- `C_REPO/` - Control repository
  - `ctrl/` - Control files
  - `logs/` - Scan log and per-subtask logs
  - `status/` - Magic status files for crash recovery (SCAN_*.RUNNING/DONE, SUBTASK_*.RUNNING/DONE/FAILED)

**Aggregate Backup Structure:**
- Large files backed up normally to their original paths
- Small files aggregated into `.AGGR_DIR/` subdirectories
- Per-directory SQLite indexes (`AGGREGATE_IDX.sqlite` inside `.AGGR_DIR/`)
- Snowflake IDs for unique blob filenames across processes

## Testing

- Unit tests: `cargo test`
- Integration tests: `python -m pytest tests/smoke/ -v`
- Performance tests: `python -m pytest tests/perf/ -v`
- See `tests/README.md` for full documentation

## Documentation

See `docs/` directory for detailed specifications:
- `fpt.md` - High-level architecture
- `fptcli.md` - **fptcli tool documentation** (backup/restore operations)
- `ctrlfile.md` - Control file format specification
- `metafile.md` - Metadata storage format
- `hardlink.md` - Hardlink handling
- `incremental.md` - Incremental backup design
- `mtime.md` - Modification time handling
- `aggregate.md` - Aggregate backup/restore documentation
