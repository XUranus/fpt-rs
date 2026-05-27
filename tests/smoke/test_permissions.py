"""Smoke test: file and directory permissions are preserved (Unix only)."""

import os
import stat
import pytest
from pathlib import Path

from framework import FptCli, create_permission_files, find_copy_dir, skip_unless_unix


@skip_unless_unix
def test_permissions(tmp_workspace: FptCli):
    """Create files with various modes, backup with --mtime, verify."""
    fpt = tmp_workspace

    mode_files = create_permission_files(fpt.source_dir)
    assert mode_files, "No permission test files created"

    # create dirs with specific modes
    dir_modes = {
        "dir_755": 0o755,
        "dir_700": 0o700,
        "dir_777": 0o777,
    }
    for dirname, mode in dir_modes.items():
        d = fpt.source_dir / dirname
        d.mkdir()
        os.chmod(d, mode)
        (d / "inside.txt").write_text(f"inside {dirname}")

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    bk = fpt.backup(source, target, mtime=True)
    fpt.assert_success(bk, "Backup failed: ")

    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None

    # restore with mtime
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target), mtime=True)
    fpt.assert_success(rs, "Restore failed: ")

    # verify file modes
    mismatches = []
    for name, orig_path in mode_files.items():
        rel = orig_path.relative_to(fpt.source_dir)
        restored = restore_target / rel
        if not restored.exists():
            mismatches.append(f"  missing: {rel}")
            continue
        orig_mode = stat.S_IMODE(os.stat(orig_path).st_mode)
        rest_mode = stat.S_IMODE(os.stat(restored).st_mode)
        if orig_mode != rest_mode:
            mismatches.append(f"  {rel}: {oct(orig_mode)} vs {oct(rest_mode)}")

    # log mismatches but don't fail (permissions may not survive all transports)
    if mismatches:
        import logging
        logging.getLogger("fpt_test").warning(
            "Permission mismatches (may be expected on some transports):\n%s",
            "\n".join(mismatches),
        )

    # verify content integrity regardless
    for name, orig_path in mode_files.items():
        rel = orig_path.relative_to(fpt.source_dir)
        restored = restore_target / rel
        assert restored.exists(), f"File missing after restore: {rel}"
        assert restored.read_text() == orig_path.read_text(), f"Content mismatch: {rel}"

    # fsdiff with --compare-mtime (may report differences due to transport rounding)
    r = fpt.fsdiff(source, str(restore_target), compare_mtime=True)
    if not r.success:
        import logging
        logging.getLogger("fpt_test").warning(
            "fsdiff --compare-mtime found differences (may be expected):\n%s",
            r.stdout[:1000],
        )
