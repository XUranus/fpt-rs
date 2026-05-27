"""Shared pytest fixtures for fpt tests.

Provides:
- ``fpt``: an :class:`~framework.FptCli` instance scoped per test
- ``fptbin``: resolved binary directory path (session scope)
- transport parametrization marks (``needs_nfs``, ``needs_smb``)
- ``tmp_workspace``: unique per-test workspace directory with automatic cleanup
"""

from __future__ import annotations

import os
import shutil
import sys
import tempfile
import uuid
from pathlib import Path

# Ensure the tests/ directory is importable so framework.py can be found
sys.path.insert(0, str(Path(__file__).resolve().parent))

import pytest

from framework import (
    FptCli,
    Transport,
    data_root,
    transport_available,
    transport_mount,
)


# ---------------------------------------------------------------------------
# Binary directory
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def fptbin() -> Path:
    """Resolve the directory containing fpt binaries."""
    override = os.environ.get("FPT_BIN_DIR")
    if override:
        p = Path(override)
        assert p.is_dir(), f"FPT_BIN_DIR={p} does not exist"
        return p

    # walk up to find the project root (contains target/release)
    cwd = Path.cwd()
    for candidate in [cwd, *cwd.parents]:
        p = candidate / "target" / "release"
        if p.is_dir() and (p / "fptcli").exists():
            return p
        p = candidate / "target" / "debug"
        if p.is_dir() and (p / "fptcli").exists():
            return p

    pytest.skip("fpt binaries not found — run cargo build first")


# ---------------------------------------------------------------------------
# Per-test workspace
# ---------------------------------------------------------------------------

@pytest.fixture()
def tmp_workspace(fptbin: Path, request) -> FptCli:
    """Create a unique workspace for each test, auto-cleaned after.

    The workspace lives under ``FPT_DATA_ROOT/local/<uuid>/`` so that NFS/SMB
    transports can also reach it when mapped to the same data root.
    """
    test_id = uuid.uuid4().hex[:12]
    test_name = request.node.name.replace("/", "_").replace("::", "_")
    ws_root = data_root() / "local" / f"{test_name}_{test_id}"
    ws_root.mkdir(parents=True, exist_ok=True)

    cli = FptCli(bin_dir=fptbin, work_dir=ws_root, verbose=1)

    yield cli

    # cleanup unless test failed and user wants to inspect
    if not hasattr(request.node, "rep_call") or request.node.rep_call.passed:
        keep = os.environ.get("FPT_KEEP_ON_FAILURE", "").lower() in ("1", "true")
        if not keep:
            shutil.rmtree(ws_root, ignore_errors=True)


@pytest.hookimpl(tryfirst=True, hookwrapper=True)
def pytest_runtest_makereport(item, call):
    """Store test result on the item so fixtures can inspect pass/fail."""
    outcome = yield
    rep = outcome.get_result()
    setattr(item, f"rep_{rep.when}", rep)


# ---------------------------------------------------------------------------
# Transport availability marks
# ---------------------------------------------------------------------------

def pytest_collection_modifyitems(config, items):
    """Auto-skip tests that require NFS/SMB when transports are not configured."""
    needs_nfs = pytest.mark.skip(reason="NFS transport not configured (set FPT_NFS_MOUNT)")
    needs_smb = pytest.mark.skip(reason="SMB transport not configured (set FPT_SMB_MOUNT)")

    for item in items:
        if "nfs" in item.keywords and not transport_available(Transport.NFS):
            item.add_marker(needs_nfs)
        if "smb" in item.keywords and not transport_available(Transport.SMB):
            item.add_marker(needs_smb)


# ---------------------------------------------------------------------------
# Parametrize helpers
# ---------------------------------------------------------------------------

def available_transports() -> list[Transport]:
    """Return transports that are currently configured."""
    return [t for t in Transport if transport_available(t)]


def transport_params() -> list[Transport]:
    """Parametrize values for a single transport axis."""
    return available_transports()


def transport_pair_params() -> list[tuple[Transport, Transport]]:
    """Parametrize values for source x target transport pairs."""
    avail = available_transports()
    return [(s, t) for s in avail for t in avail]
