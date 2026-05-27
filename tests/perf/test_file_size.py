"""Performance test: backup/restore across different file sizes.

Uses vdbench to generate filesets with files of size 1K, 128K, 1M, 10M.
Measures backup and restore duration.

Each sub-test should complete within 5 minutes.
"""

import time
import pytest
from pathlib import Path

from framework import FptCli, find_copy_dir, count_files


# (label, file_size, files_per_dir, dirs, depth, max_seconds)
# 1K:   1000*5 = 5000 files * 1K   = 5MB
# 128K:  200*5 = 1000 files * 128K  = 128MB
# 1M:    100*5 =  500 files * 1M    = 500MB
# 10M:    20*5 =  100 files * 10M   = 1GB
FILE_SIZE_CASES = [
    ("1K",   "1K",    1000, 5, 1, 120),
    ("128K", "128K",   200, 5, 1, 180),
    ("1M",   "1M",     100, 5, 1, 300),
    ("10M",  "10M",     20, 5, 1, 300),
]


@pytest.mark.parametrize(
    "label,size,files,dirs,depth,max_sec",
    FILE_SIZE_CASES,
    ids=[c[0] for c in FILE_SIZE_CASES],
)
def test_perf_file_size(
    tmp_workspace: FptCli,
    label: str, size: str, files: int, dirs: int, depth: int, max_sec: int,
):
    """Measure backup+restore time for a given file size profile."""
    fpt = tmp_workspace

    # generate fileset via vdbench
    fpt.vdbench(
        fpt.source_dir,
        depth=depth, files=files, dirs=dirs, size=size,
    )

    n = count_files(fpt.source_dir)
    assert n > 0, f"vdbench produced no files for size={size}"
    print(f"\n[{label}] {n} files x ~{size} generated")

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    # measure backup
    t0 = time.monotonic()
    bk = fpt.backup(source, target, timeout=max_sec)
    backup_time = time.monotonic() - t0
    fpt.assert_success(bk, f"Backup failed for {label}: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # measure restore
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    t0 = time.monotonic()
    rs = fpt.restore(str(copy_dir), str(restore_target), timeout=max_sec)
    restore_time = time.monotonic() - t0
    fpt.assert_success(rs, f"Restore failed for {label}: ")

    total_time = backup_time + restore_time
    print(f"[{label}] backup={backup_time:.2f}s  restore={restore_time:.2f}s  total={total_time:.2f}s")

    assert total_time < max_sec, (
        f"[{label}] Total time {total_time:.1f}s exceeded limit {max_sec}s"
    )
