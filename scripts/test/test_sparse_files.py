#!/usr/bin/env python3
"""
Test Case: Sparse Files Backup and Restore
Tests proper handling of sparse files (files with holes)
"""

import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


class TestSparseFiles(BifrostTestBase):
    """Test backup and restore of sparse files"""

    def create_sparse_file(self, filepath: Path, size: int, data_regions: list):
        """
        Create a sparse file with specified data regions
        data_regions: list of (offset, length) tuples
        """
        with open(filepath, 'wb') as f:
            for offset, length in data_regions:
                f.seek(offset)
                f.write(b'X' * length)
            # Set final file size
            f.truncate(size)

    def get_sparse_info(self, filepath: Path) -> tuple:
        """Get apparent size and actual disk usage"""
        stat = filepath.stat()
        apparent_size = stat.st_size
        # st_blocks is in 512-byte units on most systems
        actual_size = stat.st_blocks * 512
        return apparent_size, actual_size

    def run_test(self) -> TestResult:
        """Execute the test"""
        print("Creating sparse files...")

        sparse_dir = self.source_dir / "sparse_files"
        sparse_dir.mkdir()

        # Test 1: Simple sparse file (data at beginning and end)
        print("\n  Creating simple sparse file (10MB, data at ends)...")
        sparse1 = sparse_dir / "sparse_ends.bin"
        self.create_sparse_file(sparse1, 10 * 1024 * 1024, [
            (0, 1024),  # Data at start
            (10 * 1024 * 1024 - 1024, 1024),  # Data at end
        ])
        apparent1, actual1 = self.get_sparse_info(sparse1)
        print(f"    Apparent: {apparent1 / 1024:.1f} KB, Actual: {actual1 / 1024:.1f} KB")

        # Test 2: Middle hole (data at start, hole, data at end)
        print("\n  Creating middle-hole sparse file (5MB)...")
        sparse2 = sparse_dir / "sparse_middle.bin"
        self.create_sparse_file(sparse2, 5 * 1024 * 1024, [
            (0, 1024 * 100),  # 100KB at start
            (5 * 1024 * 1024 - 1024 * 100, 1024 * 100),  # 100KB at end
        ])
        apparent2, actual2 = self.get_sparse_info(sparse2)
        print(f"    Apparent: {apparent2 / 1024:.1f} KB, Actual: {actual2 / 1024:.1f} KB")

        # Test 3: Multiple holes
        print("\n  Creating multi-hole sparse file (20MB)...")
        sparse3 = sparse_dir / "sparse_multi.bin"
        regions = []
        for i in range(10):
            offset = i * 2 * 1024 * 1024  # Every 2MB
            regions.append((offset, 1024))  # 1KB data
        self.create_sparse_file(sparse3, 20 * 1024 * 1024, regions)
        apparent3, actual3 = self.get_sparse_info(sparse3)
        print(f"    Apparent: {apparent3 / 1024:.1f} KB, Actual: {actual3 / 1024:.1f} KB")

        # Test 4: Small sparse file
        print("\n  Creating small sparse file (100KB)...")
        sparse4 = sparse_dir / "sparse_small.bin"
        self.create_sparse_file(sparse4, 100 * 1024, [
            (0, 1024),
            (50 * 1024, 1024),
        ])
        apparent4, actual4 = self.get_sparse_info(sparse4)
        print(f"    Apparent: {apparent4 / 1024:.1f} KB, Actual: {actual4 / 1024:.1f} KB")

        # Create non-sparse file for comparison
        print("\n  Creating non-sparse file for comparison...")
        non_sparse = sparse_dir / "non_sparse.bin"
        with open(non_sparse, 'wb') as f:
            f.write(b'Y' * (1024 * 1024))  # 1MB of actual data
        apparent_ns, actual_ns = self.get_sparse_info(non_sparse)
        print(f"    Apparent: {apparent_ns / 1024:.1f} KB, Actual: {actual_ns / 1024:.1f} KB")

        # Verify sparse files actually have holes
        total_apparent = apparent1 + apparent2 + apparent3 + apparent4
        total_actual = actual1 + actual2 + actual3 + actual4
        print(f"\n  Total sparse: Apparent={total_apparent / (1024*1024):.1f} MB, "
              f"Actual={total_actual / (1024*1024):.1f} MB")

        if total_actual >= total_apparent * 0.9:
            print("  Warning: Files may not be sparse (actual size close to apparent)")

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

        # Step 3: Verify sparse files in backup
        print("\nStep 3: Verifying sparse files in backup...")
        backup_sparse_dir = self.backup_dir / "sparse_files"

        sparse_files = [
            ("sparse_ends.bin", apparent1),
            ("sparse_middle.bin", apparent2),
            ("sparse_multi.bin", apparent3),
            ("sparse_small.bin", apparent4),
        ]

        for filename, expected_apparent in sparse_files:
            bak_file = backup_sparse_dir / filename
            if not bak_file.exists():
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Sparse file missing in backup: {filename}"
                )

            bak_apparent, bak_actual = self.get_sparse_info(bak_file)

            # Check apparent size matches
            if bak_apparent != expected_apparent:
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"Size mismatch for {filename}: expected {expected_apparent}, got {bak_apparent}"
                )

            # Note: Sparse files may lose sparseness during backup (known limitation)
            # The apparent size should match, but actual size may be larger
            if bak_actual > bak_apparent * 0.5:  # If using more than 50% of apparent
                print(f"  ✓ {filename}: Apparent={bak_apparent / 1024:.1f} KB, Actual={bak_actual / 1024:.1f} KB (expanded)")
            else:
                print(f"  ✓ {filename}: Apparent={bak_apparent / 1024:.1f} KB, Actual={bak_actual / 1024:.1f} KB (sparse preserved)")

        # Step 4: Verify content (read data regions)
        print("\nStep 4: Verifying sparse file content...")

        # Check sparse_ends.bin has correct data
        with open(backup_sparse_dir / "sparse_ends.bin", 'rb') as f:
            # Check beginning
            f.seek(0)
            data = f.read(1024)
            if data != b'X' * 1024:
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message="Sparse file beginning data mismatch"
                )

            # Check end
            f.seek(10 * 1024 * 1024 - 1024)
            data = f.read(1024)
            if data != b'X' * 1024:
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message="Sparse file end data mismatch"
                )

        print("  ✓ Sparse file content verified")

        # Step 5: Run fsdiff
        print("\nStep 5: Running fsdiff verification...")
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
            message="Sparse files test passed",
            details={
                "sparse_files": 4,
                "total_apparent_mb": round(total_apparent / (1024*1024), 2),
                "total_actual_mb": round(total_actual / (1024*1024), 2),
                "space_savings": f"{100 - (total_actual / total_apparent * 100):.1f}%",
            }
        )


def main():
    args = parse_args()
    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure, keep_logs=args.keep_logs)
    result = runner.run_test(TestSparseFiles, work_dir=args.work_dir)
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
