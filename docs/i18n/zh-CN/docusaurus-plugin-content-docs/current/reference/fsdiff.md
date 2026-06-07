---
sidebar_position: 4
title: fsdiff 参考
description: fsdiff 目录比较工具的完整参考
---

# fsdiff 参考

`fsdiff` 比较两个目录树并报告差异。它用于验证备份副本与源是否相同，或比较数据集的两个版本。

## 概要

```text
fsdiff --source <DIR> --target <DIR> [OPTIONS]
```

## 必需标志

| 标志 | 短标志 | 描述 |
|---|---|---|
| `--source <DIR>` | `-s` | 源目录路径 |
| `--target <DIR>` | `-t` | 目标目录路径 |

## 可选标志

| 标志 | 默认值 | 描述 |
|---|---|---|
| `--strip-source-prefix <PREFIX>` | | 比较时去除源路径前缀 |
| `--strip-target-prefix <PREFIX>` | | 比较时去除目标路径前缀 |
| `--follow-links` | `false` | 跟随符号链接 |
| `--compare-acl` | `false` | 比较 ACL（仅 Linux） |
| `--compare-xattrs` | `false` | 比较扩展属性（仅 Linux） |
| `--compare-mtime` | `false` | 比较目录修改时间 |
| `--verbose` | `false` | 打印每个正在比较的文件 |

## 比较逻辑

```mermaid
flowchart TD
    A[收集源文件] --> B[收集目标文件]
    B --> C[对于每个源条目]
    C --> D{存在于目标中?}
    D -->|否| E[仅在源中]
    D -->|是| F{都是符号链接?}
    F -->|是| G{相同目标?}
    G -->|否| H[符号链接不匹配]
    G -->|是| I[相同]
    F -->|否| J{相同大小?}
    J -->|否| K[大小不匹配]
    J -->|是| L{相同 SHA256?}
    L -->|否| M[校验和不匹配]
    L -->|是| I
```

## 退出代码

| 代码 | 含义 |
|---|---|
| 0 | 目录相同 |
| 1 | 发现差异 |

## 示例

### 基本比较

```bash
fsdiff --source /opt/dataset --target /backup/dataset
```

### 带路径前缀去除的比较

```bash
fsdiff --source /opt/dataset --target /backup/copy1/data --strip-target-prefix /data
```

### CI/CD 验证

```bash
if fsdiff --source "$SRC" --target "$DST"; then
    echo "备份验证成功"
else
    echo "备份验证失败"
    exit 1
fi
```
