# Scanner Optimization

This document records the current scanner optimization work that makes metadata
serialization and copy-control generation scale better with multiple writer
threads.

## What Changed

The scanner previously accepted `writer_count`, but the metadata writer path was
not truly partitioned:

- multiple writer threads all opened the same logical metadata repository root
- metadata file naming effectively started from `meta_0.dat` per writer
- `dcache` / `fcache` files were writer-local, but `meta_fid` and `fcache_fid`
  handling did not consistently reflect that ownership
- full scan still generated one monolithic `copy.txt`

The current implementation fixes that in two areas:

1. metadata writer sharding
2. copy-control sharding

## Metadata Writer Sharding

Each scanner writer now owns its own metadata shard.

Physical metadata files are named:

```text
meta_<writer_shard>_<segment>.dat
```

Examples:

```text
meta_0_0.dat
meta_0_1.dat
meta_1_0.dat
```

Notes:

- `writer_shard` is the metadata writer thread id
- `segment` is the rollover counter for that writer
- metadata records remain addressable through `(meta_fid, meta_offset)`
- `meta_fid` is still a single `u32`, but it now encodes `(writer_shard, segment)`

Current encoding:

```text
meta_fid = (writer_shard << 16) | segment
```

Readers decode that id to find the physical metadata file.

## Cache Ownership

`dcache` and `fcache` remain writer-local:

```text
dcache_<writer>.dat
fcache_<writer>.dat
```

The scanner now records the real writer-owned `fcache_fid` in `DirCacheEntry`
instead of hardcoding `0`.

That means:

- one directory batch is written by exactly one metadata writer
- that directory's `DirCacheEntry` points at the correct writer-local file cache
- later control-file generation can read shard-local metadata and caches without
  a compulsory merge step

## Copy Control Sharding

The scanner can now emit sharded copy control files:

```text
copy_00000000_0000.txt
copy_00000001_0000.txt
copy_0000000A_0001.txt
```

Behavior:

- shard id is derived from a deterministic hash of the directory path
- one directory and all of its file entries always stay in the same copy shard
- rollover can create multiple files for the same shard id
- backup already discovers all `copy_*.txt` files and runs them as parallel
  copy subtasks
- restore also discovers `copy_*.txt`

Current control-file codec notes:

- each control file starts with a fixed `4096` byte header
- the header is rewritten on `finish()` so final counts are accurate
- records after the header are binary length-prefixed payloads
- this avoids line-oriented parsing problems for paths containing spaces,
  newlines, carriage returns, or other special characters

## Full Scan Path

When scanner sharding is enabled:

- full scan writes sharded copy control files directly
- mtime remains a single `mtime.txt`
- hardlink and delete control files remain single-file today

## Incremental Scan Path

Incremental diff generation still builds the logical copy stream first, then the
scanner post-splits `copy.txt` into `copy_*.txt` when control sharding is
enabled.

This keeps the incremental refactor small while still unlocking parallel backup
subtasks.

## Why There Is No Mandatory Cache Merge

The scanner does not currently force a merge of `dcache_*` / `fcache_*` after
scan completion.

Reason:

- merge would add another heavy post-scan I/O phase
- readers already iterate all cache shards
- the main performance goal is to avoid serialization bottlenecks, not to
  compact outputs immediately

So the current design is:

- shard-native metadata
- shard-native cache files
- shard-aware control generation

If later profiling shows too many cache shards hurt diff/control generation,
compaction can be added as a separate optimization.

## Operational Result

This change is intended to unlock:

- real multi-writer metadata throughput
- lower scanner serialization contention
- multiple copy subtasks from one scan via `copy_*.txt`

It does not yet attempt:

- sharded `delete_*.txt`
- sharded `hardlink_*.txt`
- sharded `mtime_*.txt`
- cache compaction / merge
- scan-result reuse inside the backup engine
