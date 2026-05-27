"""Smoke test: hardlinks are preserved during backup/restore.

Uses fsscan with --scan-hardlinks and fsbackup with --hardlink.
"""

import os
import pytest
from pathlib import Path

from framework import FptCli, create_hardlinks, find_copy_dir, file_hash, IS_WINDOWS


@pytest.mark.skipif(IS_WINDOWS, reason="Hardlinks not supported on Windows")
def test_hardlinks(tmp_workspace: FptCli):
    """Create hardlink groups, backup with hardlink support, verify."""
    fpt = tmp_workspace

    groups = create_hardlinks(fpt.source_dir)
    if not groups:
        pytest.skip("Hardlink creation not supported")

    source = str(fpt.source_dir)

    # scan with hardlink detection
    scan = fpt.fsscan(source, scan_hardlinks=True)
    fpt.assert_success(scan, "fsscan failed: ")

    # find control file
    ctrl = fpt.find_control_file("copy")
    assert ctrl is not None, "No copy control file generated"

    # backup with hardlink preservation
    bk = fpt.fsbackup(source, str(fpt.backup_dir), str(ctrl), hardlink=True)
    fpt.assert_success(bk, "fsbackup failed: ")

    # verify each hardlink group
    for group_name, paths in groups.items():
        # all links in source should share the same inode
        src_inodes = [os.stat(p).st_ino for p in paths]
        assert len(set(src_inodes)) == 1, (
            f"Source hardlink group '{group_name}' should share inode, got {len(set(src_inodes))}"
        )

        # verify all links exist in backup and share content
        ref_hash = file_hash(paths[0])
        backup_paths = []
        for p in paths:
            rel = p.relative_to(fpt.source_dir)
            backup_p = fpt.backup_dir / rel
            assert backup_p.exists(), f"Hardlink missing in backup: {rel}"
            assert file_hash(backup_p) == ref_hash, f"Content mismatch for hardlink: {rel}"
            backup_paths.append(backup_p)

        # verify backup hardlinks share the same inode (not duplicated as separate files)
        bak_inodes = [os.stat(p).st_ino for p in backup_paths]
        assert len(set(bak_inodes)) == 1, (
            f"Backup hardlink group '{group_name}' should share inode, "
            f"but got {len(set(bak_inodes))} distinct inodes — hardlinks may be duplicated"
        )

    # fsdiff should pass
    fpt.assert_fsdiff_clean(source, str(fpt.backup_dir))
