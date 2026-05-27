"""Smoke test: Linux ACL preservation (Linux only).

Requires setfacl/getfacl utilities.
"""

import shutil
import subprocess
import pytest
from pathlib import Path

from framework import FptCli, skip_unless_linux, find_copy_dir


def _has_setfacl() -> bool:
    return shutil.which("setfacl") is not None


@pytest.mark.skipif(not _has_setfacl(), reason="setfacl not available")
@skip_unless_linux
def test_linux_acl(tmp_workspace: FptCli):
    """Create files with ACLs, scan with --scan-acl, verify with fsdiff --compare-acl."""
    fpt = tmp_workspace

    # create test files
    file1 = fpt.source_dir / "acl_file.txt"
    file1.write_text("ACL test content")
    file2 = fpt.source_dir / "acl_file2.txt"
    file2.write_text("ACL test content 2")

    # set ACLs
    try:
        subprocess.run(
            ["setfacl", "-m", "u:root:r-x", str(file1)],
            check=True, capture_output=True,
        )
        subprocess.run(
            ["setfacl", "-m", "u:root:r--,g::r-x", str(file2)],
            check=True, capture_output=True,
        )
    except subprocess.CalledProcessError as e:
        pytest.skip(f"Failed to set ACL: {e.stderr.decode(errors='replace')}")

    source = str(fpt.source_dir)

    # scan with ACL support
    scan = fpt.fsscan(source, scan_acl=True)
    fpt.assert_success(scan, "fsscan --scan-acl failed: ")

    # backup
    ctrl = fpt.find_control_file("copy")
    assert ctrl is not None

    bk = fpt.fsbackup(source, str(fpt.backup_dir), str(ctrl))
    fpt.assert_success(bk, "fsbackup failed: ")

    # verify files exist in backup
    assert (fpt.backup_dir / "acl_file.txt").exists()
    assert (fpt.backup_dir / "acl_file2.txt").exists()

    # fsdiff compare with ACL checking
    fpt.assert_fsdiff_clean(source, str(fpt.backup_dir), compare_acl=True)
