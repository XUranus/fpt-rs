---
sidebar_position: 1
title: fptcli Reference
description: Complete reference for the fptcli backup and restore CLI tool
---

# fptcli Reference

`fptcli` is the primary CLI tool for creating backup copies and restoring data.
It supports local, NFS, and SMB sources and targets with a unified interface.

**Source file:** `src/bin/fptcli.rs`

## Synopsis

```text
fptcli <COMMAND> [OPTIONS]
```

## CLI Structure

The CLI is built with `clap` using the derive API:

```rust
#[derive(Parser, Debug)]
#[command(name = "fptcli")]
#[command(about = "File Protection Tool - Backup and Restore CLI")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Backup { ... },
    Restore { ... },
}
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

Location parsing (`parse_data_location()`) detects the transport by URL prefix:

```rust
fn parse_data_location(spec: &str, ...) -> Result<DataLocation, ...> {
    if spec.starts_with("nfs://") {
        // Parse NfsLocation::from_url(), apply connection_count, uid/gid
    } else if spec.starts_with("smb://") || spec.starts_with(r"smb:\\") {
        // Parse SmbLocation::from_url()
    } else {
        Ok(DataLocation::local(PathBuf::from(spec)))
    }
}
```

#### Backup Format Flags

| Flag                      | Short | Default  | Description                                       |
|---------------------------|-------|----------|---------------------------------------------------|
| `--format <FORMAT>`       | `-f`  | `common` | Backup format: `common` or `aggregated`           |
| `--aggregate`             |       | `false`  | Shortcut for `--format aggregated`                |
| `--incremental-base <DIR>`| `-i`  |          | Previous backup copy for incremental (aggregated only) |
| `--aggregate-layout <LAYOUT>`|  | `shard`  | Aggregate layout: `dir-level` or `shard`          |
| `--blob-size <MB>`        |       | `4`      | Aggregate blob size in MB (aggregated only)       |
| `--threshold <KB>`        |       | `1024`   | Aggregate file threshold in KB (aggregated only)  |

The actual Rust definitions:

```rust
#[arg(long, short = 'f', value_enum, default_value = "common")]
format: BackupFormat,

#[arg(long, action = clap::ArgAction::SetTrue)]
aggregate: bool,

#[arg(long, short = 'i', value_name = "DIR")]
incremental_base: Option<PathBuf>,

#[arg(long, value_enum, default_value = "shard")]
aggregate_layout: AggregateLayoutArg,

#[arg(long, default_value = "4", value_name = "MB")]
blob_size: u64,

#[arg(long, default_value = "1024", value_name = "KB")]
threshold: u64,
```

:::caution
Incremental backup is only valid with aggregated format. The CLI returns an
error if `--incremental-base` is used with `--format common`.
:::

#### Phase Flags

| Flag           | Default | Description                    |
|----------------|---------|--------------------------------|
| `--hardlink`   | `false` | Enable hardlink phase          |
| `--delete`     | `false` | Enable delete phase            |
| `--mtime`      | `false` | Enable mtime phase             |

Phase flags are disabled for aggregated format:

```rust
enable_hardlink: hardlink && !matches!(format, BackupFormat::Aggregated),
enable_delete: delete && !matches!(format, BackupFormat::Aggregated),
enable_mtime: mtime && !matches!(format, BackupFormat::Aggregated),
```

#### Scan Filter Flags

| Flag                          | Description                                      |
|-------------------------------|--------------------------------------------------|
| `--include-dir-pattern <PAT>` | Include directories matching pattern (repeatable) |
| `--include-file-pattern <PAT>`| Include files matching pattern (repeatable)       |
| `--exclude-dir-pattern <PAT>` | Exclude directories matching pattern (repeatable) |
| `--exclude-file-pattern <PAT>`| Exclude files matching pattern (repeatable)       |

These are bundled in a `ScanFilterArgs` struct and compiled into a
`ScanPathFilterSet`:

```rust
#[derive(clap::Args, Debug, Clone, Default)]
struct ScanFilterArgs {
    #[arg(long, value_name = "PATTERN")]
    include_dir_pattern: Vec<String>,
    #[arg(long, value_name = "PATTERN")]
    include_file_pattern: Vec<String>,
    #[arg(long, value_name = "PATTERN")]
    exclude_dir_pattern: Vec<String>,
    #[arg(long, value_name = "PATTERN")]
    exclude_file_pattern: Vec<String>,
}

impl ScanFilterArgs {
    fn compile(&self) -> Result<Option<ScanPathFilterSet>, std::io::Error> {
        ScanPathFilterSet::compile(
            self.include_dir_pattern.clone(),
            self.include_file_pattern.clone(),
            self.exclude_dir_pattern.clone(),
            self.exclude_file_pattern.clone(),
        ).map_err(std::io::Error::other)
    }
}
```

#### Concurrency Flags

| Flag                        | Short | Default | Description                             |
|-----------------------------|-------|---------|-----------------------------------------|
| `--jobs <COUNT>`            | `-j`  | `4`     | Maximum concurrent subtasks             |
| `--workers <COUNT>`         | `-w`  | `8`     | Number of worker threads per subtask    |
| `--nfs-connections <COUNT>` |       | `32`    | Parallel NFS connections                |
| `--smb-connections <COUNT>` |       | `4`     | SMB client connections per endpoint     |
| `--smb-copy-tasks <COUNT>`  |       | `0`     | Concurrent SMB copy tasks (0 = auto)    |
| `--buffer-size <SIZE_KB>`   |       | `1024`  | Per-file copy buffer size in KB         |

The actual Rust definitions with defaults:

```rust
#[arg(long, short = 'j', default_value = "4", value_name = "COUNT")]
jobs: usize,

#[arg(long, short = 'w', default_value = "8", value_name = "COUNT")]
workers: usize,

#[arg(long, default_value = "32", value_name = "COUNT")]
nfs_connections: usize,

#[arg(long, default_value = "4", value_name = "COUNT")]
smb_connections: usize,

#[arg(long, default_value = "0", value_name = "COUNT")]
smb_copy_tasks: usize,

#[arg(long, default_value = "1024", value_name = "SIZE_KB")]
buffer_size: usize,
```

The buffer size is clamped at job creation:

```rust
copy_buffer_size: (buffer_size * 1024).clamp(256 * 1024, 4 * 1024 * 1024),
```

#### NFS Flags

| Flag              | Description                                           |
|-------------------|-------------------------------------------------------|
| `--nfs-uid <UID>` | AUTH_UNIX uid (overrides `uid=` in URL)               |
| `--nfs-gid <GID>` | AUTH_UNIX gid (overrides `gid=` in URL)               |

NFS URL parsing applies connection count and credential overrides:

```rust
#[cfg(feature = "nfs")]
fn parse_nfs_location(url: &str, connections: usize, uid: Option<u32>, gid: Option<u32>)
    -> Result<DataLocation, Box<dyn std::error::Error>>
{
    let mut loc = NfsLocation::from_url(url)?.connection_count(connections);
    let final_uid = uid.unwrap_or(loc.uid);
    let final_gid = gid.unwrap_or(loc.gid);
    loc = loc.credentials(final_uid, final_gid);
    Ok(DataLocation::nfs(loc))
}
```

#### Retry Flags

| Flag                    | Default  | Description                                  |
|-------------------------|----------|----------------------------------------------|
| `--operation-retries`   | `3`      | Retries before recording failure             |
| `--retry-delay-ms`      | `1000`   | Delay between retries (ms)                   |
| `--retry-backoff`       | `1.0`    | Exponential backoff multiplier               |
| `--retry-max-delay-ms`  | `1000`   | Maximum retry delay (ms)                     |
| `--retry-jitter`        | `0.0`    | Jitter ratio (0.0..1.0)                      |

These are combined into a `RetryPolicy`:

```rust
let retry_policy = RetryPolicy::new(
    operation_retries,
    std::time::Duration::from_millis(retry_delay_ms),
)
.with_backoff(retry_backoff, std::time::Duration::from_millis(retry_max_delay_ms))
.with_jitter(retry_jitter);
```

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

The actual Rust definitions:

```rust
#[arg(long, short = 'p', value_enum, default_value = "replace")]
policy: RestorePolicyArg,

#[arg(long, short = 'j', default_value = "4", value_name = "COUNT")]
jobs: usize,

#[arg(long, short = 'w', default_value = "8", value_name = "COUNT")]
workers: usize,

#[arg(long, action = clap::ArgAction::SetTrue)]
hardlinks: bool,

#[arg(long, action = clap::ArgAction::SetTrue, default_value = "true")]
mtime: bool,

#[arg(long = "path", value_name = "PATH")]
paths: Vec<String>,
```

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

## File Descriptor Limit

On Unix, `fptcli` automatically raises the file descriptor soft limit to the
hard limit at startup. This prevents `EMFILE` ("Too many open files") errors
when backing up large datasets with many small files:

```rust
#[cfg(unix)]
{
    use nix::sys::resource::{getrlimit, setrlimit, Resource};
    match getrlimit(Resource::RLIMIT_NOFILE) {
        Ok((soft, hard)) => {
            if soft < hard {
                let _ = setrlimit(Resource::RLIMIT_NOFILE, hard, hard);
            }
        }
        Err(e) => eprintln!("Warning: failed to query fd limit: {}", e),
    }
}
```

## Backup Summary Output

After a successful backup, `fptcli` prints a summary:

```text
============================================================
Backup Summary
============================================================
Source type : NFS
Target type : Local
Format      : AGGR
Aggregation : enabled
Layout      : shard
Blob size   : 4 MiB
Threshold   : 1024 KiB
Source path : nfs://192.168.1.10/export?sub=/ds1
Target path : /backup/nfs_copy
Copy UUID   : 550e8400-e29b-41d4-a716-446655440000
Copy root   : /backup/nfs_copy/550e8400-...
Subtasks    : 4 ok, 0 failed
Total files : 15000
Total dirs  : 500
Total bytes : 1073741824
Elapsed     : 2m 15.123s
File rate   : 111.11 files/s
Data rate   : 7.95 MiB/s

Backup completed successfully!
```

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
