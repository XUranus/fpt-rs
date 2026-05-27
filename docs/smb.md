# SMB Design

This document records the current SMB design and implementation plan for Fpt.

## Feasibility Assessment

SMB support is feasible in the current codebase.

Reasons:

1. Fpt already has a split between local BIO paths and async remote paths.
2. `smb-rs` provides an async client with share connect and file read/write primitives.
3. The existing NFS scan bridge already proved that a remote async scanner can feed the existing local metadata/control-file writers.
4. The frame layer already uses `DataLocation` and runtime dispatch, so SMB can fit the same entry points.

Main risks:

- The current NFS implementation is still transport-specific in too many places.
- If SMB is added by copying the NFS shape directly, the repo will grow into 9 explicit source/target combinations for local/NFS/SMB.
- SMB paths carry credentials, so logging and manifests must not leak passwords.

Conclusion:

- Feasible: yes.
- Clean if implemented as transport traits plus direction orchestration: yes.
- Clean if implemented as another full copy of the NFS layout: no.

## Recommended Connect String

Preferred canonical form:

```text
smb://127.0.0.1/share_name/root/path?username=user&password=pass
```

Accepted compatibility form:

```text
smb:\\127.0.0.1\share_name\root\path?username=user?password=pass
```

Internally the parser normalizes the path portion to `/`.

## Current Scaffolding

The repository now has:

- feature-gated SMB transport support in `Cargo.toml`
- `src/smb/mod.rs` with `SmbLocation`
- SMB helpers for:
  - share-root UNC construction
  - root-path UNC construction
  - authenticated share/root accessibility validation via `smb-rs`
- `DataLocation::Smb(...)`
- `fptcli` path parsing for SMB connect strings
- SMB prerequisite connectivity validation in the frame layer
- `SmbFileScanner` wiring in the frame layer
- SMB async write helpers in `src/smb/aio/`
- `local -> SMB` backup support for:
  - direct `D_REPO` file copy during subtask execution
  - staged `M_REPO`, `C_REPO`, and `manifest.json` upload in post-job
- explicit "not implemented yet" errors for the remaining SMB backup/restore directions

The NFS AIO path was also refactored to reduce duplication before SMB copy
support is added:

- shared control-entry path mapping now lives in `src/backup/aio/entry.rs`
- shared local file helpers now live in `src/backup/aio/local_fs.rs`
- existing `local_to_nfs`, `nfs_to_local`, and `nfs_to_nfs` pipelines now reuse those helpers

This is intentional. It stabilizes the location/connectivity model and starts
the transport-oriented refactor before the remaining SMB runtime engines are added.

## Clean Architecture Plan

### Problem To Avoid

Today NFS backup uses direction-specific modules:

- `local_to_nfs`
- `nfs_to_local`
- `nfs_to_nfs`

That was manageable with one remote transport, but with SMB added the naive matrix becomes:

- local -> nfs
- nfs -> local
- nfs -> nfs
- local -> smb
- smb -> local
- smb -> smb
- nfs -> smb
- smb -> nfs

That is the wrong long-term shape.

### Recommended Split

Keep the orchestration separated into:

1. Source adapters
2. Target adapters
3. Phase orchestration

Proposed module direction:

```text
src/
  transport/
    mod.rs
    location.rs
    local/
    nfs/
    smb/
  backup/
    aio/
      mod.rs
      source/
        local.rs
        nfs.rs
        smb.rs
      target/
        local.rs
        nfs.rs
        smb.rs
      phases/
        copy.rs
        hardlink.rs
        delete.rs
        mtime.rs
```

Conceptually:

- source adapters know how to enumerate, open, and read source entries
- target adapters know how to create dirs, write files, hardlink, delete, and set mtimes
- phase orchestration combines one source adapter and one target adapter

That avoids transport-pair explosion.

### Scanner Plan

Scanner should follow the same principle:

- local scanner remains blocking traversal
- NFS scanner remains async RPC traversal
- SMB scanner becomes async directory traversal over `smb-rs`
- all scanner implementations emit the same `DirBatchScanResult`

The existing bridge pattern already works for NFS and should be reused for SMB.

### Internal Path Normalization

For SMB source scans, use a synthetic root:

```text
/__smb/<host>/<share>/<sub_path>
```

This keeps:

- `PathBuf`
- `strip_prefix`
- relative target path derivation

stable across platforms without relying on backslash-heavy UNC semantics in the core pipeline.

User-facing displays should still show `smb://...`.

## Implementation Plan

### Phase 1: Transport Scaffolding

- Add `SmbLocation`
- Add `DataLocation::Smb`
- Add `fptcli` SMB connect-string parsing
- Keep password redacted in logs/display

Status: started.
Status: completed.

### Phase 2: SMB Scanner

- Add `src/smb/scanner.rs`
- Connect via `smb-rs` `Client`
- Walk directories under a connected share root
- Convert SMB metadata into `DirMeta` / `FileMeta`
- Feed the existing writer bridge

Status: started.
Current state:

- the frame layer now delegates SMB scan requests to `SmbFileScanner`
- `SmbFileScanner` now drives a real SMB scan through `crate::scanner::run_smb_scan(...)`
- `src/smb/scanner.rs` traverses the configured share/sub-path and emits `DirBatchScanResult`
- the standard metadata writers and control-file generation path are reused unchanged
- hardlink counts are queried on demand when `scan_hardlinks` is enabled
- SMB scan performance has been improved substantially by:
  - reusing parent directory-entry metadata for non-root directories instead of re-querying each directory
  - making the SMB query-directory buffer size configurable via `ScanOption::smb_query_buffer_size` and `fsscan --smb-query-buffer-mb`
  - avoiding synchronous per-directory `CLOSE` waits on the scan hot path and letting `smb-rs` close dropped handles asynchronously

Remaining gaps:

- reparse points are currently treated conservatively and are not resolved to symlink targets
- traversal still pays one SMB `CREATE` per scanned directory, which remains the dominant scanner cost on deep trees
- deferred close currently causes some benign `smb-rs` close errors during client disconnect because background close tasks race with session teardown

### SMB Backup Stability Notes

During `local -> SMB` backup testing with large files, a separate stall was found in
the SMB write loop:

- the backup thread stayed alive but stopped making progress
- the last log line was typically a `write_block()` on the same file
- `smb-rs` logged `STATUS_PENDING` for the write, but completion never arrived

The Fpt-side mitigation now used in `src/smb/aio.rs` is:

- query negotiated SMB `max_read_size` / `max_write_size`
- avoid fixed `1 MiB` transfer chunks
- clamp active SMB writes to a conservative `256 KiB`
- allow active SMB reads up to `1 MiB`

This was enough to make the previously stuck `local -> SMB` backup complete
reliably on the local Samba test server.

Reads and writes intentionally use different safety caps. The observed stall was
on write completion, so writes remain conservative. SMB source reads are the
dominant cost in `SMB -> SMB` backup and have not shown the same stall behavior,
so the read cap is larger to reduce request count.

### SMB Backup Performance Notes

The `SMB -> SMB` copy path now emits detailed copy-operation timing for:

- open latency
- read/write byte counts
- average read/write chunk size
- max read/write latency
- effective read/write throughput
- max active copy/read/write concurrency

On the local Samba smoke dataset used during development:

```text
56 files, 24 dirs, 156.89 MiB total
```

Before increasing the SMB read cap, the run took about `37s`:

- read operations: `706`
- average read size: about `233 KiB`
- total read time: about `28s`
- write time: about `2s`

After increasing the default SMB read cap from `256 KiB` to `1 MiB`, the run took about
`18s`:

- read operations: `241`
- average read size: about `667 KiB`
- total read time: about `9s`
- write time: about `2s`

The key finding is that source-side SMB reads were RPC-count dominated. Reducing
the number of read requests gave about a `2x` end-to-end improvement without
changing write chunk size.

With `--buffer-size 2048`, the same dataset improved further to about `15s` by
reducing read operations to `171`. A `4096 KiB` read cap stalled on the local
Samba server, so SMB reads are capped at `2048 KiB`.

After adding double-buffer read-ahead in `copy_relative_file_streaming()`, the
same `--buffer-size 2048` run improved again to about `7s`:

- copy wait: about `5s`
- read operations: `171`
- total read time: about `5.5s`
- total write time: about `1.4s`
- effective end-to-end data rate: about `22 MiB/s`
- active overlap: `copy=2 read=2 write=2`

This shows the SMB path benefits substantially from overlapping source reads
with target writes, even with a single SMB client connection per endpoint.

SMB copy task concurrency is now decoupled from connection count. By default,
the scheduler allows two file copy tasks per SMB connection, capped at `16`
total tasks. This keeps `--smb-connections 1` usable while still allowing one
task to hide another task's open/read latency. The selected task limit is printed
in the `SMB->SMB timing` line as `copy_task_limit`.

Use `--smb-copy-tasks N` in `fptcli backup` or `fsbackup` to override the
automatic task limit. `0` means auto. Non-zero values are clamped to `1..16`.

Current remaining bottlenecks from the same timing data:

- source open: roughly `35ms` per file in the current local test
- target open: roughly `16ms` per file in the current local test
- SMB source read latency: roughly `32ms` per read at `2048 KiB`
- SMB writes are no longer dominant for this workload

Likely next improvement paths:

- Use `--buffer-size` to compare `1 MiB` and `2 MiB` SMB source reads without
  rebuilding. The default remains `1024 KiB`; SMB reads are capped at `2 MiB`;
  SMB writes remain capped at `256 KiB`. A `4 MiB` SMB read cap stalled during
  local Samba testing and is not currently considered safe.
- Batch or avoid target directory/file opens where possible, because open
  latency is now material for many-small-file datasets.
- Profile `smb-rs` with `perf` on a long `SMB -> SMB` run to check whether the
  remaining read latency is server wait time, signing/encryption CPU cost, or
  client-side request serialization.

### SMB Scanner Performance Notes

Measured on a local Samba server with a tree of about `3905` files and `3906`
directories:

- initial SMB scan time was about `40s`
- reusing parent directory metadata reduced scan time to about `20s`
- increasing SMB query buffer size from the default to `8 MiB` reduced scan time further to about `18s`
- deferring per-directory close waits reduced scan time again to about `10s`

Instrumented timing showed that the dominant cost was not directory enumeration
itself, but the per-directory open/close lifecycle:

- `QUERY_DIRECTORY` time was negligible
- root `query_info` time was negligible
- per-directory `CREATE` calls dominated scan time
- synchronous per-directory `CLOSE` calls were also very expensive until deferred

This means the next meaningful SMB scanner wins are likely to come from:

- reducing the number of directory opens required by the traversal model
- using more efficient relative-handle directory operations if `smb-rs` grows that capability
- minimizing optional per-file metadata work for workloads that do not need it

### Phase 3: SMB Read/Write Primitives

- Add `src/smb/aio/reader.rs`
- Add `src/smb/aio/writer.rs`
- Implement open/create/read_at/write_at/close wrappers
- Implement path resolution and directory creation helpers

Status: started.
Current state:

- `src/smb/aio/mod.rs` now owns SMB client connect helpers
- recursive target-directory creation is implemented
- buffered remote file creation and write is implemented
- local directory/file upload helpers are implemented for post-job repo publish

Remaining gaps:

- there is no SMB source-side read adapter yet
- the current SMB write path uploads full file buffers rather than streaming reads from the source

### Phase 4: SMB Target Phases

- Add SMB hardlink, delete, and mtime support if supported cleanly by the protocol/client
- If hardlink is not available or not reliable through the client API, document and gate it explicitly

This point needs validation against `smb-rs` API surface before promising parity.

Current state:

- `local -> SMB` backup currently supports the copy phase only
- hardlink/delete/mtime flags are accepted by the top-level backup flow but are skipped for SMB targets with an explicit log message

### Phase 5: Cross-Transport Orchestration Refactor

Before wiring all SMB directions, refactor the current NFS-specific AIO layout into source/target adapters.

That refactor should happen before:

- local -> smb
- smb -> local
- smb -> smb
- nfs <-> smb

Status: started.
Current state:

- duplicated control-file entry production was extracted from the NFS AIO pipelines
- duplicated local read/write helpers were extracted from the NFS AIO pipelines
- the async backup module now hosts both NFS and SMB transport entry points behind feature gates
- the next refactor step is to introduce reusable remote source/target adapters on top of those shared pieces

### Phase 6: Restore

- Support restore from local copy root to SMB target
- Then support restore from SMB copy source once remote copy-source support is generalized

## Notes On `smb-rs`

Based on the current project README and docs:

- it is a pure Rust SMB2/3 client
- it has an async backend
- it exposes `Client`, `ClientConfig`, `UncPath`, `share_connect`, `create_file`, and async file I/O methods

Sources:

- https://github.com/afiffon/smb-rs
- https://docs.rs/crate/smb/latest
