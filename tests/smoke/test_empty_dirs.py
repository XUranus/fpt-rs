"""Smoke test: empty directories are preserved during backup/restore."""

import pytest
from pathlib import Path

from framework import FptCli, create_empty_dirs, find_copy_dir, count_files, count_dirs


def test_empty_directories(tmp_workspace: FptCli):
    """Verify that empty directories survive backup and restore."""
    fpt = tmp_workspace

    dir_names = [
        "empty1",
        "empty2",
        "nested/empty_child",
        "deep/level1/level2/empty_leaf",
        "sibling1",
        "sibling2",
        "parent/child1/empty",
        "parent/child2/empty",
    ]
    created = create_empty_dirs(fpt.source_dir, dir_names)

    # non-empty directory for contrast
    non_empty = fpt.source_dir / "non_empty"
    non_empty.mkdir(parents=True, exist_ok=True)
    (non_empty / "file.txt").write_text("content")

    # mixed directory: empty subdir alongside non-empty sibling in same parent
    mixed = fpt.source_dir / "mixed"
    mixed.mkdir(parents=True, exist_ok=True)
    (mixed / "empty_subdir").mkdir()
    (mixed / "with_file").mkdir()
    (mixed / "with_file" / "data.txt").write_text("data content")

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    bk = fpt.backup(source, target)
    fpt.assert_success(bk, "Backup failed: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # verify empty dirs exist in backup D_REPO
    d_repo = copy_dir / "D_REPO"
    for d in created:
        rel = d.relative_to(fpt.source_dir)
        backup_d = d_repo / rel
        assert backup_d.is_dir(), f"Empty dir missing in backup: {rel}"
        assert count_files(backup_d) == 0, f"Dir should be empty: {rel}"

    # verify mixed directory: empty_subdir preserved, with_file has content
    assert (d_repo / "mixed" / "empty_subdir").is_dir(), "Empty subdir in mixed dir missing"
    assert count_files(d_repo / "mixed" / "empty_subdir") == 0, "Empty subdir should remain empty"
    assert (d_repo / "mixed" / "with_file" / "data.txt").exists(), "File in mixed dir missing"

    # restore and verify
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, "Restore failed: ")

    for d in created:
        rel = d.relative_to(fpt.source_dir)
        restored_d = restore_target / rel
        assert restored_d.is_dir(), f"Empty dir missing after restore: {rel}"
        assert count_files(restored_d) == 0, f"Restored dir should be empty: {rel}"

    # non-empty dir should still have its file
    assert (restore_target / "non_empty" / "file.txt").read_text() == "content"

    # mixed dir after restore
    assert (restore_target / "mixed" / "empty_subdir").is_dir()
    assert count_files(restore_target / "mixed" / "empty_subdir") == 0
    assert (restore_target / "mixed" / "with_file" / "data.txt").read_text() == "data content"

    # fsdiff verification
    fpt.assert_fsdiff_clean(source, str(restore_target))
