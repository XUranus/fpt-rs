---
sidebar_position: 8
title: 日志
---

# 日志

fpt-rs 使用基于路由的日志系统，根据 Rust 模块路径将日志记录定向到特定文件。本指南涵盖日志详细级别、基于模块的路由、`--log-file` 标志和结构化失败日志。

## 日志格式

所有日志行遵循一致的格式：

```
2026-06-07 14:30:00 [INFO] fpt::frame::backup_job - copy phase started
```

## 详细级别

| 标志 | 级别 | 可见内容 |
|---|---|---|
| （无） | INFO | 高级进度：阶段转换、完成、错误 |
| `-v` | INFO | 与默认相同（显式） |
| `-vv` | DEBUG | 每文件操作、RPC 调用、元数据写入 |
| `-vvv` | TRACE | 完整内部状态 |

:::caution
TRACE 详细级别会产生大量输出。仅在诊断特定问题时使用，并始终与 `--log-file` 结合使用。
:::

## 基于模块的日志路由

路由逻辑在 `src/logging.rs` 中实现。`RoutingLogger` 根据模块路径前缀将日志记录定向到特定文件。

路由决策在 `RoutingLogger::log()` 中发生：

```rust
// src/logging.rs -- 简化版
fn log(&self, record: &log::Record) {
    let target = record.target();
    // 查找最具体（最长）的匹配路由
    for route in &st.routes {
        if target.starts_with(&route.prefix) {
            // 路由匹配 -> 仅写入路由文件
            write_to_file(route.file, line);
            break;
        }
    }
    // 始终写入 catch-all 文件（--log-file）
}
```

### 内置路由

| 模块前缀 | 目标文件 | 内容 |
|---|---|---|
| `fpt::scanner` | `C_REPO/logs/scan.log` | 扫描器遍历和元数据生成 |
| `fpt::frame` | `C_REPO/logs/frame.log` | 作业编排、子任务调度 |
| `fpt::backup` | `C_REPO/logs/subtask_{N}.log` | 每子任务的复制/硬链接/删除/修改时间操作 |

没有特定路由的记录（例如来自 CLI 二进制文件本身）输出到 stdout。

## `--log-file` 标志

`--log-file` 标志添加一个 catch-all 文件，接收**每条**日志记录，无论路由如何。

```bash
./target/release/fptcli backup \
  --data /source \
  --target /backup \
  --log-file /var/log/fpt/full-run.log \
  -vv
```

## 日志抑制

fpt-rs 自动抑制来自 SMB 库的嘈杂日志消息。抑制逻辑在 `src/logging.rs` 中：

```rust
fn should_suppress_record(record: &log::Record) -> bool {
    if record.target() != "smb::resource" { return false; }
    let msg = record.args().to_string();
    msg.starts_with("Error closing file:")
        && (msg.contains("Unexpected Message...")
            || msg.contains("Network Name Deleted..."))
}
```
