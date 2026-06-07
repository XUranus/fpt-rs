---
sidebar_position: 3
title: fsscan Reference
description: Complete reference for the fsscan filesystem scanner tool
---

# fsscan Reference

`fsscan` is a standalone filesystem scanner that traverses one or more source
paths and generates metadata and control files for use by `fsbackup` or
`fptcli`. It supports local paths, NFS export URLs, and SMB share URLs.

## Synopsis

```text
fsscan <PATH_OR_URL> [PATH_OR_URL ...] [OPTIONS]
```

Multiple sources can be scanned in sequence. Each source is scanned
independently and a combined summary is printed at the end.

## Arguments

| Argument       | Description                                      |
|----------------|--------------------------------------------------|
| `PATH_OR_URL`  | Source path(s) to scan (repeatable, at least 1)  |

### Source Formats

| Format | Example                                                     |
|--------|-------------------------------------------------------------|
| Local  | `/opt/dataset/ds2`                                          |
| NFS    | `nfs://127.0.0.1/opt/dataset?sub=/out`                     |
| SMB    | `smb://127.0.0.1/share/root?username=u&password=p`         |

## Flags

### Output Directories

| Flag                 | Short | Default          | Description                         |
|----------------------|-------|------------------|-------------------------------------|
| `--ctrl-dir <DIR>`   | `-c`  | `/tmp/fpt/ctrl`  | Control file output directory       |
| `--meta-dir <DIR>`   | `-m`  | `/tmp/fpt/meta`  | Metadata output directory           |
| `--temp-dir <DIR>`   | `-t`  | `/tmp/fpt/cache` | Temporary directory for spill queues |

### Concurrency

| Flag               | Short | Default | Description                                    |
|--------------------|-------|---------|------------------------------------------------|
| `--workers <COUNT>`| `-w`  | `8`     | Traversal workers (threads for local, concurrent RPCs for NFS/SMB) |
| `--writers <COUNT>`| `-W`  | `1`     | Metadata writer threads                        |

### Scan Behavior

| Flag                   | Default | Description                              |
|------------------------|---------|------------------------------------------|
| `--follow-symlinks`    | `false` | Follow symbolic links during scanning    |
| `--scan-hidden`        | `false` | Include hidden files and directories     |
| `--max-depth <DEPTH>`  |         | Maximum recursion depth (unlimited)      |
| `--scan-acl`           | `false` | Scan Access Control Lists                |
| `--scan-xattrs`        | `false` | Scan extended attributes (xattrs)        |
| `--scan-hardlinks`     | `false` | Scan and track hardlinks                 |
| `--skip-block-devices` | `true`  | Skip block devices during scanning       |
| `--skip <NAME>`        |         | Entry names to skip (repeatable)         |
| `--stats-only`         | `false` | Only print summary stats; skip metadata/control generation |

### Path Filter Flags

| Flag                          | Description                                      |
|-------------------------------|--------------------------------------------------|
| `--include-dir-pattern <PAT>` | Include directories matching pattern (repeatable) |
| `--include-file-pattern <PAT>`| Include files matching pattern (repeatable)       |
| `--exclude-dir-pattern <PAT>` | Exclude directories matching pattern (repeatable) |
| `--exclude-file-pattern <PAT>`| Exclude files matching pattern (repeatable)       |

### Incremental Scanning

| Flag                   | Description                                    |
|------------------------|------------------------------------------------|
| `--prev-meta-dir <DIR>`| Previous metadata directory for incremental scan |

### Sharding Flags

| Flag                              | Default | Description                        |
|-----------------------------------|---------|------------------------------------|
| `--shard`                         | `false` | Enable sharded control files       |
| `--shard-num <COUNT>`             | `16`    | Number of shards                   |
| `--shard-max-entries-copy <N>`    |         | Max entries per shard (copy phase) |
| `--shard-max-entries-other <N>`   |         | Max entries per shard (other phases)|
| `--shard-max-size <BYTES>`        |         | Max shard file size in bytes       |

### NFS Flags

| Flag                    | Default | Description                            |
|-------------------------|---------|----------------------------------------|
| `--nfs-connections <N>` | `32`    | Parallel NFS connections               |
| `--nfs-uid <UID>`       |         | AUTH_UNIX uid (overrides URL)          |
| `--nfs-gid <GID>`       |         | AUTH_UNIX gid (overrides URL)          |

### SMB Flags

| Flag                      | Default | Description                            |
|---------------------------|---------|----------------------------------------|
| `--smb-query-buffer-mb <N>`| `8`    | SMB query-directory buffer size in MiB |

### Retry Flags

| Flag                    | Default  | Description                              |
|-------------------------|----------|------------------------------------------|
| `--operation-retries`   | `3`      | Retries before recording failure         |
| `--retry-delay-ms`      | `1000`   | Delay between retries (ms)               |
| `--retry-backoff`       | `1.0`    | Exponential backoff multiplier           |
| `--retry-max-delay-ms`  | `1000`   | Maximum retry delay (ms)                 |
| `--retry-jitter`        | `0.0`    | Jitter ratio (0.0..1.0)                  |

### Failure Logging

| Flag                      | Description                                      |
|---------------------------|--------------------------------------------------|
| `--failure-log <FILE>`    | Structured failure log output path               |
| `--failure-log-format <FMT>` | Format: `csv`, `json`, `xml`                  |

### Logging

| Flag           | Short | Description                                   |
|----------------|-------|-----------------------------------------------|
| `--verbose`    | `-v`  | Verbosity: `-v`=INFO, `-vv`=DEBUG, `-vvv`=TRACE |
| `--log-file`   |       | Log file path (append mode)                   |

## Output

The scanner writes:

1. **Metadata files** (`meta_*.dat`) in `--meta-dir` -- binary serialized file
   and directory metadata.
2. **Cache files** (`dcache_*.dat`, `fcache_*.dat`) in `--meta-dir` -- directory
   and file cache entries for incremental comparison.
3. **Control files** (`copy_*.control.bin`, `hardlink_*.control.bin`,
   `delete_*.control.bin`, `mtime_*.control.bin`) in `--ctrl-dir` -- binary
   control files that drive the backup copy phase.
4. **Failure logs** (optional) -- structured failure records in CSV, JSON, or
   XML format.

## Examples

### Scan a Local Path

```bash
fsscan /opt/dataset \
    --ctrl-dir /tmp/fpt/ctrl \
    --meta-dir /tmp/fpt/meta \
    --workers 16 \
    -v
```

### Scan an NFS Export

```bash
fsscan "nfs://192.168.1.10/export?sub=/dataset1" \
    --nfs-connections 64 \
    --nfs-uid 1000 \
    --nfs-gid 1000 \
    --ctrl-dir /tmp/fpt/ctrl \
    --meta-dir /tmp/fpt/meta \
    -vv
```

### Scan an SMB Share

```bash
fsscan "smb://nas.local/data?username=admin&password=secret" \
    --ctrl-dir /tmp/fpt/ctrl \
    --meta-dir /tmp/fpt/meta \
    -v
```

### Incremental Scan

```bash
fsscan /opt/dataset \
    --prev-meta-dir /backup/previous/meta \
    --ctrl-dir /tmp/fpt/ctrl \
    --meta-dir /tmp/fpt/meta \
    -v
```

### Stats-Only Mode

```bash
fsscan /opt/dataset --stats-only -v
```

Output:

```text
Scanning: /opt/dataset
Scan complete: 15000 files, 500 dirs, 1024.00 MB, 0 failed files, 0 failed dirs, elapsed 12.345s
```

### Multiple Sources

```bash
fsscan /data/set1 /data/set2 \
    --ctrl-dir /tmp/fpt/ctrl \
    --meta-dir /tmp/fpt/meta \
    -v
```

### Scan with Filters

```bash
fsscan /opt/data \
    --include-dir-pattern "/data/**" \
    --exclude-file-pattern "**/*.tmp" \
    --exclude-file-pattern "**/node_modules" \
    --ctrl-dir /tmp/fpt/ctrl \
    --meta-dir /tmp/fpt/meta \
    -v
```

### Scan with ACLs and Xattrs

```bash
fsscan /opt/dataset \
    --scan-acl \
    --scan-xattrs \
    --scan-hardlinks \
    --ctrl-dir /tmp/fpt/ctrl \
    --meta-dir /tmp/fpt/meta \
    -v
```
