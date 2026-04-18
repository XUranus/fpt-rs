# Incremental Backup and Delete Phase

## Overview

**Incremental backup** is a backup strategy that only backs up files that have changed since the last backup, significantly reducing backup time and storage requirements.

**Delete phase** is an essential part of incremental backup that removes files from the target that were deleted from the source since the last backup.

## Architecture

### Full vs Incremental Backup

| Aspect | Full Backup | Incremental Backup |
|--------|-------------|-------------------|
| Scan | Complete filesystem scan | Complete filesystem scan |
| Metadata | Full metadata generated | Full metadata generated |
| Cache | Complete dcache/fcache | Complete dcache/fcache |
| Control Files | All entries | Only modified/deleted/new entries |
| Data Transfer | All files | Only modified/new files |

### Backup Phases

```
┌─────────────────────────────────────────────────────────────┐
│                    Backup Phases                            │
├─────────────────────────────────────────────────────────────┤
│  1. COPY Phase    - Copy new/modified files                 │
│  2. HARDLINK Phase - Create hardlinks (optional)           │
│  3. DELETE Phase  - Remove deleted files (incremental)     │
│  4. MTIME Phase   - Restore directory timestamps           │
└─────────────────────────────────────────────────────────────┘
```

## Delete Control File Format

The delete control file (`delete.txt`) records files and directories that need to be removed from the target.

### Format

```
#BIFROST_DELETE_CTRL_FILE V1 FILES=<N> DIRS=<M> TIME=<UNIX_TIMESTAMP>

D <PATH_LEN:8HEX> <PATH>
F <PATH_LEN:8HEX> <PATH>
```

**Entry Types:**
- `D`: Directory to delete
- `F`: File to delete

### Example

```
#BIFROST_DELETE_CTRL_FILE V1 FILES=2 DIRS=1 TIME=1700000000

F 00000014 /home/user/old_file.txt
F 00000018 /home/user/temp_file.dat
D 00000012 /home/user/old_dir
```

## Diff Algorithm

The incremental diff algorithm compares current and previous cache files:

### Directory Comparison

1. Load previous and current dcache entries (sorted by inode)
2. Compare entries:
   - **New directory**: Inode exists only in current
   - **Modified directory**: Inode exists in both, hash changed
   - **Unchanged directory**: Inode exists in both, hash unchanged
   - **Deleted directory**: Inode exists only in previous

### File Comparison

1. For each directory that exists in both backups:
   - Compare files using the same logic as directories
   - Track new, modified, and deleted files

### Entry Modes

| Mode | Description |
|------|-------------|
| `nn` | New (not in previous backup) |
| `dm` | Data modified (content changed) |
| `mm` | Metadata modified (only metadata changed) |
| `bm` | Both modified (content and metadata) |
| `dd` | Deleted (in previous but not current) |

## Implementation

### Scanner Module

```rust
use bifrost::scanner::metadata::{
    IncrementalDiff, DiffStats, DiffType,
    DeleteControlFileWriter, DeleteEntry, DeleteEntryType,
};

// Perform incremental diff
let diff = IncrementalDiff::new(
    Some(prev_dcache_path),
    Some(prev_fcache_path),
    curr_dcache_path,
    curr_fcache_path,
)?;

// Generate control files
let stats = diff.generate_control_files(
    ctrl_file_path,
    delete_file_path,
)?;
```

### Delete Phase

```rust
use bifrost::backup::bio::delete;

// Run delete phase
let stats = delete::run_delete_phase(
    &ctrl_dir,           // Directory containing delete.txt
    &source_dir_base,    // Source base path
    &target_dir_base,    // Target base path
)?;

println!("Files deleted: {}, Dirs deleted: {}", 
    stats.files_deleted, 
    stats.dirs_deleted);
```

## Usage

### Command Line

**Full Backup:**
```bash
# Scan
./fsscan -c ./ctrl -m ./meta /source/path

# Backup
./fsbackup -s /source/path -t /target/path -m ./meta -c ./meta/ctrl.txt
```

**Incremental Backup:**
```bash
# Scan (generates full metadata + incremental control files)
./fsscan -c ./ctrl -m ./meta --incremental --prev-dcache ./prev/dcache \
    --prev-fcache ./prev/fcache /source/path

# Backup with delete phase
./fsbackup -s /source/path -t /target/path -m ./meta \
    -c ./meta/ctrl_incr.txt --delete --ctrl-dir ./ctrl
```

### Using Test Script

```bash
# Full backup
./scripts/bifrost_test.sh -i /source/path -o /target/path

# Incremental backup with delete phase
./scripts/bifrost_test.sh -i /source/path -o /target/path --backup-delete
```

## Statistics

### Diff Statistics

| Statistic | Description |
|-----------|-------------|
| `new_dirs` | New directories found |
| `modified_dirs` | Directories with changes |
| `deleted_dirs` | Directories to delete |
| `new_files` | New files found |
| `modified_files` | Files with content/metadata changes |
| `deleted_files` | Files to delete |

### Delete Phase Statistics

| Statistic | Description |
|-----------|-------------|
| `entries_processed` | Total entries processed |
| `files_deleted` | Files successfully deleted |
| `dirs_deleted` | Directories successfully deleted |
| `entries_failed` | Entries that failed to delete |
| `entries_skipped` | Entries skipped (not found) |

## Best Practices

1. **Schedule**: Run incremental backups frequently (e.g., hourly), full backups less frequently (e.g., weekly)

2. **Retention**: Keep multiple incremental backups between full backups for recovery flexibility

3. **Verification**: Always verify incremental backups with diff tools

4. **Delete Phase**: Always enable delete phase for incremental backups to maintain target consistency

5. **Order of Phases**: The correct order is: Copy → Hardlink → Delete → Mtime

## Notes

- The scanner always performs a full scan, even for incremental backups
- Cache files (dcache/fcache) are always complete, not incremental
- Only control files contain incremental (diff) information
- The delete phase runs after hardlink phase but before mtime phase
- Directory deletion is recursive if the directory is not empty
