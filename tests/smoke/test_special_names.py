"""Smoke test: files with special characters in names."""

import pytest
from pathlib import Path

from framework import FptCli, create_special_filenames, find_copy_dir, file_hash


def test_special_filenames(tmp_workspace: FptCli):
    """Backup and restore files with dots, dashes, uppercase, etc."""
    fpt = tmp_workspace

    created = create_special_filenames(fpt.source_dir)
    assert created, "No special-name files created"

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    bk = fpt.backup(source, target)
    fpt.assert_success(bk, "Backup failed: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # restore
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, "Restore failed: ")

    # verify each file exists and matches
    for p in created:
        rel = p.relative_to(fpt.source_dir)
        restored = restore_target / rel
        assert restored.exists(), f"File missing after restore: {rel}"
        assert file_hash(p) == file_hash(restored), f"Content mismatch: {rel}"

    # fsdiff
    fpt.assert_fsdiff_clean(source, str(restore_target))
