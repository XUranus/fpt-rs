# fptserver

`fptserver` is a process-supervised RPC server for creating and managing Bifrost scan, backup, and restore tasks.

It is intentionally built as:

- one long-lived root server process
- one child worker process per task
- file-based request/status IPC under a runtime directory

This gives each task both a UUID and a PID, which makes `stop` / `kill` straightforward and keeps task failures isolated from the server itself.

## Server Mode

Start the server:

```bash
./target/debug/fptserver \
  --host 127.0.0.1 \
  --port 3000 \
  --runtime-dir /tmp/fptserver \
  --max-scanners-count 1 \
  --max-subtasks-count 4
```

Flags:

- `--host`: bind address, default `127.0.0.1`
- `--port`: listen port, default `3000`
- `--runtime-dir`: server-owned task runtime directory, default `/tmp/fptserver`
- `--max-scanners-count`: max concurrently active scan tasks
- `--max-subtasks-count`: max concurrently active backup/restore tasks

## Worker Mode

`fptserver` also contains an internal worker mode:

```bash
./target/debug/fptserver worker \
  --task-file /tmp/fptserver/<uuid>/request.json \
  --status-file /tmp/fptserver/<uuid>/status.json
```

This mode is not intended for direct operator use. The root server spawns it automatically.

## Runtime Layout

Each task gets a runtime directory:

```text
/tmp/fptserver/<uuid>/
  request.json
  status.json
  worker.log
```

- `request.json`: serialized task spec
- `status.json`: latest server/worker-visible task state
- `worker.log`: stdout/stderr from the worker process

## JSON-RPC API

`POST /rpc`

Supported methods:

- `task.create_scan`
- `task.create_backup`
- `task.create_restore`
- `task.stop`
- `task.kill`
- `task.get`
- `task.list`
- `task.all`

### Create Scan

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "task.create_scan",
  "params": {
    "source": "/opt/dataset/ds2",
    "ctrl_dir": "/tmp/scan/ctrl",
    "meta_dir": "/tmp/scan/meta",
    "temp_dir": "/tmp/scan/cache",
    "workers": 8,
    "writers": 1,
    "stats_only": false
  }
}
```

Important scan params:

- `source`: local path, `nfs://...`, or `smb://...`
- `ctrl_dir`, `meta_dir`, `temp_dir`
- `workers`, `writers`
- `follow_symlinks`, `scan_hidden`, `scan_acl`, `scan_xattrs`, `scan_hardlinks`
- `prev_meta_dir`
- sharding fields: `shard`, `shard_num`, `shard_max_entries_copy`, `shard_max_entries_other`, `shard_max_size`
- transport fields: `nfs_connections`, `nfs_uid`, `nfs_gid`, `smb_query_buffer_mb`
- failure/retry fields: `failure_log_format`, `retry`

### Create Backup

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "task.create_backup",
  "params": {
    "source": "/opt/dataset/ds2",
    "target": "smb://127.0.0.1/dataset/out?username=xuranus&password=123456789",
    "format": "common",
    "temp_dir": "/opt/target/work",
    "scan_workers": 8,
    "scan_writers": 1,
    "jobs": 4,
    "smb_connections": 4,
    "smb_copy_tasks": 2,
    "buffer_size_kb": 1024
  }
}
```

Important backup params:

- `source`, `target`: local path, `nfs://...`, or `smb://...`
- `format`: `common` or `aggregated`
- `incremental_base`: optional path to previous copy
- `temp_dir`
- `scan_workers`, `scan_writers`
- `jobs`: max concurrent copy subtasks inside the job
- `hardlink`, `delete`, `mtime`
- `aggregate`: aggregation block:
  - `enabled`
  - `layout`: `dir_level` or `shard`
  - `blob_size_mb`
  - `threshold_kb`
  - `shard_count`
- transport tuning:
  - `nfs_connections`, `nfs_uid`, `nfs_gid`
  - `smb_connections`, `smb_copy_tasks`
  - `buffer_size_kb`
- failure/retry:
  - `failure_log_format`
  - `retry`

### Create Restore

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "task.create_restore",
  "params": {
    "copy_source": "/opt/dataset/out/COPY_COMMON_FULL_xxx",
    "target": "/opt/dataset/restore",
    "policy": "replace",
    "temp_dir": "/opt/target/work",
    "jobs": 4
  }
}
```

Important restore params:

- `copy_source`: local path, `nfs://...`, or `smb://...`
- `target`: local path, `nfs://...`, or `smb://...`
- `policy`: `replace` or `skip`
- `temp_dir`
- `jobs`
- transport tuning: `nfs_connections`, `nfs_uid`, `nfs_gid`

### Stop / Kill / Get / List

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "task.kill",
  "params": {
    "uuid": "72310adf-5911-421a-a646-03be5d52a175"
  }
}
```

`task.stop` sends `SIGTERM`.

`task.kill` sends `SIGKILL`.

Both operate on the worker process PID currently recorded for the task.

## REST API

Read-only endpoints:

- `GET /health`
- `GET /tasks`
- `GET /tasks/<uuid>`
- `GET /tasks/<uuid>/status`
- `GET /tasks/<uuid>/logs`

`/tasks/<uuid>/logs` returns the worker stdout/stderr capture from `worker.log`.

## Status Model

Task states:

- `created`
- `starting`
- `running`
- `stopping`
- `stopped`
- `completed`
- `failed`
- `killed`

Each task status includes:

- `uuid`
- `kind`
- `state`
- `pid`
- `created_at`
- `started_at`
- `finished_at`
- `message`
- `exit_code`
- `stats`
- `request`

Current `stats` payloads:

- scan: file/dir/size/failure totals
- backup/restore: final file/dir/byte/subtask totals

## Current Limitations

- Status reporting is currently phase/state oriented. It does not yet stream fine-grained live progress from backup/restore internals.
- Task registry is in-memory for the server process. Runtime files are persisted, but startup reconciliation is not implemented yet.
- `stop()` is process-level termination, not cooperative in-engine cancellation.
- Authentication, transport, and path parsing follow the current `fsscan`, `fptcli`, and `fsbackup` behavior; this server does not introduce a second transport model.

## Validation

The first implementation was validated locally with:

- `cargo build --bin fptserver --features smb --features nfs`
- `task.create_scan`
- `task.list`
- `task.get`
- `task.kill`
- `GET /tasks/<uuid>/status`
- `GET /tasks/<uuid>/logs`
