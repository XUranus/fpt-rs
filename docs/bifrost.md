# Bifrost Architecture

This document describes the current structure of Bifrost as implemented in the repository today.

## Overview

Bifrost has four main layers:

1. Scanner: walks a source tree and writes metadata plus control files.
2. Backup engine: consumes control files and performs copy, hardlink, delete, and mtime phases.
3. Frame/orchestration layer: creates copy layout, runs scan and subtasks, writes manifests, and handles NFS post-job transfer.
4. CLI binaries: `fptcli`, `fsscan`, `fsbackup`, `fsdiff`, and helper tools.

The main integrated workflow is driven by `fptcli`.

## Copy Layout

Each `fptcli backup` creates a copy root:

```text
COPY_{FORMAT}_{TYPE}_{UUID}/
  manifest.json
  D_REPO/
  M_REPO/
    meta/
  C_REPO/
    ctrl/
    logs/
    status/
```

Where:

- `D_REPO` stores backed-up data.
- `M_REPO/meta` stores metadata and cache files.
- `C_REPO/ctrl` stores control files such as `copy.txt`, `hardlink.txt`, `delete.txt`, and `mtime.txt`.
- `C_REPO/logs` stores routed logs.
- `C_REPO/status` stores sentinel files used by the job orchestration layer.

## Main Modules

### `src/scanner/`

Responsible for filesystem traversal and metadata generation.

- `engine.rs`: traversal and metadata/control-file writing
- `metadata/`: control-file, metadata, cache, diff, hardlink, delete, and mtime formats

Outputs:

- `meta_*.dat`
- `fcache_*.dat`
- `dcache_*.dat`
- `copy.txt`
- optional `hardlink.txt`, `delete.txt`, `mtime.txt`

### `src/backup/`

Responsible for executing backup subtasks.

- `bio/`: blocking local-filesystem pipeline
- `aio/`: NFS-involved backup pipelines
- `aggregate*.rs`: aggregated-format backup and restore support
- `backup.rs`: top-level backup task dispatch

For common-format backup, the phase order is:

1. copy
2. hardlink
3. delete
4. mtime

For aggregated-format backup, only the copy phase is active.

### `src/frame/`

The orchestration layer used by `fptcli`.

- `prereq.rs`: repo creation and source validation
- `scan.rs`: runs local or NFS scan
- `backup_job.rs`: coordinates scan and subtasks
- `postjob.rs`: writes manifest and uploads local repos when needed
- `repo.rs`: copy layout helpers
- `subtask.rs`: subtask selection and execution

### `src/nfs/`

NFS support used by scanning and NFS-involved backup phases.

- `connection.rs`: NFS connection pool
- `scanner.rs`: NFS tree traversal
- `aio/reader.rs`: NFS read helpers
- `aio/writer.rs`: NFS write helpers
- `aio/hardlink.rs`: NFS hardlink phase
- `aio/delete.rs`: NFS delete phase
- `aio/mtime.rs`: NFS mtime phase

## Local vs NFS Paths

At the orchestration layer, source and target are represented as `DataLocation`.

- Local paths use ordinary filesystem paths.
- NFS paths are represented by `NfsLocation`.
- `fptcli` infers which one to build from the path string: plain path means local, `nfs://...` means NFS.

## Backup Direction Matrix

For common-format backup, all four directions are wired:

| Direction | Copy engine | Post-copy phases |
|-----------|-------------|------------------|
| local -> local | `backup::bio` | local hardlink/delete/mtime |
| local -> NFS | `backup::aio::local_to_nfs` | NFS hardlink/delete/mtime |
| NFS -> local | `backup::aio::nfs_to_local` | local hardlink/delete/mtime |
| NFS -> NFS | `backup::aio::nfs_to_nfs` | NFS hardlink/delete/mtime |

For aggregated-format backup, only the copy phase runs.

## Logging Model

Bifrost uses a shared routed logger.

- scanner and NFS scan logs go to `C_REPO/logs/scan.log`
- frame/orchestration logs go to `C_REPO/logs/frame.log`
- backup and NFS AIO phase logs go to `C_REPO/logs/{subtask_uuid}.log`
- `--log-file` acts as a catch-all tee file

See [logging.md](logging.md) for details.

## Main Binaries

- `fptcli`: integrated backup and restore CLI
- `fsscan`: scan-only tool
- `fsbackup`: backup executor against existing control files
- `fsdiff`: compare source and target trees
- `cacheinspect`, `metainspect`, `vdbench`: helper tools

## Related Docs

- [fptcli.md](fptcli.md)
- [nfs.md](nfs.md)
- [aggregate.md](aggregate.md)
- [incremental.md](incremental.md)
- [ctrlfile.md](ctrlfile.md)
