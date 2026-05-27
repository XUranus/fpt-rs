"""Performance test: backup/restore across different fileset scales.

Tests 1K, 10K, and 50K file filesets using vdbench.
Total data per test stays under 2GB.

Each sub-test should complete within 5 minutes.
"""

import time
import pytest
from pathlib import Path

from framework import FptCli, find_copy_dir, count_files


# (label, depth, files_per_dir, dirs_per_dir, file_size, max_seconds)
# files = sum(d^i * f * r for i in 0..depth) where f=files_per_dir, r=dirs_per_dir
#
# 1K:  depth=2, f=100, r=5 -> 100 + 500 + 2500 = 3100 files * 32K ≈ 99MB
# 10K: depth=3, f=50,  r=5 -> 50+250+1250+6250 = 7800 files * 8K ≈ 62MB
# 50K: depth=4, f=100, r=5 -> 100+500+2500+12500+62500 = 78100 files * 1K ≈ 78MB
SCALE_CASES = [
    ("1K_files",    2,  100, 5, "32K", 120),
    ("10K_files",   3,   50, 5, "8K",  180),
    ("50K_files",   4,  100, 5, "1K",  300),
]


@pytest.mark.parametrize(
    "label,depth,files,dirs,fsize,max_sec",
    SCALE_CASES,
    ids=[c[0] for c in SCALE_CASES],
)
def test_perf_fileset_scale(
    tmp_workspace: FptCli,
    label: str, depth: int, files: int, dirs: int, fsize: str, max_sec: int,
):
    """Measure backup+restore time for different fileset scales."""
    fpt = tmp_workspace

    fpt.vdbench(
        fpt.source_dir,
        depth=depth, files=files, dirs=dirs, size=fsize,
    )

    n = count_files(fpt.source_dir)
    assert n > 0, f"vdbench produced no files for scale={label}"
    print(f"\n[{label}] {n} files generated")

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
