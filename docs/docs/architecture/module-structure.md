---
sidebar_position: 2
title: Module Structure
description: Detailed module tree and responsibility map for the fpt-rs codebase with actual file sizes and module declarations.
---

# Module Structure

This document describes the module tree of fpt-rs, explaining each module's role and how the three transport modules (native, NFS, SMB) form a symmetric, pluggable architecture. All line counts are from the actual source.

## Crate Root (`src/lib.rs`)

The crate root at `src/lib.rs:1` declares the top-level modules:

```rust
pub mod backup;
pub mod failure;
pub mod frame;
pub mod logging;
pub mod native;
pub use utility::path_util;
pub mod scanner;
pub mod utility;

#[cfg(feature = "nfs")]
pub mod nfs;

#[cfg(feature = "smb")]
pub mod smb;
```

The total codebase is approximately **32,588 lines** of Rust across all modules.

## Module Tree

```mermaid
graph TD
    subgraph ROOT["fpt-rs crate (src/lib.rs)"]
        LIB["lib.rs (14 lines)"]
    end

    subgraph SCANNER["scanner/ (src/scanner.rs, 220 lines)"]
        SC_ENGINE["engine/ (src/scanner/engine.rs, 405 lines)"]
        SC_BIO["engine/bio.rs -- Local blocking scan"]
        SC_AIO["engine/aio.rs (153 lines) -- AsyncDirScanner trait"]
        SC_COMMON["engine/common.rs (57 lines) -- Shared helpers"]
        SC_META["metadata/"]
        SC_CACHE["cache_storage.rs (341 lines)<br/>BinObjectSeqWriter"]
        SC_CONTROL_CODEC["control_codec.rs (292 lines)<br/>Control file encode/decode"]
        SC_CONTROL_PLAN["control_plan.rs (534 lines)<br/>Control file generation"]
        SC_META_STORAGE["meta_storage.rs (362 lines)<br/>Metadata file storage"]
        SC_SHARDED["sharded_control.rs (509 lines)<br/>Sharded control files"]
        SC_DIFF["diff.rs (576 lines)<br/>Incremental diff"]
        SC_DELETE["delete.rs (184 lines)<br/>Delete detection"]
        SC_HARDLINK["hardlink.rs (360 lines)<br/>Hardlink detection"]
        SC_FILTER["filter.rs (335 lines)<br/>ScanPathFilterSet"]
        SC_MODELS["models.rs (153 lines)<br/>DirBatchScanResult"]
        SC_OPTIONS["options.rs (460 lines)<br/>ScanOption"]
    end

    subgraph BACKUP["backup/ (src/backup.rs, 1075 lines)"]
        BK_AIO["aio/"]
        BK_TRANSPORT["transport.rs (137 lines)<br/>SourceReader, TargetWriter"]
        BK_PIPELINE["pipeline.rs (161 lines)"]
        BK_AGGREGATION["aggregation.rs (398 lines)<br/>Aggregate writer"]
        BK_PHASES["phases_trait.rs (71 lines)<br/>PostCopyPhases"]
        BK_RESTORE_OPS["restore_ops.rs (29 lines)<br/>RestoreOps"]
        BK_ENTRY["entry.rs (294 lines)<br/>EntryMapping"]
        BK_ORCHESTRATOR["orchestrator.rs (325 lines)<br/>spawn_backup()"]
        BK_SOURCE["source.rs (72 lines)<br/>BackupSource enum"]
        BK_TARGET["target.rs (123 lines)<br/>BackupTarget enum"]
        BK_AGGREGATE["aggregate/ (mod.rs 454 lines)"]
        BK_COPY_PLAN["copy_plan.rs (98 lines)<br/>CopyPlanEntry"]
        BK_FCB["fcb.rs (131 lines)<br/>FileControlBlock"]
        BK_COPY_BLOCK["copy_block.rs (77 lines)<br/>CopyBlock"]
        BK_RESTORE["restore_pipeline.rs (382 lines)<br/>run_restore_copy_pipeline()"]
        BK_STATS["stats.rs (160 lines)<br/>BackupStats"]
    end

    subgraph FRAME["frame/ (src/frame.rs, 120 lines)"]
        FR_BACKUP_JOB["backup_job.rs (437 lines)<br/>FileBackupJob"]
        FR_RESTORE_JOB["restore_job.rs (330 lines)<br/>RestoreJob"]
        FR_SCAN["scan.rs (256 lines)<br/>ScanJob"]
        FR_SUBTASK["subtask.rs (329 lines)<br/>run_backup_subtask()"]
        FR_LIFECYCLE["lifecycle.rs (285 lines)<br/>TaskLifecycleError"]
        FR_LOCATION["location.rs (212 lines)<br/>DataLocation enum"]
        FR_REPO["repo.rs (196 lines)<br/>RepoLayout"]
        FR_POSTJOB["postjob.rs (346 lines)<br/>BackupPostJob"]
        FR_PREREQ["prereq.rs (504 lines)<br/>BackupPrereqJob"]
        FR_SCANNER_IMPL["scanner_impls.rs (431 lines)<br/>ScannerConfig"]
        FR_BACKUP_IMPL["backup_impls.rs (658 lines)<br/>BackupConfig"]
        FR_RESTORE_IMPL["restore_impls.rs (259 lines)<br/>RestoreConfig"]
        FR_CONTROL["control_files.rs (84 lines)<br/>Control file naming"]
        FR_TRAITS["traits.rs (204 lines)<br/>BackupRestoreJob trait"]
    end

    subgraph NATIVE["native/ (src/native.rs, 215 lines)"]
        NT_SCANNER["scanner.rs (511 lines)<br/>LocalFileScanner"]
        NT_BACKUP["backup/ (mod.rs 60 lines)"]
        NT_LOCAL_COPY["local_copy.rs (178 lines)<br/>File copy"]
        NT_LOCAL_BLOCK["local_block.rs (45 lines)<br/>Block I/O"]
        NT_EXECUTOR["local_executor.rs (317 lines)<br/>Plan executor"]
        NT_METADATA["local_metadata.rs (269 lines)<br/>Metadata write"]
        NT_HARDLINK["hardlink.rs (469 lines)"]
        NT_DELETE["delete.rs (290 lines)"]
        NT_MTIME["mtime.rs (249 lines)"]
        NT_PHASES["phases_impl.rs (103 lines)<br/>PostCopyPhases impl"]
        NT_RESTORE_OPS["restore_ops.rs (21 lines)<br/>RestoreOps impl"]
        NT_FSTAT["fstat.rs (433 lines)<br/>File stat helpers"]
        NT_FWRITE["fwrite_meta.rs (230 lines)<br/>Metadata write format"]
    end

    subgraph NFSMOD["nfs/ (src/nfs.rs, 298 lines) -- feature: nfs"]
        NF_SCANNER["scanner.rs (670 lines)<br/>NfsScanner"]
        NF_CONNECTION["connection.rs (245 lines)<br/>NfsConnectionPool"]
        NF_BACKUP["backup/ (mod.rs 10 lines)"]
        NF_READER["reader.rs (234 lines)<br/>NfsSourceReader"]
        NF_WRITER["writer.rs (554 lines)<br/>NfsTargetWriter"]
        NF_TRANSPORT["transport.rs (94 lines)"]
        NF_PIPELINE["pipeline.rs (211 lines)"]
        NF_HARDLINK["hardlink.rs (205 lines)"]
        NF_DELETE["delete.rs (189 lines)"]
        NF_MTIME["mtime.rs (128 lines)"]
        NF_PHASES["phases_impl.rs (94 lines)"]
        NF_ERROR["error.rs (48 lines)"]
    end

    subgraph SMBMOD["smb/ (src/smb.rs, 398 lines) -- feature: smb"]
        SM_SCANNER["scanner.rs (538 lines)<br/>SmbScanner"]
        SM_CONNECTION["connection.rs (84 lines)<br/>SmbClientPool"]
        SM_BACKUP["backup/ (mod.rs 11 lines)"]
        SM_EXECUTOR["executor.rs (278 lines)<br/>SMB executor"]
        SM_WRITER["writer.rs (523 lines)<br/>SmbTargetWriter"]
        SM_TRANSPORT["transport.rs (65 lines)"]
        SM_PIPELINE["pipeline.rs (820 lines)"]
        SM_HARDLINK["hardlink.rs (172 lines)"]
        SM_DELETE["delete.rs (168 lines)"]
        SM_MTIME["mtime.rs (142 lines)"]
        SM_PHASES["phases_impl.rs (87 lines)"]
        SM_METRICS["metrics.rs (206 lines)"]
    end

    subgraph UTILITY["utility/"]
        UT_BLOCKING["blocking_queue.rs (157 lines)<br/>BlockingQueue"]
        UT_SPILL["spill_queue.rs (526 lines)<br/>SpillQueue"]
        UT_PATH["path_util.rs (340 lines)<br/>Path normalization"]
    end

    subgraph BIN["bin/"]
        BIN_FPTCLI["fptcli.rs (844 lines)"]
        BIN_FSSCAN["fsscan.rs (501 lines)"]
        BIN_FSBACKUP["fsbackup.rs (427 lines)"]
        BIN_FSDIFF["fsdiff.rs (550 lines)"]
        BIN_METAINSPECT["metainspect.rs (712 lines)"]
        BIN_FPTSERVER["fptserver.rs (1359 lines)"]
    end

    ROOT --> SCANNER
    ROOT --> BACKUP
    ROOT --> FRAME
    ROOT --> NATIVE
    ROOT --> NFSMOD
    ROOT --> SMBMOD
    ROOT --> UTILITY
    ROOT --> BIN
```

## Module Responsibilities

### `scanner/` -- Scan Engine

The scanner module is responsible for **traversing a filesystem** and producing metadata and control files. It is transport-agnostic at the core: it defines the data structures and processing pipeline, while the actual directory listing is delegated to transport-specific implementations.

The scanner module declaration at `src/scanner.rs:29`:

```rust
pub(crate) mod engine;
pub mod filter;
pub mod metadata;
pub(crate) mod models;
pub mod options;
```

| Submodule | Lines | Responsibility |
|-----------|-------|---------------|
| `engine/bio.rs` | -- | Blocking I/O scan engine for local filesystems. Spawns worker threads that read directories via `std::fs`. |
| `engine/aio.rs` | 153 | Async scan engine for remote transports. Defines `AsyncDirScanner` trait and `run_aio_scan()` scaffolding. |
| `engine/common.rs` | 57 | Shared helpers such as `retry_async()` used by both BIO and AIO engines. |
| `metadata/meta_storage.rs` | 362 | Writes `FileMeta` and `DirMeta` to binary `.dat` files in `M_REPO/meta/`. |
| `metadata/cache_storage.rs` | 341 | `BinObjectSeqWriter<T>` -- generic fixed-size binary object serializer. |
| `metadata/control_codec.rs` | 292 | Encodes/decodes control file entries (copy, hardlink, delete, mtime). |
| `metadata/control_plan.rs` | 534 | Generates control files from metadata by comparing current and previous scans. |
| `metadata/sharded_control.rs` | 509 | Sharded control file writing for large backups. |
| `metadata/diff.rs` | 576 | Incremental diff logic: compares `FileMeta` records to detect changes. |
| `metadata/delete.rs` | 184 | Detects files/dirs present in the previous scan but absent in the current scan. |
| `metadata/hardlink.rs` | 360 | Detects hardlinked files by matching inode numbers across the scan. |
| `filter.rs` | 335 | `ScanPathFilterSet` -- include/exclude path filters applied during traversal. |
| `models.rs` | 153 | Core data structures: `DirBatchScanResult`, `DirScanEntry`, `ScanStatistics`. |
| `options.rs` | 460 | `ScanOption` -- all configuration knobs for the scanner. |

### `backup/` -- Backup Engine

The backup engine reads control files and metadata, then orchestrates data transfer through the transport traits. The module root `src/backup.rs` (1075 lines) contains the `BackupOption`, `BackupTask`, `RestoreOption`, `RestoreTask`, and all restore phase dispatch logic.

The AIO submodules are conditionally compiled:

```rust
#[cfg(any(feature = "nfs", feature = "smb"))]
pub(crate) mod aio;
```

| Submodule | Lines | Responsibility |
|-----------|-------|---------------|
| `aio/transport.rs` | 137 | Defines `SourceReader` and `TargetWriter` traits. Provides `LocalSource` and `LocalTarget` implementations. |
| `aio/pipeline.rs` | 161 | Generic async copy pipeline that reads from a `SourceReader` and writes to a `TargetWriter`. |
| `aio/orchestrator.rs` | 325 | `spawn_backup()` -- top-level orchestrator that composes source+target transports. |
| `aio/source.rs` | 72 | `BackupSource` enum -- connected source transport abstraction. |
| `aio/target.rs` | 123 | `BackupTarget` enum -- connected target transport + post-copy phase dispatch. |
| `aio/aggregation.rs` | 398 | Packs small files into aggregate blobs for efficient storage. |
| `aio/phases_trait.rs` | 71 | `PostCopyPhases` trait -- hardlink, delete, mtime phases after copy. |
| `aio/restore_ops.rs` | 29 | `RestoreOps` trait -- symlink creation and metadata restoration during restore. |
| `aio/entry.rs` | 294 | `EntryMapping` -- translates source paths to target paths for control file entries. |
| `copy_plan.rs` | 98 | `CopyPlanEntry` and `FileCopyPlan` -- what to copy and how (direct vs. aggregate). |
| `fcb.rs` | 131 | `FileControlBlock` (FCB) -- state machine for a single file's backup/restore operation. |
| `copy_block.rs` | 77 | `CopyBlock` -- the unit of data transfer in the async pipeline. |
| `restore_pipeline.rs` | 382 | `run_restore_copy_pipeline()` -- generic restore pipeline parameterized by `T: TargetWriter` and `R: RestoreOps`. |
| `aggregate/` | 454 (mod) | Aggregate blob management: engine, indexing, local operations, restore. |
| `stats.rs` | 160 | `BackupStats`, `BackupStatsSnapshot` -- real-time metrics. |

### `frame/` -- Orchestration Layer

The frame layer manages the **full job lifecycle** and dispatches to the correct transport. The module root is `src/frame.rs` (120 lines).

| Submodule | Lines | Responsibility |
|-----------|-------|---------------|
| `backup_job.rs` | 437 | `FileBackupJob` -- the top-level backup job. Runs 4 phases: prereq, scan, subtasks, post-job. |
| `restore_job.rs` | 330 | `RestoreJob` -- the top-level restore job. Reads manifest, dispatches restore subtasks. |
| `scan.rs` | 256 | `ScanJob` -- dispatches scanning to the correct transport based on `DataLocation`. |
| `subtask.rs` | 329 | Splits control files into parallel subtasks and spawns transport-specific backup executors. |
| `location.rs` | 212 | `DataLocation` enum -- `Local(PathBuf)`, `Nfs(NfsLocation)`, `Smb(SmbLocation)`. |
| `repo.rs` | 196 | `RepoLayout` -- describes the `COPY_{format}_{type}_{uuid}/` directory structure. |
| `postjob.rs` | 346 | `BackupPostJob` -- writes `manifest.json` and uploads repos to remote targets. |
| `prereq.rs` | 504 | `BackupPrereqJob` -- validates source/target accessibility before starting. |
| `scanner_impls.rs` | 431 | `ScannerConfig`, `LocalFileScanner`, `NfsFileScanner`, `SmbFileScanner`. |
| `backup_impls.rs` | 658 | `BackupConfig` -- shared configuration for all backup implementations. |
| `restore_impls.rs` | 259 | Restore dispatch: builds the correct `TargetWriter` + `RestoreOps` for the target `DataLocation`. |
| `control_files.rs` | 84 | Control file naming conventions: `copy_*.control.bin`, `hardlink_*.control.bin`, etc. |
| `lifecycle.rs` | 285 | `TaskLifecycleError` -- common lifecycle error types. |
| `traits.rs` | 204 | `BackupRestoreJob`, `FileScanner`, `FileBackup`, `FileRestore` traits. |

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

The native transport module declaration (`src/native.rs:3`):

```rust
pub mod backup;
pub(crate) mod fstat;
mod fwrite_meta;
pub mod scanner;
```

The NFS transport module declaration (`src/nfs.rs:26`):

```rust
pub mod backup;
pub mod connection;
pub mod error;
pub(crate) mod fstat;
pub mod scanner;
```

The SMB transport module declaration (`src/smb.rs:11`):

```rust
pub mod backup;
pub(crate) mod connection;
pub(crate) mod fstat;
pub mod scanner;
```

| Transport | Scanner | SourceReader | TargetWriter | PostCopyPhases | RestoreOps | Connection |
|-----------|---------|-------------|-------------|----------------|------------|------------|
| `native/` | `LocalFileScanner` (511 lines) | `LocalSource` (shared) | `LocalTarget` (shared) | `LocalPostCopyPhases` | `LocalRestoreOps` | N/A (direct syscalls) |
| `nfs/` | `NfsScanner` (670 lines) | `NfsSourceReader` (234 lines) | `NfsTargetWriter` (554 lines) | `NfsPostCopyPhases` | N/A (default no-op) | `NfsConnectionPool` (245 lines) |
| `smb/` | `SmbScanner` (538 lines) | N/A (reads from local) | `SmbTargetWriter` (523 lines) | `SmbPostCopyPhases` | N/A (default no-op) | `SmbClientPool` (84 lines) |

### `utility/` -- Shared Utilities

| Module | Lines | Responsibility |
|--------|-------|---------------|
| `blocking_queue.rs` | 157 | `BlockingQueue<T>` -- bounded, thread-safe queue used between scanner workers and metadata writers. |
| `spill_queue.rs` | 526 | `SpillQueue<T>` -- memory-bounded queue that spills to disk when memory pressure is high. |
| `path_util.rs` | 340 | Path normalization functions for logical paths used in control files. |

### `bin/` -- CLI Binaries

| Binary | Lines | Purpose |
|--------|-------|---------|
| `fptcli` | 844 | Main CLI entry point -- unified interface for scan, backup, restore, diff. |
| `fsscan` | 501 | Standalone scan tool. |
| `fsbackup` | 427 | Standalone backup tool. |
| `fsdiff` | 550 | Diff tool -- compares two scans or a scan against a live filesystem. |
| `metainspect` | 712 | Inspector for binary metadata and control files. |
| `fptserver` | 1359 | Server daemon mode for remote management. |
| `smbprobe` | 65 | SMB connectivity probe tool. |
| `vdbench` | 422 | Performance benchmarking tool. |

## Symmetric Pluggable Engines

The key architectural pattern is that `native/`, `nfs/`, and `smb/` are **symmetric pluggable engines**. They share:

- The same `scanner/` + `backup/` internal layout.
- The same trait implementations (`AsyncDirScanner` for NFS/SMB, `PostCopyPhases` for all, `SourceReader`/`TargetWriter` for all).
- The same integration point: the frame layer dispatches via `DataLocation` matching.

```mermaid
classDiagram
    class AsyncDirScanner {
        <<trait>>
        type Error
        +scan(self, scan_option, tx) Pin~Box~Future~~
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
