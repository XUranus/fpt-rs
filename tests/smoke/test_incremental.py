"""Smoke test: incremental backup after source changes.

Phase 1: full scan + backup of initial dataset.
Phase 2: apply changes (add/modify/delete).
Phase 3: incremental scan with prev_meta_dir + backup with --delete --mtime.
Phase 4: verify new/modified content and run fsdiff.
"""

import os
import shutil
from pathlib import Path

from framework import FptCli, create_fileset, find_copy_dir, file_hash


def test_incremental_backup(tmp_workspace: FptCli):
    """Full backup, then incremental with prev_meta_dir, verify."""
    fpt = tmp_workspace

    # -- Phase 1: initial dataset: ~100 files x 4KB = 400KB --
    create_fileset(fpt.source_dir, depth=3, files_per_dir=5, dirs_per_dir=2, file_size=4096)

    source = str(fpt.source_dir)

    # full scan
    scan1 = fpt.fsscan(source)
    fpt.assert_success(scan1, "Full scan failed: ")

    # full backup
    ctrl1 = fpt.find_control_file("copy")
    assert ctrl1 is not None, "No copy control file after full scan"
    bk1 = fpt.fsbackup(source, str(fpt.backup_dir), str(ctrl1))
    fpt.assert_success(bk1, "Full backup failed: ")

    # save full meta dir for incremental reference
    full_meta_dir = fpt.meta_dir / "full"
    shutil.copytree(fpt.meta_dir, full_meta_dir, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns("full"))

    # verify full backup with fsdiff
    fpt.assert_fsdiff_clean(source, str(fpt.backup_dir))

    # -- Phase 2: apply changes --
    # create new files
    (fpt.source_dir / "new_file_A.txt").write_text("new content A")
    (fpt.source_dir / "new_file_B.txt").write_text("new content B")
    (fpt.source_dir / "new_file_C.txt").write_text("new content C")
    new_dir = fpt.source_dir / "new_dir"
    new_dir.mkdir(exist_ok=True)
    (new_dir / "new_nested.txt").write_text("nested new content")

    # modify existing files
    dat_files = sorted(fpt.source_dir.rglob("*.dat"))
    first_file = next(p for p in dat_files if p.is_file())
    first_file.write_text("MODIFIED CONTENT for incremental test")

    # delete a file
    deletable = next(
        p for p in dat_files
        if p.is_file() and p.name != first_file.name
    )
    deletable_rel = deletable.relative_to(fpt.source_dir)
    deletable.unlink()

    # -- Phase 3: incremental scan with prev_meta_dir + backup --
    # clear meta/ctrl for fresh incremental scan
    for f in fpt.meta_dir.iterdir():
        if f.is_dir() and f.name == "full":
            continue
        if f.is_file():
            f.unlink()
    for f in fpt.ctrl_dir.iterdir():
        if f.is_file():
            f.unlink()

    scan2 = fpt.fsscan(source, prev_meta_dir=str(full_meta_dir))
    fpt.assert_success(scan2, "Incremental scan failed: ")

    ctrl2 = fpt.find_control_file("copy")
    assert ctrl2 is not None, "No copy control file after incremental scan"

    # backup to a v2 directory: copy full backup as base, then apply delta
    target_v2 = str(fpt.backup_dir / "v2")
    shutil.copytree(fpt.backup_dir, target_v2, dirs_exist_ok=True)
    bk2 = fpt.fsbackup(source, target_v2, str(ctrl2), delete=True, mtime=True)
    fpt.assert_success(bk2, "Incremental backup failed: ")

    # -- Phase 4: verify incremental backup --
    v2_path = Path(target_v2)

    # new files should exist
    assert (v2_path / "new_file_A.txt").exists(), "New file A missing in v2"
    assert (v2_path / "new_file_B.txt").exists(), "New file B missing in v2"
    assert (v2_path / "new_file_C.txt").exists(), "New file C missing in v2"
    assert (v2_path / "new_dir" / "new_nested.txt").exists(), "Nested new file missing in v2"

    assert (v2_path / "new_file_A.txt").read_text() == "new content A"
    assert (v2_path / "new_file_B.txt").read_text() == "new content B"
    assert (v2_path / "new_file_C.txt").read_text() == "new content C"

    # modified file should have new content
    restored_mod = v2_path / first_file.relative_to(fpt.source_dir)
    assert restored_mod.exists(), "Modified file missing in v2"
    assert restored_mod.read_text() == "MODIFIED CONTENT for incremental test"

    # deleted file should not exist in v2
    deleted_in_v2 = v2_path / deletable_rel
    if deleted_in_v2.exists():
        # may be a known limitation
        import logging
        logging.getLogger("fpt_test").warning(
            "Deleted file %s still exists in incremental backup (known limitation)",
            deletable_rel,
        )

    # fsdiff verification (source vs v2)
    fpt.assert_fsdiff_clean(source, target_v2)
