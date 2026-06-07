---
sidebar_position: 3
title: NFS 传输
description: fpt-rs 如何通过直接 RPC 访问 NFSv3 导出，无需内核挂载
---

# NFS 传输

NFS 传输使 fpt-rs 能够使用 `nfs3_client` crate 通过直接 RPC 调用读取和写入 NFSv3 导出。无需内核 NFS 挂载 -- fpt-rs 完全在用户空间与 NFS 服务器通信。

:::info 特性标志
NFS 传输受 `nfs` Cargo 特性控制。使用 `--features nfs` 构建以启用。
:::

## NFS URL 格式

NFS 位置使用 `nfs://` 方案的 URL 指定：

```text
nfs://host[:port]/export[?sub=path][&uid=N][&gid=N]
```

| 组件 | 必需 | 描述 |
|-------------|----------|------------------------------------------------|
| `host` | 是 | NFS 服务器主机名或 IP 地址 |
| `port` | 否 | NFS 端口（默认：2049，mountd 自动检测） |
| `export` | 是 | NFS 导出路径（例如 `/data`） |
| `sub` | 否 | 导出内的子路径 |
| `uid` | 否 | AUTH_UNIX uid（默认：0） |
| `gid` | 否 | AUTH_UNIX gid（默认：0） |

## NfsConnectionPool

`NfsConnectionPool`（`src/nfs/connection.rs`）管理到 NFS 服务器的 TCP 连接池。因为 `Nfs3Connection` 每个 RPC 调用都需要 `&mut self`，单个连接本质上是顺序的。池维护 `connection_count` 个独立连接以实现并发。

```rust
pub struct NfsConnectionPool {
    connections: Vec<Mutex<PooledConnection>>,
    next: AtomicUsize,           // 轮询索引
    root_fh: nfs_fh3,            // 有效根文件句柄
    pub server_rtmax: u32,       // 服务器最大读取传输大小
    pub server_wtmax: u32,       // 服务器最大写入传输大小
}
```

## NfsScanner

`NfsScanner`（`src/nfs/scanner.rs`）使用 NFSv3 的 **READDIRPLUS** 操作遍历 NFS 目录，该操作在单个 RPC 中返回目录条目及其属性。

```rust
pub struct NfsScanner {
    pool: Arc<NfsConnectionPool>,
    sem: Arc<Semaphore>,         // 限制并发 READDIRPLUS RPC
    retry_policy: RetryPolicy,
    failure_recorder: Option<FailureRecorder>,
}
```

## 备份流水线

### NfsSource（SourceReader）

`NfsSource` 实现 `SourceReader` 从 NFS 读取数据。使用 `FileHandleCache` 避免对同一文件的重复 LOOKUP RPC。

### NfsTarget（TargetWriter）

`NfsTarget` 实现 `TargetWriter` 向 NFS 写入数据。`create_dir()` 使用 `DirHandleCache` 缓存目录句柄。

### 复制后阶段

| 阶段 | 使用的 NFS 操作 |
|-----------|----------------------------------|
| 硬链接 | LOOKUP + LINK RPC |
| 删除 | LOOKUP + REMOVE / RMDIR RPC |
| 修改时间 | LOOKUP + SETATTR RPC |
