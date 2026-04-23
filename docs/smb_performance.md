# SMB Performance Notes

This document records the current SMB backup performance findings and the changes made to improve SMB-to-SMB copy throughput.

## Dataset Used

The measurements below use a local Samba server with this source dataset:

- source: `smb://127.0.0.1/dataset/ds2?username=xuranus&password=...`
- target: `smb://127.0.0.1/dataset/out?username=xuranus&password=...`
- files: `3905`
- directories: `3906`
- bytes: `39,987,200` (`38.13 MiB`)
- average file size: about `10 KiB`

This dataset is metadata-heavy, so SMB file-operation latency dominates unless the pipeline avoids per-file read/write RPCs.

## Root Cause Summary

Initial SMB-to-SMB backup was much slower than NFS-to-NFS because the old path paid full SMB client-side copy cost for every file:

- source file open
- target file create/open
- SMB reads
- SMB writes
- close on both sides

For many tiny files, transfer size is small but the per-file RPC count is large. Scan time was not the main issue:

- `fsscan --stats-only` on the same SMB dataset completed in about `2.8s`
- backup time was dominated by SMB copy-phase RPC latency

## Improvements Implemented

### 1. Remove Extra EOF Read In SMB Streaming Copy

The old streaming loop issued an extra read per file to discover EOF and allocated the read buffer using the negotiated chunk size even for very small files.

Change:

- pass the expected file size into `copy_relative_file_streaming()`
- stop reading once the known file size is reached
- allocate only `min(chunk_size, remaining_size)` for each read

Result:

- `read` count dropped from about `7810` to about `3905`
- improvement was real but not large enough on its own

### 2. Enable SMB Server-Side Copy For Same-Share SMB->SMB

When source and target are on the same SMB server/share/credentials, Bifrost now uses `srv_copy()` from the SMB client library instead of client-side read/write transfer.

This uses SMB FSCTL operations:

- `FSCTL_SRV_REQUEST_RESUME_KEY`
- `FSCTL_SRV_COPYCHUNK`

Change:

- detect same-share SMB source/target
- open source and target files
- call `target_file.srv_copy(&source_file)`
- fall back to the existing streaming path when server-side copy is not applicable

Result:

- `read=0`
- `write=0`
- SMB data transfer leaves the client hot path

### 2.1 Fall Back To Streaming When `srv_copy()` Is Rejected

Some Samba setups accept `COPYCHUNK` for small files but reject it for larger files with errors such as:

- `STATUS_INVALID_PARAMETER (0xc000000d)`

Treating that as a hard failure made mixed-size SMB->SMB backups unreliable even though ordinary streaming copy still worked.

Change:

- keep `srv_copy()` as the preferred same-share fast path
- if `srv_copy()` fails for a file, log a warning and fall back to the normal streaming read/write loop for that file
- expose a `fallback=` counter in the SMB timing summary so mixed fast-path/fallback runs are visible in logs

Result:

- same-share SMB->SMB backup no longer fails just because the server rejects `COPYCHUNK` for some files
- small files can still benefit from server-side copy while larger files continue via streaming fallback

### 3. Increase SMB Copy Concurrency

After `srv_copy()` was enabled, the new bottleneck became per-file open + server-side copy latency. Higher concurrency improved throughput significantly on the local Samba server.

Changes:

- `SMB_MAX_CONCURRENT_TASKS`: `16 -> 32`
- `SMB_TASKS_PER_CONNECTION`: `2 -> 8`

With the default `--smb-connections 4`, the auto-selected SMB copy task limit now becomes `32`.

## Measured Results

### Before Server-Side Copy

Representative copy timing:

- total pipeline: about `80s`
- source opens: `3905`
- target opens: `3905`
- reads: `7810`
- writes: `3905`

Representative CLI wall time:

- `~1m 28s`

### After `srv_copy()` But Before Higher Concurrency

Representative copy timing:

- total pipeline: about `37.4s`
- `mkdir_wait=17.9s`
- `copy_wait=18.8s`
- `srv_copy=3905`

Representative CLI wall time:

- `~45s`

### After Raising SMB Copy Concurrency

Representative copy timing from the default command:

- total pipeline: `6.065s`
- `mkdir_wait=2.515s`
- `copy_wait=2.784s`
- `copy_task_limit=32`
- `srv_copy=3905`
- `read=0`
- `write=0`

Representative CLI wall time:

- `13.878s`

This reduced SMB-to-SMB backup time by roughly `6x` compared with the original `~88s` run on the same dataset.

## Failed Experiment: Lazy Per-File Directory Ensure

An attempted optimization removed the eager directory-creation phase and instead called `ensure_relative_directory()` from each file copy task.

This was worse on the measured dataset because it turned directory checks into a per-file cost:

- `ensure_dir=3905`
- average ensure time was roughly `33ms`
- wall time regressed to about `1m 22s`

That experiment was reverted.

Conclusion:

- eager directory creation is still better than per-file parent ensure on this Samba server
- the remaining SMB bottlenecks are directory creation latency and per-file open/server-side-copy metadata RPCs

## Current Best Known State

For same-share SMB-to-SMB backup of many small files:

- use SMB server-side copy (`srv_copy`)
- keep eager directory creation
- use the higher default SMB copy concurrency

## Next Optimization Targets

Possible future work:

- reduce `mkdir_wait` further with a better directory scheduling strategy
- reduce source/target open latency if the SMB client allows a cheaper access mask or create mode
- benchmark whether `32` is close to the best task limit across more SMB servers
- add more detailed server-side-copy fallback reason aggregation for unsupported servers
