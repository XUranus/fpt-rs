---
sidebar_position: 5
title: NFS 设置
---

# NFS 设置

本指南涵盖设置 NFS 服务器、导出共享、本地挂载以及使用 fptcli 通过 `nfs://` URL 方案备份和恢复数据。

:::info
NFS 支持需要使用 `nfs` 特性标志构建：`cargo build --release --features nfs`
:::

## 前提条件

- NFS v3 服务器（Linux `nfs-kernel-server` 或任何 NFS v3 兼容 NAS）。
- fpt-rs 主机和 NFS 服务器之间的网络连接。
- 使用 `--features nfs` 构建的 fpt-rs。

## 步骤 1：设置 NFS 服务器

```bash
sudo apt update
sudo apt install nfs-kernel-server

sudo mkdir -p /export/dataset
sudo chown nobody:nogroup /export/dataset
```

编辑 `/etc/exports`：

```
/export/dataset  192.168.1.0/24(rw,sync,no_subtree_check,no_root_squash)
```

```bash
sudo exportfs -ra
sudo systemctl restart nfs-kernel-server
```

## 步骤 2：从 NFS 源备份

```bash
./target/release/fptcli backup \
  --data "nfs://192.168.1.100/export/dataset" \
  --target /tmp/nfs-backup \
  -w 16 \
  --nfs-connections 32 \
  -v
```

## 步骤 3：从 NFS 备份恢复

```bash
./target/release/fptcli restore \
  --copy "nfs://192.168.1.100/export/backups/COPY_COMMON_FULL_<timestamp>" \
  --target /tmp/restored \
  -v
```

## 故障排除

| 问题 | 原因 | 解决方案 |
|---|---|---|
| 连接被拒绝 | NFS 服务器未运行 | 检查防火墙规则和端口 2049 |
| 权限被拒绝 | uid/gid 不匹配 | 检查 `/etc/exports` 中的导出选项 |
| 性能慢 | 连接数不足 | 增加 `--nfs-connections` 和 `-w` |
