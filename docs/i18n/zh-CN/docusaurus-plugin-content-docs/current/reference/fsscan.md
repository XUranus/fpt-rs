---
sidebar_position: 3
title: fsscan 参考
description: fsscan 文件系统扫描器工具的完整参考
---

# fsscan 参考

`fsscan` 是一个独立的文件系统扫描器，遍历一个或多个源路径并生成元数据和控制文件，供 `fsbackup` 或 `fptcli` 使用。

## 概要

```text
fsscan <PATH_OR_URL> [PATH_OR_URL ...] [OPTIONS]
```

## 参数

| 参数 | 描述 |
|---|---|
| `PATH_OR_URL` | 要扫描的源路径（可重复，至少 1 个） |

### 源格式

| 格式 | 示例 |
|---|---|
| 本地 | `/opt/dataset/ds2` |
| NFS | `nfs://127.0.0.1/opt/dataset?sub=/out` |
| SMB | `smb://127.0.0.1/share/root?username=u&password=p` |

## 标志

### 输出目录

| 标志 | 短标志 | 默认值 | 描述 |
|---|---|---|---|
| `--ctrl-dir <DIR>` | `-c` | `/tmp/fpt/ctrl` | 控制文件输出目录 |
| `--meta-dir <DIR>` | `-m` | `/tmp/fpt/meta` | 元数据输出目录 |
| `--temp-dir <DIR>` | `-t` | `/tmp/fpt/cache` | 溢出队列的临时目录 |

### 并发

| 标志 | 短标志 | 默认值 | 描述 |
|---|---|---|---|
| `--workers <COUNT>` | `-w` | `8` | 遍历工作线程 |
| `--writers <COUNT>` | `-W` | `1` | 元数据写入线程 |

### 扫描行为

| 标志 | 默认值 | 描述 |
|---|---|---|
| `--follow-symlinks` | `false` | 跟随符号链接 |
| `--scan-hidden` | `false` | 包含隐藏文件和目录 |
| `--max-depth <DEPTH>` | | 最大递归深度（无限） |
| `--scan-acl` | `false` | 扫描 ACL |
| `--scan-xattrs` | `false` | 扫描扩展属性 |
| `--scan-hardlinks` | `false` | 扫描和跟踪硬链接 |
| `--skip-block-devices` | `true` | 跳过块设备 |
| `--stats-only` | `false` | 仅打印统计信息 |

### NFS 标志

| 标志 | 默认值 | 描述 |
|---|---|---|
| `--nfs-connections <N>` | `32` | 并行 NFS 连接 |

### SMB 标志

| 标志 | 默认值 | 描述 |
|---|---|---|
| `--smb-query-buffer-mb <N>` | `8` | SMB 查询目录缓冲区大小（MiB） |

## 示例

### 扫描本地路径

```bash
fsscan /opt/dataset --ctrl-dir /tmp/fpt/ctrl --meta-dir /tmp/fpt/meta -v
```

### 扫描 NFS 导出

```bash
fsscan "nfs://192.168.1.10/export?sub=/dataset1" --nfs-connections 64 -vv
```

### 仅统计模式

```bash
fsscan /opt/dataset --stats-only -v
```
