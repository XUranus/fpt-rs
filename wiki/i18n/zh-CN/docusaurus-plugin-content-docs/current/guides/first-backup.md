---
sidebar_position: 4
title: 首次备份演练
---

# 首次备份演练

本指南将带你完成一个完整的备份周期：创建真实的测试数据、运行备份、检查输出布局、恢复并验证正确性。我们还将介绍用于小文件密集型工作负载的聚合备份模式。

## 1. 创建测试数据

```bash
mkdir -p /tmp/fpt-walkthrough/source/{project/{src,docs},media,logs}

echo "fn main() { println!(\"hello\"); }" > /tmp/fpt-walkthrough/source/project/src/main.rs
echo "fn lib() -> i32 { 42 }" > /tmp/fpt-walkthrough/source/project/src/lib.rs
echo "# Project README" > /tmp/fpt-walkthrough/source/project/docs/readme.md

dd if=/dev/urandom of=/tmp/fpt-walkthrough/source/media/photo.jpg bs=1M count=5
ln -s photo.jpg /tmp/fpt-walkthrough/source/media/latest.jpg

echo "shared configuration data" > /tmp/fpt-walkthrough/source/project/config.ini
ln /tmp/fpt-walkthrough/source/project/config.ini \
   /tmp/fpt-walkthrough/source/project/config.backup.ini
```

## 2. 运行备份

```bash
./target/release/fptcli backup \
  --data /tmp/fpt-walkthrough/source \
  --target /tmp/fpt-walkthrough/target \
  --hardlink \
  -v
```

## 3. 检查备份布局

```bash
ls /tmp/fpt-walkthrough/target/
```

典型布局：

```
COPY_COMMON_FULL_<timestamp>/
  D_REPO/                    # 数据：镜像源树
  C_REPO/                    # 控制和日志文件
  M_REPO/                    # 元数据
```

## 4. 聚合备份模式

```bash
./target/release/fptcli backup \
  --data /tmp/fpt-walkthrough/source \
  --target /tmp/fpt-walkthrough/target-aggr \
  --aggregate \
  --blob-size 8 \
  --threshold 512 \
  -v
```

## 5. 增量备份

```bash
echo "updated content" >> /tmp/fpt-walkthrough/source/project/src/main.rs

./target/release/fptcli backup \
  --data /tmp/fpt-walkthrough/source \
  --target /tmp/fpt-walkthrough/target-incr \
  --aggregate \
  --incremental-base /tmp/fpt-walkthrough/target-aggr/COPY_AGGR_FULL_<timestamp> \
  -v
```

## 6. 恢复

```bash
./target/release/fptcli restore \
  --copy /tmp/fpt-walkthrough/target/COPY_COMMON_FULL_<timestamp> \
  --target /tmp/fpt-walkthrough/restored \
  --hardlinks \
  -v
```

## 7. 验证

```bash
./target/release/fsdiff \
  --source /tmp/fpt-walkthrough/source \
  --target /tmp/fpt-walkthrough/restored \
  --verbose
```

## 清理

```bash
rm -rf /tmp/fpt-walkthrough
```
