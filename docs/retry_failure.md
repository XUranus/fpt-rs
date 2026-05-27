# Retry And Failure Logs

Fpt can keep scanning and backup tasks running when individual entries fail, while recording structured failure details for later triage.

This feature covers:

- scan failures such as directory open errors, stat/getattr failures, SMB query failures, and NFS RPC failures
- backup copy failures such as source open, read block, target directory creation, write block, and aggregate blob creation
- local follow-up phase failures for hardlink, delete, and mtime phases

## Failure Log Files

`fptcli backup` writes structured failure files under the copy's `C_REPO/logs` directory when `--failure-log-format` is provided.

File names:

- `{copy_uuid}_SCAN_FAILURE.csv|json|xml`
- `SUBTASK_{subtask_uuid}_FAILURE.csv|json|xml`

Low-level tools can write failure logs directly:

- `fsscan --failure-log-format <fmt> [--failure-log <file>]`
- `fsbackup --failure-log-format <fmt> [--failure-log <file>]`

If `--failure-log` is omitted:

- `fsscan` writes `<ctrl-dir>/SCAN_FAILURE.<fmt>`
- `fsbackup` writes `<ctrl-dir>/FSBACKUP_FAILURE.<fmt>`

## Record Format

Each failure record contains:

- `time`: UTC timestamp
- `phase`: `scan` or `backup`
- `operation`: operation name, for example `read_block`, `write_block`, `open_dir`, `create_dir`, `delete_file`
- `item_type`: `file`, `directory`, `symlink`, `special`, `block`, or `unknown`
- `path`: source or target path related to the failure
- `code`: normalized code such as `EPERM`, `EACCES`, `ENOENT`, `ENOSPC`, `NFS3ERR_ACCES`, or `UNKNOWN`
- `detail`: original error text
- `attempts`: total attempts made before the item was considered failed

CSV example:

```csv
time,phase,operation,item_type,path,code,detail,attempts
2026-04-22T10:12:30Z,backup,write_block,file,/backup/D_REPO/d1/f0,ENOSPC,No space left on device,4
```

JSON output is a top-level array of records. XML output is wrapped in a `<failures>` root element.

## Retry Policy

The retry policy is shared by scan and backup paths.

Options:

| Option | Default | Meaning |
|--------|---------|---------|
| `--operation-retries` | `3` | Retry count after the initial attempt |
| `--retry-delay-ms` | `1000` | Base retry delay |
| `--retry-backoff` | `1.0` | Exponential backoff multiplier; `1.0` means fixed delay |
| `--retry-max-delay-ms` | `1000` | Maximum delay after backoff |
| `--retry-jitter` | `0.0` | Deterministic jitter ratio, range `0.0..1.0` |

Attempt count is `operation-retries + 1`. For example, `--operation-retries 3` allows one initial attempt and three retries, so terminal records show `attempts=4`.

Delay calculation:

```text
delay(attempt) = min(retry-delay-ms * retry-backoff^(attempt - 1), retry-max-delay-ms)
```

If jitter is enabled, a deterministic bounded jitter is applied around that delay. This avoids all retrying tasks waking at exactly the same time while keeping retry behavior reproducible.

## Examples

Integrated backup with CSV failure logs and exponential backoff:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format common \
  --failure-log-format csv \
  --operation-retries 5 \
  --retry-delay-ms 500 \
  --retry-backoff 2.0 \
  --retry-max-delay-ms 8000 \
  --retry-jitter 0.2
```

Standalone scan with JSON failure logs:

```bash
./target/release/fsscan /opt/dataset/source \
  -c /tmp/scan/ctrl \
  -m /tmp/scan/meta \
  --failure-log-format json \
  --operation-retries 3
```

Standalone backup with XML failure logs:

```bash
./target/release/fsbackup \
  -s /opt/dataset/source \
  -t /backup/root \
  -m /tmp/scan/meta \
  -c /tmp/scan/ctrl/copy.txt \
  --failure-log-format xml \
  --retry-delay-ms 250 \
  --retry-backoff 2.0 \
  --retry-max-delay-ms 4000
```

## Implementation Notes

The retry primitive lives in `src/failure.rs`.

- `RetryPolicy` owns retry count, base delay, backoff, max delay, and jitter.
- `retry_sync` and `retry_async` handle plain operations.
- `retry_sync_item` and `retry_async_item` handle move-only retry items such as copy blocks.
- The retry queue keeps the failed item and schedules the next attempt after the policy-calculated delay.

The current queue is per operation/task, not a global shared retry worker pool. This keeps `CopyBlock` ownership simple and avoids cross-task contention. A global pool can be added later if central throttling across subtasks is needed.

## Current Limits

- Remote target hardlink/delete/mtime paths log normal phase errors, but structured failure records are currently most complete for local follow-up phases and copy paths.
- Failure logs are created only when `--failure-log-format` is provided.
- Fatal setup errors, such as failure to connect to a remote endpoint before a scan or backup starts, are reported as task errors rather than per-entry failure records.
