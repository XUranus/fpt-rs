#!/usr/bin/env python3
"""
NFS Backup / Restore Test Cases
================================
Tests three backup directions against a live NFSv3 export:

  TC1 – NFS source  → local-FS target
        Scan the NFS export via the local kernel mount, run the BIO copy
        pipeline to a temporary local directory, verify with fsdiff.

  TC2 – local-FS source → NFS target
        Seed a fresh dataset on the local FS, scan it, push it to the NFS
        server via the AIO pipeline (fsbackup --nfs-target-*), verify the
        result by reading back through the local kernel mount.

  TC3 – NFS source → NFS target (same server, different sub-paths)
        Scan an existing NFS dataset (ds2) via the local kernel mount, copy
        it to a separate sub-path on the same NFS server via the AIO pipeline,
        verify via local mount.

Prerequisites
-------------
  - An NFSv3 export reachable at NFS_HOST:NFS_EXPORT (default 127.0.0.1:/opt/dataset).
  - The export is also accessible as a local filesystem path (default /mnt/nfs).
  - The export contains sub-directories `ds1` and `ds2` with real data.
  - fsbackup/fsscan/fsdiff must be built with `--features nfs`.

Usage
-----
  # Run all three test cases:
  python test_nfs_backup.py

  # Run a single test case:
  python test_nfs_backup.py --tc tc1
  python test_nfs_backup.py --tc tc2
  python test_nfs_backup.py --tc tc3

  # Override NFS coordinates:
  python test_nfs_backup.py --nfs-host 192.168.1.10 \
                             --nfs-export /export/data \
                             --local-mount /mnt/nfsdata

  # Keep the work directory even on success (useful for debugging):
  python test_nfs_backup.py --keep-on-failure -v
"""

import sys
import os
import shutil
import hashlib
import argparse
import subprocess
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


# ---------------------------------------------------------------------------
# Default NFS coordinates — can be overridden via CLI or constructor kwargs
# ---------------------------------------------------------------------------
DEFAULT_NFS_HOST   = "127.0.0.1"
DEFAULT_NFS_EXPORT = "/opt/dataset"
DEFAULT_LOCAL_MOUNT = "/mnt/nfs"


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

def seed_dataset(root: Path) -> int:
    """
    Create a small, deterministic dataset under *root*.
    Returns the number of regular files created.
    """
    dirs = [
        root / "docs",
        root / "data" / "sub",
        root / "config" / "nested",
    ]
    for d in dirs:
        d.mkdir(parents=True, exist_ok=True)

    files = [
        ("readme.txt",          b"NFS backup test\n" * 64),
        ("docs/design.md",      b"# Design\n\nContent.\n" * 64),
        ("docs/notes.txt",      b"Note A\nNote B\n" * 128),
        ("data/binary.bin",     bytes(range(256)) * 128),
        ("data/sub/item.txt",   b"sub-item\n" * 64),
        ("config/app.conf",     b"key=value\nfoo=bar\n" * 64),
        ("config/nested/x.cfg", b"[section]\nparam=1\n" * 64),
        ("empty.txt",           b""),
    ]
    for rel, content in files:
        (root / rel).write_bytes(content)

    return len(files)


def collect_files(root: Path) -> dict:
    """
    Return {relative_path_str: file_size_bytes} for every regular file
    under *root*.  Symlinks are skipped.
    """
    result = {}
    for entry in root.rglob("*"):
        if entry.is_file() and not entry.is_symlink():
            result[str(entry.relative_to(root))] = entry.stat().st_size
    return result


def verify_directories(src: Path, dst: Path) -> tuple:
    """
    Compare two directory trees by (relative path, byte size).
    Returns (True, message) or (False, error_message).
    """
    src_files = collect_files(src)
    dst_files = collect_files(dst)

    missing = set(src_files) - set(dst_files)
    if missing:
        return False, f"{len(missing)} file(s) missing in target, e.g. {next(iter(missing))}"

    extra = set(dst_files) - set(src_files)
    if extra:
        return False, f"{len(extra)} unexpected file(s) in target, e.g. {next(iter(extra))}"

    for path, src_size in src_files.items():
        dst_size = dst_files[path]
        if src_size != dst_size:
            return False, f"size mismatch for {path!r}: {src_size} vs {dst_size}"

    return True, f"{len(src_files)} file(s) match"


# ---------------------------------------------------------------------------
# Base class that adds NFS-aware helpers on top of BifrostTestBase
# ---------------------------------------------------------------------------

class NfsBackupTestBase(BifrostTestBase):
    """
    Extends BifrostTestBase with helpers for NFS-related backup tests.

    Additional constructor kwargs
    -----------------------------
    nfs_host      : str  – NFS server IP / hostname
    nfs_export    : str  – Export path on the server
    local_mount   : str  – Local kernel-mount point of the export
    """

    def __init__(self, work_dir=None, verbose=False, keep_logs=False,
                 nfs_host=DEFAULT_NFS_HOST,
                 nfs_export=DEFAULT_NFS_EXPORT,
                 local_mount=DEFAULT_LOCAL_MOUNT):
        super().__init__(work_dir=work_dir, verbose=verbose, keep_logs=keep_logs)
        self.nfs_host    = nfs_host
        self.nfs_export  = nfs_export
        self.local_mount = Path(local_mount)

    # ------------------------------------------------------------------ setup

    def setup(self) -> bool:
        if not super().setup():
            return False

        # Check the local mount is accessible
        if not self.local_mount.exists():
            self.error_message = (
                f"Local NFS mount not found at {self.local_mount}.\n"
                f"Mount the export first, e.g.:\n"
                f"  sudo mount -t nfs {self.nfs_host}:{self.nfs_export} {self.local_mount}"
            )
            return False

        # Verify fsbackup binary exists
        if "fsbackup" not in self.binaries:
            self.error_message = "fsbackup binary not found; build with: cargo build --features nfs"
            return False

        return True

    # ------------------------------------------------------------------ helpers

    def run_fsscan_on(self, source: Path, extra_args=None) -> bool:
        """Scan *source* (a local-accessible path, may be NFS-mounted)."""
        return self.run_fsscan(source, extra_args=extra_args)

    def run_fsbackup_local(self, source: Path, target: Path,
                           ctrl_file: Path = None,
                           enable_hardlink=False,
                           enable_delete=False,
                           enable_mtime=False) -> bool:
        """Run the BIO pipeline: local source → local target."""
        return self.run_fsbackup(
            source, target,
            control_file=ctrl_file,
            enable_hardlink=enable_hardlink,
            enable_delete=enable_delete,
            enable_mtime=enable_mtime,
        )

    def run_fsbackup_to_nfs(self, source: Path,
                             nfs_sub_path: str,
                             ctrl_file: Path = None,
                             enable_hardlink=False,
                             enable_delete=False,
                             enable_mtime=False,
                             connections: int = 4) -> bool:
        """
        Run the AIO pipeline: local source → NFS target.

        Files are written to <nfs_export>/<nfs_sub_path> via NFSv3 RPCs.
        The local --target-dir is set to a throw-away placeholder directory;
        it is not used when an NFS target is configured.
        """
        placeholder = self.work_dir / "_nfs_placeholder"
        placeholder.mkdir(parents=True, exist_ok=True)

        if ctrl_file is None:
            ctrl_file = self.get_primary_control_file("copy")
            if ctrl_file is None:
                self.error_message = f"No copy control file found under {self.ctrl_dir}"
                return False

        cmd = [
            str(self.binaries["fsbackup"]),
            "-s", str(source),
            "-t", str(placeholder),          # ignored by the AIO path
            "-m", str(self.meta_dir),
            "-c", str(ctrl_file),
            "--nfs-target-host",    self.nfs_host,
            "--nfs-target-export",  self.nfs_export,
            "--nfs-target-sub-path", nfs_sub_path,
            "--nfs-target-connections", str(connections),
        ]

        if enable_hardlink or enable_delete or enable_mtime:
            cmd.extend(["--ctrl-dir", str(self.ctrl_dir)])
        if enable_hardlink:
            cmd.append("--hardlink")
        if enable_delete:
            cmd.append("--delete")
        if enable_mtime:
            cmd.append("--mtime")
        if self.verbose:
            cmd.append("-v")

        if self.verbose:
            print(f"  Running: {' '.join(cmd)}")

        try:
            rc, stdout, stderr = self.run_command(cmd, capture=True)
            if self.fsbackup_log_file:
                with open(self.fsbackup_log_file, "w") as f:
                    f.write(f"Command: {' '.join(cmd)}\nReturn code: {rc}\n")
                    f.write(f"\n--- STDOUT ---\n{stdout or '(empty)'}\n")
                    f.write(f"\n--- STDERR ---\n{stderr or '(empty)'}\n")
            if rc != 0:
                self.error_message = f"fsbackup (NFS target) failed (rc={rc}): {stderr}"
                return False
            return True
        except subprocess.CalledProcessError as e:
            self.error_message = f"fsbackup exception: {e.stderr}"
            return False

    def nfs_path(self, *parts) -> Path:
        """Return a path inside the local NFS mount."""
        return self.local_mount.joinpath(*parts)

    def clean_nfs_subdir(self, sub_path: str) -> bool:
        """Remove a sub-directory from the NFS export via the local mount."""
        p = self.nfs_path(sub_path)
        if p.exists():
            try:
                shutil.rmtree(p)
            except OSError as e:
                self.error_message = f"Failed to clean NFS sub-dir {p}: {e}"
                return False
        return True


# ---------------------------------------------------------------------------
# TC1: NFS source → local-FS target
# ---------------------------------------------------------------------------

class TestNfsSourceToLocal(NfsBackupTestBase):
    """
    TC1 – Backup FROM an NFS source TO a local filesystem target.

    The NFS export is accessed through the local kernel mount (VFS layer),
    so the scanner treats it like any other local directory.  The BIO copy
    pipeline writes the backed-up files into a temporary local directory.

    Pass/fail criteria
    ------------------
    - fsscan must complete without errors.
    - fsbackup BIO pipeline must complete without errors.
    - fsdiff must report no differences between the NFS source and the
      local backup target.
    - File count and byte sizes must match (direct directory comparison).
    """

    def run_test(self) -> TestResult:
        # The pre-existing `ds1` dataset on the NFS export is the source.
        nfs_src = self.nfs_path("ds1")
        if not nfs_src.exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"NFS dataset ds1 not found at {nfs_src}",
            )

        # ── Step 1: scan the NFS source via local mount ───────────────────
        print("\n  [1/3] Scanning NFS source (ds1) via local mount …")
        if not self.run_fsscan_on(nfs_src):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"fsscan failed: {self.error_message}",
            )
        ctrl_file = self.get_primary_control_file("copy")
        if ctrl_file is None:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="copy control file was not created by fsscan",
            )
        print(f"    control file: {ctrl_file}")

        # ── Step 2: BIO copy to local target ─────────────────────────────
        print("  [2/3] BIO copy to local target …")
        if not self.run_fsbackup_local(nfs_src, self.backup_dir,
                                        ctrl_file=ctrl_file,
                                        enable_mtime=True):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"fsbackup failed: {self.error_message}",
            )

        # ── Step 3: verify ────────────────────────────────────────────────
        print("  [3/3] Verifying with fsdiff …")
        if not self.run_fsdiff(nfs_src, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"fsdiff failed: {self.error_message}",
            )

        ok, msg = verify_directories(nfs_src, self.backup_dir)
        if not ok:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"directory comparison failed: {msg}",
            )
        print(f"    {msg}")

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message=msg,
            details={
                "nfs_source": str(nfs_src),
                "local_target": str(self.backup_dir),
            },
        )


# ---------------------------------------------------------------------------
# TC2: local-FS source → NFS target
# ---------------------------------------------------------------------------

class TestLocalSourceToNfs(NfsBackupTestBase):
    """
    TC2 – Backup FROM a local filesystem source TO an NFS target (AIO pipeline).

    A deterministic dataset is seeded inside the test's work directory.
    The AIO pipeline connects to the NFS server via NFSv3 RPCs (no kernel
    mount on the target side) and writes files directly using WRITE RPCs.

    The result is verified by reading back through the local kernel mount
    and comparing file paths and byte sizes against the original source.

    Pass/fail criteria
    ------------------
    - fsscan must complete without errors.
    - fsbackup AIO pipeline must complete with zero failed files.
    - All files written to NFS must be visible at local_mount/<nfs_sub_path>
      with the correct sizes.
    """

    NFS_SUB_PATH = "bifrost_tc2"

    def run_test(self) -> TestResult:
        nfs_dst_local = self.nfs_path(self.NFS_SUB_PATH)

        # ── Step 0: seed local source ─────────────────────────────────────
        print(f"\n  [0/3] Seeding local source dataset in {self.source_dir} …")
        n = seed_dataset(self.source_dir)
        print(f"    {n} files created")

        # Wipe any leftover from a previous run
        if not self.clean_nfs_subdir(self.NFS_SUB_PATH):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=self.error_message,
            )

        # ── Step 1: scan local source ─────────────────────────────────────
        print("  [1/3] Scanning local source …")
        if not self.run_fsscan_on(self.source_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"fsscan failed: {self.error_message}",
            )
        ctrl_file = self.get_primary_control_file("copy")
        if ctrl_file is None:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="copy control file was not created by fsscan",
            )
        print(f"    control file: {ctrl_file}")

        # ── Step 2: AIO copy to NFS target ────────────────────────────────
        print(f"  [2/3] AIO copy to NFS target "
              f"({self.nfs_host}:{self.nfs_export}/{self.NFS_SUB_PATH}) …")
        if not self.run_fsbackup_to_nfs(self.source_dir,
                                         self.NFS_SUB_PATH,
                                         ctrl_file=ctrl_file,
                                         enable_mtime=True):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"fsbackup (NFS target) failed: {self.error_message}",
            )

        # ── Step 3: verify via local mount ────────────────────────────────
        print("  [3/3] Verifying via local mount …")
        if not nfs_dst_local.exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"NFS target path not visible: {nfs_dst_local}",
            )

        ok, msg = verify_directories(self.source_dir, nfs_dst_local)
        if not ok:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"directory comparison failed: {msg}",
            )
        print(f"    {msg}")

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message=msg,
            details={
                "local_source": str(self.source_dir),
                "nfs_target": f"{self.nfs_host}:{self.nfs_export}/{self.NFS_SUB_PATH}",
                "nfs_target_local": str(nfs_dst_local),
            },
        )

    def teardown(self, keep_on_failure=True) -> None:
        """Clean up NFS sub-path created by this test, then call super."""
        if self.test_passed:
            self.clean_nfs_subdir(self.NFS_SUB_PATH)
        super().teardown(keep_on_failure=keep_on_failure)


# ---------------------------------------------------------------------------
# TC3: NFS source → NFS target (same server, different sub-paths)
# ---------------------------------------------------------------------------

class TestNfsSourceToNfs(NfsBackupTestBase):
    """
    TC3 – Backup FROM an NFS source TO a different NFS sub-path on the same
    server (AIO pipeline).

    The existing `ds2` dataset on the NFS export is the source; it is
    accessed via the local kernel mount for scanning.  The AIO pipeline
    pushes the files to a separate sub-path (`bifrost_tc3`) on the same NFS
    server via NFSv3 RPCs.  Verification is done by comparing `ds2` with
    `bifrost_tc3` through the local kernel mount.

    This test exercises the most realistic NFS-to-NFS migration scenario
    where source reading uses the VFS layer and target writing uses the
    native NFSv3 AIO path.

    Pass/fail criteria
    ------------------
    - fsscan must complete without errors.
    - fsbackup AIO pipeline must complete with zero failed files.
    - All files written to NFS must be visible at local_mount/bifrost_tc3
      with the correct paths and sizes compared to ds2.
    """

    NFS_SRC_DIR  = "ds2"
    NFS_DST_PATH = "bifrost_tc3"

    def run_test(self) -> TestResult:
        nfs_src_local = self.nfs_path(self.NFS_SRC_DIR)
        nfs_dst_local = self.nfs_path(self.NFS_DST_PATH)

        if not nfs_src_local.exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"NFS source dataset {self.NFS_SRC_DIR} not found at {nfs_src_local}",
            )

        # Wipe any leftover destination
        if not self.clean_nfs_subdir(self.NFS_DST_PATH):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=self.error_message,
            )

        # ── Step 1: scan NFS source (ds2) via local mount ─────────────────
        print(f"\n  [1/3] Scanning NFS source ({self.NFS_SRC_DIR}) via local mount …")
        if not self.run_fsscan_on(nfs_src_local):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"fsscan failed: {self.error_message}",
            )
        ctrl_file = self.get_primary_control_file("copy")
        if ctrl_file is None:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="copy control file was not created by fsscan",
            )
        print(f"    control file: {ctrl_file}")

        # ── Step 2: AIO copy to NFS target ────────────────────────────────
        print(f"  [2/3] AIO copy to NFS target "
              f"({self.nfs_host}:{self.nfs_export}/{self.NFS_DST_PATH}) …")
        if not self.run_fsbackup_to_nfs(nfs_src_local,
                                         self.NFS_DST_PATH,
                                         ctrl_file=ctrl_file,
                                         enable_mtime=True):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"fsbackup (NFS target) failed: {self.error_message}",
            )

        # ── Step 3: verify via local mount ────────────────────────────────
        print("  [3/3] Verifying via local mount …")
        if not nfs_dst_local.exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"NFS target path not visible: {nfs_dst_local}",
            )

        ok, msg = verify_directories(nfs_src_local, nfs_dst_local)
        if not ok:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"directory comparison failed: {msg}",
            )
        print(f"    {msg}")

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message=msg,
            details={
                "nfs_source": f"{self.nfs_host}:{self.nfs_export}/{self.NFS_SRC_DIR}",
                "nfs_target": f"{self.nfs_host}:{self.nfs_export}/{self.NFS_DST_PATH}",
                "verified_via": str(nfs_dst_local),
            },
        )

    def teardown(self, keep_on_failure=True) -> None:
        """Clean up NFS sub-path created by this test, then call super."""
        if self.test_passed:
            self.clean_nfs_subdir(self.NFS_DST_PATH)
        super().teardown(keep_on_failure=keep_on_failure)


# ---------------------------------------------------------------------------
# CLI entry-point
# ---------------------------------------------------------------------------

def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="NFS Backup Integration Tests",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Test cases:
  tc1  NFS source  → local-FS target
  tc2  local-FS source → NFS target  (AIO pipeline)
  tc3  NFS source  → NFS target      (AIO pipeline, same server)

Examples:
  python test_nfs_backup.py
  python test_nfs_backup.py --tc tc2 -v
  python test_nfs_backup.py --nfs-host 192.168.1.10 --nfs-export /data
        """,
    )
    parser.add_argument("-v",  "--verbose",        action="store_true")
    parser.add_argument("-w",  "--work-dir",        help="Base work directory")
    parser.add_argument("--keep-on-failure",        action="store_true")
    parser.add_argument("--keep-logs",              action="store_true")
    parser.add_argument("--nfs-host",               default=DEFAULT_NFS_HOST)
    parser.add_argument("--nfs-export",             default=DEFAULT_NFS_EXPORT)
    parser.add_argument("--local-mount",            default=DEFAULT_LOCAL_MOUNT)
    parser.add_argument(
        "--tc",
        choices=["tc1", "tc2", "tc3"],
        action="append",
        dest="tcs",
        help="Run only the specified test case(s) (can be repeated)",
    )
    return parser


def main():
    parser = build_arg_parser()
    args = parser.parse_args()

    nfs_kwargs = dict(
        nfs_host    = args.nfs_host,
        nfs_export  = args.nfs_export,
        local_mount = args.local_mount,
    )

    # Map TC name → class
    tc_map = {
        "tc1": TestNfsSourceToLocal,
        "tc2": TestLocalSourceToNfs,
        "tc3": TestNfsSourceToNfs,
    }

    # Determine which TCs to run
    selected = args.tcs if args.tcs else list(tc_map.keys())

    runner = TestRunner(
        verbose=args.verbose,
        keep_on_failure=args.keep_on_failure,
        keep_logs=args.keep_logs,
    )

    for tc_name in selected:
        cls = tc_map[tc_name]
        print(f"\nRunning {cls.__name__} …")
        runner.run_test(
            cls,
            work_dir=args.work_dir,
            **nfs_kwargs,
        )

    runner.print_summary()
    return 0 if runner.all_passed() else 1


if __name__ == "__main__":
    sys.exit(main())
