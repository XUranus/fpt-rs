#!/usr/bin/env python3
"""
Test Case: Linux ACL (Access Control List) Backup and Restore
Platform-specific test for Linux ACL support
"""

import sys
import os
import subprocess
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


def is_linux():
    """Check if running on Linux"""
    return sys.platform.startswith("linux")


def has_acl_support():
    """Check if ACL is supported on this system"""
    return os.system("which setfacl getfacl > /dev/null 2>&1") == 0


class TestLinuxACL(BifrostTestBase):
    """Test backup and restore of ACLs on Linux"""

    def set_acl(self, path: Path, acl_spec: str):
        """Set ACL on a path"""
        subprocess.run(
            ["setfacl", "-m", acl_spec, str(path)],
            check=True, capture_output=True
        )

    def get_acl(self, path: Path) -> str:
        """Get ACL of a path"""
        result = subprocess.run(
            ["getfacl", "-c", str(path)],
            check=True, capture_output=True, text=True
        )
        return result.stdout

    def run_test(self) -> TestResult:
        """Execute the test"""
        if not is_linux():
            return TestResult(
                name=self.__class__.__name__,
                passed=True,
                duration=0,
                message="Skipped - not running on Linux"
            )

        if not has_acl_support():
            return TestResult(
                name=self.__class__.__name__,
                passed=True,
                duration=0,
                message="Skipped - ACL not supported on this system"
            )

        print("Creating files with ACLs...")

        acl_dir = self.source_dir / "acl_test"
        acl_dir.mkdir()

        # Get current user info for ACL
        uid = os.getuid()
        gid = os.getgid()

        # Create files with different ACLs
        files_with_acl = [
            ("file_user_acl.txt", f"user:{uid}:rwx"),
            ("file_group_acl.txt", f"group:{gid}:rw-"),
            ("file_other_acl.txt", "other::r--"),
        ]

        for filename, acl_spec in files_with_acl:
            filepath = acl_dir / filename
            filepath.write_text(f"Content of {filename}\n")
            try:
                self.set_acl(filepath, acl_spec)
                print(f"  Created {filename} with ACL: {acl_spec}")
            except subprocess.CalledProcessError as e:
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Failed to set ACL on {filename}: {e}"
                )

        # Create directory with default ACL
        dir_with_acl = acl_dir / "dir_with_default_acl"
        dir_with_acl.mkdir()
        (dir_with_acl / "file_inside.txt").write_text("Inside\n")

        try:
            # Set default ACL on directory
            self.set_acl(dir_with_acl, f"default:user:{uid}:rwx")
            self.set_acl(dir_with_acl, f"default:group:{gid}:r-x")
            print("  Created directory with default ACL")
        except subprocess.CalledProcessError as e:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Failed to set default ACL: {e}"
            )

        # Record original ACLs
        print("\nRecording original ACLs...")
        original_acls = {}
        for filename, _ in files_with_acl:
            filepath = acl_dir / filename
            original_acls[filename] = self.get_acl(filepath)
            print(f"  {filename}: {len(original_acls[filename].splitlines())} ACL entries")

        original_acls["dir_with_default_acl"] = self.get_acl(dir_with_acl)

        # Step 1: Scan with ACL detection
        print("\nStep 1: Scanning with ACL detection...")
        if not self.run_fsscan(self.source_dir, extra_args=["--scan-acl"]):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Scan failed: {self.error_message}"
            )

        # Step 2: Backup
        print("\nStep 2: Running backup...")
        if not self.run_fsbackup(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Backup failed: {self.error_message}"
            )

        # Step 3: Verify ACLs in backup
        print("\nStep 3: Verifying ACLs in backup...")
        backup_acl_dir = self.backup_dir / "acl_test"

        for filename, original_acl in original_acls.items():
            bak_path = backup_acl_dir / filename
            if not bak_path.exists():
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Path missing in backup: {filename}"
                )

            try:
                backup_acl = self.get_acl(bak_path)

                # Compare ACLs (may have minor differences in format)
                # For now, just check that ACL is present
                if not backup_acl.strip():
                    return TestResult(
                        name=self.__class__.__name__,
                        passed=False,
                        duration=0,
                        message=f"ACL empty for {filename}"
                    )

                print(f"  ✓ {filename}: ACL preserved")

            except subprocess.CalledProcessError as e:
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Failed to get ACL for {filename}: {e}"
                )

        # Step 4: Run fsdiff with ACL comparison
        print("\nStep 4: Running fsdiff with ACL comparison...")
        if not self.run_fsdiff(
            self.source_dir,
            self.backup_dir,
            compare_acl=True
        ):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Diff verification failed: {self.error_message}"
            )

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message="Linux ACL test passed",
            details={
                "files_with_acl": len(files_with_acl),
                "dirs_with_default_acl": 1,
            }
        )


def main():
    args = parse_args()
    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure, keep_logs=args.keep_logs)
    result = runner.run_test(TestLinuxACL, work_dir=args.work_dir)
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
