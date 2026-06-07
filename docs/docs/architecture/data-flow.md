---
sidebar_position: 3
title: Data Flow
description: End-to-end data flow for backup, restore, and incremental operations in fpt-rs, with actual function signatures and struct definitions.
---

# Data Flow

This document traces the end-to-end data flow through fpt-rs for the three primary operations: **backup**, **restore**, and **incremental backup**. Sequence diagrams show how `DirBatchScanResult`, `FileControlBlock`, and `CopyBlock` move through the pipeline, with actual code references.

## Core Data Structures

Before examining the flows, here is how the key data structures relate. All definitions are from the actual source code.

```mermaid
graph TD
    DBSR["DirBatchScanResult<br/>(src/scanner/models.rs:30)<br/>{dir: DirMeta,<br/>files: Vec&lt;FileMeta&gt;,<br/>partial: bool,<br/>complete: bool}"]

    META_FILE["meta_*.dat<br/>(binary: FileMeta entries)"]
    META_DIR["dcache_*.dat<br/>(binary: DirCacheEntry entries)"]
    CTRL_FILE["copy_*.control.bin<br/>(binary: ControlFileEntry entries)"]

    FCB["FileControlBlock<br/>(src/backup/fcb.rs:53)<br/>{meta: Box&lt;FileMeta&gt;,<br/>buffer: Vec&lt;u8&gt;,<br/>src_state, dst_state,<br/>src_path, dst_path,<br/>src_offset, dst_offset}"]

    DCB["DirControlBlock<br/>(src/backup/fcb.rs:79)<br/>{meta: Box&lt;DirMeta&gt;,<br/>src_path, dst_path}"]

    COPY_PLAN["CopyPlanEntry<br/>(src/backup/copy_plan.rs:7)<br/>Directory {meta, dst_path}<br/>File(FileCopyPlan)"]

    COPY_BLOCK["CopyBlock<br/>(src/backup/copy_block.rs:14)<br/>{meta: Arc&lt;FileMeta&gt;,<br/>src_path, dst_path,<br/>src_offset, dst_offset,<br/>file_size, data, is_last}"]

    DBSR -->|"metadata writers<br/>serialize"| META_FILE
    DBSR -->|"metadata writers<br/>serialize"| META_DIR
    META_FILE -->|"control plan<br/>generates"| CTRL_FILE
    META_DIR -->|"control plan<br/>references"| CTRL_FILE

    CTRL_FILE -->|"entry reader<br/>deserializes"| FCB
    CTRL_FILE -->|"entry reader<br/>deserializes"| DCB
    FCB -->|"copy plan<br/>produces"| COPY_PLAN
    DCB -->|"copy plan<br/>produces"| COPY_PLAN
    FCB -->|"CopyBlock::from_fcb()<br/>converts"| COPY_BLOCK
```

### DirBatchScanResult

Defined at `src/scanner/models.rs:30`, this is the fundamental scan output unit:

```rust
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct DirBatchScanResult {
    pub dir: DirMeta,
    pub files: Vec<FileMeta>,
    pub partial: bool,
    pub complete: bool,
}
```

### FileControlBlock

Defined at `src/backup/fcb.rs:53`, the FCB is the central state machine for each file operation. It carries all state needed for a single file's backup or restore:

```rust
pub struct FileControlBlock {
    pub meta: Box<FileMeta>,
    pub buffer: Vec<u8>,
    pub buffer_len: usize,
    pub src_state: SourceHandleState,
    pub dst_state: TargetHandleState,
    pub src_path: PathBuf,
    pub dst_path: PathBuf,
    pub src_offset: u64,
    pub dst_offset: u64,
}
```

The two state machines (`src/backup/fcb.rs:28-44`):

```rust
pub enum SourceHandleState { Inited, Read, PartialRead }
pub enum TargetHandleState { Inited, PartialWritten, Written }
```

### CopyBlock

Defined at `src/backup/copy_block.rs:14`, this is the transfer unit that flows between `SourceReader` and `TargetWriter`:

```rust
#[derive(Debug, Clone)]
pub struct CopyBlock {
    pub meta: Arc<FileMeta>,
    pub src_path: PathBuf,
    pub dst_path: PathBuf,
    pub src_offset: u64,
    pub dst_offset: u64,
    pub file_size: u64,
    pub data: Vec<u8>,
    pub is_last: bool,
}
```

Conversion between FCB and CopyBlock (`src/backup/copy_block.rs:26`):

```rust
impl CopyBlock {
    pub fn from_fcb(fcb: FileControlBlock) -> Self {
        let meta = Arc::new((*fcb.meta).clone());
        let file_size = meta.size;
        Self {
            meta, src_path: fcb.src_path, dst_path: fcb.dst_path,
            src_offset: fcb.src_offset, dst_offset: fcb.dst_offset,
            file_size, data: fcb.buffer, is_last: fcb.src_offset >= file_size,
        }
    }

    pub fn read_complete(&self) -> bool { self.src_offset >= self.file_size }
    pub fn write_complete(&self) -> bool { self.dst_offset >= self.file_size }
    pub fn clear_data(&mut self) { self.data.clear(); }
}
```

### CopyPlanEntry

Defined at `src/backup/copy_plan.rs:7`:

```rust
pub(crate) enum CopyPlanEntry {
    Directory { meta: DirMeta, dst_path: PathBuf },
    File(FileCopyPlan),
}

pub(crate) enum FileCopyPlan {
    Direct { meta: FileMeta, src_path: PathBuf, dst_path: PathBuf },
    Aggregate { meta: FileMeta, src_path: PathBuf },
}
```

## Backup Flow

The backup flow has four phases, orchestrated by `FileBackupJob::run()`.

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

### Entry Point: BackupTask::start()

The backup entry point is `BackupTask::start()` at `src/backup.rs:301`. It inspects the source and target to select the pipeline:

```rust
pub fn start(self) -> Result<RunningBackup, BackupError> {
    let source_dir_base = self.option.source.base_path();
    let target_dir_base = self.option.target.base_path();
    // ...
    if !self.option.source.is_local() || !self.option.target.is_local() {
        // AIO path: generic orchestrator for any remote-involved direction
        let params = BackupPipelineParams { control_file, meta_dir, ctrl_dir, /* ... */ };
        let terminate_handle = spawn_backup(
            self.option.source.clone(), self.option.target.clone(),
            params, Arc::clone(&terminate_indicator),
        );
        return Ok(Self::running_backup(self.option, stats, terminate_handle, terminate_indicator));
    }
    // BIO path: local-to-local uses blocking threads
    let terminate_handle = spawn_local_backup_pipeline(/* ... */);
    Ok(Self::running_backup(self.option, stats, terminate_handle, terminate_indicator))
}
```

### The AIO Orchestrator

`spawn_backup()` at `src/backup/aio/orchestrator.rs:50` is the generic entry point for all remote-involved backups:

```rust
pub fn spawn_backup(
    source_location: DataLocation,
    target_location: DataLocation,
    params: BackupPipelineParams,
    terminate_indicator: Arc<AtomicBool>,
) -> thread::JoinHandle<()>
```

The internal `run_backup()` function (`src/backup/aio/orchestrator.rs:81`) follows four steps:

```rust
async fn run_backup(source_location, target_location, params) -> Result<(), String> {
    // 1. Connect source
    let source = BackupSource::connect(&source_location, /* ... */).await?;
    // 2. Connect target
    let target = BackupTarget::connect(&target_location, /* ... */).await?;
    // 3. Run copy pipeline (dispatches to the correct source+target combo)
    run_copy_for_source_target(&source, &target, &params).await;
    // 4. Run post-copy phases
    target.run_post_copy_phases(&params.ctrl_dir, &params.source_dir_base,
        &params.target_prefix, params.phase_flags, params.retry_policy,
        params.failure_recorder.as_ref()).await;
    Ok(())
}
```

### DirBatchScanResult Flow

The `DirBatchScanResult` flows through a multi-stage pipeline. The AIO scan scaffolding is at `src/scanner/engine/aio.rs:60`:

```rust
pub async fn run_aio_scan<S>(scanner: S, scan_option: ScanOption) -> Result<AioScanResult, String>
where
    S: AsyncDirScanner,
{
    let output_queue = Arc::new(BlockingQueue::<DirBatchScanResult>::new(DEFAULT_SCAN_QUEUE_CAPACITY));
    let stats = Arc::new(ScanStatistics::default());
    // ...
    // Start metadata writers (they drain output_queue synchronously)
    let writer_handles = start_meta_writers(&context, writer_count, None);
    // Spawn the async scanner
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DirBatchScanResult>(256);
    let scan_handle = tokio::spawn(async move { scanner.scan(scan_opt_for_task, tx).await });
    // Bridge: tokio mpsc -> BlockingQueue
    while let Some(batch) = rx.recv().await {
        let _ = oq.push(batch);
        // update stats...
    }
    // Wait for scanner, close queue, join writers
    // Generate control files
    engine::generate_control_files(&scan_opt_arc)?;
    Ok(AioScanResult { total_files, total_dirs, total_size, failed_files, failed_dirs })
}
```

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

The restore flow reads from a backup copy and writes to a restore target. The generic restore pipeline signature (`src/backup/restore_pipeline.rs:155`):

```rust
pub async fn run_restore_copy_pipeline<T, R>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    original_source_base: PathBuf,
    source: LocalRepoRestoreSource,
    target: T,
    restore_ops: R,
    target_local_base: Option<PathBuf>,
    policy: RestorePolicy,
    stats: Arc<Mutex<RestoreStats>>,
    log_prefix: &'static str,
    max_concurrent_tasks: usize,
) where
    T: TargetWriter,
    R: RestoreOps + Clone + Send + Sync + 'static,
```

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

The `run_restore_copy_pipeline()` function uses a producer-consumer pattern with tokio channels. The key loop from `src/backup/restore_pipeline.rs:282`:

```rust
loop {
    block = match source2.read_block(block).await {
        Ok(block) => block,
        Err((failed_block, msg)) => {
            error!("{log_prefix}: read {:?}: {msg}", failed_block.src_path);
            stats2.lock().unwrap().files_failed += 1;
            return;
        }
    };
    block = match target2.write_block(block).await {
        Ok(block) => block,
        Err((failed_block, msg)) => {
            error!("{log_prefix}: write {:?}: {msg}", failed_block.dst_path);
            stats2.lock().unwrap().files_failed += 1;
            return;
        }
    };
    if block.read_complete() && block.write_complete() {
        restore_ops2.restore_metadata(&restore_full_path, &block.meta.common);
        stats2.lock().unwrap().files_restored += 1;
        break;
    }
    block.clear_data();
}
```

The `RestorePolicy` enum (`src/backup.rs:446`) controls how existing files are handled:

```rust
pub enum RestorePolicy {
    Replace,    // Always overwrite
    Skip,       // Skip if target exists
    KeepNewer,  // Only restore if source is newer
}
```

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
        SKIP_CHECK["should_skip_restore()?<br/>(RestorePolicy check)"]
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

When the target is remote (NFS or SMB), the backup uses the AIO async pipeline. The orchestrator dispatches to the correct pipeline based on source+target combination (`src/backup/aio/orchestrator.rs:134`):

```rust
async fn run_copy_for_source_target(source: &BackupSource, target: &BackupTarget, params: &BackupPipelineParams) {
    match (source, target) {
        (BackupSource::Local { .. }, BackupTarget::Local { .. }) => {
            unreachable!("local->local backup uses the BIO pipeline, not AIO");
        }
        #[cfg(feature = "nfs")]
        (BackupSource::Local { source_dir_base }, BackupTarget::Nfs { pool }) => {
            crate::nfs::backup::pipeline::run_local_to_nfs(/* ... */).await;
        }
        #[cfg(feature = "smb")]
        (BackupSource::Local { source_dir_base }, BackupTarget::Smb { location, pool }) => {
            crate::smb::backup::pipeline::run_local_to_smb_copy_pipeline(/* ... */).await;
        }
        // ... NFS->Local, NFS->NFS, NFS->SMB, SMB->Local, SMB->SMB, SMB->NFS
    }
}
```

```mermaid
sequenceDiagram
    participant PLAN as Copy Plan
    participant FCB as FileControlBlock
    participant READER as SourceReader<br/>(LocalSource/NfsSource)
    participant BLOCK as CopyBlock
    participant WRITER as TargetWriter<br/>(NfsTarget/SmbTarget)
    participant TARGET as Remote Filesystem

    PLAN->>FCB: produce CopyPlanEntry::File
    FCB->>FCB: CopyBlock::from_fcb(fcb)
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
        Note over BLOCK: File done -- clear_data()
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
