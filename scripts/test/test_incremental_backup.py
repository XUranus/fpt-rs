#!/usr/bin/env python3
"""
Test Case: Incremental Backup
Tests full backup followed by incremental backup with file changes
"""

import sys
import os
import shutil
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


class TestIncrementalBackup(BifrostTestBase):
    """Test incremental backup functionality"""

    def run_test(self) -> TestResult:
        """Execute the test"""
        print("Phase 1: Creating initial test data...")

        # Create initial directory structure
        dirs = ["docs", "data", "config", "images"]
        for d in dirs:
            (self.source_dir / d).mkdir()

        # Create initial files
        initial_files = {
            "readme.txt": "Initial readme\n",
            "docs/file1.txt": "Doc file 1\n",
            "docs/file2.txt": "Doc file 2\n",
            "data/data1.bin": "Data content 1\n" * 50,
            "config/settings.conf": "key1=value1\n",
            "images/image1.txt": "Image placeholder 1\n",
        }

        for rel_path, content in initial_files.items():
            filepath = self.source_dir / rel_path
            filepath.parent.mkdir(parents=True, exist_ok=True)
            filepath.write_text(content)

        print(f"  Created {len(initial_files)} initial files")

        # Step 1: Full backup scan
        print("\nStep 1: Full backup scan...")
        full_meta_dir = self.work_dir / "meta_full"
        full_ctrl_dir = self.work_dir / "ctrl_full"
        full_meta_dir.mkdir()
        full_ctrl_dir.mkdir()

        # Temporarily use full backup directories
        orig_meta_dir = self.meta_dir
        orig_ctrl_dir = self.ctrl_dir
        self.meta_dir = full_meta_dir
        self.ctrl_dir = full_ctrl_dir

        if not self.run_fsscan(self.source_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Full scan failed: {self.error_message}"
            )

        # Run full backup
        print("\nStep 2: Running full backup...")
        if not self.run_fsbackup(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Full backup failed: {self.error_message}"
            )

        # Verify full backup
        print("\nStep 3: Verifying full backup...")
        if not self.run_fsdiff(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Full backup verification failed: {self.error_message}"
            )
        print("  Full backup verified")

        # Phase 2: Simulate changes
        print("\nPhase 2: Simulating file changes...")

        # 1. Create new files
        new_files = {
            "new_file.txt": "This is a new file\n",
            "docs/new_doc.txt": "New document\n",
            "data/new_data.bin": "New data content\n" * 20,
        }
        for rel_path, content in new_files.items():
            filepath = self.source_dir / rel_path
            filepath.write_text(content)
        print(f"  Created {len(new_files)} new files")

        # 2. Modify existing files
        (self.source_dir / "readme.txt").write_text("Modified readme\n")
        (self.source_dir / "docs/file1.txt").write_text("Modified doc file 1\n")
        print("  Modified 2 existing files")

        # 3. Delete files
        (self.source_dir / "images/image1.txt").unlink()
        print("  Deleted 1 file")

        # 4. Create new directory
        (self.source_dir / "new_dir").mkdir()
        (self.source_dir / "new_dir" / "new_file.txt").write_text("In new dir\n")
        print("  Created 1 new directory with file")

        # Step 4: Incremental backup scan
        print("\nStep 4: Incremental backup scan...")
        self.meta_dir = orig_meta_dir
        self.ctrl_dir = orig_ctrl_dir

        if not self.run_fsscan(self.source_dir, prev_meta_dir=full_meta_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Incremental scan failed: {self.error_message}"
            )

        # Check that copy.txt was created
        if self.get_primary_control_file("copy") is None:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="Incremental copy control file was not created"
            )

        # Step 5: Run incremental backup
        print("\nStep 5: Running incremental backup...")
        backup_dir_incr = self.work_dir / "backup_incremental"
        backup_dir_incr.mkdir()

        # Copy full backup as base
        shutil.copytree(self.backup_dir, backup_dir_incr, dirs_exist_ok=True)

        if not self.run_fsbackup(
            self.source_dir,
            backup_dir_incr,
            enable_delete=True,
            enable_mtime=True
        ):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Incremental backup failed: {self.error_message}"
            )

        # Step 6: Verify incremental backup
        print("\nStep 6: Verifying incremental backup...")

        # Check new files exist
        for rel_path in new_files.keys():
            if not (backup_dir_incr / rel_path).exists():
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"New file missing in incremental backup: {rel_path}"
                )
        print("  ✓ New files exist in backup")

        # Check modified files
        content = (backup_dir_incr / "readme.txt").read_text()
        if "Modified" not in content:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="Modified file not updated in backup"
            )
        print("  ✓ Modified files updated")

        # Check deleted files
        if (backup_dir_incr / "images/image1.txt").exists():
            print("  ! Warning: Deleted file still exists (known limitation)")
        else:
            print("  ✓ Deleted files removed")

        # Final diff
        print("\nStep 7: Running final diff verification...")
        if not self.run_fsdiff(self.source_dir, backup_dir_incr):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Final diff verification failed: {self.error_message}"
            )

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message="Incremental backup test passed",
            details={
                "initial_files": len(initial_files),
                "new_files": len(new_files),
                "modified_files": 2,
                "deleted_files": 1,
            }
        )


def main():
    args = parse_args()
    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure, keep_logs=args.keep_logs)
    result = runner.run_test(TestIncrementalBackup, work_dir=args.work_dir)
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
