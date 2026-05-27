# Runtime Memory Configuration

This document describes where Fpt currently uses memory during scan,
backup, and restore, and which runtime knobs control the peak memory footprint.

It documents the current implementation, not a future budget-based memory
controller.

## Overview

Memory is mainly consumed in four places:

1. scanner traversal and output queues
2. per-worker copy buffers in backup and restore
3. aggregate-mode blob assembly buffers
4. remote scan / remote copy transport buffers

The largest practical contributors today are usually:

- `workers * copy_buffer_size` during copy-heavy backup/restore
- large in-memory scanner queues when the filesystem tree is deep or very wide
- aggregate blob assembly buffers when aggregated backup is enabled

## Scanner Memory

### Traversal Queue

The scanner uses a spillable queue for pending directories.

Implementation:

- [src/scanner.rs](/home/xuranus/workspace/fpt/src/scanner.rs:56)
- [src/scanner/options.rs](/home/xuranus/workspace/fpt/src/scanner/options.rs:151)

Relevant settings:

- `memory_upper_bound`
- `memory_lower_bound`
- `spill_load_batch_size`

Current defaults:

```text
memory_upper_bound = 100000
memory_lower_bound = 50000
spill_load_batch_size = 20000
```

Meaning:

- when the in-memory pending-directory count grows past `memory_upper_bound`,
  extra queue items spill to disk
- when in-memory queue usage drops below `memory_lower_bound`, spill files are
  loaded back in batches of `spill_load_batch_size`

This is the main built-in scanner memory bound.

### Output Queue

Completed directory scan results are pushed into a bounded in-memory
`BlockingQueue`.

Implementation:

- [src/scanner.rs](/home/xuranus/workspace/fpt/src/scanner.rs:69)

Current bound:

```text
BlockingQueue<DirBatchScanResult>(1000)
```

This queue is bounded, but one item can still be large.

### Per-Directory Scan Result Size

The local traversal path currently accumulates one `DirBatchScanResult` per
directory and stores all file metadata for that directory in memory before
pushing it downstream.

Implementation:

- [src/scanner/engine/bio/traversal.rs](/home/xuranus/workspace/fpt/src/scanner/engine/bio/traversal.rs:82)

Important consequence:

- a single huge directory can consume significant memory even if the queue
  bounds are small

This is currently the weakest part of scan-side memory control.

### Scanner Worker Counts

The scanner has:

- `worker_count` for traversal
- `writer_count` for metadata serialization

Implementation:

- [src/scanner/options.rs](/home/xuranus/workspace/fpt/src/scanner/options.rs:35)
- [src/scanner/engine.rs](/home/xuranus/workspace/fpt/src/scanner/engine.rs:26)

Effects:

- higher `worker_count` increases the number of directories being expanded and
  metadata objects being produced concurrently
- higher `writer_count` increases metadata-writing parallelism, but also
  increases queue drain parallelism and open-file state

### SMB Scan Buffer

SMB scanning also uses a query-directory buffer.

Implementation:

- [src/scanner/options.rs](/home/xuranus/workspace/fpt/src/scanner/options.rs:58)

Default:

```text
smb_query_buffer_size = 8 MiB
```

Larger values reduce SMB query round-trips but increase memory used by SMB scan
workers.

## Backup / Restore Memory

### Local Common Copy

Local common backup allocates one copy buffer per copy worker thread.

Implementation:

- [src/backup/bio/local_copy.rs](/home/xuranus/workspace/fpt/src/backup/bio/local_copy.rs:56)
- [src/backup/local_executor.rs](/home/xuranus/workspace/fpt/src/backup/local_executor.rs:98)

The buffer size is:

```text
copy_buffer_size clamped to 256 KiB .. 4 MiB
```

Approximate memory:

```text
peak_local_copy_memory ~= worker_count * copy_buffer_size
```

There is also a bounded job queue:

- queue capacity is `worker_count * 2`
- jobs mostly hold metadata and paths, not full file contents

Implementation:

- [src/backup/bio/local_copy.rs](/home/xuranus/workspace/fpt/src/backup/bio/local_copy.rs:48)

### Generic Async NFS / SMB Copy

Remote-capable backup and restore paths use `copy_buffer_size` as the cap for
block reads and writes.

Implementation:

- [src/backup/aio/transport.rs](/home/xuranus/workspace/fpt/src/backup/aio/transport.rs:16)

Approximate memory:

```text
peak_remote_copy_memory ~= active_copy_tasks * copy_buffer_size
```

Plus:

- bounded entry channels, usually `256` entries
- transport state such as NFS connection pools or SMB client/session state

The channel entries are usually much smaller than file data buffers.

### Copy Buffer Runtime Knob

The main runtime knob for copy memory is:

- `--buffer-size` on `fptcli backup`
- `--buffer-size` on `fsbackup`

This value is specified in KiB and then clamped to:

```text
256 KiB .. 4 MiB
```

Implementation:

- [src/frame/backup_impls.rs](/home/xuranus/workspace/fpt/src/frame/backup_impls.rs:104)

## Aggregate Mode Memory

Aggregate mode can use much more memory than common mode because it assembles
blob files in memory before flushing them.

Implementation:

- [src/backup/aio/aggregation.rs](/home/xuranus/workspace/fpt/src/backup/aio/aggregation.rs:165)
- [src/backup/local_executor.rs](/home/xuranus/workspace/fpt/src/backup/local_executor.rs:93)

Relevant knobs:

- `--blob-size`
- `--threshold`

Defaults:

```text
max_aggregate_blob_size = 64 MiB
aggregate_file_threshold = 1 MiB
```

Effects:

- larger `--blob-size` can increase peak memory notably
- larger `--threshold` causes more files to be aggregated, which may increase
  blob staging pressure

## Practical Runtime Controls

### To Reduce Scanner Memory

Use smaller values for:

- `--workers`
- `--writers`
- `--smb-query-buffer-mb` for SMB-heavy scans

And when possible:

- use `--stats-only` if you only need scan statistics

Current limitation:

- spill queue bounds are not exposed as CLI flags yet, so the strongest scanner
  memory knob remains in-code configuration

### To Reduce Backup / Restore Memory

Use smaller values for:

- `--workers`
- `--buffer-size`
- `--smb-copy-tasks`
- `--smb-connections`
- `--nfs-connections`

This is the most effective way to cap runtime memory during backup/restore.

### To Reduce Aggregate-Mode Memory

Use smaller values for:

- `--blob-size`
- `--threshold`

If memory pressure matters more than aggregate packing efficiency, prefer common
format or a smaller aggregate blob size.

## Rule Of Thumb

For common backup/restore:

```text
peak_memory ~= scan_queues + (copy_workers_or_tasks * copy_buffer_size)
```

For aggregated backup:

```text
peak_memory ~= scan_queues + (copy_workers_or_tasks * copy_buffer_size) + aggregate_blob_buffers
```

Where:

- `scan_queues` depends on directory fanout and spill-queue limits
- `aggregate_blob_buffers` depends mainly on `--blob-size`

## Current Gaps

Fpt does not yet provide:

- one unified `--memory-limit` knob
- automatic derivation of workers/buffer sizes from a target memory budget
- scan-side chunking for very large single directories

If tighter memory control is needed, the next useful improvements are:

1. expose spill-queue bounds as CLI options
2. add scanner batch splitting for huge directories
3. add a budget-based controller that derives workers and buffer sizes from a
   configured memory cap
