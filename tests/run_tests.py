#!/usr/bin/env python3
"""
fpt test runner — unified entry point for smoke and performance tests.

Usage:
    python tests/run_tests.py smoke              # run smoke tests (local only)
    python tests/run_tests.py smoke --all-transports  # run smoke across all configured transports
    python tests/run_tests.py perf               # run performance tests
    python tests/run_tests.py all                # run everything
    python tests/run_tests.py smoke -k test_hardlinks  # run a specific test

Environment variables:
    FPT_BIN_DIR         Path to compiled binaries (default: target/release)
    FPT_DATA_ROOT       Local data root (default: /opt/fpt_test_data)
    FPT_KEEP_ON_FAILURE Set to 1 to preserve workspace on test failure
    FPT_NFS_MOUNT       NFS mount point (enables NFS transport tests)
    FPT_SMB_MOUNT       SMB mount point (enables SMB transport tests)
    FPT_NFS_HOST        NFS server host (default: 127.0.0.1)
    FPT_NFS_EXPORT      NFS export path (default: /opt/dataset)
    FPT_NFS_UID         NFS AUTH_UNIX uid
    FPT_NFS_GID         NFS AUTH_UNIX gid
    FPT_SMB_SHARE       SMB share name (default: dataset)
    FPT_SMB_USER        SMB username
    FPT_SMB_PASSWORD    SMB password
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

import pytest


def main():
    parser = argparse.ArgumentParser(
        description="fpt test runner",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument(
        "suite",
        choices=["smoke", "perf", "all"],
        help="Test suite to run",
    )
    parser.add_argument(
        "--all-transports",
        action="store_true",
        help="Run smoke tests across all configured transports (default: local only)",
    )
    parser.add_argument(
        "-k", "--keyword",
        help="Filter tests by keyword expression (pytest -k)",
    )
    parser.add_argument(
        "-v", "--verbose",
        action="count", default=0,
        help="Increase verbosity (pass -v or -vv to pytest)",
    )
    parser.add_argument(
        "--keep-on-failure",
        action="store_true",
        help="Preserve workspace directories on test failure",
    )
    parser.add_argument(
        "--timeout",
        type=int, default=None,
        help="Per-test timeout in seconds",
    )
    parser.add_argument(
        "-x", "--stop-on-first-failure",
        action="store_true",
        help="Stop on first test failure",
    )
    parser.add_argument(
        "--junit-xml",
        help="Path to JUnit XML output file",
    )
    args, extra_args = parser.parse_known_args()

    # set env from CLI flags
    if args.keep_on_failure:
        os.environ["FPT_KEEP_ON_FAILURE"] = "1"

    # build pytest args
    root = Path(__file__).parent
    pytest_args: list[str] = []

    if args.suite == "smoke":
        pytest_args.append(str(root / "smoke"))
    elif args.suite == "perf":
        pytest_args.append(str(root / "perf"))
    else:  # all
        pytest_args.append(str(root))

    if args.keyword:
        pytest_args += ["-k", args.keyword]

    if args.verbose:
        pytest_args += ["-v"] * args.verbose

    if args.stop_on_first_failure:
        pytest_args.append("-x")

    if args.timeout:
        pytest_args += ["--timeout", str(args.timeout)]

    if args.junit_xml:
        pytest_args += ["--junit-xml", args.junit_xml]

    # show transport status
    from framework import Transport, transport_available
    transports = {t.value: transport_available(t) for t in Transport}
    print("Transport availability:")
    for name, avail in transports.items():
        status = "available" if avail else "not configured"
        print(f"  {name:6s}: {status}")
    print()

    pytest_args += extra_args

    print(f"Running {args.suite} tests with args: {pytest_args}")
    return pytest.main(pytest_args)


if __name__ == "__main__":
    sys.exit(main())
