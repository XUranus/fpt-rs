"""Performance test: backup/restore across different directory depths.

Tests 1-layer, 4-layer, and 10-layer deep directory structures
with files spread across them.

Each sub-test should complete within 5 minutes.
"""

import time
import pytest
from pathlib import Path

from framework import FptCli, find_copy_dir, count_files, count_dirs


# (label, depth, files_per_dir, dirs_per_dir, file_size, max_seconds)
# depth=1:  f*r = 200*5 = 1000 files * 64K = 64MB
# depth=4:  sum(d^i * f * r) for i=0..4, f=30, r=3 -> 30+90+270+810+2430 = 3630 files * 8K ≈ 29MB
# depth=10: sum(2^i * 8 * 2) for i=0..10 = 8*(2^11-1) = 16376 files * 2K ≈ 33MB
DEPTH_CASES = [
    ("depth_1",  1,  200, 5, "64K", 120),
    ("depth_4",  4,   30, 3, "8K",  180),
    ("depth_10", 10,   8, 2, "2K",  300),
]


@pytest.mark.parametrize(
    "label,depth,files,dirs,fsize,max_sec",
    DEPTH_CASES,
    ids=[c[0] for c in DEPTH_CASES],
)
def test_perf_directory_depth(
    tmp_workspace: FptCli,
    label: str, depth: int, files: int, dirs: int, fsize: str, max_sec: int,
):
    """Measure backup+restore time for different directory tree depths."""
    fpt = tmp_workspace

    fpt.vdbench(
        fpt.source_dir,
        depth=depth, files=files, dirs=dirs, size=fsize,
    )

    n_files = count_files(fpt.source_dir)
    n_dirs = count_dirs(fpt.source_dir)
    assert n_files > 0, f"vdbench produced no files for depth={depth}"
    print(f"\n[{label}] {n_files} files, {n_dirs} dirs, depth={depth}")

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
