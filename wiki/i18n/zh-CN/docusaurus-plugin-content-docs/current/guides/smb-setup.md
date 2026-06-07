---
sidebar_position: 6
title: SMB 设置
---

# SMB 设置

本指南涵盖配置 SMB/CIFS 共享、使用 fptcli 通过 `smb://` URL 方案备份和恢复数据，以及使用 `smbprobe` 诊断连接问题。

:::info
SMB 支持需要使用 `smb` 特性标志构建：`cargo build --release --features smb`
:::

## 前提条件

- SMB/CIFS 服务器（Linux 上的 Samba、Windows 文件共享或 NAS 设备）。
- fpt-rs 使用 `--features smb` 构建。

## 步骤 1：设置 SMB 共享

```bash
sudo apt update
sudo apt install samba

sudo mkdir -p /srv/samba/share
sudo chmod 0777 /srv/samba/share
```

编辑 `/etc/samba/smb.conf`：

```ini
[share]
   path = /srv/samba/share
   browsable = yes
   writable = yes
   guest ok = no
   valid users = backupuser
```

```bash
sudo smbpasswd -a backupuser
sudo systemctl restart smbd
```

## 步骤 2：使用 smbprobe 诊断

```bash
./target/release/smbprobe \
  --target "smb://192.168.1.200/share?username=backupuser&password=secret"
```

## 步骤 3：从 SMB 源备份

```bash
./target/release/fptcli backup \
  --data "smb://192.168.1.200/share/projects?username=backupuser&password=secret" \
  --target /tmp/smb-backup \
  --smb-connections 4 \
  -v
```

## 步骤 4：从 SMB 备份恢复

```bash
./target/release/fptcli restore \
  --copy "smb://192.168.1.200/backups/COPY_COMMON_FULL_<timestamp>?username=backupuser&password=secret" \
  --target /tmp/restored \
  -v
```

## 安全注意事项

:::warning
SMB URL 包含明文凭据。避免在日志中记录它们或存储在 shell 历史中。
:::

建议使用环境变量：

```bash
export SMB_USER=backupuser
export SMB_PASS=secret

./target/release/fptcli backup \
  --data "smb://192.168.1.200/share?username=${SMB_USER}&password=${SMB_PASS}" \
  --target /tmp/smb-backup
```

## 故障排除

| 问题 | 原因 | 解决方案 |
|---|---|---|
| 认证失败 | 用户名/密码错误 | 检查 `smbpasswd` 和 `smb.conf` 设置 |
| 共享未找到 | 共享名称不匹配 | 确保 URL 中的共享名称与配置匹配 |
| 连接超时 | 防火墙阻止 | 检查端口 445 和 139 的防火墙规则 |
| 访问被拒绝 | 用户权限不足 | 检查服务器上的共享和文件系统权限 |
