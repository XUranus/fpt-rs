#!/usr/bin/env python3
"""
Test Case: Large Fileset Backup and Restore
Tests handling of large numbers of files and directories
"""

import sys
import os
import argparse
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


class TestLargeFileset(BifrostTestBase):
    """Test backup of large filesets"""

    def __init__(self, work_dir=None, verbose=False, keep_logs=False, num_files=1000,
                 num_dirs=100, file_size=1024):
        super().__init__(work_dir, verbose, keep_logs)
        self.num_files = num_files
        self.num_dirs = num_dirs
        self.file_size = file_size

    def run_test(self) -> TestResult:
        """Execute the test"""
        print(f"Creating large fileset: {self.num_files} files, {self.num_dirs} dirs")

        # Create directory structure
        print("\nCreating directory structure...")
        dirs_created = 0
        for i in range(self.num_dirs):
            # Create nested structure
            depth = i % 5  # Vary nesting depth 0-4
            path = self.source_dir
            for d in range(depth):
                path = path / f"level{d}"
            path = path / f"dir_{i}"
            path.mkdir(parents=True, exist_ok=True)
            dirs_created += 1

        print(f"  Created {dirs_created} directories")

        # Create files distributed across directories
        print("\nCreating files...")
        files_created = 0
        for i in range(self.num_files):
            # Distribute files across directories
            dir_idx = i % self.num_dirs
            depth = dir_idx % 5

            path = self.source_dir
            for d in range(depth):
                path = path / f"level{d}"
            path = path / f"dir_{dir_idx}"

            # Create file with deterministic content
            filepath = path / f"file_{i}.txt"
            content = f"File {i}: " + "X" * (self.file_size - len(f"File {i}: "))
            filepath.write_text(content)
            files_created += 1

            if self.verbose and i > 0 and i % 100 == 0:
                print(f"  Created {i} files...")

        print(f"  Created {files_created} files")

        # Calculate total size
        total_size = sum(
            f.stat().st_size
            for f in self.source_dir.rglob("*")
            if f.is_file()
        )
        print(f"  Total size: {total_size / (1024*1024):.2f} MB")

        # Step 1: Scan
        print("\nStep 1: Scanning large fileset...")
        if not self.run_fsscan(self.source_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Scan failed: {self.error_message}"
            )

        # Verify control file
        copy_txt = self.ctrl_dir / "copy.txt"
        if not copy_txt.exists():
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message="copy.txt was not created"
            )

        # Count entries in control file
        with open(copy_txt) as f:
            line_count = sum(1 for _ in f)
        print(f"  Control file entries: {line_count}")

        # Step 2: Backup
        print("\nStep 2: Running backup...")
        if not self.run_fsbackup(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Backup failed: {self.error_message}"
            )

        # Verify backup file count
        backup_files = list(self.backup_dir.rglob("*.txt"))
        print(f"  Backed up files: {len(backup_files)}")

        if len(backup_files) != self.num_files:
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"File count mismatch: expected {self.num_files}, got {len(backup_files)}"
            )

        # Step 3: Verify with fsdiff
        print("\nStep 3: Running fsdiff verification...")
        if not self.run_fsdiff(self.source_dir, self.backup_dir):
            return TestResult(
                name=self.__class__.__name__,
                passed=False,
                duration=0,
                message=f"Diff verification failed: {self.error_message}"
            )

        # Step 4: Sample content verification (full verification would be too slow)
        print("\nStep 4: Sampling content verification...")
        sample_size = min(100, self.num_files)
        import random
        random.seed(42)  # Reproducible

        for i in range(sample_size):
            file_idx = random.randint(0, self.num_files - 1)
            dir_idx = file_idx % self.num_dirs
            depth = dir_idx % 5

            path = Path(f"dir_{dir_idx}") / f"file_{file_idx}.txt"
            for d in range(depth - 1, -1, -1):
                path = Path(f"level{d}") / path

            src_file = self.source_dir / path
            bak_file = self.backup_dir / path

            if not bak_file.exists():
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Sample file missing: {path}"
                )

            if self.get_file_hash(src_file) != self.get_file_hash(bak_file):
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Sample file content mismatch: {path}"
                )

        print(f"  ✓ Verified {sample_size} random files")

        self.test_passed = True
        return TestResult(
            name=self.__class__.__name__,
            passed=True,
            duration=0,
            message="Large fileset test passed",
            details={
                "files_created": files_created,
                "dirs_created": dirs_created,
                "total_size_mb": round(total_size / (1024*1024), 2),
                "sample_verified": sample_size,
            }
        )


def main():
    parser = argparse.ArgumentParser(description="Large Fileset Test")
    parser.add_argument("-w", "--work-dir", help="Working directory for test")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="Verbose output")
    parser.add_argument("--keep-on-failure", action="store_true",
                        help="Keep work directory on failure")
    parser.add_argument("--keep-logs", action="store_true",
                        help="Keep logs directory even when test passes")
    parser.add_argument("--files", type=int, default=1000,
                        help="Number of files to create (default: 1000)")
    parser.add_argument("--dirs", type=int, default=100,
                        help="Number of directories to create (default: 100)")
    parser.add_argument("--size", type=int, default=1024,
                        help="File size in bytes (default: 1024)")
    args = parser.parse_args()

    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure, keep_logs=args.keep_logs)
    result = runner.run_test(
        TestLargeFileset,
        work_dir=args.work_dir,
        num_files=args.files,
        num_dirs=args.dirs,
        file_size=args.size
    )
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
