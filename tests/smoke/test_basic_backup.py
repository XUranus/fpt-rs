"""Smoke test: basic backup and restore round-trip.

Covers normal files across nested directories using fptcli backup/restore
and verifies with fsdiff.
"""

from pathlib import Path

from framework import FptCli, create_fileset, find_copy_dir


def test_basic_backup_restore(tmp_workspace: FptCli):
    """Backup a small fileset, restore it, and verify with fsdiff."""
    fpt = tmp_workspace

    # create source data: ~100 files, 4KB each, 3 levels deep (~400KB total)
    create_fileset(fpt.source_dir, depth=3, files_per_dir=5, dirs_per_dir=2, file_size=4096)

    source = str(fpt.source_dir)
    target = str(fpt.backup_dir)

    # backup
    bk = fpt.backup(source, target)
    fpt.assert_success(bk, "Backup failed: ")

    # find copy dir
    copy_dir = find_copy_dir(fpt.backup_dir)
    assert copy_dir is not None, f"No COPY_* directory found in {fpt.backup_dir}"

    # verify backup D_REPO matches source via fsdiff
    fpt.assert_fsdiff_clean(source, str(copy_dir / "D_REPO"))

    # restore
    restore_target = fpt.restore_dir / "restored"
    restore_target.mkdir()
    rs = fpt.restore(str(copy_dir), str(restore_target))
    fpt.assert_success(rs, "Restore failed: ")

    # verify restore matches source
    fpt.assert_fsdiff_clean(source, str(restore_target))
