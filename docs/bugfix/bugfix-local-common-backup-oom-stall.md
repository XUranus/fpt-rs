# Local Common Backup OOM/Stall Fix

## Symptom

Backing up a large local fileset to a local target with the common format could either be killed by the kernel OOM killer or stop making progress near the copy phase.

Example workload:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/ds3 \
  --target /opt/dataset/out \
  --temp-dir /opt/target/work \
  --format common
```

`/opt/dataset/ds3` was a roughly 20 GiB fileset made of 4 MiB files. The OOM killer reported multi-GiB anonymous RSS for `fptcli`.

## Root Cause

The old local blocking-I/O copy path moved whole-file `FileControlBlock` values through a multi-stage reader/writer queue graph. For large filesets this had two failure modes:

- Queues could accumulate many fully-read file buffers, causing high anonymous memory usage.
- Attempts to add bounded queues exposed shutdown and cyclic queue-dependency stalls.

This design differed from the DPA POSIX backup engine, which processes bounded blocks rather than circulating whole-file payload objects through multiple queues.

## Fix

Non-aggregated local-to-local common backup now uses a simpler bounded block pipeline:

- A producer reads the control file and creates target directories.
- File jobs are sent through a bounded queue.
- Each worker copies one file end-to-end using a reusable bounded buffer.
- The default copy buffer is 1 MiB and can be configured with `--buffer-size`.

This removes whole-file payload circulation from the common local copy path and keeps memory bounded by roughly `workers * buffer_size` plus metadata overhead.

## Scope

This fix applies to local source, local target, common-format backup.

Aggregated local backup and remote transports have separate copy paths. Remote transports already perform chunked I/O through their adapters; `--buffer-size` is being extended as a cap for those paths separately.
