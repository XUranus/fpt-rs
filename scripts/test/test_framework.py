#!/usr/bin/env python3
"""
Bifrost Test Framework
Base classes and utilities for cross-platform testing
"""

import os
import sys
import shutil
import tempfile
import subprocess
import argparse
import platform
from pathlib import Path
from typing import Optional, List, Dict, Any, Tuple
from dataclasses import dataclass
from datetime import datetime
import hashlib
import json


@dataclass
class TestResult:
    """Result of a single test"""
    name: str
    passed: bool
    duration: float
    message: str = ""
    details: Dict[str, Any] = None

    def __post_init__(self):
        if self.details is None:
            self.details = {}


class BifrostTestBase:
    """Base class for all Bifrost tests"""

    def __init__(self, work_dir: Optional[str] = None, verbose: bool = False, keep_logs: bool = False):
        self.verbose = verbose
        self.keep_logs = keep_logs
        self.work_dir = work_dir or tempfile.mkdtemp(prefix="bifrost_test_")
        self.work_dir = Path(self.work_dir).resolve()

        # Create subdirectories
        self.source_dir = self.work_dir / "source"
        self.backup_dir = self.work_dir / "backup"
        self.restore_dir = self.work_dir / "restore"
        self.meta_dir = self.work_dir / "meta"
        self.ctrl_dir = self.work_dir / "ctrl"
        self.logs_dir = self.work_dir / "logs"

        # Platform detection
        self.platform = platform.system().lower()

        # Find binaries
        self.bifrost_root = self._find_bifrost_root()
        self.binaries = self._find_binaries()

        # Test tracking
        self.test_passed = False
        self.error_message = ""

        # Log tracking
        self.fsscan_log_file: Optional[Path] = None
        self.fsbackup_log_file: Optional[Path] = None

    def _find_bifrost_root(self) -> Path:
        """Find the Bifrost project root"""
        # Start from script location and go up
        script_dir = Path(__file__).parent.resolve()
        # Go up to scripts, then to project root
        return script_dir.parent.parent

    def _find_binaries(self) -> Dict[str, Path]:
        """Find Bifrost binaries - prefers release builds over debug"""
        binary_names = ["fsscan", "fsbackup", "fsdiff", "cacheinspect", "metainspect"]
        binaries = {}

        # Check release build first (preferred), then debug
        for build_type in ["release", "debug"]:
            target_dir = self.bifrost_root / "target" / build_type
            if target_dir.exists():
                for name in binary_names:
                    binary_path = target_dir / name
                    # Only add if not already found (release takes precedence)
                    if binary_path.exists() and name not in binaries:
                        binaries[name] = binary_path

        return binaries

    def setup(self) -> bool:
        """Setup test environment"""
        try:
            # Create directories
            for d in [self.source_dir, self.backup_dir, self.restore_dir,
                      self.meta_dir, self.ctrl_dir, self.logs_dir]:
                d.mkdir(parents=True, exist_ok=True)

            # Define log files
            self.fsscan_log_file = self.logs_dir / "fsscan.log"
            self.fsbackup_log_file = self.logs_dir / "fsbackup.log"

            # Check binaries exist
            required = ["fsscan", "fsbackup", "fsdiff"]
            for binary in required:
                if binary not in self.binaries:
                    self.error_message = f"Required binary '{binary}' not found. Build with: cargo build --release"
                    return False

            return True
        except Exception as e:
            self.error_message = f"Setup failed: {e}"
            return False

    def teardown(self, keep_on_failure: bool = True) -> None:
        """Cleanup test environment"""
        if not self.test_passed and keep_on_failure:
            print(f"  Test failed. Keeping work directory: {self.work_dir}")
            return

        if self.test_passed and self.keep_logs:
            print(f"  Test passed. Keeping logs: {self.logs_dir}")
            # Remove everything except logs directory
            for item in self.work_dir.iterdir():
                if item.name != "logs":
                    try:
                        if item.is_dir():
                            shutil.rmtree(item)
                        else:
                            item.unlink()
                    except Exception as e:
                        print(f"  Warning: Failed to remove {item}: {e}")
            return

        try:
            if self.work_dir.exists():
                shutil.rmtree(self.work_dir)
        except Exception as e:
            print(f"  Warning: Failed to cleanup {self.work_dir}: {e}")

    def run_command(self, cmd: List[str], cwd: Optional[str] = None,
                    check: bool = True, capture: bool = True) -> Tuple[int, str, str]:
        """Run a shell command"""
        if self.verbose:
            print(f"  Running: {' '.join(cmd)}")

        result = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=capture,
            text=True,
            check=False
        )

        if check and result.returncode != 0:
            raise subprocess.CalledProcessError(
                result.returncode, cmd,
                output=result.stdout, stderr=result.stderr
            )

        return result.returncode, result.stdout, result.stderr

    def run_fsscan(self, source: Path, prev_meta_dir: Optional[Path] = None,
                   extra_args: Optional[List[str]] = None) -> bool:
        """Run fsscan binary and capture logs to file"""
        cmd = [
            str(self.binaries["fsscan"]),
            "-c", str(self.ctrl_dir),
            "-m", str(self.meta_dir),
            "-w", "4",
            "-W", "1",
            str(source)
        ]

        if prev_meta_dir:
            cmd.extend(["--prev-meta-dir", str(prev_meta_dir)])

        if extra_args:
            cmd.extend(extra_args)

        if self.verbose:
            cmd.append("-v")

        try:
            print(f"  Running fsscan (logs: {self.fsscan_log_file})")
            returncode, stdout, stderr = self.run_command(cmd, capture=True)

            # Write logs to file
            with open(self.fsscan_log_file, 'w') as f:
                f.write("=" * 70 + "\n")
                f.write("FSSCAN LOG\n")
                f.write("=" * 70 + "\n")
                f.write(f"Command: {' '.join(cmd)}\n")
                f.write(f"Return code: {returncode}\n")
                f.write("\n--- STDOUT ---\n")
                f.write(stdout if stdout else "(no output)\n")
                f.write("\n--- STDERR ---\n")
                f.write(stderr if stderr else "(no output)\n")
                f.write("=" * 70 + "\n")

            if returncode != 0:
                self.error_message = f"fsscan failed: {stderr}"
                return False
            return True
        except subprocess.CalledProcessError as e:
            # Write error logs to file
            with open(self.fsscan_log_file, 'w') as f:
                f.write("=" * 70 + "\n")
                f.write("FSSCAN LOG - ERROR\n")
                f.write("=" * 70 + "\n")
                f.write(f"Command: {' '.join(cmd)}\n")
                f.write(f"Return code: {e.returncode}\n")
                f.write("\n--- STDOUT ---\n")
                f.write(e.output if e.output else "(no output)\n")
                f.write("\n--- STDERR ---\n")
                f.write(e.stderr if e.stderr else "(no output)\n")
                f.write("=" * 70 + "\n")
            self.error_message = f"fsscan failed: {e.stderr}"
            return False

    def run_fsbackup(self, source: Path, target: Path,
                     control_file: Optional[Path] = None,
                     enable_hardlink: bool = False,
                     enable_delete: bool = False,
                     enable_mtime: bool = False) -> bool:
        """Run fsbackup binary and capture logs to file"""
        cmd = [
            str(self.binaries["fsbackup"]),
            "-s", str(source),
            "-t", str(target),
            "-m", str(self.meta_dir),
        ]

        if control_file:
            cmd.extend(["-c", str(control_file)])
        else:
            # Default to copy.txt
            cmd.extend(["-c", str(self.ctrl_dir / "copy.txt")])

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

        try:
            print(f"  Running fsbackup (logs: {self.fsbackup_log_file})")
            returncode, stdout, stderr = self.run_command(cmd, capture=True)

            # Write logs to file
            with open(self.fsbackup_log_file, 'w') as f:
                f.write("=" * 70 + "\n")
                f.write("FSBACKUP LOG\n")
                f.write("=" * 70 + "\n")
                f.write(f"Command: {' '.join(cmd)}\n")
                f.write(f"Return code: {returncode}\n")
                f.write("\n--- STDOUT ---\n")
                f.write(stdout if stdout else "(no output)\n")
                f.write("\n--- STDERR ---\n")
                f.write(stderr if stderr else "(no output)\n")
                f.write("=" * 70 + "\n")

            if returncode != 0:
                self.error_message = f"fsbackup failed: {stderr}"
                return False
            return True
        except subprocess.CalledProcessError as e:
            # Write error logs to file
            with open(self.fsbackup_log_file, 'w') as f:
                f.write("=" * 70 + "\n")
                f.write("FSBACKUP LOG - ERROR\n")
                f.write("=" * 70 + "\n")
                f.write(f"Command: {' '.join(cmd)}\n")
                f.write(f"Return code: {e.returncode}\n")
                f.write("\n--- STDOUT ---\n")
                f.write(e.output if e.output else "(no output)\n")
                f.write("\n--- STDERR ---\n")
                f.write(e.stderr if e.stderr else "(no output)\n")
                f.write("=" * 70 + "\n")
            self.error_message = f"fsbackup failed: {e.stderr}"
            return False

    def run_fsdiff(self, source: Path, target: Path,
                   compare_acl: bool = False,
                   compare_xattrs: bool = False,
                   compare_mtime: bool = False) -> bool:
        """Run fsdiff binary"""
        cmd = [
            str(self.binaries["fsdiff"]),
            "--source", str(source),
            "--target", str(target)
        ]

        if compare_acl:
            cmd.append("--compare-acl")

        if compare_xattrs:
            cmd.append("--compare-xattrs")

        if compare_mtime:
            cmd.append("--compare-mtime")

        if self.verbose:
            cmd.append("-v")

        try:
            self.run_command(cmd)
            return True
        except subprocess.CalledProcessError as e:
            self.error_message = f"fsdiff failed: {e.stderr}"
            return False

    def get_file_hash(self, filepath: Path) -> str:
        """Calculate SHA256 hash of a file"""
        sha256 = hashlib.sha256()
        with open(filepath, "rb") as f:
            for chunk in iter(lambda: f.read(8192), b""):
                sha256.update(chunk)
        return sha256.hexdigest()

    def compare_directories(self, source: Path, target: Path,
                            compare_content: bool = True) -> Tuple[bool, str]:
        """Compare two directories recursively"""
        source_files = set()
        target_files = set()

        for root, dirs, files in os.walk(source):
            for f in files:
                rel_path = Path(root).relative_to(source) / f
                source_files.add(rel_path)

        for root, dirs, files in os.walk(target):
            for f in files:
                rel_path = Path(root).relative_to(target) / f
                target_files.add(rel_path)

        # Check for missing files
        missing_in_target = source_files - target_files
        missing_in_source = target_files - source_files

        if missing_in_target:
            return False, f"Files missing in target: {missing_in_target}"

        if missing_in_source:
            return False, f"Extra files in target: {missing_in_source}"

        # Compare content
        if compare_content:
            for rel_path in source_files:
                src_file = source / rel_path
                tgt_file = target / rel_path

                if self.get_file_hash(src_file) != self.get_file_hash(tgt_file):
                    return False, f"Content mismatch: {rel_path}"

        return True, "Directories match"

    def create_test_files(self, count: int, size: int = 1024,
                          subdirs: int = 0) -> None:
        """Create test files in source directory"""
        for i in range(count):
            if subdirs > 0:
                subdir = self.source_dir / f"subdir_{i % subdirs}"
                subdir.mkdir(exist_ok=True)
                filepath = subdir / f"file_{i}.txt"
            else:
                filepath = self.source_dir / f"file_{i}.txt"

            # Write deterministic content
            content = f"File {i} content: " + "A" * (size - len(f"File {i} content: "))
            filepath.write_text(content)

    def run_test(self) -> TestResult:
        """Run the test - to be implemented by subclasses"""
        raise NotImplementedError("Subclasses must implement run_test()")


class TestRunner:
    """Runner for multiple tests"""

    def __init__(self, verbose: bool = False, keep_on_failure: bool = True, keep_logs: bool = False):
        self.verbose = verbose
        self.keep_on_failure = keep_on_failure
        self.keep_logs = keep_logs
        self.results: List[TestResult] = []

    def run_test(self, test_class, work_dir: Optional[str] = None,
                 **kwargs) -> TestResult:
        """Run a single test class"""
        test_name = test_class.__name__
        print(f"\n{'='*60}")
        print(f"Running: {test_name}")
        print(f"{'='*60}")

        start_time = datetime.now()

        try:
            # Pass keep_logs to test class if it supports it
            test_kwargs = {'verbose': self.verbose, 'keep_logs': self.keep_logs}
            test_kwargs.update(kwargs)
            test = test_class(work_dir=work_dir, **test_kwargs)

            if not test.setup():
                result = TestResult(
                    name=test_name,
                    passed=False,
                    duration=0,
                    message=f"Setup failed: {test.error_message}"
                )
                self.results.append(result)
                return result

            result = test.run_test()
            test.teardown(keep_on_failure=self.keep_on_failure)

        except Exception as e:
            duration = (datetime.now() - start_time).total_seconds()
            result = TestResult(
                name=test_name,
                passed=False,
                duration=duration,
                message=f"Exception: {str(e)}"
            )
            self.results.append(result)
            return result

        duration = (datetime.now() - start_time).total_seconds()
        result.duration = duration
        self.results.append(result)

        # Print result
        status = "PASSED" if result.passed else "FAILED"
        print(f"\nResult: {status} ({duration:.2f}s)")
        if not result.passed and result.message:
            print(f"Error: {result.message}")

        return result

    def print_summary(self) -> None:
        """Print test summary"""
        print(f"\n{'='*60}")
        print("TEST SUMMARY")
        print(f"{'='*60}")

        passed = sum(1 for r in self.results if r.passed)
        failed = sum(1 for r in self.results if not r.passed)

        for result in self.results:
            status = "✓ PASS" if result.passed else "✗ FAIL"
            print(f"{status:8} {result.name:40} ({result.duration:.2f}s)")

        print(f"\nTotal: {len(self.results)} tests")
        print(f"Passed: {passed}")
        print(f"Failed: {failed}")

        if failed == 0:
            print("\nAll tests PASSED!")
        else:
            print(f"\n{failed} test(s) FAILED!")

    def all_passed(self) -> bool:
        """Check if all tests passed"""
        return all(r.passed for r in self.results)


def parse_args():
    """Parse command line arguments"""
    parser = argparse.ArgumentParser(description="Bifrost Test")
    parser.add_argument("-w", "--work-dir", help="Working directory for test")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="Verbose output")
    parser.add_argument("--keep-on-failure", action="store_true",
                        help="Keep work directory on failure")
    parser.add_argument("--keep-logs", action="store_true",
                        help="Keep logs directory even when test passes")
    return parser.parse_args()
