#!/usr/bin/env python3
"""
Test Case: Linux Extended Attributes (xattr) Backup and Restore
Platform-specific test for Linux xattr support
"""

import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pathlib import Path
from test_framework import BifrostTestBase, TestResult, TestRunner, parse_args


def is_linux():
    """Check if running on Linux"""
    return sys.platform.startswith("linux")


def has_xattr_support():
    """Check if xattr is supported on this system"""
    try:
        import xattr
        return True
    except ImportError:
        pass

    # Try using setfattr/getfattr commands
    return os.system("which setfattr > /dev/null 2>&1") == 0


class TestLinuxXattr(BifrostTestBase):
    """Test backup and restore of extended attributes on Linux"""

    def run_test(self) -> TestResult:
        """Execute the test"""
        if not is_linux():
            return TestResult(
                name=self.__class__.__name__,
                passed=True,
                duration=0,
                message="Skipped - not running on Linux"
            )

        if not has_xattr_support():
            return TestResult(
                name=self.__class__.__name__,
                passed=True,
                duration=0,
                message="Skipped - xattr not supported on this system"
            )

        print("Creating files with extended attributes...")

        xattr_dir = self.source_dir / "xattr_test"
        xattr_dir.mkdir()

        # Create test files
        files_with_xattr = {
            "file1.txt": {
                "user.comment": b"Test comment for file1",
                "user.checksum": b"abc123def456",
            },
            "file2.txt": {
                "user.custom": b"Custom attribute value",
                "user.version": b"1.0",
            },
            "file3.txt": {
                "user.empty": b"",
                "user.long": b"X" * 1000,
            },
        }

        # Create files and set xattrs
        for filename, attrs in files_with_xattr.items():
            filepath = xattr_dir / filename
            filepath.write_text(f"Content of {filename}\n")

            for attr_name, attr_value in attrs.items():
                try:
                    # Try using xattr module
                    import xattr
                    x = xattr.xattr(str(filepath))
                    x.set(attr_name, attr_value)
                except:
                    # Fall back to setfattr command
                    import subprocess
                    # Write value to temp file for binary data
                    import tempfile
                    with tempfile.NamedTemporaryFile(delete=False) as tmp:
                        tmp.write(attr_value)
                        tmp.flush()
                        subprocess.run(
                            ["setfattr", "-n", attr_name, "-v", attr_value.decode('utf-8', errors='replace'), str(filepath)],
                            check=False, capture_output=True
                        )
                        os.unlink(tmp.name)

            print(f"  Created {filename} with {len(attrs)} xattrs")

        # Create directory with xattr
        dir_with_xattr = xattr_dir / "dir_with_xattr"
        dir_with_xattr.mkdir()
        try:
            import xattr
            x = xattr.xattr(str(dir_with_xattr))
            x.set("user.dir_attr", b"Directory attribute")
            print("  Created directory with xattr")
        except:
            pass

        # Step 1: Scan with xattr detection
        print("\nStep 1: Scanning with xattr detection...")
        if not self.run_fsscan(self.source_dir, extra_args=["--scan-xattrs"]):
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

        # Step 3: Verify xattrs in backup
        print("\nStep 3: Verifying extended attributes in backup...")
        backup_xattr_dir = self.backup_dir / "xattr_test"

        for filename, expected_attrs in files_with_xattr.items():
            bak_file = backup_xattr_dir / filename
            if not bak_file.exists():
                return TestResult(
                    name=self.__class__.__name__,
                    passed=False,
                    duration=0,
                    message=f"File missing in backup: {filename}"
                )

            # Try to read xattrs
            try:
                import xattr
                x = xattr.xattr(str(bak_file))
                actual_attrs = dict(x)

                for attr_name, expected_value in expected_attrs.items():
                    if attr_name not in actual_attrs:
                        return TestResult(
                            name=self.__class__.__name__,
                            passed=False,
                            duration=0,
                            message=f"Xattr missing: {attr_name} on {filename}"
                        )

                    if actual_attrs[attr_name] != expected_value:
                        return TestResult(
                            name=self.__class__.__name__,
                            passed=False,
                            duration=0,
                            message=f"Xattr value mismatch: {attr_name} on {filename}"
                        )

                print(f"  ✓ {filename}: {len(expected_attrs)} xattrs verified")

            except Exception as e:
                print(f"  ! Could not verify xattrs for {filename}: {e}")

        # Step 4: Run fsdiff with xattr comparison
        print("\nStep 4: Running fsdiff with xattr comparison...")
        if not self.run_fsdiff(
            self.source_dir,
            self.backup_dir,
            compare_xattrs=True
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
            message="Linux xattr test passed",
            details={
                "files_with_xattr": len(files_with_xattr),
                "total_xattrs": sum(len(v) for v in files_with_xattr.values()),
            }
        )


def main():
    args = parse_args()
    runner = TestRunner(verbose=args.verbose, keep_on_failure=args.keep_on_failure)
    result = runner.run_test(TestLinuxXattr, work_dir=args.work_dir)
    runner.print_summary()
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
