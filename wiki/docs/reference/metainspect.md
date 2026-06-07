---
sidebar_position: 5
title: metainspect Reference
description: Complete reference for the metainspect metadata inspection tool
---

# metainspect Reference

`metainspect` is a diagnostic tool for inspecting the binary metadata and
control files produced by the fpt-rs scanner. It reads the internal binary
formats and outputs human-readable records in tab-separated, CSV, or JSON
format.

## Synopsis

```text
metainspect [OPTIONS] [FILE]
```

:::tip Auto-Detection
When a positional `FILE` argument is provided, metainspect automatically detects
the file type based on the filename pattern.
:::

## Input Flags

Choose one of the following to specify the input file:

| Flag               | Description                                                       |
|--------------------|-------------------------------------------------------------------|
| `FILE` (positional) | Input file path with automatic type detection                    |
| `--meta <FILE>`    | Inspect a metadata file (e.g. `meta_0_0.dat`)                    |
| `--dcache <FILE>`  | Inspect a directory cache file (e.g. `dcache_0.dat`)             |
| `--fcache <FILE>`  | Inspect a file cache file (e.g. `fcache_0.dat`)                  |
| `--control <FILE>` | Inspect a control file (e.g. `copy_<hash>.control.bin`)          |

### File Type Detection

When using the positional argument, the type is detected from the filename:

| Pattern                          | Detected Type |
|----------------------------------|---------------|
| `meta_*.dat`                     | Metadata      |
| `dcache_*.dat`                   | Directory cache |
| `fcache_*.dat`                   | File cache    |
| `*.control.bin`                  | Control file  |

### Supported File Types

#### Metadata Files (`meta_*.dat`)

Binary-serialized file and directory metadata entries. Each record contains:
- File/directory name and path
- Size, inode, mode, timestamps (atime/ctime/mtime)
- ACLs, xattrs, symlink targets (if present)
- Device number and link count

#### Directory Cache Files (`dcache_*.dat`)

Directory cache entries used for incremental scanning. Each record contains:
- Directory path
- Cached metadata snapshot

#### File Cache Files (`fcache_*.dat`)

File cache entries used for incremental scanning. Each record contains:
- File path
- Cached size, mtime, and other metadata

#### Control Files (`*.control.bin`)

Binary control files that drive backup phases:
- `copy_<hash>.control.bin` -- files to copy
- `hardlink_<hash>.control.bin` -- hardlinks to create
- `delete_<hash>.control.bin` -- entries to delete
- `mtime_<hash>.control.bin` -- mtimes to restore

Each record contains the source path, target path, and phase-specific metadata.

## Output Flags

| Flag                | Short | Default  | Description                    |
|---------------------|-------|----------|--------------------------------|
| `--format <FMT>`    |       | `tab`    | Output format: `json`, `csv`, `tab` |
| `--json`            |       | `false`  | Shortcut for `--format json`   |
| `--csv`             |       | `false`  | Shortcut for `--format csv`    |
| `--tab`             |       | `false`  | Shortcut for `--format tab`    |
| `--output <FILE>`   | `-o`  |          | Output file (stdout if omitted)|

### Output Formats

#### Tab-Separated (`tab`)

Default format. Each record is one line with tab-separated fields. Header row
included. Best for terminal inspection.

#### CSV (`csv`)

Standard CSV with quoted fields. Header row included. Best for spreadsheet
import or scripting.

#### JSON (`json`)

JSON array of objects. Each record is a JSON object with named fields.
Best for programmatic consumption.

## Examples

### Inspect a Metadata File

```bash
metainspect /tmp/fpt/meta/meta_0_0.dat
```

Output (tab format):

```text
index	dir	name	size	mode	uid	gid	atime	ctime	mtime	inode	links
0	/data	file1.txt	1024	0100644	1000	1000	1704067200	1704067200	1704067200	12345	1
1	/data	file2.txt	2048	0100644	1000	1000	1704067200	1704067200	1704067200	12346	1
```

### Inspect a Control File as JSON

```bash
metainspect --control /tmp/fpt/ctrl/copy_abc123.control.bin --json
```

Output:

```json
[
  {
    "index": 0,
    "src_path": "/data/file1.txt",
    "dst_path": "/data/file1.txt",
    "size": 1024,
    "mtime": 1704067200
  }
]
```

### Inspect with CSV Output to File

```bash
metainspect --meta /tmp/fpt/meta/meta_0_0.dat --csv --output metadata.csv
```

### Auto-Detect File Type

```bash
metainspect /tmp/fpt/meta/meta_0_0.dat        # detected as meta
metainspect /tmp/fpt/meta/dcache_0.dat         # detected as dcache
metainspect /tmp/fpt/meta/fcache_0.dat         # detected as fcache
metainspect /tmp/fpt/ctrl/copy_abc.control.bin # detected as control
```

### Inspect a Directory Cache

```bash
metainspect --dcache /tmp/fpt/meta/dcache_0.dat --tab
```

### Inspect a File Cache

```bash
metainspect --fcache /tmp/fpt/meta/fcache_0.dat --json
```

### Pipe to jq for Filtering

```bash
metainspect --meta /tmp/fpt/meta/meta_0_0.dat --json | jq '.[] | select(.size > 1000000)'
```
