# Logging

Fpt uses a shared routed logger implemented in `src/logging.rs`.

The important behavior is:

- unmatched records go to stdout
- route-matched records go to a specific log file instead of stdout
- `--log-file` adds a catch-all extra file that receives every record

## Log Format

Every line has the same format:

```text
2026-04-18 19:05:49 [DEBUG] exacl::failx - acl_get_file/default("/path") returned null, err=Permission denied (os error 13)
```

Fields:

- local timestamp
- level
- Rust log target
- message

## Route Model

Routes are matched by prefix against `record.target()`. The most specific route wins.

Current `fptcli backup` routing:

| Target prefix | Destination |
|---------------|-------------|
| `fpt::scanner` | `C_REPO/logs/scan.log` |
| `fpt::nfs` | `C_REPO/logs/scan.log` during scan unless a more specific NFS AIO route matches |
| `fpt::frame` | `C_REPO/logs/frame.log` |
| `fpt::backup` | current subtask log |
| `fpt::nfs::aio` | current subtask log |
| `exacl` | `scan.log` during scan, then current subtask log during backup |

This routing is configured in `src/frame/backup_job.rs`.

## Files Under `C_REPO/logs`

Current log files are:

- `scan.log`
- `frame.log`
- `{subtask_uuid}.log`

There is no longer a job-wide `backup.log` file in the current implementation.

### `scan.log`

Contains scan-related logs:

- scanner traversal
- NFS scan activity
- ACL-related `exacl::*` messages emitted during scan

### `frame.log`

Contains orchestration logs:

- phase markers
- prerequisite setup
- job lifecycle
- subtask scheduling
- post-job manifest and upload activity

### `{subtask_uuid}.log`

Contains per-subtask backup logs:

- local BIO copy-phase logs
- NFS AIO copy logs
- hardlink/delete/mtime phase logs
- ACL-related `exacl::*` messages emitted during backup

## `--log-file`

Passing `--log-file <PATH>` adds an extra catch-all file.

That file receives:

- frame logs
- scan logs
- subtask logs
- any unmatched stdout-bound logs

This makes it useful as a single merged capture file.

## Example

```bash
./target/debug/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format common \
  --log-file /tmp/fpt.log \
  -v
```

Expected result:

- routed logs are written under `COPY_.../C_REPO/logs/`
- the same records are also appended to `/tmp/fpt.log`

## NFS and ACL Notes

`exacl::failx` used to leak to stdout during some backup paths. The current routing explicitly sends `exacl` logs to repository log files:

- `scan.log` during scan
- current subtask log during backup

## Source Files

Key files involved in logging behavior:

- `src/logging.rs`
- `src/frame/backup_job.rs`
- `src/frame/repo.rs`
