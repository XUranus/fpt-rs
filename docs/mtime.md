# Mtime Backup Phase

## Overview

The **mtime backup phase** is a specialized backup phase that runs after the copy and hardlink phases to restore directory modification times (mtime) and access times (atime).

## Why Mtime Phase is Needed

During the backup process:
1. **Copy phase** creates new files and directories in the target location
2. **Hardlink phase** may modify directories by creating hardlinks

Both operations can affect the modification time of directories. The mtime phase ensures that directory timestamps are restored to their original values from the source, preserving the complete backup fidelity.

## Control File Format

The mtime control file (`mtime.txt`) is a text-based format that records directory paths and their original timestamps.

### Header

```
#BIFROST_MTIME_CTRL_FILE V1 DIRS=<N> TIME=<UNIX_TIMESTAMP>
```

- `DIRS=<N>`: Number of directory entries in the file
- `TIME=<UNIX_TIMESTAMP>`: Unix timestamp when the file was created

### Directory Entries

```
D <PATH_LEN:8HEX> <PATH> <MODE:8HEX> <UID:8HEX> <GID:8HEX> <ATIME:16HEX> <MTIME:16HEX>
```

Fields:
- `D`: Entry type (Directory)
- `PATH_LEN:8HEX`: Length of the path in hexadecimal (8 digits)
- `PATH`: Full path to the directory
- `MODE:8HEX`: File mode/permissions in hexadecimal
- `UID:8HEX`: User ID in hexadecimal
- `GID:8HEX`: Group ID in hexadecimal
- `ATIME:16HEX`: Access time (seconds since Unix epoch) in hexadecimal
- `MTIME:16HEX`: Modification time (seconds since Unix epoch) in hexadecimal

### Example

```
#BIFROST_MTIME_CTRL_FILE V1 DIRS=3 TIME=1700000000

D 00000010 /home/user/docs 000041ED 000003E8 000003E8 00000170B5D7A300 00000170B5D7A300
D 0000000E /home/user/src 000041ED 000003E8 000003E8 00000170B5D7A400 00000170B5D7A400
D 00000014 /home/user/data 000041ED 000003E8 000003E8 00000170B5D7A500 00000170B5D7A500
```

## Implementation

### Scanning Process

During the scanning phase, each directory's metadata is recorded:

1. For each directory encountered during traversal:
   - Extract path, mode, uid, gid, atime, mtime
   - Write entry to mtime control file

2. The mtime control file is generated alongside the main control file

### Backup Process

The mtime phase runs after copy and hardlink phases:

1. Read the mtime control file (`mtime.txt`)
2. For each directory entry:
   - Calculate the target path
   - Set the directory's atime and mtime to the original values

### Usage

#### Command Line

**Scan with mtime tracking:**
```bash
./fsscan -c ./ctrl -m ./meta -w 4 /source/path
```

**Backup with mtime phase:**
```bash
./fsbackup -s /source/path -t /target/path -m ./meta -c ./meta/ctrl.txt --mtime --ctrl-dir ./ctrl
```

**Verify with mtime comparison:**
```bash
./fsdiff --source /source/path --target /target/path --compare-mtime
```

#### Using Test Script

```bash
./scripts/bifrost_test.sh -i /source/path -o /target/path --backup-mtime
```

## API Reference

### Scanner Module

```rust
use bifrost::scanner::metadata::{MtimeControlFileWriter, MtimeDirEntry};

// Create mtime control file
let mut writer = MtimeControlFileWriter::new("mtime.txt")?;

// Write directory entry
writer.write_dir(&MtimeDirEntry {
    path: "/home/user/docs".to_string(),
    mode: 0o40755,
    uid: 1000,
    gid: 1000,
    atime: 1700000000,
    mtime: 1700000000,
})?;

writer.finish()?;
```

### Backup Module

```rust
use bifrost::backup::bio::mtime;

// Run mtime phase
let stats = mtime::run_mtime_phase(
    &ctrl_dir,           // Directory containing mtime.txt
    &source_dir_base,    // Source base path
    &target_dir_base,    // Target base path
)?;

println!("Restored: {}, Failed: {}", 
    stats.dirs_restored, 
    stats.dirs_failed);
```

## Statistics

The mtime phase tracks the following statistics:

| Statistic | Description |
|-----------|-------------|
| `dirs_processed` | Total directories processed |
| `dirs_restored` | Directories with mtime successfully restored |
| `dirs_failed` | Directories that failed to restore |
| `dirs_skipped` | Directories skipped (not found in target) |

## Platform Support

- **Linux/Unix**: Full support using `utimes` system call
- **Windows**: Supported using `SetFileTime` API with backup semantics

## Notes

- The mtime phase only affects directories, not files
- File timestamps are preserved during the copy phase
- The mtime phase is optional but recommended for complete backup fidelity
- Directory timestamps are restored using the original values from the source
