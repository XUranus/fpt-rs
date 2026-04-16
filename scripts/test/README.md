# Bifrost Test Suite

Cross-platform Python test suite for Bifrost backup/restore functionality.

## Quick Start

```bash
# Run all tests
python scripts/test/test_all.py

# Run with verbose output
python scripts/test/test_all.py -v

# Run specific test suite
python scripts/test/test_all.py --suite basic

# Run specific test
python scripts/test/test_all.py --test basic --test hardlinks

# Keep work directories on failure for debugging
python scripts/test/test_all.py --keep-on-failure

# Specify working directory
python scripts/test/test_all.py -w /tmp/bifrost_tests
```

## Test Structure

### Test Framework (`test_framework.py`)

Base classes and utilities:
- `BifrostTestBase`: Base class for all tests
- `TestRunner`: Runs tests and collects results
- `TestResult`: Data class for test results

### Test Cases

| Test File | Description |
|-----------|-------------|
| `test_basic_backup.py` | Basic file backup and restore |
| `test_incremental_backup.py` | Full + incremental backup workflow |
| `test_special_files.py` | Symlinks, special names, empty files |
| `test_hardlinks.py` | Hardlink preservation |
| `test_sparse_files.py` | Sparse file handling |
| `test_permissions.py` | File permissions and timestamps |
| `test_empty_directories.py` | Empty directory preservation |
| `test_large_fileset.py` | Scalability with many files |
| `test_linux_xattr.py` | Linux extended attributes (Linux only) |
| `test_linux_acl.py` | Linux ACLs (Linux only) |

### Test Suites

- **basic**: `test_basic_backup`, `test_empty_directories`, `test_special_files`
- **intermediate**: `test_permissions`, `test_hardlinks`, `test_sparse_files`
- **advanced**: `test_incremental_backup`
- **scalability**: `test_large_fileset`
- **all**: All tests including platform-specific ones

## Running Individual Tests

```bash
# Run single test directly
python scripts/test/test_basic_backup.py -v

# Run with custom work directory
python scripts/test/test_basic_backup.py -w /tmp/my_test

# Large fileset test with custom parameters
python scripts/test/test_large_fileset.py --files 2000 --dirs 200 --size 2048
```

## Writing New Tests

1. Create a new file `test_your_feature.py`
2. Inherit from `BifrostTestBase`
3. Implement `run_test()` method
4. Return `TestResult` with pass/fail status

Example:

```python
from test_framework import BifrostTestBase, TestResult

class TestYourFeature(BifrostTestBase):
    def run_test(self) -> TestResult:
        # Setup test data
        (self.source_dir / "test.txt").write_text("content")

        # Run scan
        if not self.run_fsscan(self.source_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Scan failed: {self.error_message}"
            )

        # Run backup
        if not self.run_fsbackup(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Backup failed: {self.error_message}"
            )

        # Verify
        if not self.run_fsdiff(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="Diff failed"
            )

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message="Test passed"
        )
```

## Platform-Specific Tests

Platform-specific tests check the platform at runtime and skip if not applicable:

```python
def is_linux():
    return sys.platform.startswith("linux")

def run_test(self) -> TestResult:
    if not is_linux():
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message="Skipped - not running on Linux"
        )
    # ... test code
```

## Requirements

- Python 3.7+
- Bifrost binaries built (`cargo build --release`)
- Linux: `setfattr`, `getfattr`, `setfacl`, `getfacl` for xattr/ACL tests

## Troubleshooting

### Binaries not found

Ensure Bifrost is built:
```bash
cargo build --release
```

### Permission denied for xattr/ACL tests

Run with appropriate permissions:
```bash
sudo python scripts/test/test_linux_xattr.py
```

### Test fails but directory already cleaned

Run with `--keep-on-failure` to preserve work directories for debugging.

## Test Output

On failure, the test framework reports:
- Test name and status
- Error message
- Work directory path (if `--keep-on-failure` is used)

Example output:
```
============================================================
Running: TestBasicBackup
============================================================
Creating test files...
  Created 6 files in source directory

Step 1: Scanning source directory...
Step 2: Running backup...
Step 3: Verifying backup with fsdiff...
Step 4: Verifying file contents...
  Directories match

Result: PASSED (2.34s)
```
