#!/usr/bin/env python3
"""
Test Case: Empty Directories Backup and Restore
Tests proper handling of empty directories
"""

import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


class TestEmptyDirectories(BifrostTestBase):
    """Test backup and restore of empty directories"""

    def run_test(self) -> TestResult:
        """Execute the test"""
        print("Creating empty directory structure...")

        # Create various empty directories
        empty_dirs = [
            "empty_dir_1",
            "empty_dir_2",
            "nested/empty/deep",
            "sibling1",
            "sibling2",
            "parent/child1/empty",
            "parent/child2/empty",
            "deep/nested/empty/dir/structure",
        ]

        for dir_path in empty_dirs:
            (self.source_dir / dir_path).mkdir(parents=True)
            print(f"  Created empty dir: {dir_path}")

        # Create some non-empty directories for comparison
        (self.source_dir / "with_files").mkdir()
        (self.source_dir / "with_files" / "file.txt").write_text("Content\n")

        (self.source_dir / "mixed").mkdir()
        (self.source_dir / "mixed" / "empty_subdir").mkdir()
        (self.source_dir / "mixed" / "with_file").mkdir()
        (self.source_dir / "mixed" / "with_file" / "data.txt").write_text("Data\n")

        print(f"\n  Created {len(empty_dirs)} empty directories")
        print(f"  Created 2 non-empty directories for comparison")

        # Step 1: Scan
        print("\nStep 1: Scanning source directory...")
        if not self.run_fsscan(self.source_dir):
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

        # Step 3: Verify empty directories exist in backup
        print("\nStep 3: Verifying empty directories in backup...")

        missing_dirs = []
        for dir_path in empty_dirs:
            bak_dir = self.backup_dir / dir_path
            if not bak_dir.exists():
                missing_dirs.append(dir_path)
            elif not bak_dir.is_dir():
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Path exists but is not a directory: {dir_path}"
                )

        if missing_dirs:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Empty directories missing in backup: {missing_dirs}"
            )

        print(f"  ✓ All {len(empty_dirs)} empty directories preserved")

        # Step 4: Verify directories are still empty
        print("\nStep 4: Verifying directories remain empty...")

        for dir_path in empty_dirs:
            bak_dir = self.backup_dir / dir_path
            contents = list(bak_dir.iterdir())
            if contents:
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Directory not empty in backup: {dir_path} contains {len(contents)} items"
                )

        print("  ✓ All directories remain empty")

        # Step 5: Verify non-empty directories still have their files
        print("\nStep 5: Verifying non-empty directories...")

        if not (self.backup_dir / "with_files" / "file.txt").exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="File in non-empty directory missing"
            )

        if not (self.backup_dir / "mixed" / "with_file" / "data.txt").exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="File in mixed directory missing"
            )

        if not (self.backup_dir / "mixed" / "empty_subdir").exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="Empty subdirectory in mixed directory missing"
            )

        print("  ✓ Non-empty directories preserved correctly")

        # Step 6: Run fsdiff
        print("\nStep 6: Running fsdiff verification...")
        if not self.run_fsdiff(self.source_dir, self.backup_dir):
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
            message="Empty directories test passed",
            details={
                "empty_directories": len(empty_dirs),
                "nested_levels": max(len(p.split("/")) for p in empty_dirs),
            }
        )


def main():
    args = parse_args()
    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure)
    result = runner.run_test(TestEmptyDirectories, work_dir=args.work_dir)
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
