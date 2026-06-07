---
sidebar_position: 2
title: Module Structure
description: Detailed module tree and responsibility map for the fpt-rs codebase.
---

# Module Structure

This document describes the module tree of fpt-rs, explaining each module's role and how the three transport modules (native, NFS, SMB) form a symmetric, pluggable architecture.

## Module Tree

```mermaid
graph TD
    subgraph ROOT["fpt-rs crate"]
        LIB["lib.rs<br/>crate root"]
        MAIN["main.rs<br/>entry point"]
    end

    subgraph SCANNER["scanner/ -- Scan Engine"]
        SC_ENGINE["engine/"]
        SC_BIO["engine/bio.rs<br/>Local blocking scan"]
        SC_AIO["engine/aio.rs<br/>Async scan (NFS/SMB)"]
        SC_COMMON["engine/common.rs<br/>Shared helpers"]
        SC_META["metadata/"]
        SC_CACHE["cache_storage.rs<br/>Binary object I/O"]
        SC_CONTROL_CODEC["control_codec.rs<br/>Control file encode/decode"]
        SC_CONTROL_PLAN["control_plan.rs<br/>Control file generation"]
        SC_META_STORAGE["meta_storage.rs<br/>Metadata file storage"]
        SC_DIFF["diff.rs<br/>Incremental diff"]
        SC_DELETE["delete.rs<br/>Delete detection"]
        SC_HARDLINK["hardlink.rs<br/>Hardlink detection"]
        SC_FILTER["filter.rs<br/>Path filters"]
        SC_MODELS["models.rs<br/>DirBatchScanResult"]
        SC_OPTIONS["options.rs<br/>ScanOption"]
    end

    subgraph BACKUP["backup/ -- Backup Engine"]
        BK_AIO["aio/"]
        BK_TRANSPORT["aio/transport.rs<br/>SourceReader, TargetWriter"]
        BK_PIPELINE["aio/pipeline.rs<br/>Copy pipeline"]
        BK_AGGREGATION["aio/aggregation.rs<br/>Aggregate writer"]
        BK_PHASES["aio/phases_trait.rs<br/>PostCopyPhases"]
        BK_RESTORE_OPS["aio/restore_ops.rs<br/>RestoreOps"]
        BK_ENTRY["aio/entry.rs<br/>Entry mapping"]
        BK_ORCHESTRATOR["aio/orchestrator.rs<br/>Pipeline orchestrator"]
        BK_AGGREGATE["aggregate/"]
        BK_COPY_PLAN["copy_plan.rs<br/>Copy plan generation"]
        BK_FCB["fcb.rs<br/>FileControlBlock"]
        BK_COPY_BLOCK["copy_block.rs<br/>CopyBlock"]
        BK_RESTORE["restore_pipeline.rs<br/>Restore pipeline"]
        BK_STATS["stats.rs<br/>BackupStats"]
    end

    subgraph FRAME["frame/ -- Orchestration"]
        FR_BACKUP_JOB["backup_job.rs<br/>FileBackupJob"]
        FR_RESTORE_JOB["restore_job.rs<br/>RestoreJob"]
        FR_SCAN["scan.rs<br/>ScanJob"]
        FR_SUBTASK["subtask.rs<br/>Subtask dispatch"]
        FR_LIFECYCLE["lifecycle.rs<br/>Task lifecycle"]
        FR_LOCATION["location.rs<br/>DataLocation enum"]
        FR_REPO["repo.rs<br/>RepoLayout"]
        FR_POSTJOB["postjob.rs<br/>PostJob"]
        FR_PREREQ["prereq.rs<br/>Prerequisites"]
        FR_SCANNER_IMPL["scanner_impls.rs<br/>Scanner implementations"]
        FR_BACKUP_IMPL["backup_impls.rs<br/>BackupConfig"]
        FR_RESTORE_IMPL["restore_impls.rs<br/>Restore implementations"]
        FR_CONTROL["control_files.rs<br/>Control file naming"]
        FR_TRAITS["traits.rs<br/>BackupRestoreJob trait"]
    end

    subgraph NATIVE["native/ -- Local FS Transport"]
        NT_SCANNER["scanner.rs<br/>LocalFileScanner"]
        NT_BACKUP["backup/"]
        NT_LOCAL_COPY["local_copy.rs<br/>File copy"]
        NT_LOCAL_BLOCK["local_block.rs<br/>Block I/O"]
        NT_EXECUTOR["local_executor.rs<br/>Plan executor"]
        NT_METADATA["local_metadata.rs<br/>Metadata write"]
        NT_HARDLINK["hardlink.rs"]
        NT_DELETE["delete.rs"]
        NT_MTIME["mtime.rs"]
        NT_PHASES["phases_impl.rs<br/>PostCopyPhases impl"]
        NT_RESTORE_OPS["restore_ops.rs<br/>RestoreOps impl"]
        NT_FSTAT["fstat.rs<br/>File stat helpers"]
        NT_FWRITE["fwrite_meta.rs<br/>Metadata write format"]
    end

    subgraph NFS["nfs/ -- NFSv3 Transport"]
        NF_SCANNER["scanner.rs<br/>NfsScanner"]
        NF_CONNECTION["connection.rs<br/>NfsConnectionPool"]
        NF_BACKUP["backup/"]
        NF_READER["reader.rs<br/>NfsSourceReader"]
        NF_WRITER["writer.rs<br/>NfsTargetWriter"]
        NF_TRANSPORT["transport.rs<br/>NFS transport"]
        NF_PIPELINE["pipeline.rs<br/>NFS pipeline"]
        NF_HARDLINK["hardlink.rs"]
        NF_DELETE["delete.rs"]
        NF_MTIME["mtime.rs"]
        NF_PHASES["phases_impl.rs<br/>PostCopyPhases impl"]
        NF_FSTAT["fstat.rs"]
        NF_ERROR["error.rs"]
    end

    subgraph SMB["smb/ -- SMB Transport"]
        SM_SCANNER["scanner.rs<br/>SmbScanner"]
        SM_CONNECTION["connection.rs<br/>SMB connection pool"]
        SM_BACKUP["backup/"]
        SM_EXECUTOR["executor.rs<br/>SMB executor"]
        SM_WRITER["writer.rs<br/>SmbTargetWriter"]
        SM_TRANSPORT["transport.rs<br/>SMB transport"]
        SM_PIPELINE["pipeline.rs<br/>SMB pipeline"]
        SM_HARDLINK["hardlink.rs"]
        SM_DELETE["delete.rs"]
        SM_MTIME["mtime.rs"]
        SM_PHASES["phases_impl.rs<br/>PostCopyPhases impl"]
        SM_METRICS["metrics.rs"]
        SM_FSTAT["fstat.rs"]
    end

    subgraph UTILITY["utility/ -- Shared Utilities"]
        UT_BLOCKING["blocking_queue.rs<br/>BlockingQueue"]
        UT_SPILL["spill_queue.rs<br/>SpillQueue"]
        UT_PATH["path_util.rs<br/>Path normalization"]
    end

    subgraph BIN["bin/ -- CLI Binaries"]
        BIN_FPTCLI["fptcli.rs<br/>Main CLI"]
        BIN_FSSCAN["fsscan.rs<br/>Scan CLI"]
        BIN_FSBACKUP["fsbackup.rs<br/>Backup CLI"]
        BIN_FSDIFF["fsdiff.rs<br/>Diff CLI"]
        BIN_METAINSPECT["metainspect.rs<br/>Metadata inspector"]
        BIN_FPTSERVER["fptserver.rs<br/>Server daemon"]
    end

    ROOT --> SCANNER
    ROOT --> BACKUP
    ROOT --> FRAME
    ROOT --> NATIVE
    ROOT --> NFS
    ROOT --> SMB
    ROOT --> UTILITY
    ROOT --> BIN
```

## Module Responsibilities

### `scanner/` -- Scan Engine

The scanner module is responsible for **traversing a filesystem** and producing metadata and control files. It is transport-agnostic at the core: it defines the data structures and processing pipeline, while the actual directory listing is delegated to transport-specific implementations.

| Submodule | Responsibility |
|-----------|---------------|
| `engine/bio.rs` | Blocking I/O scan engine for local filesystems. Spawns worker threads that read directories via `std::fs`. |
| `engine/aio.rs` | Async scan engine for remote transports. Defines the `AsyncDirScanner` trait and `run_aio_scan()` scaffolding. |
| `engine/common.rs` | Shared helpers such as `retry_async()` used by both BIO and AIO engines. |
| `metadata/meta_storage.rs` | Writes `FileMeta` and `DirMeta` to binary `.dat` files in `M_REPO/meta/`. |
| `metadata/cache_storage.rs` | `BinObjectSeqWriter<T>` -- generic fixed-size binary object serializer. |
| `metadata/control_codec.rs` | Encodes/decodes control file entries (copy, hardlink, delete, mtime). |
| `metadata/control_plan.rs` | Generates control files from metadata by comparing current and previous scans. |
| `metadata/diff.rs` | Incremental diff logic: compares `FileMeta` records to detect changes. |
| `metadata/delete.rs` | Detects files/dirs present in the previous scan but absent in the current scan. |
| `metadata/hardlink.rs` | Detects hardlinked files by matching inode numbers across the scan. |
| `filter.rs` | `ScanPathFilterSet` -- include/exclude path filters applied during traversal. |
| `models.rs` | Core data structures: `DirBatchScanResult`, `DirScanEntry`, `ScanStatistics`. |
| `options.rs` | `ScanOption` -- all configuration knobs for the scanner. |

### `backup/` -- Backup Engine

The backup engine reads control files and metadata, then orchestrates data transfer through the transport traits.

| Submodule | Responsibility |
|-----------|---------------|
| `aio/transport.rs` | Defines `SourceReader` and `TargetWriter` traits. Provides `LocalSource` and `LocalTarget` implementations. |
| `aio/pipeline.rs` | Generic async copy pipeline that reads from a `SourceReader` and writes to a `TargetWriter`. |
| `aio/orchestrator.rs` | Top-level orchestrator that coordinates the copy pipeline with post-copy phases. |
| `aio/aggregation.rs` | Packs small files into aggregate blobs for efficient storage. |
| `aio/phases_trait.rs` | `PostCopyPhases` trait -- hardlink, delete, mtime phases after copy. |
| `aio/restore_ops.rs` | `RestoreOps` trait -- symlink creation and metadata restoration during restore. |
| `aio/entry.rs` | `EntryMapping` -- translates source paths to target paths for control file entries. |
| `copy_plan.rs` | `CopyPlanEntry` and `FileCopyPlan` -- what to copy and how (direct vs. aggregate). |
| `fcb.rs` | `FileControlBlock` (FCB) -- state machine for a single file's backup/restore operation. |
| `copy_block.rs` | `CopyBlock` -- the unit of data transfer in the async pipeline. |
| `restore_pipeline.rs` | `run_restore_copy_pipeline()` -- generic restore pipeline parameterized by `TargetWriter` and `RestoreOps`. |
| `aggregate/` | Aggregate blob management: engine, indexing, local operations, restore. |

### `frame/` -- Orchestration Layer

The frame layer manages the **full job lifecycle** and dispatches to the correct transport.

| Submodule | Responsibility |
|-----------|---------------|
| `backup_job.rs` | `FileBackupJob` -- the top-level backup job. Runs 4 phases: prereq, scan, subtasks, post-job. |
| `restore_job.rs` | `RestoreJob` -- the top-level restore job. Reads manifest, dispatches restore subtasks. |
| `scan.rs` | `ScanJob` -- dispatches scanning to the correct transport based on `DataLocation`. |
| `subtask.rs` | Splits control files into parallel subtasks and spawns transport-specific backup executors. |
| `location.rs` | `DataLocation` enum -- `Local(PathBuf)`, `Nfs(NfsLocation)`, `Smb(SmbLocation)`. |
| `repo.rs` | `RepoLayout` -- describes the `COPY_{format}_{type}_{uuid}/` directory structure. |
| `postjob.rs` | `BackupPostJob` -- writes `manifest.json` and uploads repos to remote targets. |
| `prereq.rs` | `BackupPrereqJob` -- validates source/target accessibility before starting. |
| `scanner_impls.rs` | `ScannerConfig`, `LocalFileScanner`, `NfsFileScanner`, `SmbFileScanner`. |
| `backup_impls.rs` | `BackupConfig` -- shared configuration for all backup implementations. |
| `restore_impls.rs` | Restore dispatch: builds the correct `TargetWriter` + `RestoreOps` for the target `DataLocation`. |
| `control_files.rs` | Control file naming conventions: `copy_*.control.bin`, `hardlink_*.control.bin`, etc. |
| `lifecycle.rs` | `TaskLifecycleError` -- common lifecycle error types. |
| `traits.rs` | `BackupRestoreJob` trait -- the common interface for backup and restore jobs. |

### Transport Modules

The three transport modules (`native/`, `nfs/`, `smb/`) are **symmetric** in structure. Each provides:

```mermaid
graph LR
    subgraph Transport Module
        SC["scanner.rs<br/>FileScanner impl"]
        BK["backup/<br/>"]
        BK_RD["reader.rs / local_copy.rs<br/>SourceReader impl"]
        BK_WR["writer.rs<br/>TargetWriter impl"]
        BK_HL["hardlink.rs<br/>Hardlink phase"]
        BK_DL["delete.rs<br/>Delete phase"]
        BK_MT["mtime.rs<br/>Mtime phase"]
        BK_PH["phases_impl.rs<br/>PostCopyPhases impl"]
        BK_RO["restore_ops.rs<br/>RestoreOps impl (native only)"]
        FS["fstat.rs<br/>File stat helpers"]
        CONN["connection.rs<br/>Connection pool (NFS/SMB)"]
    end

    SC --> BK
    BK_RD --> BK_WR
    BK --> BK_HL
    BK --> BK_DL
    BK --> BK_MT
    BK --> BK_PH
```

| Transport | Scanner | SourceReader | TargetWriter | PostCopyPhases | RestoreOps | Connection |
|-----------|---------|-------------|-------------|----------------|------------|------------|
| `native/` | `LocalFileScanner` (BIO) | `LocalSource` (shared) | `LocalTarget` (shared) | `LocalPhases` | `LocalRestoreOps` | N/A (direct syscalls) |
| `nfs/` | `NfsScanner` (AIO) | `NfsSourceReader` | `NfsTargetWriter` | `NfsPhases` | N/A (default no-op) | `NfsConnectionPool` |
| `smb/` | `SmbScanner` (AIO) | N/A (reads from local) | `SmbTargetWriter` | `SmbPhases` | N/A (default no-op) | `SMB connection pool` |

### `utility/` -- Shared Utilities

| Module | Responsibility |
|--------|---------------|
| `blocking_queue.rs` | `BlockingQueue<T>` -- bounded, thread-safe queue used between scanner workers and metadata writers. |
| `spill_queue.rs` | `SpillQueue<T>` -- memory-bounded queue that spills to disk when memory pressure is high. |
| `path_util.rs` | Path normalization functions for logical paths used in control files. |

### `bin/` -- CLI Binaries

| Binary | Purpose |
|--------|---------|
| `fptcli` | Main CLI entry point -- unified interface for scan, backup, restore, diff. |
| `fsscan` | Standalone scan tool. |
| `fsbackup` | Standalone backup tool. |
| `fsdiff` | Diff tool -- compares two scans or a scan against a live filesystem. |
| `metainspect` | Inspector for binary metadata and control files. |
| `fptserver` | Server daemon mode for remote management. |
| `smbprobe` | SMB connectivity probe tool. |
| `vdbench` | Performance benchmarking tool. |

## Symmetric Pluggable Engines

The key architectural pattern is that `native/`, `nfs/`, and `smb/` are **symmetric pluggable engines**. They share:

- The same `scanner/` + `backup/` internal layout.
- The same trait implementations (`AsyncDirScanner` for NFS/SMB, `PostCopyPhases` for all, `SourceReader`/`TargetWriter` for all).
- The same integration point: the frame layer dispatches via `DataLocation` matching.

```mermaid
classDiagram
    class AsyncDirScanner {
        <<trait>>
        +scan(scan_option, tx) Result
    }
    class PostCopyPhases {
        <<trait>>
        +run_hardlink_phase()
        +run_delete_phase()
        +run_mtime_phase()
        +run_all_phases()
    }
    class SourceReader {
        <<trait>>
        +read_block(block) Result~CopyBlock~
        +finish() Result
    }
    class TargetWriter {
        <<trait>>
        +create_dir(path) Result
        +write_block(block) Result~CopyBlock~
        +write_file(fcb) Result~FCB~
        +finish() Result
    }
    class RestoreOps {
        <<trait>>
        +create_symlink(path, target) Result
        +restore_metadata(path, meta)
    }

    class NfsScanner {
        +scan(root_fh, root_path, opt, tx)
    }
    class SmbScanner {
        +scan(opt, tx)
    }
    class LocalFileScanner {
        +scan() ScanStats
    }

    class NfsSourceReader {
        +read_block(block)
        +finish()
    }
    class NfsTargetWriter {
        +create_dir(path)
        +write_block(block)
        +finish()
    }
    class SmbTargetWriter {
        +create_dir(path)
        +write_block(block)
        +finish()
    }

    NfsScanner ..|> AsyncDirScanner : via NfsScanAdapter
    SmbScanner ..|> AsyncDirScanner : via SmbScanAdapter
    NfsSourceReader ..|> SourceReader
    NfsTargetWriter ..|> TargetWriter
    SmbTargetWriter ..|> TargetWriter
```

This design makes it straightforward to add new transports: implement the traits, wire them into `DataLocation` dispatch in the frame layer, and the rest of the pipeline works unchanged.
