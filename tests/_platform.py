"""Platform abstraction for cross-platform test support.

Centralizes all platform-dependent behavior so that test code and the
framework never need scattered ``if platform.system() == ...`` checks.

Usage::

    from platform import bin_name, can_detect_sparse, IS_WINDOWS

    exe = bin_name("fptcli")          # "fptcli.exe" on Windows
"""

from __future__ import annotations

import os
import platform as _platform
from pathlib import Path

import pytest

# ---------------------------------------------------------------------------
# Detection
# ---------------------------------------------------------------------------

IS_LINUX: bool = _platform.system() == "Linux"
IS_WINDOWS: bool = _platform.system() == "Windows"
IS_UNIX: bool = os.name == "posix"

# ---------------------------------------------------------------------------
# Binary helpers
# ---------------------------------------------------------------------------

def bin_name(name: str) -> str:
    """Return the platform-correct binary name (``.exe`` suffix on Windows)."""
    return f"{name}.exe" if IS_WINDOWS else name


def resolve_bin_dir(candidates: list[Path]) -> Path | None:
    """Return the first candidate directory that contains ``fptcli``."""
    target = bin_name("fptcli")
    for d in candidates:
        if d.is_dir() and (d / target).exists():
            return d
    return None

# ---------------------------------------------------------------------------
# Sparse file detection
# ---------------------------------------------------------------------------

def can_detect_sparse() -> bool:
    """Whether the platform can detect sparse files via ``st_blocks``."""
    return IS_UNIX

# ---------------------------------------------------------------------------
# Pytest marks
# ---------------------------------------------------------------------------

skip_unless_linux = pytest.mark.skipif(not IS_LINUX, reason="Linux only")
skip_unless_unix = pytest.mark.skipif(not IS_UNIX, reason="Unix/POSIX only")
skip_unless_symlink = pytest.mark.skipif(
    IS_WINDOWS, reason="Symlink handling varies on Windows",
)
skip_unless_hardlink = pytest.mark.skipif(
    IS_WINDOWS, reason="Hardlinks not fully supported on Windows",
)
