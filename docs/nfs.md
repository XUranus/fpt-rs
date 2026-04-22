# NFS Support

This document describes the NFS-related code that exists in the repository today.

## Overview

Bifrost supports NFS-backed paths in three places:

1. Scanning an NFS source.
2. Backing up between local and NFS in any direction for common-format backup.
3. Using NFS URLs in `fptcli` path arguments.

NFS locations are expressed as URLs such as:

```text
nfs://127.0.0.1/opt/dataset?sub=/ds1
```

## CLI Behavior

`fptcli` infers path type directly from the string:

- `/opt/dataset/ds1` means local filesystem
- `nfs://127.0.0.1/opt/dataset?sub=/ds1` means NFS

The old split flags such as `--data-nfs` and `--target-nfs` are gone.

Optional NFS tuning flags remain:

- `--nfs-connections`
- `--nfs-uid`
- `--nfs-gid`

## Module Layout

### `src/nfs/`

- `mod.rs`: `NfsLocation` and exports
- `connection.rs`: connection-pool creation and server `fsinfo`
- `scanner.rs`: NFS directory traversal
- `aio/reader.rs`: NFS file reads
- `aio/writer.rs`: NFS file and directory writes
- `aio/hardlink.rs`: NFS hardlink phase
- `aio/delete.rs`: NFS delete phase
- `aio/mtime.rs`: NFS mtime phase

### `src/backup/aio/`

This is the NFS-involved backup execution layer.

- `mod.rs`: runtime/bootstrap and direction-level orchestration
- `local_to_nfs.rs`
- `nfs_to_local.rs`
- `nfs_to_nfs.rs`

NFS copy logic now uses the shared async transport pipeline. Direction wrappers live in `src/backup/aio/directions.rs`; the actual copy loop is in `src/backup/aio/pipeline.rs` and runs over `SourceReader` / `TargetWriter` adapters.

## Backup Direction Support

For common-format backup, the current implementation supports all four phases in all NFS-involved directions:

| Direction | Copy | Hardlink | Delete | Mtime |
|-----------|------|----------|--------|-------|
| local -> NFS | yes | yes | yes | yes |
| NFS -> local | yes | yes | yes | yes |
| NFS -> NFS | yes | yes | yes | yes |

Local-to-local continues to use the blocking local entry point, but it now consumes the same `CopyPlanEntry` model and uses bounded block copy workers rather than the old whole-file FCB queue graph.

Important exception:

- Aggregated backup still runs copy only, even when NFS is involved.

## Runtime Split

The current split is:

- `src/backup.rs`: top-level task dispatch
- `src/backup/bio.rs`: local/BIO pipeline bootstrap
- `src/backup/copy_plan.rs`: shared control-file-to-copy-plan production
- `src/backup/copy_block.rs`: bounded transfer unit used by async adapters and restore
- `src/backup/aio.rs`: NFS runtime bootstrap, Tokio runtime creation, connection-pool creation, and direction dispatch
- `src/backup/aio/directions.rs`: thin direction wrappers
- `src/backup/aio/pipeline.rs`: generic async copy executor
- `src/backup/aio/transport.rs`: local/NFS/SMB source and target adapters
- `src/nfs/aio/*.rs`: NFS post-copy phase helpers

This keeps NFS-specific runtime setup out of `backup.rs`.

## Scan and Post-Job Behavior

For NFS-target jobs:

- `D_REPO` data may be written directly to the NFS destination during subtasks
- `M_REPO` and `C_REPO` are still maintained locally during job execution
- post-job uploads `M_REPO`, `C_REPO`, and `manifest.json` to the final NFS copy root

For NFS-source scans:

- the scan phase produces the same metadata and control files as a local scan
- logs are routed to `C_REPO/logs/scan.log`

## Path Prefix Handling For NFS Targets

NFS post-copy phases operate relative to the copy's `D_REPO` prefix, not the raw export root. This matters for:

- hardlink
- delete
- mtime

Without that prefix handling, those phases would run against the wrong subtree on the NFS server.

## Build

Build with NFS support:

```bash
cargo build --release --features nfs
```

## Example Commands

Local to NFS:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target "nfs://127.0.0.1/opt/backup?sub=/copies" \
  --format common \
  --hardlink \
  --delete \
  --mtime
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

## Source Files

Primary files to inspect:

- `src/bin/fptcli.rs`
- `src/backup.rs`
- `src/backup/aio/mod.rs`
- `src/backup/aio/local_to_nfs.rs`
- `src/backup/aio/nfs_to_local.rs`
- `src/backup/aio/nfs_to_nfs.rs`
- `src/nfs/connection.rs`
- `src/nfs/scanner.rs`
- `src/nfs/aio/reader.rs`
- `src/nfs/aio/writer.rs`
- `src/nfs/aio/hardlink.rs`
- `src/nfs/aio/delete.rs`
- `src/nfs/aio/mtime.rs`
