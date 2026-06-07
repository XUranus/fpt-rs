---
sidebar_position: 5
title: metainspect 参考
description: metainspect 元数据检查工具的完整参考
---

# metainspect 参考

`metainspect` 是一个诊断工具，用于检查 fpt-rs 扫描器产生的二进制元数据和控制文件。它读取内部二进制格式并以制表符分隔、CSV 或 JSON 格式输出人类可读的记录。

## 概要

```text
metainspect [OPTIONS] [FILE]
```

:::tip 自动检测
当提供位置 `FILE` 参数时，metainspect 根据文件名模式自动检测文件类型。
:::

## 输入标志

| 标志 | 描述 |
|---|---|
| `FILE`（位置参数） | 带自动类型检测的输入文件路径 |
| `--meta <FILE>` | 检查元数据文件（例如 `meta_0_0.dat`） |
| `--dcache <FILE>` | 检查目录缓存文件（例如 `dcache_0.dat`） |
| `--fcache <FILE>` | 检查文件缓存文件（例如 `fcache_0.dat`） |
| `--control <FILE>` | 检查控制文件（例如 `copy_<hash>.control.bin`） |

### 文件类型检测

| 模式 | 检测类型 |
|---|---|
| `meta_*.dat` | 元数据 |
| `dcache_*.dat` | 目录缓存 |
| `fcache_*.dat` | 文件缓存 |
| `*.control.bin` | 控制文件 |

## 输出标志

| 标志 | 短标志 | 默认值 | 描述 |
|---|---|---|---|
| `--format <FMT>` | | `tab` | 输出格式：`json`、`csv`、`tab` |
| `--json` | | `false` | `--format json` 的快捷方式 |
| `--csv` | | `false` | `--format csv` 的快捷方式 |
| `--tab` | | `false` | `--format tab` 的快捷方式 |
| `--output <FILE>` | `-o` | | 输出文件（省略则为 stdout） |

## 示例

### 检查元数据文件

```bash
metainspect /tmp/fpt/meta/meta_0_0.dat
```

### 以 JSON 格式检查控制文件

```bash
metainspect --control /tmp/fpt/ctrl/copy_abc123.control.bin --json
```

### 自动检测文件类型

```bash
metainspect /tmp/fpt/meta/meta_0_0.dat        # 检测为 meta
metainspect /tmp/fpt/meta/dcache_0.dat         # 检测为 dcache
metainspect /tmp/fpt/ctrl/copy_abc.control.bin # 检测为 control
```

### 管道到 jq 进行过滤

```bash
metainspect --meta /tmp/fpt/meta/meta_0_0.dat --json | jq '.[] | select(.size > 1000000)'
```
