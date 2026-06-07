---
slug: /
sidebar_position: 1
title: Fpt Backup Engine Documentation
---

# Fpt Backup Engine

Welcome to the **Fpt** documentation. Fpt is a high-performance, cross-platform file backup and recovery system written in Rust.

## Quick Links

- **[Getting Started](./guides/quick-start)** — Build and run your first backup
- **[Architecture Overview](./architecture/overview)** — How Fpt works under the hood
- **[CLI Reference](./reference/fptcli)** — Complete command reference
- **[Transport Engines](./transports/overview)** — Local, NFS, and SMB transports

## Key Features

- **Multi-transport**: Local filesystem, NFS, and SMB as source and target
- **Aggregate backup**: Packs small files into blob files for efficiency
- **Incremental backup**: Only backs up changed files
- **Hardlink preservation**: Detects and preserves hardlinks
- **4-phase pipeline**: Copy, hardlink, delete, mtime phases
- **Cross-platform**: Linux and Windows support
