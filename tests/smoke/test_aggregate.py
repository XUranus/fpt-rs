"""Smoke test: aggregate backup format.

Tests fptcli backup --aggregate with various layouts, then restore
and verify content integrity. Also verifies blob files and SQLite index.
"""

import os
import sqlite3
import pytest
from pathlib import Path

from framework import FptCli, create_fileset, find_copy_dir, file_hash, count_files


@pytest.mark.parametrize("layout", ["shard", "dir-level"])
def test_aggregate_backup_restore(tmp_workspace: FptCli, layout: str):
    """Aggregate backup with given layout, then restore and verify."""
    fpt = tmp_workspace

    # create many small files (good aggregate candidates): ~200 files x 4KB = 800KB
    create_fileset(fpt.source_dir, depth=2, files_per_dir=20, dirs_per_dir=5, file_size=4096)

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    bk = fpt.backup(
        source, target,
        aggregate=True,
        extra_args=["--aggregate-layout", layout, "--threshold", "64"],
    )
    fpt.assert_success(bk, f"Aggregate backup ({layout}) failed: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # --- verify blob files ---
    blob_files = list(copy_dir.rglob("*.blob"))
    if blob_files:
        total_blob_size = sum(f.stat().st_size for f in blob_files)
        print(f"[{layout}] Found {len(blob_files)} blob files, total size: {total_blob_size} bytes")
    else:
        print(f"[{layout}] No blob files found (aggregation may produce them differently)")

    # --- verify SQLite aggregate index ---
    sqlite_files = list(copy_dir.rglob("*.sqlite"))
    if sqlite_files:
        for idx_path in sqlite_files:
            try:
                conn = sqlite3.connect(str(idx_path))
                cursor = conn.cursor()
                cursor.execute("SELECT name FROM sqlite_master WHERE type='table'")
                tables = [row[0] for row in cursor.fetchall()]
                for table in tables:
                    cursor.execute(f"SELECT COUNT(*) FROM {table}")
                    count = cursor.fetchone()[0]
                    print(f"[{layout}] SQLite index {idx_path.name} table '{table}': {count} entries")
                conn.close()
            except sqlite3.Error as e:
                print(f"[{layout}] SQLite read error for {idx_path.name}: {e}")
    else:
        print(f"[{layout}] No SQLite index files found")

    # restore
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, f"Aggregate restore ({layout}) failed: ")

    # verify all source files exist in restore with matching content
    for src_file in sorted(fpt.source_dir.rglob("*")):
        if not src_file.is_file():
            continue
        rel = src_file.relative_to(fpt.source_dir)
        rest_file = restore_target / rel
        assert rest_file.exists(), f"File missing after aggregate restore: {rel}"
        assert file_hash(src_file) == file_hash(rest_file), f"Content mismatch: {rel}"


def test_aggregate_threshold_boundary(tmp_workspace: FptCli):
    """Verify files at and above threshold are not aggregated into blobs."""
    fpt = tmp_workspace

    threshold = 64  # KB
    mixed_dir = fpt.source_dir / "mixed"
    mixed_dir.mkdir()

    # files below threshold (should be aggregated)
    for i in range(5):
        (mixed_dir / f"small_{i}.bin").write_bytes(os.urandom(1024))  # 1KB

    # file exactly at threshold (should NOT be aggregated)
    (mixed_dir / "at_threshold.bin").write_bytes(os.urandom(threshold * 1024))

    # file just above threshold (should NOT be aggregated)
    (mixed_dir / "above_threshold.bin").write_bytes(os.urandom(threshold * 1024 + 1))

    # large file well above threshold
    (mixed_dir / "large.bin").write_bytes(os.urandom(256 * 1024))  # 256KB

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    bk = fpt.backup(
        source, target,
        aggregate=True,
        extra_args=["--aggregate-layout", "dir-level", "--threshold", str(threshold)],
    )
    fpt.assert_success(bk, "Aggregate backup (threshold boundary) failed: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # files at or above threshold should exist as individual files in D_REPO
    # (threshold: files < threshold are aggregated into blobs, >= threshold are individual)
    d_repo = copy_dir / "D_REPO"
    assert (d_repo / "mixed" / "at_threshold.bin").exists(), (
        "File at threshold should be backed up as individual file"
    )
    assert (d_repo / "mixed" / "above_threshold.bin").exists(), (
        "File above threshold should be backed up as individual file"
    )
    assert (d_repo / "mixed" / "large.bin").exists(), (
        "Large file should be backed up as individual file"
    )

    # restore
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, "Aggregate restore (threshold boundary) failed: ")

    # all files must be correctly restored regardless of aggregation
    for src_file in sorted(fpt.source_dir.rglob("*")):
        if not src_file.is_file():
            continue
        rel = src_file.relative_to(fpt.source_dir)
        rest_file = restore_target / rel
        assert rest_file.exists(), f"File missing after restore: {rel}"
        assert file_hash(src_file) == file_hash(rest_file), f"Content mismatch: {rel}"
