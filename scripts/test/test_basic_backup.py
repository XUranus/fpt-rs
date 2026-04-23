#!/usr/bin/env python3
"""
Test Case: Basic Backup and Restore
Tests fundamental backup functionality with regular files and directories
"""

import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


class TestBasicBackup(BifrostTestBase):
    """Test basic backup and restore of regular files"""

    def run_test(self) -> TestResult:
        """Execute the test"""
        print("Creating test files...")

        # Create test directory structure
        (self.source_dir / "docs").mkdir()
        (self.source_dir / "data").mkdir()
        (self.source_dir / "config").mkdir()

        # Create various files
        files = [
            ("readme.txt", "This is a readme file\n"),
            ("docs/document1.txt", "Document 1 content\n" * 10),
            ("docs/document2.txt", "Document 2 content\n" * 20),
            ("data/datafile.bin", "Binary data " * 100),
            ("config/settings.conf", "setting1=value1\nsetting2=value2\n"),
            ("empty_file.txt", ""),
        ]

        for rel_path, content in files:
            filepath = self.source_dir / rel_path
            filepath.parent.mkdir(parents=True, exist_ok=True)
            filepath.write_text(content)

        # Create nested directories
        for i in range(3):
            nested = self.source_dir / "nested" / f"level{i}" / "files"
            nested.mkdir(parents=True)
            (nested / f"file_{i}.txt").write_text(f"Nested file {i}\n")

        print(f"  Created {len(files)} files in source directory")

        # Step 1: Scan
        print("\nStep 1: Scanning source directory...")
        if not self.run_fsscan(self.source_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Scan failed: {self.error_message}"
            )

        # Verify control file was created
        copy_ctrl = self.get_primary_control_file("copy")
        if copy_ctrl is None:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="copy control file was not created"
            )
        print(f"  Control file created: {copy_ctrl}")

        # Step 2: Backup
        print("\nStep 2: Running backup...")
        if not self.run_fsbackup(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Backup failed: {self.error_message}"
            )
        print("  Backup completed")

        # Step 3: Verify with fsdiff
        print("\nStep 3: Verifying backup with fsdiff...")
        if not self.run_fsdiff(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Diff verification failed: {self.error_message}"
            )
        print("  Diff verification passed")

        # Step 4: Additional content verification
        print("\nStep 4: Verifying file contents...")
        match, message = self.compare_directories(self.source_dir, self.backup_dir)
        if not match:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Content verification failed: {message}"
            )
        print(f"  {message}")

        self.test_passed = True

        # Print log locations
        print(f"\n  Logs written to:")
        print(f"    fsscan:  {self.fsscan_log_file}")
        print(f"    fsbackup: {self.fsbackup_log_file}")

        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message="Basic backup and restore test passed",
            details={
                "files_created": len(files),
                "source_dir": str(self.source_dir),
                "backup_dir": str(self.backup_dir),
                "fsscan_log": str(self.fsscan_log_file),
                "fsbackup_log": str(self.fsbackup_log_file),
            }
        )


def main():
    args = parse_args()
    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure, keep_logs=args.keep_logs)
    result = runner.run_test(TestBasicBackup, work_dir=args.work_dir)
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
