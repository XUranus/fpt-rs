# Copy Pipeline Refactor

This document records the current copy-pipeline structure after the local/remote backup refactor.

## Goals

- Avoid one full implementation per source/target pair.
- Keep memory bounded by block size and worker count.
- Share control-file planning between backup and restore.
- Preserve transport-specific optimizations where they matter.

## Shared Planning

`src/backup/copy_plan.rs` is the common boundary between scanner control files and copy execution.

It converts control entries plus metadata into:

- `CopyPlanEntry::Directory`
- `CopyPlanEntry::File(FileCopyPlan::Direct)`
- `CopyPlanEntry::File(FileCopyPlan::Aggregate)`

Local backup, async transport backup, SMB-to-SMB streaming backup, and restore copy all consume this planning layer. This keeps path mapping, metadata lookup, and aggregation decisions out of transport-specific code.

## Bounded Transfer Blocks

`src/backup/copy_block.rs` defines `CopyBlock`, the common transfer unit for async transport adapters and restore.

The block carries:

- file metadata
- logical source and target paths
- source and target offsets
- bounded payload data
- end-of-file state

Adapters implement `SourceReader::read_block` and `TargetWriter::write_block`. This replaces whole-file buffering in the generic copy loop and allows `--buffer-size` to cap copy payload size.

## Local Backup

Local-to-local backup still uses a blocking worker pool, but the old multi-stage whole-file FCB queue graph has been removed.

Current local modules:

- `bio/local_copy.rs`: starts the local worker pool and follow-up phases
- `local_executor.rs`: executes direct and aggregate local file plans
- `local_block.rs`: bounded local stream-copy helpers
- `local_metadata.rs`: local xattr, ACL, symlink, and common metadata helpers
- `phases.rs`: local hardlink/delete/mtime phase orchestration

For common backup, each worker copies one file end-to-end with a reusable bounded buffer. For aggregate backup, workers enqueue pending source path metadata and stream source files into blobs when a bucket flushes.

## Async Transport Backup

`src/backup/aio/pipeline.rs` is the generic async copy executor for remote-capable source/target combinations.

Direction wrappers in `src/backup/aio/directions.rs` only assemble:

- an `EntryMapping`
- a source adapter
- a target adapter
- optional `AggregatingTarget`
- a concurrency limit

The transport adapters live in `src/backup/aio/transport.rs` and currently cover local, NFS, and SMB.

SMB-to-SMB common backup keeps a specialized direct streaming path through `copy_relative_file_streaming` because it avoids unnecessary intermediate buffering. It still consumes `CopyPlanEntry`, so planning remains shared.

## Restore Copy

`src/backup/restore_pipeline.rs` also consumes `CopyPlanEntry`.

Restore reads from the local copy repository:

- direct files from `D_REPO`
- aggregated files through the selected aggregate layout index

It then writes through the selected target adapter, so local, NFS, and SMB restore targets share the same restore copy loop.

## Removed Code

`src/backup/bio/copy.rs` was removed. It was the old local whole-file `FileControlBlock` queue graph and was the source of the local common backup OOM/stall failure mode documented in [bugfix-local-common-backup-oom-stall.md](bugfix/bugfix-local-common-backup-oom-stall.md).
