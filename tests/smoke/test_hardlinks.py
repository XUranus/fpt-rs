"""Smoke test: hardlinks are preserved during backup/restore.

Uses fsscan with --scan-hardlinks and fsbackup with --hardlink.
Platform-aware: works on both Linux and Windows (NTFS).
"""

import os
import pytest
from pathlib import Path

from framework import FptCli, create_hardlinks, find_copy_dir, file_hash
from _platform import IS_WINDOWS


def test_hardlinks(tmp_workspace: FptCli):
    """Create hardlink groups, backup with hardlink support, verify."""
    fpt = tmp_workspace

    groups = create_hardlinks(fpt.source_dir)
    if not groups:
        pytest.skip("Hardlink creation not supported on this filesystem")

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
        # verify all links in source share the same content
        ref_hash = file_hash(paths[0])
        for p in paths[1:]:
            assert file_hash(p) == ref_hash, (
                f"Source hardlink group '{group_name}' content mismatch"
            )

        # On Unix, verify source links share the same inode
        if not IS_WINDOWS:
            src_inodes = [os.stat(p).st_ino for p in paths]
            assert len(set(src_inodes)) == 1, (
                f"Source hardlink group '{group_name}' should share inode"
            )

        # verify all links exist in backup and share content
        backup_paths = []
        for p in paths:
            rel = p.relative_to(fpt.source_dir)
            backup_p = fpt.backup_dir / rel
            assert backup_p.exists(), f"Hardlink missing in backup: {rel}"
            assert file_hash(backup_p) == ref_hash, (
                f"Content mismatch for hardlink: {rel}"
            )
            backup_paths.append(backup_p)

        # verify backup hardlinks share the same inode (not duplicated)
        # On Windows, st_ino may be 0 or unreliable, so only check on Unix
        if not IS_WINDOWS:
            bak_inodes = [os.stat(p).st_ino for p in backup_paths]
            assert len(set(bak_inodes)) == 1, (
                f"Backup hardlink group '{group_name}' should share inode, "
                f"but got {len(set(bak_inodes))} distinct inodes"
            )
        else:
            # On Windows, verify that backup doesn't have MORE files than source
            # (duplicates would mean each link is a separate file)
            total_source_files = sum(1 for _ in fpt.source_dir.rglob("*") if _.is_file())
            total_backup_files = sum(1 for _ in fpt.backup_dir.rglob("*")
                                     if _.is_file() and "COPY_" not in str(_))
            # Backup should have same file count as source (hardlinks not duplicated)
            assert total_backup_files <= total_source_files + 2, (
                f"Backup has too many files ({total_backup_files} vs source {total_source_files}), "
                f"hardlinks may be duplicated"
            )

    # fsdiff should pass
    fpt.assert_fsdiff_clean(source, str(fpt.backup_dir))
