#!/usr/bin/env python3
"""
Bifrost Test Suite - Main Test Runner
Runs all test cases in sequence and reports results
"""

import sys
import os
import argparse
from pathlib import Path
from datetime import datetime

# Add test directory to path
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from test_framework import TestRunner

# Import all test classes
from test_basic_backup import TestBasicBackup
from test_incremental_backup import TestIncrementalBackup
from test_special_files import TestSpecialFiles
from test_hardlinks import TestHardlinks
from test_sparse_files import TestSparseFiles
from test_permissions import TestPermissions
from test_empty_directories import TestEmptyDirectories
from test_large_fileset import TestLargeFileset


# Define test suites
BASIC_TESTS = [
    TestBasicBackup,
    TestEmptyDirectories,
    TestSpecialFiles,
]

INTERMEDIATE_TESTS = [
    TestPermissions,
    TestHardlinks,
    TestSparseFiles,
]

ADVANCED_TESTS = [
    TestIncrementalBackup,
]

SCALABILITY_TESTS = [
    (TestLargeFileset, {"num_files": 500, "num_dirs": 50, "file_size": 1024}),
]

ALL_TESTS = BASIC_TESTS + INTERMEDIATE_TESTS + ADVANCED_TESTS + SCALABILITY_TESTS


def parse_args():
    """Parse command line arguments"""
    parser = argparse.ArgumentParser(
        description="Bifrost Test Suite - Run all tests",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Test Suites:
  basic        - Basic functionality tests (backup, empty dirs, special files)
  intermediate - Permission, hardlinks, sparse files
  advanced     - Incremental backup
  scalability  - Large fileset handling
  all          - Run all tests (default)

Examples:
  python test_all.py
  python test_all.py -v --suite basic
  python test_all.py -w /tmp/bifrost_tests --keep-on-failure
        """
    )

    parser.add_argument("-w", "--work-dir",
                        help="Base working directory for tests (default: temp dir)")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="Enable verbose output")
    parser.add_argument("--keep-on-failure", action="store_true",
                        help="Keep work directories for failed tests")
    parser.add_argument("--suite", choices=["basic", "intermediate", "advanced",
                                            "scalability", "all"],
                        default="all",
                        help="Test suite to run (default: all)")
    parser.add_argument("--list", action="store_true",
                        help="List all available tests and exit")
    parser.add_argument("--test", action="append",
                        help="Run specific test by name (can be specified multiple times)")

    return parser.parse_args()


def get_tests_to_run(args) -> list:
    """Get list of tests to run based on arguments"""
    if args.list:
        return []

    if args.test:
        # Map test names to classes
        test_map = {
            "basic": TestBasicBackup,
            "empty_dirs": TestEmptyDirectories,
            "special_files": TestSpecialFiles,
            "permissions": TestPermissions,
            "hardlinks": TestHardlinks,
            "sparse_files": TestSparseFiles,
            "incremental": TestIncrementalBackup,
            "large_fileset": (TestLargeFileset, {"num_files": 500, "num_dirs": 50}),
        }

        tests = []
        for test_name in args.test:
            test_name = test_name.lower().replace("-", "_")
            if test_name in test_map:
                tests.append(test_map[test_name])
            else:
                print(f"Warning: Unknown test '{test_name}'")
                print(f"Available tests: {', '.join(test_map.keys())}")
        return tests

    # Return tests based on suite
    suite_map = {
        "basic": BASIC_TESTS,
        "intermediate": INTERMEDIATE_TESTS,
        "advanced": ADVANCED_TESTS,
        "scalability": SCALABILITY_TESTS,
        "all": ALL_TESTS,
    }

    return suite_map[args.suite]


def list_tests():
    """List all available tests"""
    print("Available Tests:")
    print()

    test_suites = [
        ("Basic Tests", BASIC_TESTS),
        ("Intermediate Tests", INTERMEDIATE_TESTS),
        ("Advanced Tests", ADVANCED_TESTS),
        ("Scalability Tests", SCALABILITY_TESTS),
    ]

    for suite_name, tests in test_suites:
        print(f"{suite_name}:")
        for test in tests:
            if isinstance(test, tuple):
                test_class = test[0]
            else:
                test_class = test
            print(f"  - {test_class.__name__}")
        print()

    print("Usage:")
    print("  python test_all.py --test basic")
    print("  python test_all.py --test hardlinks --test sparse_files")


def main():
    args = parse_args()

    if args.list:
        list_tests()
        return 0

    # Get tests to run
    tests = get_tests_to_run(args)
    if not tests:
        print("No tests to run!")
        return 1

    # Create base work directory
    if args.work_dir:
        base_work_dir = Path(args.work_dir).resolve()
        base_work_dir.mkdir(parents=True, exist_ok=True)
    else:
        base_work_dir = Path(tempfile.mkdtemp(prefix="bifrost_test_suite_"))

    print("=" * 70)
    print("Bifrost Test Suite")
    print("=" * 70)
    print(f"Start time: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    print(f"Base work directory: {base_work_dir}")
    print(f"Test suite: {args.suite}")
    print(f"Tests to run: {len(tests)}")
    print("=" * 70)

    # Run tests
    runner = TestRunner(
        verbose=args.verbose,
        keep_on_failure=args.keep_on_failure
    )

    for i, test in enumerate(tests, 1):
        # Create individual work directory for each test
        test_work_dir = base_work_dir / f"test_{i:02d}"
        test_work_dir.mkdir(exist_ok=True)

        # Handle test with parameters
        if isinstance(test, tuple):
            test_class, kwargs = test
            print(f"\n[{i}/{len(tests)}] Running {test_class.__name__} with params: {kwargs}")
            result = runner.run_test(
                test_class,
                work_dir=str(test_work_dir),
                **kwargs
            )
        else:
            print(f"\n[{i}/{len(tests)}] Running {test.__name__}")
            result = runner.run_test(test, work_dir=str(test_work_dir))

    # Print final summary
    print("\n" + "=" * 70)
    print("FINAL SUMMARY")
    print("=" * 70)
    runner.print_summary()

    # Report work directory location
    if args.keep_on_failure and not runner.all_passed():
        print(f"\nWork directories preserved in: {base_work_dir}")
        print("Failed test directories:")
        for result in runner.results:
            if not result.passed:
                test_dir = base_work_dir / f"test_{runner.results.index(result) + 1:02d}"
                print(f"  - {result.name}: {test_dir}")

    print(f"\nEnd time: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")

    return 0 if runner.all_passed() else 1


if __name__ == "__main__":
    import tempfile
    sys.exit(main())
