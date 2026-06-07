---
sidebar_position: 3
title: 数据流
description: fpt-rs 中备份、恢复和增量操作的端到端数据流，包含实际函数签名和结构体定义。
---

# 数据流

本文档追踪 fpt-rs 中三种主要操作的端到端数据流：**备份**、**恢复**和**增量备份**。序列图展示了 `DirBatchScanResult`、`FileControlBlock` 和 `CopyBlock` 如何在流水线中移动。

## 核心数据结构

### DirBatchScanResult

定义在 `src/scanner/models.rs:30`，这是基本的扫描输出单元：

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

定义在 `src/backup/fcb.rs:53`，FCB 是每个文件操作的核心状态机：

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

### CopyBlock

定义在 `src/backup/copy_block.rs:14`，这是在 `SourceReader` 和 `TargetWriter` 之间流动的传输单元：

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

## 备份流程

备份流程有四个阶段，由 `FileBackupJob::run()` 编排。

```mermaid
sequenceDiagram
    participant CLI as CLI / fptcli
    participant JOB as FileBackupJob
    participant PREREQ as BackupPrereqJob
    participant SCAN as ScanJob
    participant SCANNER as 传输扫描器<br/>(本地/NFS/SMB)
    participant META_W as 元数据写入器
    participant CTRL as 控制文件生成器
    participant SUBTASK as 子任务调度器
    participant EXECUTOR as 传输执行器<br/>(本地/NFS/SMB)
    participant POST as BackupPostJob

    CLI->>JOB: run()

    rect rgb(240, 248, 255)
        Note over JOB,PREREQ: 阶段 1 -- 前置条件
        JOB->>PREREQ: run_sync()
        PREREQ->>PREREQ: 验证源可访问性
        PREREQ->>PREREQ: 验证目标可访问性
        PREREQ-->>JOB: OK
    end

    rect rgb(240, 255, 240)
        Note over JOB,CTRL: 阶段 2 -- 扫描
        JOB->>SCAN: run()
        SCAN->>SCANNER: scan(root_path, ScanOption)
        loop 每个目录批次
            SCANNER->>SCANNER: 列出目录条目
            SCANNER->>SCANNER: stat() 每个条目
            SCANNER-->>META_W: DirBatchScanResult
            META_W->>META_W: 序列化 FileMeta 到 meta_*.dat
        end
        SCAN->>CTRL: generate_control_files()
        CTRL-->>SCAN: 控制文件已生成
        SCAN-->>JOB: ScanStats
    end

    rect rgb(255, 248, 240)
        Note over JOB,EXECUTOR: 阶段 3 -- 子任务
        JOB->>SUBTASK: spawn_and_join_subtasks()
        loop 每个控制文件（并行子任务）
            SUBTASK->>EXECUTOR: execute_backup(control_file)
            EXECUTOR->>EXECUTOR: 读取控制文件条目
            EXECUTOR->>EXECUTOR: produce_copy_plan()
            EXECUTOR->>EXECUTOR: 复制文件
            EXECUTOR->>EXECUTOR: run_all_phases()
        end
    end

    rect rgb(248, 240, 255)
        Note over JOB,POST: 阶段 4 -- 后置作业
        JOB->>POST: run()
        POST->>POST: 写入 manifest.json
    end

    JOB-->>CLI: JobResult
```

### 入口点：BackupTask::start()

备份入口在 `src/backup.rs:301`。它检查源和目标来选择流水线：

```rust
pub fn start(self) -> Result<RunningBackup, BackupError> {
    if !self.option.source.is_local() || !self.option.target.is_local() {
        // AIO 路径：用于任何涉及远程的组合
        let params = BackupPipelineParams { /* ... */ };
        let terminate_handle = spawn_backup(
            self.option.source.clone(), self.option.target.clone(),
            params, Arc::clone(&terminate_indicator),
        );
        return Ok(Self::running_backup(...));
    }
    // BIO 路径：本地到本地使用阻塞线程
    let terminate_handle = spawn_local_backup_pipeline(/* ... */);
    Ok(Self::running_backup(...))
}
```

## 恢复流程

恢复流程从备份副本读取数据并写入恢复目标。通用恢复流水线签名在 `src/backup/restore_pipeline.rs:155`：

```rust
pub async fn run_restore_copy_pipeline<T, R>(
    control_file: PathBuf,
    meta_dir: PathBuf,
    source: LocalRepoRestoreSource,
    target: T,
    restore_ops: R,
    policy: RestorePolicy,
    stats: Arc<Mutex<RestoreStats>>,
    max_concurrent_tasks: usize,
) where
    T: TargetWriter,
    R: RestoreOps + Clone + Send + Sync + 'static,
```

`RestorePolicy` 枚举（`src/backup.rs:446`）控制如何处理现有文件：

```rust
pub enum RestorePolicy {
    Replace,    // 始终覆盖
    Skip,       // 如果目标存在则跳过
    KeepNewer,  // 仅在源更新时恢复
}
```

## 增量备份流程

增量备份复用与全量备份相同的流水线，在阶段 2 有一个关键差异：差异引擎比较当前和之前的元数据，仅产生包含新/更改/删除条目的增量控制文件。

```mermaid
sequenceDiagram
    participant JOB as FileBackupJob
    participant SCAN as ScanJob
    participant DIFF as 差异引擎
    participant CTRL as 控制文件生成器

    Note over JOB: incremental_base = Some(previous_copy_root)
    JOB->>SCAN: run()
    SCAN->>SCAN: 扫描当前源
    SCAN-->>JOB: 当前扫描完成

    DIFF->>DIFF: 读取当前 M_REPO/meta/*.dat
    DIFF->>DIFF: 读取之前 M_REPO/meta/*.dat
    DIFF->>DIFF: 比较 FileMeta 记录
    DIFF->>DIFF: 分类每个文件：NEW/CHANGED/DELETED/UNCHANGED

    CTRL->>CTRL: 写入 copy_*.control.bin（仅 NEW + CHANGED）
    CTRL->>CTRL: 写入 delete_*.control.bin（DELETED）
```
