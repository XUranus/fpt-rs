---
sidebar_position: 4
title: SMB 传输
description: fpt-rs 如何使用异步 smb_client crate 访问 SMB 共享
---

# SMB 传输

SMB 传输使 fpt-rs 能够使用异步 SMB2/3 客户端读取和写入 SMB/CIFS 文件共享。它支持 Windows 共享、Samba 和任何 SMB2 兼容服务器。

:::info 特性标志
SMB 传输受 `smb` Cargo 特性控制。使用 `--features smb` 构建以启用。
:::

## SMB URL 格式

```text
smb://host[:port]/share[/sub-path][?username=u&password=p]
```

| 组件 | 必需 | 描述 |
|-------------|----------|--------------------------------------------------|
| `host` | 是 | SMB 服务器主机名或 IP 地址 |
| `port` | 否 | SMB 端口（默认：445） |
| `share` | 是 | SMB 共享名称（例如 `backups`） |
| `sub-path` | 否 | 共享内的子路径 |
| `username` | 否 | 认证用户名 |
| `password` | 否 | 认证密码 |

## SmbClientPool

`SmbClientPool`（`src/smb/connection.rs`）使用轮询选择管理已认证 SMB 客户端连接的池。

```rust
pub struct SmbClientPool {
    clients: Vec<Arc<smb_client::Client>>,
    next: AtomicUsize,
}
```

每个 `connect_client()` 调用执行：连接 -> 协商 -> 认证 -> 树连接。

## SmbScanner

`SmbScanner`（`src/smb/scanner.rs`）使用 SMB2 的 **QueryDirectory** 操作遍历 SMB 共享。

SMB 扫描使用 `JoinSet` 进行结构化并发，与 NFS（使用共享工作队列）不同。

## 备份流水线

### SmbTarget（TargetWriter）

`SmbTarget` 实现 `TargetWriter` 向 SMB 共享写入数据。使用 `DirCache` 避免冗余的 MKDIR RPC。

### SMB 复制后阶段

| 阶段 | 使用的 SMB 操作 |
|-----------|-----------------------------------------------|
| 硬链接 | SMB2 IOCTL |
| 删除 | SMB2 SetInfo（处置删除）+ Close |
| 修改时间 | SMB2 SetInfo（基本信息）+ Close |

:::warning 安全注意事项
SMB URL 包含明文凭据。避免在日志中记录它们或将它们存储在 shell 历史中。
:::
