"""Smoke test: Linux extended attribute preservation (Linux only).

Requires xattr support on the filesystem.
"""

import subprocess
import shutil
import pytest
from pathlib import Path

from framework import FptCli, skip_unless_linux


def _has_setfattr() -> bool:
    return shutil.which("setfattr") is not None


@pytest.mark.skipif(not _has_setfattr(), reason="setfattr not available")
@skip_unless_linux
def test_linux_xattr(tmp_workspace: FptCli):
    """Create files with xattrs, scan with --scan-xattrs, verify with fsdiff."""
    fpt = tmp_workspace

    attrs = {
        "user.comment": "test comment",
        "user.checksum": "abc123",
        "user.version": "1.0",
        "user.empty": "",
    }

    file1 = fpt.source_dir / "xattr_file.txt"
    file1.write_text("xattr test content")

    # set xattrs
    for name, value in attrs.items():
        try:
            subprocess.run(
                ["setfattr", "-n", name, "-v", value, str(file1)],
                check=True, capture_output=True,
            )
        except subprocess.CalledProcessError as e:
            pytest.skip(f"Failed to set xattr {name}: {e.stderr.decode(errors='replace')}")

    source = str(fpt.source_dir)

    # scan with xattr support
    scan = fpt.fsscan(source, scan_xattrs=True)
    fpt.assert_success(scan, "fsscan --scan-xattrs failed: ")

    # backup
    ctrl = fpt.find_control_file("copy")
    assert ctrl is not None

    bk = fpt.fsbackup(source, str(fpt.backup_dir), str(ctrl))
    fpt.assert_success(bk, "fsbackup failed: ")

    # verify file exists in backup
    assert (fpt.backup_dir / "xattr_file.txt").exists()

    # fsdiff compare with xattr checking
    fpt.assert_fsdiff_clean(source, str(fpt.backup_dir), compare_xattrs=True)
