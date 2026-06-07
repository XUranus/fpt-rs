---
sidebar_position: 1
title: fptcli 参考
description: fptcli 备份和恢复 CLI 工具的完整参考
---

# fptcli 参考

`fptcli` 是用于创建备份副本和恢复数据的主要 CLI 工具。它支持本地、NFS 和 SMB 源和目标的统一接口。

**源文件：** `src/bin/fptcli.rs`

## 子命令

### `backup`

从源创建备份副本到目标。

```text
fptcli backup --data <PATH_OR_URL> --target <PATH_OR_URL> [OPTIONS]
```

#### 必需标志

| 标志 | 短标志 | 描述 |
|---|---|---|
| `--data <PATH_OR_URL>` | `-d` | 源数据路径或 URL |
| `--target <PATH_OR_URL>` | `-t` | 副本将被创建的目标路径 |

#### 源/目标格式

| 格式 | 示例 |
|---|---|
| 本地 | `/opt/dataset` |
| NFS | `nfs://127.0.0.1/opt/dataset?sub=/ds1` |
| SMB | `smb://127.0.0.1/share/root?username=u&password=p` |

#### 备份格式标志

| 标志 | 默认值 | 描述 |
|---|---|---|
| `--format <FORMAT>` | `common` | 备份格式：`common` 或 `aggregated` |
| `--aggregate` | `false` | `--format aggregated` 的快捷方式 |
| `--incremental-base <DIR>` | | 增量备份的先前副本 |
| `--blob-size <MB>` | `4` | 聚合 blob 大小（MB） |
| `--threshold <KB>` | `1024` | 聚合文件阈值（KB） |

#### 并发标志

| 标志 | 短标志 | 默认值 | 描述 |
|---|---|---|---|
| `--jobs <COUNT>` | `-j` | `4` | 最大并发子任务 |
| `--workers <COUNT>` | `-w` | `8` | 每子任务的工作线程数 |
| `--nfs-connections <COUNT>` | | `32` | 并行 NFS 连接 |
| `--smb-connections <COUNT>` | | `4` | SMB 客户端连接 |
| `--buffer-size <SIZE_KB>` | | `1024` | 每文件复制缓冲区（KB） |

#### 重试标志

| 标志 | 默认值 | 描述 |
|---|---|---|
| `--operation-retries` | `3` | 记录失败前的重试次数 |
| `--retry-delay-ms` | `1000` | 重试之间的延迟（毫秒） |
| `--retry-backoff` | `1.0` | 指数退避乘数 |
| `--retry-max-delay-ms` | `1000` | 最大重试延迟（毫秒） |
| `--retry-jitter` | `0.0` | 抖动比率（0.0..1.0） |

---

### `restore`

从备份副本恢复数据到目标。

```text
fptcli restore --copy <PATH_OR_URL> --target <PATH_OR_URL> [OPTIONS]
```

#### 恢复选项

| 标志 | 短标志 | 默认值 | 描述 |
|---|---|---|---|
| `--policy <POLICY>` | `-p` | `replace` | 恢复策略：`replace`、`skip`、`keep-newer` |
| `--jobs <COUNT>` | `-j` | `4` | 最大并发子任务 |
| `--workers <COUNT>` | `-w` | `8` | 每子任务的工作线程 |
| `--hardlinks` | | `false` | 恢复硬链接 |
| `--mtime` | | `true` | 恢复修改时间 |

## 恢复策略

| 策略 | 行为 |
|---|---|
| `replace` | 无条件覆盖现有文件 |
| `skip` | 跳过目标上已存在的文件 |
| `keep-newer` | 仅在源文件比目标更新时替换 |

## 示例

### 本地到本地备份

```bash
fptcli backup --data /opt/dataset --target /backup/dataset --jobs 4 --workers 8 -v
```

### NFS 源增量聚合备份

```bash
fptcli backup \
    --data "nfs://192.168.1.10/export?sub=/dataset1" \
    --target /backup/nfs_copy \
    --aggregate \
    --incremental-base /backup/previous_copy \
    --nfs-connections 64 -vv
```

### SMB 源到本地目标

```bash
fptcli backup \
    --data "smb://nas.local/data?username=backup&password=secret" \
    --target /backup/smb_data \
    --smb-connections 8 --hardlink --delete --mtime -v
```
