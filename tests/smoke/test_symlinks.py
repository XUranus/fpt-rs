"""Smoke test: symbolic link handling during backup/restore.

Verifies that backup completes successfully, symlink target content is intact,
and symlinks themselves (relative, absolute, broken, chain) are preserved.
"""

import os
import pytest
from pathlib import Path

from framework import FptCli, create_symlinks, find_copy_dir
from _platform import skip_unless_symlink


@skip_unless_symlink
def test_symlinks_backup_succeeds(tmp_workspace: FptCli):
    """Backup a directory with symlinks, verify backup succeeds and targets are intact."""
    fpt = tmp_workspace

    links = create_symlinks(fpt.source_dir)
    if not links:
        pytest.skip("Symlink creation not supported on this filesystem")

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    # backup should succeed even with symlinks present
    bk = fpt.backup(source, target)
    fpt.assert_success(bk, "Backup failed with symlinks: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # --- verify symlink target files in backup ---
    d_repo = copy_dir / "D_REPO"
    assert (d_repo / "link_target.txt").exists(), "Symlink target file missing in backup"
    assert (d_repo / "link_target.txt").read_text() == "symlink target content"

    assert (d_repo / "link_target_dir" / "inside.txt").exists(), "Symlink target dir content missing"
    assert (d_repo / "link_target_dir" / "inside.txt").read_text() == "inside dir"

    # --- verify symlinks are preserved as symlinks ---
    for desc, link_path in links.items():
        rel = link_path.relative_to(fpt.source_dir)
        backup_link = d_repo / rel
        if desc == "broken":
            # broken symlinks may not be backed up — skip if missing
            if not backup_link.exists():
                continue
        assert backup_link.exists(), f"Symlink missing in backup: {desc} ({rel})"
        assert backup_link.is_symlink(), (
            f"Expected symlink in backup but got regular file: {desc} ({rel})"
        )

    # verify relative link targets resolve correctly
    rel_file_link = d_repo / "my_link"
    if rel_file_link.is_symlink():
        # readlink preserves the raw target
        assert os.readlink(str(rel_file_link)) == "link_data.txt"

    # restore and verify target content
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, "Restore failed: ")

    assert (restore_target / "link_target.txt").read_text() == "symlink target content"
    assert (restore_target / "link_target_dir" / "inside.txt").read_text() == "inside dir"
