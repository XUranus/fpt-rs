# Smoke Matrix Test

`scripts/smoke_matrix.sh` runs a full functional smoke matrix across:

- backup and restore
- local, NFS, and SMB source/target combinations
- common format
- aggregate format with configurable layouts
- restore verification with `fsdiff`

The script is intended for a development machine where one directory is visible through local filesystem, NFS, and SMB.

## Default Assumptions

Defaults match the local development setup:

```bash
TEST_ROOT_DIR=/opt/dataset
TEST_TEMP_DIR=/opt/target/work
TEST_NFS_HOST=127.0.0.1
TEST_NFS_EXPORT=/opt/dataset
TEST_SMB_HOST=127.0.0.1
TEST_SMB_SHARE=dataset
TEST_SMB_USER=xuranus
TEST_SMB_PASSWORD=123456789
```

The script creates:

```text
$TEST_ROOT_DIR/test_smoke
$TEST_ROOT_DIR/out
$TEST_ROOT_DIR/restore/{copy_uuid}
$TEST_ROOT_DIR/smoke_logs/{timestamp}
```

`TEST_ROOT_DIR` must be exported by both NFS and SMB if the full default matrix is used.

## Dataset

The script calls `vdbench` several times to generate a bounded dataset:

- 1 KiB files
- 128 KiB files
- 1 MiB files
- 4 MiB files
- 100 MiB files

Default size is below 1 GiB and below 100 files. The current default creates 57 files and about 257 MiB of file data.

## Run

Full default matrix:

```bash
scripts/smoke_matrix.sh
```

Local-only quick validation:

```bash
TEST_ROOT_DIR=/tmp/bifrost-smoke \
TEST_BUILD=0 \
TEST_TRANSPORTS=local \
TEST_AGGREGATE_LAYOUTS=shard \
scripts/smoke_matrix.sh
```

Use only one aggregate layout for a shorter remote run:

```bash
TEST_AGGREGATE_LAYOUTS=shard scripts/smoke_matrix.sh
```

## Environment

| Variable | Default | Meaning |
|----------|---------|---------|
| `TEST_ROOT_DIR` | `/opt/dataset` | Local root used for dataset, copies, restores, and logs |
| `TEST_TEMP_DIR` | `/opt/target/work` | `fptcli --temp-dir` |
| `TEST_BIN_DIR` | `target/release` | Directory containing `fptcli`, `fsdiff`, and `vdbench` |
| `TEST_BUILD` | `1` | Build release binaries with `nfs` and `smb` features before running |
| `TEST_CLEAN` | `1` | Remove previous `test_smoke`, `out`, and `restore` directories |
| `TEST_TIMEOUT_SEC` | `60` | Per-command timeout; timeout is treated as possible hang |
| `TEST_TRANSPORTS` | `local nfs smb` | Transports included in the source/target matrix |
| `TEST_AGGREGATE_LAYOUTS` | `shard dir-level` | Aggregate layouts tested for each source/target pair |
| `TEST_NFS_HOST` | `127.0.0.1` | NFS server |
| `TEST_NFS_EXPORT` | `$TEST_ROOT_DIR` | NFS export path |
| `TEST_NFS_UID` | `id -u` | AUTH_UNIX uid passed to `fptcli` |
| `TEST_NFS_GID` | `id -g` | AUTH_UNIX gid passed to `fptcli` |
| `TEST_SMB_HOST` | `127.0.0.1` | SMB server |
| `TEST_SMB_SHARE` | `dataset` | SMB share name |
| `TEST_SMB_USER` | `xuranus` | SMB username |
| `TEST_SMB_PASSWORD` | `123456789` | SMB password |

## Matrix

For each configured transport pair, the script runs:

```text
source -> target, common
source -> target, aggregated shard
source -> target, aggregated dir-level
```

With the default transport list, this covers:

```text
local -> local
local -> nfs
local -> smb
nfs   -> local
nfs   -> nfs
nfs   -> smb
smb   -> local
smb   -> nfs
smb   -> smb
```

For every backup copy, the script restores to:

```text
$TEST_ROOT_DIR/restore/{copy_uuid}
```

Then it verifies:

```bash
fsdiff --source "$TEST_ROOT_DIR/test_smoke" --target "$TEST_ROOT_DIR/restore/{copy_uuid}"
```

## Timeout Behavior

Each backup, restore, diff, and dataset-generation command runs in a separate process through `timeout`.

If a command runs longer than `TEST_TIMEOUT_SEC`, the script prints:

```text
[HANG?] ... exceeded ... and was killed
```

The failed command's stdout/stderr are kept in the log directory.

## vdbench Naming

The smoke dataset uses the newer `vdbench` naming options:

```bash
--dir-prefix vdb.1k.dir.
--file-prefix file.
--level-names
--index-base 1
```

With `--level-names`, directory names include their full logical index path, for example:

```text
vdb.1k.dir.1/vdb.1k.dir.1.2/file.4
```

File contents are deterministic pseudo-random bytes, not zero-filled data.
