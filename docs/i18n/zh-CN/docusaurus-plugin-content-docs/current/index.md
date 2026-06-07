---
slug: /
sidebar_position: 1
title: Fpt 备份引擎文档
---

# Fpt 备份引擎

欢迎阅读 **Fpt** 文档。Fpt 是一款用 Rust 编写的高性能、跨平台文件备份与恢复系统。

## 快速链接

- **[快速开始](./guides/quick-start)** -- 构建并运行你的第一次备份
- **[架构概览](./architecture/overview)** -- Fpt 的内部工作原理
- **[CLI 参考](./reference/fptcli)** -- 完整的命令行参考
- **[传输引擎](./transports/overview)** -- 本地、NFS 和 SMB 传输

## 核心特性

- **多传输协议**：本地文件系统、NFS 和 SMB 均可作为源和目标
- **聚合备份**：将小文件打包为 blob 文件以提高效率
- **增量备份**：仅备份已更改的文件
- **硬链接保留**：检测并保留硬链接
- **四阶段流水线**：复制、硬链接、删除、修改时间阶段
- **跨平台**：支持 Linux 和 Windows
