"""Smoke test: sparse files are backed up correctly.

Sparse file sparseness may not be preserved (platform-dependent),
but apparent size and content must match.
"""

import os
import pytest
from pathlib import Path

from framework import FptCli, create_sparse_files, find_copy_dir, is_sparse


def test_sparse_files(tmp_workspace: FptCli):
    """Create sparse files, backup, restore, verify sizes and content."""
    fpt = tmp_workspace

    sparse = create_sparse_files(fpt.source_dir)
    if not sparse:
        pytest.skip("Sparse file creation not supported")

    # create a non-sparse file for comparison
    non_sparse = fpt.source_dir / "non_sparse.dat"
    non_sparse.write_bytes(b"X" * (1024 * 1024))  # 1MB of actual data

    # record apparent sizes before backup
    sizes = {name: os.stat(p).st_size for name, p in sparse.items()}
    sizes["non_sparse.dat"] = os.stat(non_sparse).st_size

    # verify source sparse files actually have holes
    for name, p in sparse.items():
        if os.stat(p).st_size > 10 * 1024 * 1024:  # only check large files
            assert is_sparse(p), f"Source file {name} should be sparse but isn't"

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    bk = fpt.backup(source, target)
    fpt.assert_success(bk, "Backup failed: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # verify apparent sizes in backup match source
    d_repo = copy_dir / "D_REPO"
    for name, orig_size in sizes.items():
        bak_p = d_repo / name
        assert bak_p.exists(), f"File missing in backup: {name}"
        bak_size = os.stat(bak_p).st_size
        assert bak_size == orig_size, (
            f"Size mismatch for {name}: source={orig_size} backup={bak_size}"
        )

    # restore
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, "Restore failed: ")

    # verify apparent sizes match after restore
    for name, orig_size in sizes.items():
        restored_p = restore_target / name
        assert restored_p.exists(), f"Sparse file missing after restore: {name}"
        rest_size = os.stat(restored_p).st_size
        assert rest_size == orig_size, (
            f"Sparse file size mismatch for {name}: {orig_size} vs {rest_size}"
        )

    # verify content at data regions (first and last 4KB)
    for name, p in sparse.items():
        restored_p = restore_target / name
        orig_data = p.read_bytes()
        rest_data = restored_p.read_bytes()
        assert orig_data[:4096] == rest_data[:4096], f"Content mismatch at head: {name}"
        assert orig_data[-4096:] == rest_data[-4096:], f"Content mismatch at tail: {name}"

    # fsdiff
    fpt.assert_fsdiff_clean(source, str(restore_target))
