---
sidebar_position: 3
title: 安装
---

# 安装

本指南涵盖从源代码构建 fpt-rs 的构建要求、cargo 特性标志和平台特定说明。

## 构建要求

| 要求 | 最低版本 | 说明 |
|---|---|---|
| Rust 工具链 | 1.70+ | 通过 [rustup](https://rustup.rs/) 安装 |
| C 编译器 | GCC 或 Clang | `rusqlite` 捆绑的 SQLite 构建需要 |
| (仅 Linux) | libacl 头文件 | ACL 扫描支持（`exacl` crate） |
| (NFS) | 网络访问 | 可达的 NFS v3 服务器 |
| (SMB) | 网络访问 | 可达的 SMB/CIFS 服务器 |

在 Debian/Ubuntu 上安装系统依赖：

```bash
sudo apt update
sudo apt install build-essential libacl1-dev
```

## Cargo 构建变体

### 默认构建（仅本地）

```bash
cargo build --release
```

### 启用 NFS 支持

```bash
cargo build --release --features nfs
```

### 启用 SMB 支持

```bash
cargo build --release --features smb
```

### 所有传输

```bash
cargo build --release --features "nfs smb"
```

## 特性标志参考

| 特性 | 默认 | 依赖 | 启用内容 |
|---|---|---|---|
| `sqlite` | 是 | `rusqlite`（捆绑） | 基于 SQLite 的元数据和缓存存储 |
| `nfs` | 否 | `nfs3_client`、`async-channel` | NFS v3 扫描器、备份和恢复 |
| `smb` | 否 | `smb_client`、`sspi` | SMB/CIFS 扫描器、备份和恢复 |

## 平台说明

### Linux

Linux 是主要开发平台。所有功能完全支持。

### Windows

Windows 构建使用 `windows` crate 进行原生文件操作。

## 验证构建

```bash
./target/release/fptcli --version
./target/release/fptcli --help
```
