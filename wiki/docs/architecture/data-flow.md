---
sidebar_position: 3
title: Data Flow
description: End-to-end data flow for backup, restore, and incremental operations in fpt-rs.
---

# Data Flow

This document traces the end-to-end data flow through fpt-rs for the three primary operations: **backup**, **restore**, and **incremental backup**. Sequence diagrams show how `DirBatchScanResult`, `FileControlBlock`, and `CopyBlock` move through the pipeline.

## Core Data Structures

Before examining the flows, here is how the key data structures relate:

```mermaid
graph TD
    DBSR["DirBatchScanResult<br/>{dir: DirMeta,<br/>files: Vec&lt;FileMeta&gt;,<br/>partial: bool,<br/>complete: bool}"]

    META_FILE["meta_*.dat<br/>(binary: FileMeta entries)"]
    META_DIR["dcache_*.dat<br/>(binary: DirCacheEntry entries)"]
    CTRL_FILE["copy_*.control.bin<br/>(binary: ControlFileEntry entries)"]

    FCB["FileControlBlock<br/>{meta: FileMeta,<br/>src_path, dst_path,<br/>buffer, offsets,<br/>src_state, dst_state}"]

    DCB["DirControlBlock<br/>{meta: DirMeta,<br/>src_path, dst_path}"]

    COPY_PLAN["CopyPlanEntry<br/>Directory {meta, dst_path}<br/>File(FileCopyPlan)"]

    COPY_BLOCK["CopyBlock<br/>{meta: Arc&lt;FileMeta&gt;,<br/>src_path, dst_path,<br/>src_offset, dst_offset,<br/>file_size, data, is_last}"]

    DBSR -->|"metadata writers<br/>serialize"| META_FILE
    DBSR -->|"metadata writers<br/>serialize"| META_DIR
    META_FILE -->|"control plan<br/>generates"| CTRL_FILE
    META_DIR -->|"control plan<br/>references"| CTRL_FILE

    CTRL_FILE -->|"entry reader<br/>deserializes"| FCB
    CTRL_FILE -->|"entry reader<br/>deserializes"| DCB
    FCB -->|"copy plan<br/>produces"| COPY_PLAN
    DCB -->|"copy plan<br/>produces"| COPY_PLAN
    FCB -->|"pipeline<br/>converts"| COPY_BLOCK
```

## Backup Flow

The backup flow has four phases, orchestrated by `FileBackupJob::run()`:

### Sequence Diagram

```mermaid
sequenceDiagram
    participant CLI as CLI / fptcli
    participant JOB as FileBackupJob
    participant PREREQ as BackupPrereqJob
    participant SCAN as ScanJob
    participant SCANNER as Transport Scanner<br/>(Local/NFS/SMB)
    participant META_W as Metadata Writers
    participant CTRL as Control File Generator
    participant SUBTASK as Subtask Dispatcher
    participant EXECUTOR as Transport Executor<br/>(Local/NFS/SMB)
    participant POST as BackupPostJob

    CLI->>JOB: run()

    rect rgb(240, 248, 255)
        Note over JOB,PREREQ: Phase 1 -- Prerequisites
        JOB->>PREREQ: run_sync()
        PREREQ->>PREREQ: Validate source accessibility
        PREREQ->>PREREQ: Validate target accessibility
        PREREQ-->>JOB: OK
    end

    rect rgb(240, 255, 240)
        Note over JOB,CTRL: Phase 2 -- Scan
        JOB->>JOB: Build ScanConfig<br/>(incremental: prev_meta_dir?)
        JOB->>SCAN: run()
        SCAN->>SCANNER: scan(root_path, ScanOption)
        loop For each directory batch
            SCANNER->>SCANNER: List directory entries
            SCANNER->>SCANNER: stat() each entry
            SCANNER-->>META_W: DirBatchScanResult<br/>{dir: DirMeta, files: Vec&lt;FileMeta&gt;}
            META_W->>META_W: Serialize FileMeta to meta_*.dat
            META_W->>META_W: Serialize DirCacheEntry to dcache_*.dat
        end
        SCANNER-->>SCAN: Scan complete
        SCAN->>CTRL: generate_control_files()
        CTRL->>CTRL: Read meta_*.dat (current + previous)
        CTRL->>CTRL: Diff: new/changed/deleted files
        CTRL->>CTRL: Write copy_*.control.bin
        CTRL->>CTRL: Write hardlink_*.control.bin
        CTRL->>CTRL: Write delete_*.control.bin
        CTRL->>CTRL: Write mtime_*.control.bin
        CTRL-->>SCAN: Control files generated
        SCAN-->>JOB: ScanStats
    end

    rect rgb(255, 248, 240)
        Note over JOB,EXECUTOR: Phase 3 -- Subtasks
        JOB->>JOB: Discover control files in C_REPO/ctrl/
        JOB->>SUBTASK: spawn_and_join_subtasks()
        loop For each control file (parallel subtasks)
            SUBTASK->>EXECUTOR: execute_backup(control_file)
            EXECUTOR->>EXECUTOR: Read control file entries
            EXECUTOR->>EXECUTOR: produce_copy_plan()
            loop For each CopyPlanEntry
                alt Directory entry
                    EXECUTOR->>EXECUTOR: create_dir(dst_path)
                    EXECUTOR->>EXECUTOR: restore_metadata()
                else File entry
                    EXECUTOR->>EXECUTOR: Open source file
                    loop Chunk by chunk
                        EXECUTOR->>EXECUTOR: Read CopyBlock from source
                        EXECUTOR->>EXECUTOR: Write CopyBlock to target
                    end
                    EXECUTOR->>EXECUTOR: Close handles, restore metadata
                end
            end
            EXECUTOR->>EXECUTOR: PostCopyPhases::run_all_phases()
            Note over EXECUTOR: hardlink, delete, mtime
            EXECUTOR-->>SUBTASK: SubtaskResult
        end
        SUBTASK-->>JOB: All subtasks complete
    end

    rect rgb(248, 240, 255)
        Note over JOB,POST: Phase 4 -- Post-Job
        JOB->>JOB: Build BackupManifest
        JOB->>POST: run()
        POST->>POST: Write manifest.json locally
        alt Remote target (NFS/SMB)
            POST->>POST: Upload M_REPO/ to target
            POST->>POST: Upload C_REPO/ to target
            POST->>POST: Upload manifest.json to target
        end
        POST-->>JOB: OK
    end

    JOB-->>CLI: JobResult {copy_uuid, stats}
```

### DirBatchScanResult Flow

The `DirBatchScanResult` is the fundamental unit of scan output. Here is how it flows through the system:

```mermaid
sequenceDiagram
    participant WORKER as Scan Worker Thread
    participant QUEUE as BlockingQueue<br/>&lt;DirBatchScanResult&gt;
    participant WRITER as Metadata Writer Thread
    participant DISK_META as M_REPO/meta/<br/>(.dat files)
    participant DISK_CTRL as C_REPO/ctrl/<br/>(.control.bin files)
    participant GEN as Control File<br/>Generator

    WORKER->>WORKER: readdir() + stat()
    WORKER->>WORKER: Build DirBatchScanResult<br/>{dir: DirMeta, files: [FileMeta...]}
    WORKER->>QUEUE: push(batch)
    QUEUE->>WRITER: pop(batch)
    WRITER->>DISK_META: write FileMeta entries<br/>to meta_*.dat (fixed-size binary)
    WRITER->>DISK_META: write DirCacheEntry<br/>to dcache_*.dat

    Note over GEN: After all batches processed
    GEN->>DISK_META: Read current meta_*.dat
    GEN->>DISK_META: Read previous meta_*.dat<br/>(if incremental)
    GEN->>GEN: Diff: compare FileMeta records<br/>(mtime, size, inode)
    GEN->>DISK_CTRL: Write changed/new files<br/>to copy_*.control.bin
    GEN->>DISK_CTRL: Write deleted files<br/>to delete_*.control.bin
    GEN->>DISK_CTRL: Write hardlink groups<br/>to hardlink_*.control.bin
    GEN->>DISK_CTRL: Write mtime entries<br/>to mtime_*.control.bin
```

## Restore Flow

The restore flow reads from a backup copy and writes to a restore target:

```mermaid
sequenceDiagram
    participant CLI as CLI / fptcli
    participant JOB as RestoreJob
    participant PREREQ as Prerequisites
    participant MANIFEST as manifest.json
    participant SUBTASK as Restore Subtask
    participant PLAN as Copy Plan Producer
    participant SOURCE as LocalRepoRestoreSource<br/>(SourceReader)
    participant TARGET as TargetWriter<br/>(Local/NFS/SMB)
    participant OPS as RestoreOps
    participant POST as RestorePostJob

    CLI->>JOB: run()

    rect rgb(240, 248, 255)
        Note over JOB,PREREQ: Phase 1 -- Prerequisites
        JOB->>PREREQ: Validate copy source
        JOB->>PREREQ: Validate restore target
    end

    rect rgb(240, 255, 240)
        Note over JOB,SUBTASK: Phase 2 -- Read Manifest & Dispatch
        JOB->>MANIFEST: Read manifest.json
        MANIFEST-->>JOB: BackupManifest {copy_uuid, subtasks[], ...}
        JOB->>JOB: Discover copy control files
    end

    rect rgb(255, 248, 240)
        Note over JOB,POST: Phase 3 -- Restore Subtasks
        loop For each control file (parallel subtasks)
            JOB->>SUBTASK: spawn_restore_subtask(control_file)
            SUBTASK->>PLAN: produce_copy_plan(control_file, meta_dir)
            PLAN->>PLAN: Read control file entries
            PLAN->>PLAN: Resolve FileMeta from M_REPO
            PLAN->>PLAN: Map source paths to target paths
            loop For each CopyPlanEntry
                alt Directory
                    SUBTASK->>TARGET: create_dir(dst_path)
                else File (Direct)
                    SUBTASK->>TARGET: create parent dirs
                    loop Chunk by chunk
                        SUBTASK->>SOURCE: read_block(CopyBlock)
                        SOURCE->>SOURCE: Read from D_REPO file
                        SOURCE-->>SUBTASK: CopyBlock with data
                        SUBTASK->>TARGET: write_block(CopyBlock)
                        TARGET-->>SUBTASK: CopyBlock with updated offsets
                    end
                    SUBTASK->>OPS: restore_metadata(path, meta)
                    Note over OPS: Permissions, xattrs, ACLs
                else File (Symlink)
                    SUBTASK->>OPS: create_symlink(path, target)
                    SUBTASK->>OPS: restore_metadata(path, meta)
                end
            end
            SUBTASK-->>JOB: RestoreStats
        end
    end

    rect rgb(248, 240, 255)
        Note over JOB,POST: Phase 4 -- Post-Job
        JOB->>POST: run()
        Note over POST: No-op for restore<br/>(data already at target)
    end

    JOB-->>CLI: JobResult
```

### Restore Pipeline Detail

The `run_restore_copy_pipeline()` function is generic over `T: TargetWriter` and `R: RestoreOps`:

```mermaid
graph TD
    subgraph Producer["Producer Thread (blocking)"]
        CTRL_READ["Read control file"]
        META_READ["Resolve FileMeta<br/>from M_REPO"]
        ENTRY_MAP["Map paths via<br/>EntryMapping"]
        SEND["Send CopyPlanEntry<br/>via mpsc channel"]
    end

    subgraph Workers["Async Worker Tasks (tokio)"]
        SEM["Semaphore<br/>(max_concurrent)"]
        MKDIR["create_dir()"]
        READ_LOOP["read_block() loop"]
        WRITE_LOOP["write_block() loop"]
        SKIP_CHECK["should_skip_restore()?<br/>(policy check)"]
        SYMLINK["create_symlink()"]
        METADATA["restore_metadata()"]
    end

    subgraph Fin["Finalization"]
        SOURCE_FIN["source.finish()"]
        TARGET_FIN["target.finish()"]
        STATS["Log RestoreStats"]
    end

    CTRL_READ --> META_READ --> ENTRY_MAP --> SEND
    SEND -->|"mpsc::channel"| SEM
    SEM --> MKDIR
    SEM --> SKIP_CHECK
    SKIP_CHECK -->|"skip"| STATS
    SKIP_CHECK -->|"proceed"| READ_LOOP
    READ_LOOP --> WRITE_LOOP
    WRITE_LOOP -->|"not done"| READ_LOOP
    WRITE_LOOP -->|"done"| METADATA
    SEM --> SYMLINK
    SYMLINK --> METADATA
    MKDIR --> STATS
    METADATA --> STATS
    SOURCE_FIN --> STATS
    TARGET_FIN --> STATS
```

## Incremental Backup Flow

Incremental backup reuses the same pipeline as full backup, with one key difference in Phase 2:

```mermaid
sequenceDiagram
    participant JOB as FileBackupJob
    participant SCAN as ScanJob
    participant SCANNER as Scanner
    participant DIFF as Diff Engine
    participant CTRL as Control File Generator

    Note over JOB: incremental_base = Some(previous_copy_root)
    JOB->>JOB: Build ScanConfig<br/>prev_meta_dir = base_repo.meta_dir

    JOB->>SCAN: run()

    SCAN->>SCANNER: scan(current_source, ScanOption)
    loop For each directory
        SCANNER-->>SCAN: DirBatchScanResult (current)
    end
    SCAN-->>JOB: Current scan complete

    Note over DIFF: Control file generation<br/>(incremental mode)
    DIFF->>DIFF: Read current M_REPO/meta/*.dat
    DIFF->>DIFF: Read previous M_REPO/meta/*.dat
    DIFF->>DIFF: For each current FileMeta:<br/>  Compare (mtime, size, inode) with previous
    DIFF->>DIFF: Classify each file:
    Note over DIFF: NEW: not in previous scan<br/>CHANGED: mtime/size/inode differ<br/>UNCHANGED: identical metadata<br/>DELETED: in previous but not current

    CTRL->>CTRL: Write copy_*.control.bin<br/>(NEW + CHANGED files only)
    CTRL->>CTRL: Write delete_*.control.bin<br/>(DELETED files)
    CTRL->>CTRL: Write hardlink_*.control.bin<br/>(hardlink groups)
    CTRL->>CTRL: Write mtime_*.control.bin<br/>(all files with changed mtime)

    Note over JOB: Phase 3 proceeds as normal<br/>but with fewer entries to copy
```

## AIO Pipeline Data Flow (Remote Targets)

When the target is remote (NFS or SMB), the backup uses the AIO async pipeline:

```mermaid
sequenceDiagram
    participant PLAN as Copy Plan
    participant FCB as FileControlBlock
    participant READER as SourceReader<br/>(LocalSource/NfsSource)
    participant BLOCK as CopyBlock
    participant WRITER as TargetWriter<br/>(NfsTarget/SmbTarget)
    participant TARGET as Remote Filesystem

    PLAN->>FCB: produce CopyPlanEntry::File
    FCB->>FCB: from_fcb() -> CopyBlock
    FCB->>READER: read_block(CopyBlock)
    READER->>READER: Read chunk from source
    READER-->>BLOCK: CopyBlock {data, offsets updated}
    BLOCK->>WRITER: write_block(CopyBlock)
    WRITER->>TARGET: Write chunk to remote file
    WRITER-->>BLOCK: CopyBlock {dst_offset updated}

    alt More data to transfer
        BLOCK->>READER: read_block(CopyBlock)
        Note over READER,WRITER: Loop until read_complete && write_complete
    else Transfer complete
        BLOCK->>BLOCK: read_complete() && write_complete()
        Note over BLOCK: File done
    end
```

## Aggregation Flow

When aggregation is enabled, small files are packed into aggregate blobs instead of being written individually:

```mermaid
sequenceDiagram
    participant PLAN as Copy Plan
    participant AGG as Aggregate Writer
    participant BLOB as Aggregate Blob<br/>(A_REPO/)
    participant INDEX as Aggregate Index

    PLAN->>PLAN: For each FileCopyPlan
    alt size < aggregate_file_threshold
        PLAN->>AGG: Aggregate {meta, src_path}
        AGG->>AGG: Read file content
        AGG->>BLOB: Append to current blob
        AGG->>INDEX: Record (blob_id, offset, size)
        Note over AGG: When blob reaches<br/>max_aggregate_blob_size:<br/>finalize and start new blob
    else size >= threshold
        PLAN->>PLAN: Direct {meta, src_path, dst_path}
        Note over PLAN: Written normally to D_REPO
    end

    AGG->>BLOB: Finalize last blob
    AGG->>INDEX: Write aggregate index
```
