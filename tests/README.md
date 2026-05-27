# fpt Test Suite

pytest-based integration tests covering functional correctness and performance
benchmarks for the fpt backup/recovery engine.

## Table of Contents

- [Directory Structure](#directory-structure)
- [Environment Variables](#environment-variables)
- [Quick Start](#quick-start)
- [Running Tests](#running-tests)
- [Smoke Tests](#smoke-tests)
- [Performance Tests](#performance-tests)
- [Transport Matrix](#transport-matrix)
- [Test Count Summary](#test-count-summary)
- [Framework Architecture](#framework-architecture)
- [Adding New Tests](#adding-new-tests)
- [CI Integration](#ci-integration)

## Directory Structure

```
tests/
├── README.md                       # This document
├── __init__.py
├── framework.py                    # Core framework: CLI wrappers, transport config, fileset utilities
├── conftest.py                     # pytest fixtures, auto-skip logic
├── run_tests.py                    # Unified entry-point script
├── smoke/                          # Smoke tests
│   ├── test_basic_backup.py        # Basic backup/restore round-trip
│   ├── test_empty_dirs.py          # Empty directory preservation
│   ├── test_symlinks.py            # Symlink handling
│   ├── test_hardlinks.py           # Hardlink preservation (Unix)
│   ├── test_sparse.py              # Sparse file size and content
│   ├── test_special_names.py       # Special-character filenames
│   ├── test_permissions.py         # File permission preservation (Unix)
│   ├── test_incremental.py         # Incremental backup (add/modify/delete)
│   ├── test_aggregate.py           # Aggregate backup (shard/dir-level)
│   ├── test_filter.py              # Path filtering (include/exclude)
│   ├── test_acl.py                 # Linux ACL (requires setfacl)
│   ├── test_xattr.py               # Linux extended attributes (requires setfattr)
│   └── test_transport_matrix.py    # 3x3 transport orthogonal matrix
└── perf/                           # Performance tests
    ├── test_file_size.py           # File sizes: 1K / 128K / 1M / 10M
    ├── test_depth.py               # Directory depths: 1 / 4 / 10 layers
    └── test_scale.py               # File counts: 1K / 10K / 50K
```

## Environment Variables

### Core

| Variable | Description | Default |
|---|---|---|
| `FPT_BIN_DIR` | Directory containing compiled binaries | `target/release` (auto-detected) |
| `FPT_DATA_ROOT` | Local data root for test artifacts | `/opt/fpt_test_data` |
| `FPT_KEEP_ON_FAILURE` | Preserve workspace when a test fails (`1` or `true`) | `0` |

### NFS Transport (optional)

Set `FPT_NFS_MOUNT` to enable NFS transport tests. All NFS tests are
auto-skipped when this variable is unset.

| Variable | Description | Default |
|---|---|---|
| `FPT_NFS_MOUNT` | Kernel NFS mount point (e.g. `/mnt/nfs`) | — |
| `FPT_NFS_HOST` | NFS server address | `127.0.0.1` |
| `FPT_NFS_EXPORT` | NFS export path | `/opt/dataset` |
| `FPT_NFS_UID` | AUTH_UNIX uid (libnfs requires explicit auth) | current process uid |
| `FPT_NFS_GID` | AUTH_UNIX gid | current process gid |

### SMB Transport (optional)

Set `FPT_SMB_MOUNT` to enable SMB transport tests. All SMB tests are
auto-skipped when this variable is unset.

| Variable | Description | Default |
|---|---|---|
| `FPT_SMB_MOUNT` | Kernel SMB/CIFS mount point (e.g. `/mnt/smb`) | — |
| `FPT_SMB_HOST` | SMB server address | `127.0.0.1` |
| `FPT_SMB_SHARE` | SMB share name | `dataset` |
| `FPT_SMB_USER` | SMB username | `xuranus` |
| `FPT_SMB_PASSWORD` | SMB password | `123456789` |

## Quick Start

```bash
# 1. Build the project
cargo build --release

# 2. Create the data root directory
sudo mkdir -p /opt/fpt_test_data
sudo chown $(whoami) /opt/fpt_test_data

# 3. Run tests
cd fpt-rs

# Option A: via run_tests.py
python tests/run_tests.py smoke        # smoke tests
python tests/run_tests.py perf         # performance tests
python tests/run_tests.py all          # everything

# Option B: directly via pytest
python -m pytest tests/smoke/ -v       # smoke tests
python -m pytest tests/perf/ -v        # performance tests
python -m pytest tests/ -v             # everything

# Option C: a single test file or keyword
python -m pytest tests/smoke/test_basic_backup.py -v
python -m pytest tests/ -k "test_hardlinks" -v
```

## Running Tests

### Via `run_tests.py`

`run_tests.py` wraps pytest and provides convenience flags:

```bash
python tests/run_tests.py <suite> [options]

Suites:
  smoke              Run smoke (functional) tests
  perf               Run performance tests
  all                Run all tests

Options:
  -k, --keyword EXPR       Filter tests by keyword (pytest -k)
  -v, --verbose            Increase verbosity (pass -v or -vv to pytest)
  -x, --stop-on-first      Stop on first failure
  --keep-on-failure        Preserve workspace on failure
  --timeout SECONDS        Per-test timeout
  --junit-xml PATH         Write JUnit XML report
```

Examples:

```bash
# Run smoke tests with verbose output
python tests/run_tests.py smoke -v

# Run perf tests, stop on first failure, 120s timeout
python tests/run_tests.py perf -x --timeout 120

# Run a specific test by keyword
python tests/run_tests.py all -k "test_transport_matrix"

# Generate JUnit XML report
python tests/run_tests.py smoke --junit-xml results.xml
```

### Via pytest directly

```bash
# Full verbosity
python -m pytest tests/smoke/ -v

# Run a single test file
python -m pytest tests/smoke/test_basic_backup.py -v

# Run a single parametrized case
python -m pytest tests/smoke/test_transport_matrix.py -k "nfs_to_smb" -v

# Show transport status
python -m pytest tests/smoke/ -v --co -q  # collect only, shows parametrized IDs
```

### Custom Data Directory

```bash
FPT_DATA_ROOT=/tmp/my_test_data python -m pytest tests/smoke/ -v
```

### Debugging Failures

When a test fails, set `FPT_KEEP_ON_FAILURE=1` to preserve the workspace
directory. The workspace path is printed in the fixture setup output.

```bash
FPT_KEEP_ON_FAILURE=1 python -m pytest tests/smoke/test_basic_backup.py -v

# Inspect the workspace
ls /opt/fpt_test/local/test_basic_backup_restore_*/source/
ls /opt/fpt_test/local/test_basic_backup_restore_*/backup/
ls /opt/fpt_test/local/test_basic_backup_restore_*/restore/
```

Each workspace contains:

```
<source_id>/
├── source/      # Original test data
├── backup/      # Backup output (COPY_* directories)
├── restore/     # Restored data
├── meta/        # Metadata directories
├── ctrl/        # Control files
└── logs/        # CLI stdout/stderr logs (backup.log, restore.log, ...)
```

## Smoke Tests

Each smoke test runs `fptcli backup` -> `fptcli restore` -> `fsdiff` verification.

### test_basic_backup -- Basic Backup/Restore

| Item | Value |
|---|---|
| Fileset | depth=3, 5 files/dir, 2 dirs/level, 4 KB/file, ~100 files, ~400 KB |
| Flow | backup -> find COPY_* -> fsdiff verify D_REPO -> restore -> fsdiff verify |
| Assertion | fsdiff exit code 0 (source and target are identical) |

### test_empty_directories -- Empty Directory Preservation

| Item | Value |
|---|---|
| Fileset | 5 empty directories (including nested) + 1 non-empty directory |
| Assertion | Empty directories exist after backup/restore with no content; non-empty directory files are intact |

### test_symlinks_backup_succeeds -- Symlinks

| Item | Value |
|---|---|
| Fileset | Link target file, directory, relative/absolute/broken/chain symlinks |
| Assertion | Backup succeeds; link target file content is correct (symlinks may be dereferenced) |

### test_hardlinks -- Hardlinks

| Item | Value |
|---|---|
| Fileset | 3 hardlink groups: 2-ref pair, 3-ref triple, cross-directory deep nesting |
| CLI | `fsscan --scan-hardlinks` + `fsbackup --hardlink` |
| Assertion | Hardlink group contents match (SHA256); fsdiff passes |

### test_sparse_files -- Sparse Files

| Item | Value |
|---|---|
| Fileset | 100 MB (data at head and tail), 50 MB (hole in middle), 10 MB (small sparse) |
| Assertion | Apparent size matches; first/last 4 KB content matches; fsdiff passes |

### test_special_filenames -- Special Filenames

| Item | Value |
|---|---|
| Fileset | Filenames with spaces, hyphens, dots, uppercase, @, +, underscores |
| Assertion | Files exist and SHA256 content matches |

### test_permissions -- Permission Preservation

| Item | Value |
|---|---|
| Fileset | Files with mode 644/600/755/400; directories with mode 755/700 |
| CLI | `fptcli backup --mtime` / `fptcli restore --mtime` |
| Assertion | File content matches (permission differences are logged but not hard-asserted, as they may be lost across transports) |

### test_incremental_backup -- Incremental Backup

| Item | Value |
|---|---|
| Phase 1 | ~100 files x 4 KB, full backup |
| Phase 2 | Add 3 files + 1 directory, modify 1 file, delete 1 file |
| Phase 3 | Second full backup (`--delete --mtime`) |
| Assertion | New files exist; modified file content is updated; fsdiff passes |

### test_aggregate_backup_restore -- Aggregate Backup

| Item | Value |
|---|---|
| Parametrized | `shard` / `dir-level` two layouts |
| Fileset | ~200 files x 4 KB |
| CLI | `fptcli backup --aggregate --threshold 64 --aggregate-layout <layout>` |
| Assertion | All files SHA256 match after restore |

### test_exclude_dir_pattern / test_include_file_pattern -- Path Filtering

| Item | Value |
|---|---|
| Fileset | Contains keep_dir, skip_dir, .txt/.csv/.log files |
| CLI | `--exclude-dir-pattern` / `--include-file-pattern` |
| Assertion | Excluded dirs/files are not in backup; included files are present |

### test_linux_acl -- ACL Preservation (Linux only)

| Item | Value |
|---|---|
| Prerequisite | `setfacl` must be available |
| Fileset | 2 files with user/group ACL entries |
| CLI | `fsscan --scan-acl` -> `fsbackup` -> `fsdiff --compare-acl` |
| Assertion | `fsdiff --compare-acl` exit code 0 |

### test_linux_xattr -- Extended Attributes (Linux only)

| Item | Value |
|---|---|
| Prerequisite | `setfattr` must be available |
| Fileset | 1 file with user.comment/checksum/version/empty xattrs |
| CLI | `fsscan --scan-xattrs` -> `fsbackup` -> `fsdiff --compare-xattrs` |
| Assertion | `fsdiff --compare-xattrs` exit code 0 |

## Performance Tests

Performance tests use `vdbench` to generate deterministic filesets and measure
backup + restore elapsed time. Each case has a maximum allowed time.

### test_perf_file_size -- File Size

| Case | files/dir | dirs | File Size | Files | Data |
|---|---|---|---|---|---|
| 1K | 1000 | 5 | 1 KB | 5,000 | ~5 MB |
| 128K | 200 | 5 | 128 KB | 1,000 | ~128 MB |
| 1M | 100 | 5 | 1 MB | 500 | ~500 MB |
| 10M | 20 | 5 | 10 MB | 100 | ~1 GB |

### test_perf_directory_depth -- Directory Depth

| Case | depth | files/dir | dirs/level | File Size | Files |
|---|---|---|---|---|---|
| depth_1 | 1 | 200 | 5 | 64 KB | 200 |
| depth_4 | 4 | 30 | 3 | 8 KB | ~3,630 |
| depth_10 | 10 | 8 | 2 | 2 KB | ~16,376 |

### test_perf_fileset_scale -- Fileset Scale

| Case | depth | files/dir | dirs/level | File Size | Files |
|---|---|---|---|---|---|
| 1K_files | 2 | 100 | 5 | 32 KB | ~600 |
| 10K_files | 3 | 50 | 5 | 8 KB | ~1,500 |
| 50K_files | 4 | 100 | 5 | 1 KB | ~15,600 |

## Transport Matrix

`test_transport_matrix.py` tests backup/restore across all configured transport
combinations in a 3x3 orthogonal matrix (source x target).

### Setup

All transports share the same underlying storage (e.g. `/opt/dataset`).

```bash
# NFS mount
sudo mount -t nfs 127.0.0.1:/opt/dataset /mnt/nfs

# SMB mount
sudo mount -t cifs //127.0.0.1/dataset /mnt/smb \
  -o username=xuranus,password=123456789,uid=$(id -u),gid=$(id -g)

# Enable all transport tests
FPT_DATA_ROOT=/opt/dpt_test_data \
  FPT_NFS_MOUNT=/mnt/nfs \
  FPT_SMB_MOUNT=/mnt/smb \
  python -m pytest tests/smoke/test_transport_matrix.py -v
```

### How It Works

1. Each test case gets a UUID-based directory name for data isolation.
2. Test data is written to the source transport's mount point.
3. `fptcli backup` runs with the source and target fptcli location URLs.
4. The target backup directory is pre-created on the target mount (required for
   NFS/SMB, as fptcli's connection pool performs a LOOKUP on the sub_path
   during initialization).
5. The COPY_* directory is found on the target mount and `fptcli restore` runs.
6. Restored data is verified file-by-file (size + SHA256) against the original.
7. Cleanup removes test data, backup, and restore directories on all transports.

### Generated Cases

With NFS and SMB configured, 9 cases are collected:

```
local_to_local    local_to_nfs    local_to_smb
nfs_to_local      nfs_to_nfs      nfs_to_smb
smb_to_local      smb_to_nfs      smb_to_smb
```

Unconfigured transports are auto-skipped. With only local transport, 1 case runs.

### fptcli Location URL Formats

| Transport | URL Format |
|---|---|
| Local | Absolute path, e.g. `/opt/fpt_test_data/local/workspace/backup` |
| NFS | `nfs://host/export?sub=subpath&uid=X&gid=Y` |
| SMB | `smb://host/share/subpath?username=u&password=p` |

NFS uid/gid default to the current process uid/gid if `FPT_NFS_UID`/`FPT_NFS_GID`
are not set. libnfs requires explicit AUTH_UNIX credentials.

## Test Count Summary

| Scenario | Local only | +NFS | +NFS+SMB |
|---|---|---|---|
| Smoke (functional) | 14 | 14 | 14 |
| Smoke (transport matrix) | 1 | 4 | 9 |
| Perf (performance) | 10 | 10 | 10 |
| **Total** | **25** | **28** | **33** |

## Framework Architecture

### `framework.py` Core Components

| Component | Description |
|---|---|
| `FptCli` | CLI tool wrapper. Provides `backup()`, `restore()`, `fsscan()`, `fsbackup()`, `fsdiff()`, `vdbench()`, `metainspect()` methods |
| `CliResult` | Dataclass holding returncode, stdout, stderr, command, duration |
| `Transport` | Enum: `LOCAL` / `NFS` / `SMB` |
| `transport_location(t, subpath)` | Build fptcli location URL for a transport + subpath |
| `transport_mount(t)` | Return the kernel mount path for a transport |
| `transport_available(t)` | Check if a transport is configured via env vars |
| `create_fileset()` | Create deterministic recursive fileset |
| `create_empty_dirs()` | Create named empty directories |
| `create_symlinks()` | Create relative/absolute/broken/chain symlinks |
| `create_hardlinks()` | Create hardlink groups (pairs, triples, deep) |
| `create_sparse_files()` | Create sparse files with data regions and holes |
| `create_special_filenames()` | Create files with unusual but valid names |
| `create_permission_files()` | Create files with various Unix permission modes |
| `file_hash()` | SHA256 hex digest of a file |
| `find_copy_dir()` | Find COPY_* subdirectory in backup output |
| `count_files()` / `count_dirs()` | Recursively count files/directories |
| `is_sparse()` | Check if a file occupies fewer blocks than apparent size |
| `skip_unless_linux` / `skip_unless_unix` | pytest skip markers for platform gating |

### `conftest.py` Fixtures

| Fixture | Scope | Description |
|---|---|---|
| `fptbin` | session | Resolves the binary directory path (checks `FPT_BIN_DIR`, then walks up looking for `target/release/fptcli`) |
| `tmp_workspace` | function | Creates a unique per-test workspace under `FPT_DATA_ROOT/local/<test_name>_<uuid>/` with `FptCli` initialized; auto-cleans after test unless `FPT_KEEP_ON_FAILURE=1` |

### Auto-Skip Logic

`pytest_collection_modifyitems` in `conftest.py` automatically adds skip markers:
- Tests with `nfs` in their keyword are skipped when `FPT_NFS_MOUNT` is not set.
- Tests with `smb` in their keyword are skipped when `FPT_SMB_MOUNT` is not set.

### Test Lifecycle

```
1. conftest.py creates tmp_workspace (UUID-isolated directory)
2. FptCli.__init__: creates source/ backup/ restore/ meta/ ctrl/ logs/
3. Test creates fileset -> backup -> restore -> fsdiff verification
4. Teardown: workspace is cleaned (preserved if FPT_KEEP_ON_FAILURE=1)
```

## Adding New Tests

```python
# tests/smoke/test_my_feature.py
from framework import FptCli, create_fileset, find_copy_dir


def test_my_feature(tmp_workspace: FptCli):
    """Test description."""
    fpt = tmp_workspace

    # 1. Create test data
    create_fileset(
        fpt.source_dir, depth=2, files_per_dir=5,
        dirs_per_dir=2, file_size=4096,
    )

    # 2. Run backup
    bk = fpt.backup(str(fpt.source_dir), str(fpt.backup_dir))
    fpt.assert_success(bk, "Backup failed: ")

    # 3. Find copy directory and restore
    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, "Restore failed: ")

    # 4. Verify
    fpt.assert_fsdiff_clean(str(fpt.source_dir), str(restore_target))
```

For platform-gated tests:

```python
import pytest
from framework import skip_unless_unix, Transport, transport_location

@pytest.mark.skipif(sys.platform == "win32", reason="Unix only")
def test_unix_feature(tmp_workspace: FptCli):
    ...
```

For transport tests that auto-skip:

```python
@pytest.mark.nfs  # auto-skipped if FPT_NFS_MOUNT not set
def test_nfs_feature(tmp_workspace: FptCli):
    ...
```

## CI Integration

```yaml
# .github/workflows/test.yml
name: Tests
on: [push, pull_request]

jobs:
  smoke:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo build --release
      - run: mkdir -p /tmp/fpt_test_data
      - run: python -m pytest tests/smoke/ -v --junit-xml=smoke-results.xml
        env:
          FPT_DATA_ROOT: /tmp/fpt_test_data
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: smoke-results
          path: smoke-results.xml
```
