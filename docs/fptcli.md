# fptcli

`fptcli` is the main integrated CLI for backup and restore.

It combines:

- prerequisite validation
- scanning
- subtask execution
- manifest writing
- local staging and NFS post-job transfer when needed

## Path Syntax

`fptcli` now infers local vs remote locations from the path string itself.

- Local path: `/opt/dataset/source`
- NFS path: `nfs://127.0.0.1/opt/dataset?sub=/source`
- SMB path: `smb://127.0.0.1/share/root?username=u&password=p`

The older split flags such as `--data-nfs` and `--target-nfs` are not used anymore.

## Backup

Syntax:

```bash
fptcli backup --data <PATH_OR_URL> --target <PATH_OR_URL> [OPTIONS]
```

Main options:

| Option | Meaning |
|--------|---------|
| `--data`, `-d` | Source path or NFS URL |
| `--target`, `-t` | Target path or NFS URL |
| `--format`, `-f` | `common` or `aggregated` |
| `--incremental-base`, `-i` | Previous copy root for aggregated incremental backup |
| `--jobs`, `-j` | Max concurrent subtasks |
| `--workers`, `-w` | Worker threads per subtask |
| `--hardlink` | Enable hardlink phase for common-format backup |
| `--delete` | Enable delete phase for common-format backup |
| `--mtime` | Enable mtime phase for common-format backup |
| `--blob-size` | Aggregated blob size in MB |
| `--threshold` | Aggregation threshold in KB |
| `--aggregate-layout` | Aggregated layout: `dir-level` or `shard` |
| `--nfs-connections` | NFS connection count |
| `--smb-connections` | SMB client connections per SMB endpoint |
| `--smb-copy-tasks` | SMB file copy task limit; `0` means auto |
| `--buffer-size` | Copy buffer size in KB; also caps SMB source read size up to 2048 KiB |
| `--nfs-uid` | AUTH_UNIX uid override |
| `--nfs-gid` | AUTH_UNIX gid override |
| `--temp-dir` | Local staging root, mainly for NFS-target jobs |
| `--log-file` | Catch-all extra log file |
| `--failure-log-format` | Enable structured scan/subtask failure logs: `csv`, `json`, or `xml` |
| `--operation-retries` | Retry count after the initial scan/copy operation attempt |
| `--retry-delay-ms` | Base retry delay in milliseconds |
| `--retry-backoff` | Exponential retry backoff multiplier; `1.0` keeps fixed delay |
| `--retry-max-delay-ms` | Maximum retry delay after backoff |
| `--retry-jitter` | Deterministic retry jitter ratio, `0.0..1.0` |
| `-v` | Verbosity, repeat for more detail |

When `--failure-log-format` is set, `fptcli backup` writes failure files under `C_REPO/logs`:

- `{copy_uuid}_SCAN_FAILURE.<fmt>`
- `SUBTASK_{subtask_uuid}_FAILURE.<fmt>`

See [retry_failure.md](retry_failure.md) for record fields, retry policy details, and `fsscan`/`fsbackup` options.

### Common Format

Local to local:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format common
```

Local to NFS:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target "nfs://127.0.0.1/opt/backup?sub=/copies" \
  --format common \
  --hardlink \
  --delete \
  --mtime \
  --nfs-uid 1000 \
  --nfs-gid 1000
```

NFS to local:

```bash
./target/release/fptcli backup \
  --data "nfs://127.0.0.1/opt/dataset?sub=/source" \
  --target /backup/root \
  --format common \
  --hardlink \
  --delete \
  --mtime
```

NFS to NFS:

```bash
./target/release/fptcli backup \
  --data "nfs://127.0.0.1/opt/dataset?sub=/source" \
  --target "nfs://127.0.0.1/opt/backup?sub=/copies" \
  --format common \
  --hardlink \
  --delete \
  --mtime
```

For common-format backup, all four directions support:

1. copy
2. hardlink
3. delete
4. mtime

### Aggregated Format

Full backup:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format aggregated \
  --aggregate-layout shard \
  --blob-size 64 \
  --threshold 1024
```

Incremental backup:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format aggregated \
  --aggregate-layout dir-level \
  --incremental-base /backup/root/COPY_AGGR_FULL_xxx
```

Important behavior:

- Aggregated backup uses only the copy phase.
- `--hardlink`, `--delete`, and `--mtime` are ignored for aggregated backup.

## Restore

Syntax:

```bash
fptcli restore --copy <PATH_OR_URL> --target <PATH_OR_URL> [OPTIONS]
```

Main options:

| Option | Meaning |
|--------|---------|
| `--copy`, `-c` | Copy root path or NFS URL |
| `--target`, `-t` | Restore target path or NFS URL |
| `--policy`, `-p` | `replace`, `skip`, or `keep-newer` |
| `--jobs`, `-j` | Max concurrent subtasks |
| `--workers`, `-w` | Worker threads per subtask |
| `--hardlinks` | Restore hardlinks |
| `--mtime` | Restore mtimes |
| `--nfs-connections` | NFS connection count |
| `--nfs-uid` | AUTH_UNIX uid override |
| `--nfs-gid` | AUTH_UNIX gid override |
| `--temp-dir` | Local staging root |
| `--log-file` | Catch-all extra log file |
| `-v` | Verbosity |

Examples:

```bash
./target/release/fptcli restore \
  --copy /backup/root/COPY_COMMON_FULL_xxx \
  --target /restore/root \
  --policy replace
```

```bash
./target/release/fptcli restore \
  --copy "nfs://127.0.0.1/opt/backup?sub=/COPY_COMMON_FULL_xxx" \
  --target /restore/root \
  --policy skip
```

## Copy Layout

Each backup creates a copy root named:

```text
COPY_{FORMAT}_{TYPE}_{UUID}
```

Example:

```text
COPY_COMMON_FULL_999931d2-acc7-477c-8fdb-80c48524f5ed/
  manifest.json
  D_REPO/
  M_REPO/
    meta/
  C_REPO/
    ctrl/
    logs/
    status/
```

Log files under `C_REPO/logs/` currently use:

- `scan.log`
- `frame.log`
- `{subtask_uuid}.log`

## Notes

- `fptcli` is the recommended workflow for normal backup and restore.
- For NFS targets, `M_REPO` and `C_REPO` are staged locally and uploaded by post-job logic.
- `D_REPO` may be written directly to the NFS target during the backup subtask phase.

## Related Docs

- [bifrost.md](bifrost.md)
- [nfs.md](nfs.md)
- [aggregate.md](aggregate.md)
- [logging.md](logging.md)
