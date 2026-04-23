# Code Organization

This document records the current module layout and the style rules used to keep new code readable.

## High-Level Layout

Core library modules live under `src/`:

- `scanner/`: traversal, metadata generation, path filters, cache/control generation
- `backup/`: copy-plan execution, local/remote transfer adapters, aggregate backup/restore
- `frame/`: job orchestration, prerequisites, repo layout, lifecycle wrappers
- `nfs/`: NFS connection pool, scanner, reader/writer/phase helpers
- `smb/`: SMB connection helpers, scanner, reader/writer/phase helpers
- `failure.rs`: retry policy and structured failure records
- `logging.rs`: routed log output
- `utility/`: generic queues and small shared data structures

CLI binaries live under `src/bin/`:

- `fptcli`: integrated backup/restore workflow
- `fsscan`: scan-only workflow
- `fsbackup`: execute backup from existing control files
- `fsdiff`: compare trees
- `fptserver`: process-supervised RPC/REST task server

## Parameter Grouping

Avoid adding functions with long positional parameter lists, especially when several arguments are always passed together.

Preferred patterns:

- Use a small `Config` or `Args` struct for CLI/build-time inputs.
- Use a `Runtime` or `Context` struct for shared handles like pools, semaphores, channels, stats, and retry/failure settings.
- Use a `Request` struct for one operation's data, such as "scan this directory".
- Use named boolean groups instead of repeated boolean arguments.

Examples currently used:

- `src/bin/fsscan.rs`
  - `PathFilterArgs` groups scanner include/exclude pattern flags.
  - `RetryArgs` groups retry-related CLI flags and converts them into `RetryPolicy`.

- `src/backup.rs`
  - `PhaseFlags` groups post-copy phase booleans (`hardlink`, `delete`, `mtime`) so callers do not pass multiple adjacent boolean arguments.

- `src/nfs/scanner.rs`
  - `NfsScanShared` holds immutable scan settings shared by every worker.
  - `NfsWorkerChannels` groups the worker queues and progress counter.
  - `NfsWorkerRuntime` is the single parameter passed into each worker task.
  - `NfsDirScan` is the single parameter used for scanning one directory.

## When To Split Files

Split a file when one of these is true:

- it contains multiple independently testable concepts
- it mixes transport-specific logic with generic pipeline logic
- a reader must scroll through unrelated code to understand one workflow
- the file is growing because several directions or modes were added

Do not split files just to create tiny one-function modules. Prefer cohesive modules with a clear owner.

## Transport Boundary

Transport-specific code should stay below its transport module:

- local filesystem: `backup/local_*`, `backup/bio/*`, scanner local traversal
- NFS: `nfs/*`
- SMB: `smb/*`

Generic orchestration should not know transport internals. It should work with:

- `DataLocation`
- scanner/backup/restore config structs
- copy-plan and transfer adapter traits
- job lifecycle traits

## Scanner Conventions

Scanner hot paths should keep disabled features cheap:

- optional features should be represented by `Option<T>` or a compact flag checked once near the hot loop
- expensive derived data, such as logical filter paths, should only be built when the feature is enabled
- traversal pruning should happen before expensive per-entry metadata queries where possible

## Current Pipeline Map

The main data path is:

```text
DataLocation
  -> ScanJob / Scanner
  -> M_REPO metadata + generated control plans
  -> BackupJob or RestoreJob
  -> copy-plan layer
  -> local/NFS/SMB transport adapters
```

Restore uses metadata-driven control-plan generation. It does not depend on
the original backup-time control files as authoritative restore input.

## Review Checklist

Before adding new public or cross-module functions:

- If the function has more than about 5 parameters, consider a struct.
- If it has adjacent booleans, consider a named flags struct.
- If several call sites pass the same argument group, name the group.
- If a transport-specific parameter reaches generic orchestration code, consider moving it behind an adapter.
