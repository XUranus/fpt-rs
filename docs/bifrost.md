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

- `meta_<writer>_<segment>.dat`
- `fcache_*.dat`
- `dcache_*.dat`
- `copy.txt` or `copy_*.txt` when copy-control sharding is enabled
- optional `hardlink.txt`, `delete.txt`, `mtime.txt`

Scanner notes:

- `writer_count` now maps to real metadata writer shards.
- each writer owns its own metadata namespace and cache files
- metadata locators encode `(writer_shard, segment)` into the stored `meta_fid`
- copy control files can be sliced into multiple `copy_*.txt` shards so backup can schedule multiple copy subtasks in parallel

### `src/backup/`

Responsible for executing backup subtasks.

- `copy_plan.rs`: turns control-file entries plus metadata into transport-neutral copy plans
- `copy_block.rs`: bounded transfer block shared by local, NFS, SMB, and restore copy loops
- `bio/`: blocking local-filesystem entry point and local post-copy phases
- `aio/`: async transport adapters and remote-capable copy pipelines
- `local_executor.rs`, `local_block.rs`, `local_metadata.rs`: local copy, block I/O, and metadata helpers
- `aggregate*.rs`: aggregated-format backup and restore support
- `backup.rs`: top-level backup task dispatch

For common-format backup, the phase order is:

1. copy
2. hardlink
3. delete
4. mtime

For aggregated-format backup, only the copy phase is active.

The copy phase is planned once and then executed by the selected transport path:

- Local-to-local uses a bounded blocking worker pool over `FileCopyPlan`.
- Local/NFS/SMB combinations use `SourceReader` and `TargetWriter` adapters over `CopyBlock`.
- SMB-to-SMB common backup keeps its direct streaming fast path, but still consumes the shared copy plan.
- Restore copy also consumes the shared copy plan, reading from the local `D_REPO` or aggregate blobs and writing through the selected target adapter.

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

## Local, NFS, and SMB Paths

At the orchestration layer, source and target are represented as `DataLocation`.

- Local paths use ordinary filesystem paths.
- NFS paths are represented by `NfsLocation`.
- SMB paths are represented by `SmbLocation`.
- `fptcli` infers which one to build from the path string: plain path means local, `nfs://...` means NFS, and `smb://...` means SMB.

## Backup Direction Matrix

For common-format backup, local/NFS/SMB source and target combinations are routed through the copy-plan layer:

| Direction | Copy engine | Post-copy phases |
|-----------|-------------|------------------|
| local -> local | `backup::bio::local_copy` + `local_executor` | local hardlink/delete/mtime |
| local -> NFS/SMB | `backup::aio::pipeline` with local source and remote target adapters | target hardlink/delete/mtime |
| NFS/SMB -> local | `backup::aio::pipeline` with remote source and local target adapters | local hardlink/delete/mtime |
| NFS/SMB -> NFS/SMB | `backup::aio::pipeline`; SMB -> SMB common uses optimized streaming | target hardlink/delete/mtime |

For aggregated-format backup, only the copy phase runs.

## Logging Model

Bifrost uses a shared routed logger.

- scanner and NFS scan logs go to `C_REPO/logs/scan.log`
- frame/orchestration logs go to `C_REPO/logs/frame.log`
- backup and NFS AIO phase logs go to `C_REPO/logs/{subtask_uuid}.log`
- `--log-file` acts as a catch-all tee file

See [logging.md](logging.md) for details.

Structured per-entry failure logs are optional and are written to `C_REPO/logs` when enabled with `--failure-log-format`. See [retry_failure.md](retry_failure.md) for scan/backup failure record formats and retry policy behavior.

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
- [retry_failure.md](retry_failure.md)
