---
sidebar_position: 1
title: fptcli Reference
description: Complete reference for the fptcli backup and restore CLI tool
---

# fptcli Reference

`fptcli` is the primary CLI tool for creating backup copies and restoring data.
It supports local, NFS, and SMB sources and targets with a unified interface.

## Synopsis

```text
fptcli <COMMAND> [OPTIONS]
```

## Subcommands

### `backup`

Create a backup copy from a source to a target.

```text
fptcli backup --data <PATH_OR_URL> --target <PATH_OR_URL> [OPTIONS]
```

#### Required Flags

| Flag                 | Short | Description                                      |
|----------------------|-------|--------------------------------------------------|
| `--data <PATH_OR_URL>`  | `-d`  | Source data path or URL                          |
| `--target <PATH_OR_URL>`| `-t`  | Target path where the copy will be created       |

#### Source/Target Formats

| Format   | Example                                                        |
|----------|----------------------------------------------------------------|
| Local    | `/opt/dataset`                                                 |
| NFS      | `nfs://127.0.0.1/opt/dataset?sub=/ds1`                        |
| SMB      | `smb://127.0.0.1/share/root?username=u&password=p`            |

#### Backup Format Flags

| Flag                      | Short | Default  | Description                                       |
|---------------------------|-------|----------|---------------------------------------------------|
| `--format <FORMAT>`       | `-f`  | `common` | Backup format: `common` or `aggregated`           |
| `--aggregate`             |       | `false`  | Shortcut for `--format aggregated`                |
| `--incremental-base <DIR>`| `-i`  |          | Previous backup copy for incremental (aggregated only) |
| `--aggregate-layout <LAYOUT>`|  | `shard`  | Aggregate layout: `dir-level` or `shard`          |
| `--blob-size <MB>`        |       | `4`      | Aggregate blob size in MB (aggregated only)       |
| `--threshold <KB>`        |       | `1024`   | Aggregate file threshold in KB (aggregated only)  |

#### Phase Flags

| Flag           | Default | Description                    |
|----------------|---------|--------------------------------|
| `--hardlink`   | `false` | Enable hardlink phase          |
| `--delete`     | `false` | Enable delete phase            |
| `--mtime`      | `false` | Enable mtime phase             |

#### Scan Filter Flags

| Flag                          | Description                                      |
|-------------------------------|--------------------------------------------------|
| `--include-dir-pattern <PAT>` | Include directories matching pattern (repeatable) |
| `--include-file-pattern <PAT>`| Include files matching pattern (repeatable)       |
| `--exclude-dir-pattern <PAT>` | Exclude directories matching pattern (repeatable) |
| `--exclude-file-pattern <PAT>`| Exclude files matching pattern (repeatable)       |

#### Concurrency Flags

| Flag                        | Short | Default | Description                             |
|-----------------------------|-------|---------|-----------------------------------------|
| `--jobs <COUNT>`            | `-j`  | `4`     | Maximum concurrent subtasks             |
| `--workers <COUNT>`         | `-w`  | `8`     | Number of worker threads per subtask    |
| `--nfs-connections <COUNT>` |       | `32`    | Parallel NFS connections                |
| `--smb-connections <COUNT>` |       | `4`     | SMB client connections per endpoint     |
| `--smb-copy-tasks <COUNT>`  |       | `0`     | Concurrent SMB copy tasks (0 = auto)    |
| `--buffer-size <SIZE_KB>`   |       | `1024`  | Per-file copy buffer size in KB         |

#### NFS Flags

| Flag              | Description                                           |
|-------------------|-------------------------------------------------------|
| `--nfs-uid <UID>` | AUTH_UNIX uid (overrides `uid=` in URL)               |
| `--nfs-gid <GID>` | AUTH_UNIX gid (overrides `gid=` in URL)               |

#### Retry Flags

| Flag                    | Default  | Description                                  |
|-------------------------|----------|----------------------------------------------|
| `--operation-retries`   | `3`      | Retries before recording failure             |
| `--retry-delay-ms`      | `1000`   | Delay between retries (ms)                   |
| `--retry-backoff`       | `1.0`    | Exponential backoff multiplier               |
| `--retry-max-delay-ms`  | `1000`   | Maximum retry delay (ms)                     |
| `--retry-jitter`        | `0.0`    | Jitter ratio (0.0..1.0)                      |

#### Logging Flags

| Flag                   | Short | Description                                   |
|------------------------|-------|-----------------------------------------------|
| `--failure-log-format` |       | Structured failure log format: `csv`, `json`, `xml` |
| `--temp-dir <DIR>`     |       | Temporary working directory (default: `/tmp/fpt`) |
| `--verbose`            | `-v`  | Verbosity: `-v`=INFO, `-vv`=DEBUG, `-vvv`=TRACE |
| `--log-file <FILE>`    |       | Log file path (append mode)                   |

---

### `restore`

Restore data from a backup copy to a target.

```text
fptcli restore --copy <PATH_OR_URL> --target <PATH_OR_URL> [OPTIONS]
```

#### Required Flags

| Flag                    | Short | Description                              |
|-------------------------|-------|------------------------------------------|
| `--copy <PATH_OR_URL>`  | `-c`  | Source backup copy path                  |
| `--target <PATH_OR_URL>`| `-t`  | Target restore path                      |

#### Restore Options

| Flag                   | Short | Default    | Description                                |
|------------------------|-------|------------|--------------------------------------------|
| `--policy <POLICY>`    | `-p`  | `replace`  | Restore policy: `replace`, `skip`, `keep-newer` |
| `--jobs <COUNT>`       | `-j`  | `4`        | Maximum concurrent subtasks                |
| `--workers <COUNT>`    | `-w`  | `8`        | Worker threads per subtask                 |
| `--hardlinks`          |       | `false`    | Restore hardlinks                          |
| `--mtime`              |       | `true`     | Restore modification times                 |
| `--path <PATH>`        |       |            | Fine-grained restore path (repeatable)     |

#### Path Filtering

The `--path` flag allows selective restore of specific files or directories.
Repeat it to restore multiple items:

```bash
fptcli restore --copy /backup/copy1 --target /restore \
    --path /data/file1.txt \
    --path /data/subdir/
```

- Files are exact matches
- Directories restore the full subtree

#### NFS Flags

| Flag              | Description                                           |
|-------------------|-------------------------------------------------------|
| `--nfs-connections`| Parallel NFS connections (default: 32)                |
| `--nfs-uid <UID>` | AUTH_UNIX uid                                         |
| `--nfs-gid <GID>` | AUTH_UNIX gid                                         |

#### Logging Flags

| Flag           | Short | Description                                   |
|----------------|-------|-----------------------------------------------|
| `--temp-dir`   |       | Temporary working directory                   |
| `--verbose`    | `-v`  | Verbosity level                               |
| `--log-file`   |       | Log file path                                 |

---

## Restore Policies

| Policy       | Behavior                                              |
|--------------|-------------------------------------------------------|
| `replace`    | Overwrite existing files unconditionally              |
| `skip`       | Skip files that already exist on the target           |
| `keep-newer` | Only replace if the source file is newer than target  |

## Examples

### Local to Local Backup

```bash
fptcli backup \
    --data /opt/dataset \
    --target /backup/dataset \
    --format common \
    --jobs 4 \
    --workers 8 \
    -v
```

### NFS Source with Incremental Aggregated Backup

```bash
fptcli backup \
    --data "nfs://192.168.1.10/export?sub=/dataset1" \
    --target /backup/nfs_copy \
    --format aggregated \
    --incremental-base /backup/previous_copy \
    --nfs-connections 64 \
    --nfs-uid 1000 \
    --nfs-gid 1000 \
    --aggregate \
    --jobs 8 \
    -vv
```

### SMB Source to Local Target

```bash
fptcli backup \
    --data "smb://nas.local/data?username=backup&password=secret" \
    --target /backup/smb_data \
    --smb-connections 8 \
    --smb-copy-tasks 16 \
    --buffer-size 2048 \
    --hardlink --delete --mtime \
    -v
```

### Cross-Transport: NFS to SMB

```bash
fptcli backup \
    --data "nfs://10.0.0.5/volume1?sub=/src" \
    --target "smb://backup-server/repo?username=admin&password=pass" \
    --nfs-connections 32 \
    --smb-connections 8 \
    --jobs 4 \
    -v
```

### Restore with Selective Paths

```bash
fptcli restore \
    --copy /backup/copy_20240101 \
    --target /restore \
    --policy keep-newer \
    --path /data/important/ \
    --path /data/config.json \
    --hardlinks --mtime \
    --jobs 4 \
    -v
```

### Backup with Path Filters

```bash
fptcli backup \
    --data /opt/data \
    --target /backup/data \
    --include-dir-pattern "/data/**" \
    --exclude-file-pattern "**/*.tmp" \
    --exclude-file-pattern "**/.cache" \
    --format aggregated \
    -v
```

### Backup with Retry Policy

```bash
fptcli backup \
    --data "nfs://server/export" \
    --target /backup \
    --operation-retries 5 \
    --retry-delay-ms 2000 \
    --retry-backoff 2.0 \
    --retry-max-delay-ms 30000 \
    --retry-jitter 0.1 \
    --failure-log-format json \
    -v
```
