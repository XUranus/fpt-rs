"""
fpt test framework — shared infrastructure for smoke and performance tests.

Provides:
- Binary discovery and CLI wrappers (fptcli, fsscan, fsbackup, fsdiff, vdbench, metainspect)
- Transport configuration (local / NFS / SMB) via environment variables
- Fileset creation and verification helpers
- Test workspace lifecycle management

Environment variables
---------------------
FPT_BIN_DIR         Path to compiled binaries (default: target/release)
FPT_DATA_ROOT       Local data root for test artifacts (default: /opt/fpt_test_data)
FPT_NFS_MOUNT       NFS mount point (e.g. /mnt/nfs)
FPT_SMB_MOUNT       SMB mount point (e.g. /mnt/smb)
FPT_NFS_HOST        NFS server host
FPT_NFS_EXPORT      NFS export path
FPT_NFS_UID         NFS AUTH_UNIX uid
FPT_NFS_GID         NFS AUTH_UNIX gid
FPT_SMB_SHARE       SMB share name
FPT_SMB_USER        SMB username
FPT_SMB_PASSWORD    SMB password
"""

from __future__ import annotations

import hashlib
import logging
import os
import platform
import re
import shutil
import subprocess
import uuid
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Optional

import pytest

logger = logging.getLogger("fpt_test")


# ---------------------------------------------------------------------------
# Transport types
# ---------------------------------------------------------------------------

class Transport(Enum):
    LOCAL = "local"
    NFS = "nfs"
    SMB = "smb"


def _env(key: str, default: str = "") -> str:
    return os.environ.get(key, default)


def _env_int(key: str, default: int) -> int:
    try:
        return int(os.environ.get(key, str(default)))
    except ValueError:
        return default


def transport_available(t: Transport) -> bool:
    """Return True if the given transport is configured via env vars."""
    if t == Transport.LOCAL:
        return True
    if t == Transport.NFS:
        return bool(_env("FPT_NFS_MOUNT"))
    if t == Transport.SMB:
        return bool(_env("FPT_SMB_MOUNT"))
    return False


def require_transport(t: Transport):
    """Return a pytest skip decorator if transport is not available."""
    if not transport_available(t):
        return pytest.mark.skip(reason=f"Transport {t.value} not configured")
    return pytest.mark.noop  # no-op decorator


def transport_id(t: Transport) -> str:
    return t.value


def data_root() -> Path:
    return Path(_env("FPT_DATA_ROOT", "/opt/fpt_test_data"))


def transport_mount(t: Transport) -> Path:
    """Return the base mount/path for a transport under the data root."""
    if t == Transport.LOCAL:
        return data_root() / "local"
    if t == Transport.NFS:
        return Path(_env("FPT_NFS_MOUNT", "/mnt/nfs"))
    if t == Transport.SMB:
        return Path(_env("FPT_SMB_MOUNT", "/mnt/smb"))
    raise ValueError(f"Unknown transport: {t}")


def nfs_location(subpath: str = "") -> str:
    """Build an nfs:// URL for fptcli.

    Format: ``nfs://host/export?sub=subpath&uid=X&gid=Y``

    UID/GID default to the current process uid/gid if not set via
    ``FPT_NFS_UID`` / ``FPT_NFS_GID`` — libnfs requires explicit auth.
    """
    host = _env("FPT_NFS_HOST", "127.0.0.1")
    export = _env("FPT_NFS_EXPORT", "/opt/dataset")
    loc = f"nfs://{host}{export}"

    params: list[str] = []
    if subpath:
        params.append(f"sub={subpath}")
    uid = _env("FPT_NFS_UID") or str(os.getuid())
    gid = _env("FPT_NFS_GID") or str(os.getgid())
    params.append(f"uid={uid}")
    params.append(f"gid={gid}")
    if params:
        loc += "?" + "&".join(params)
    return loc


def smb_location(subpath: str = "") -> str:
    """Build an smb:// URL for fptcli.

    Format: ``smb://host/share/sub/path?username=u&password=p``
    """
    host = _env("FPT_SMB_HOST", "127.0.0.1")
    share = _env("FPT_SMB_SHARE", "dataset")
    user = _env("FPT_SMB_USER", "xuranus")
    password = _env("FPT_SMB_PASSWORD", "123456789")

    loc = f"smb://{host}/{share}"
    if subpath:
        loc = f"{loc.rstrip('/')}/{subpath}"

    params: list[str] = []
    if user:
        params.append(f"username={user}")
    if password:
        params.append(f"password={password}")
    if params:
        loc += "?" + "&".join(params)
    return loc


def transport_location(t: Transport, subpath: str = "") -> str:
    """Return the fptcli location string for a transport + subpath.

    - LOCAL: absolute filesystem path
    - NFS:   ``nfs://host/export?sub=subpath&uid=X&gid=Y``
    - SMB:   ``smb://host/share/subpath?username=u&password=p``
    """
    if t == Transport.LOCAL:
        return str(transport_mount(t) / subpath) if subpath else str(transport_mount(t))
    if t == Transport.NFS:
        return nfs_location(subpath)
    if t == Transport.SMB:
        return smb_location(subpath)
    raise ValueError(f"Unknown transport: {t}")


# ---------------------------------------------------------------------------
# CLI result
# ---------------------------------------------------------------------------

@dataclass
class CliResult:
    """Result of a CLI tool invocation."""
    returncode: int
    stdout: str
    stderr: str
    command: list[str]
    duration_sec: float = 0.0

    @property
    def success(self) -> bool:
        return self.returncode == 0

    def __repr__(self) -> str:
        status = "OK" if self.success else f"FAIL({self.returncode})"
        cmd = " ".join(self.command[:4])
        return f"CliResult<{status} {cmd}... @{self.duration_sec:.1f}s>"


# ---------------------------------------------------------------------------
# CLI wrapper
# ---------------------------------------------------------------------------

class FptCli:
    """Wraps all fpt CLI binaries for convenient invocation in tests.

    Args:
        bin_dir: Directory containing compiled binaries.
        work_dir: Per-test workspace root.
        verbose: Verbosity level passed to ``-v`` flags.
    """

    def __init__(self, bin_dir: Path, work_dir: Path, verbose: int = 0):
        self.bin_dir = bin_dir
        self.work_dir = work_dir
        self.verbose = verbose

        self.source_dir: Path = work_dir / "source"
        self.backup_dir: Path = work_dir / "backup"
        self.restore_dir: Path = work_dir / "restore"
        self.meta_dir: Path = work_dir / "meta"
        self.ctrl_dir: Path = work_dir / "ctrl"
        self.log_dir: Path = work_dir / "logs"

        for d in [
            self.source_dir, self.backup_dir, self.restore_dir,
            self.meta_dir, self.ctrl_dir, self.log_dir,
        ]:
            d.mkdir(parents=True, exist_ok=True)

        self._results: list[CliResult] = []

    # -- binary resolution ---------------------------------------------------

    def _bin(self, name: str) -> str:
        p = self.bin_dir / name
        if not p.exists():
            raise FileNotFoundError(f"Binary not found: {p}")
        return str(p)

    # -- internal runner -----------------------------------------------------

    def _run(
        self,
        cmd: list[str],
        *,
        timeout: int = 300,
        log_name: str | None = None,
        check: bool = False,
    ) -> CliResult:
        """Run a command, capture output, optionally tee to a log file."""
        logger.info("$ %s", " ".join(cmd))
        import time
        t0 = time.monotonic()
        try:
            proc = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            dur = time.monotonic() - t0
            result = CliResult(
                returncode=-1, stdout="", stderr="TIMEOUT",
                command=cmd, duration_sec=dur,
            )
            self._results.append(result)
            if check:
                raise RuntimeError(f"Command timed out after {timeout}s: {' '.join(cmd)}")
            return result

        dur = time.monotonic() - t0
        result = CliResult(
            returncode=proc.returncode,
            stdout=proc.stdout,
            stderr=proc.stderr,
            command=cmd,
            duration_sec=dur,
        )
        self._results.append(result)

        if log_name:
            log_path = self.log_dir / f"{log_name}.log"
            with open(log_path, "a") as f:
                f.write(f"$ {' '.join(cmd)}\n")
                f.write(f"exit={proc.returncode}  time={dur:.2f}s\n")
                if proc.stdout:
                    f.write(proc.stdout)
                if proc.stderr:
                    f.write("--- stderr ---\n")
                    f.write(proc.stderr)
                f.write("\n")

        if check and proc.returncode != 0:
            raise RuntimeError(
                f"Command failed ({proc.returncode}): {' '.join(cmd)}\n"
                f"stderr: {proc.stderr[:500]}"
            )

        return result

    def _v_flags(self) -> list[str]:
        return ["-v"] * self.verbose if self.verbose else []

    # -- fptcli backup/restore -----------------------------------------------

    def backup(
        self,
        source: str,
        target: str,
        *,
        fmt: str = "common",
        aggregate: bool = False,
        hardlink: bool = False,
        delete: bool = False,
        mtime: bool = False,
        jobs: int = 4,
        workers: int = 8,
        timeout: int = 300,
        log_name: str = "backup",
        extra_args: list[str] | None = None,
    ) -> CliResult:
        cmd = [self._bin("fptcli"), "backup", "-d", source, "-t", target]
        if aggregate:
            cmd.append("--aggregate")
        elif fmt != "common":
            cmd += ["-f", fmt]
        if hardlink:
            cmd.append("--hardlink")
        if delete:
            cmd.append("--delete")
        if mtime:
            cmd.append("--mtime")
        cmd += ["-j", str(jobs), "-w", str(workers)]
        cmd += self._v_flags()
        if extra_args:
            cmd += extra_args
        return self._run(cmd, timeout=timeout, log_name=log_name)

    def restore(
        self,
        copy: str,
        target: str,
        *,
        policy: str = "replace",
        hardlinks: bool = True,
        mtime: bool = True,
        jobs: int = 4,
        workers: int = 8,
        timeout: int = 300,
        log_name: str = "restore",
        extra_args: list[str] | None = None,
    ) -> CliResult:
        cmd = [
            self._bin("fptcli"), "restore",
            "-c", copy, "-t", target,
            "-p", policy,
        ]
        if hardlinks:
            cmd.append("--hardlinks")
        if mtime:
            cmd.append("--mtime")
        cmd += ["-j", str(jobs), "-w", str(workers)]
        cmd += self._v_flags()
        if extra_args:
            cmd += extra_args
        return self._run(cmd, timeout=timeout, log_name=log_name)

    def backup_and_restore(
        self,
        source: str,
        target: str,
        *,
        fmt: str = "common",
        aggregate: bool = False,
        hardlink: bool = False,
        delete: bool = False,
        mtime: bool = False,
        timeout: int = 300,
    ) -> tuple[CliResult, CliResult, Path]:
        """Run backup, parse copy_root from stdout, then restore.

        Returns (backup_result, restore_result, restore_target_dir).
        """
        bk = self.backup(
            source, target,
            fmt=fmt, aggregate=aggregate,
            hardlink=hardlink, delete=delete, mtime=mtime,
            timeout=timeout, log_name="backup",
        )
        if not bk.success:
            return bk, CliResult(-1, "", "backup failed", []), Path()

        copy_root = self._parse_copy_root(bk.stdout)
        if not copy_root:
            logger.warning("Could not parse Copy root from backup output")
            return bk, CliResult(-1, "", "no copy root found", []), Path()

        restore_target = self.restore_dir / f"rt_{uuid.uuid4().hex[:8]}"
        restore_target.mkdir(parents=True, exist_ok=True)

        rs = self.restore(
            str(copy_root), str(restore_target),
            timeout=timeout, log_name="restore",
        )
        return bk, rs, restore_target

    @staticmethod
    def _parse_copy_root(stdout: str) -> Optional[Path]:
        for line in stdout.splitlines():
            m = re.match(r"Copy root\s*:\s*(.+)", line)
            if m:
                return Path(m.group(1).strip())
        return None

    # -- fsscan / fsbackup / fsdiff (low-level) ------------------------------

    def fsscan(
        self,
        source: str,
        *,
        prev_meta_dir: str | None = None,
        scan_hardlinks: bool = False,
        scan_acl: bool = False,
        scan_xattrs: bool = False,
        workers: int = 4,
        writers: int = 1,
        timeout: int = 300,
        log_name: str = "scan",
        extra_args: list[str] | None = None,
    ) -> CliResult:
        cmd = [
            self._bin("fsscan"), source,
            "-c", str(self.ctrl_dir), "-m", str(self.meta_dir),
            "-w", str(workers), "-W", str(writers),
        ]
        if prev_meta_dir:
            cmd += ["--prev-meta-dir", prev_meta_dir]
        if scan_hardlinks:
            cmd.append("--scan-hardlinks")
        if scan_acl:
            cmd.append("--scan-acl")
        if scan_xattrs:
            cmd.append("--scan-xattrs")
        cmd += self._v_flags()
        if extra_args:
            cmd += extra_args
        return self._run(cmd, timeout=timeout, log_name=log_name)

    def fsbackup(
        self,
        source: str,
        target: str,
        control_file: str,
        *,
        hardlink: bool = False,
        delete: bool = False,
        mtime: bool = False,
        aggregate: bool = False,
        timeout: int = 300,
        log_name: str = "backup_low",
        extra_args: list[str] | None = None,
    ) -> CliResult:
        cmd = [
            self._bin("fsbackup"),
            "-s", source, "-t", target,
            "-m", str(self.meta_dir), "-c", control_file,
        ]
        if hardlink:
            cmd.append("--hardlink")
        if delete:
            cmd.append("--delete")
        if mtime:
            cmd.append("--mtime")
        if aggregate:
            cmd.append("--aggregate")
        cmd += self._v_flags()
        if extra_args:
            cmd += extra_args
        return self._run(cmd, timeout=timeout, log_name=log_name)

    def fsdiff(
        self,
        source: str,
        target: str,
        *,
        compare_acl: bool = False,
        compare_xattrs: bool = False,
        compare_mtime: bool = False,
        follow_links: bool = False,
        timeout: int = 300,
        log_name: str = "diff",
    ) -> CliResult:
        cmd = [
            self._bin("fsdiff"), "-s", source, "-t", target,
        ]
        if compare_acl:
            cmd.append("--compare-acl")
        if compare_xattrs:
            cmd.append("--compare-xattrs")
        if compare_mtime:
            cmd.append("--compare-mtime")
        if follow_links:
            cmd.append("-f")
        cmd += self._v_flags()
        return self._run(cmd, timeout=timeout, log_name=log_name)

    # -- vdbench (fileset generator) -----------------------------------------

    def vdbench(
        self,
        output: str | Path,
        *,
        depth: int = 1,
        files: int = 10,
        dirs: int = 2,
        size: str = "1K",
        threads: int = 4,
        timeout: int = 120,
        log_name: str = "vdbench",
    ) -> CliResult:
        cmd = [
            self._bin("vdbench"),
            "-o", str(output),
            "-d", str(depth), "-f", str(files), "-r", str(dirs),
            "-s", size, "-t", str(threads), "-y",
        ]
        return self._run(cmd, timeout=timeout, log_name=log_name)

    # -- metainspect ---------------------------------------------------------

    def metainspect(
        self,
        file: str | Path,
        *,
        fmt: str = "json",
        timeout: int = 30,
    ) -> CliResult:
        cmd = [self._bin("metainspect"), str(file), f"--{fmt}"]
        return self._run(cmd, timeout=timeout)

    # -- control file helpers ------------------------------------------------

    def find_control_file(self, keyword: str = "copy") -> Optional[Path]:
        """Find a ``*.control.bin`` file in ctrl_dir matching *keyword*."""
        for p in self.ctrl_dir.glob("*.control.bin"):
            if keyword.lower() in p.name.lower():
                return p
        # fallback: return first control file
        ctrl_files = list(self.ctrl_dir.glob("*.control.bin"))
        return ctrl_files[0] if ctrl_files else None

    # -- assertions ----------------------------------------------------------

    def assert_success(self, result: CliResult, msg: str = ""):
        assert result.success, (
            f"{msg}Command failed (rc={result.returncode}): "
            f"{' '.join(result.command)}\nstderr: {result.stderr[:500]}"
        )

    def assert_fsdiff_clean(self, source: str, target: str, **kwargs):
        r = self.fsdiff(source, target, **kwargs)
        assert r.success, (
            f"fsdiff found differences:\n{r.stdout[:2000]}"
        )


# ---------------------------------------------------------------------------
# Fileset helpers
# ---------------------------------------------------------------------------

def create_fileset(
    root: Path,
    *,
    depth: int = 1,
    files_per_dir: int = 5,
    dirs_per_dir: int = 2,
    file_size: int = 1024,
) -> int:
    """Create a deterministic fileset under *root*.

    Returns total file count.
    """
    root.mkdir(parents=True, exist_ok=True)

    def _fill(directory: Path, level: int) -> int:
        count = 0
        for i in range(files_per_dir):
            fp = directory / f"file_{level}_{i}.dat"
            fp.write_bytes(
                (f"file L{level} N{i} " * ((file_size // 18) + 1))[:file_size].encode()
            )
            count += 1
        if level < depth:
            for j in range(dirs_per_dir):
                sub = directory / f"dir_{level}_{j}"
                sub.mkdir(exist_ok=True)
                count += _fill(sub, level + 1)
        return count

    return _fill(root, 0)


def create_empty_dirs(root: Path, names: list[str]) -> list[Path]:
    """Create empty directories under *root*."""
    dirs = []
    for n in names:
        d = root / n
        d.mkdir(parents=True, exist_ok=True)
        dirs.append(d)
    return dirs


def create_symlinks(root: Path) -> dict[str, Path]:
    """Create various symlinks. Returns dict of description -> link path.

    Skips on platforms without symlink support.
    """
    links: dict[str, Path] = {}
    if not os.access(root, os.W_OK):
        return links

    # target files/dirs for symlinks
    (root / "link_target.txt").write_text("symlink target content")
    (root / "link_target_dir").mkdir(exist_ok=True)
    (root / "link_target_dir" / "inside.txt").write_text("inside dir")

    # relative file link
    lf = root / "link_to_file"
    try:
        lf.symlink_to("link_target.txt")
        links["relative_file"] = lf
    except (OSError, NotImplementedError):
        pass

    # relative dir link
    ld = root / "link_to_dir"
    try:
        ld.symlink_to("link_target_dir")
        links["relative_dir"] = ld
    except (OSError, NotImplementedError):
        pass

    # absolute file link
    la = root / "link_abs_file"
    try:
        la.symlink_to(root / "link_target.txt")
        links["absolute_file"] = la
    except (OSError, NotImplementedError):
        pass

    # broken link
    lb = root / "link_broken"
    try:
        lb.symlink_to("nonexistent_target")
        links["broken"] = lb
    except (OSError, NotImplementedError):
        pass

    # chain link -> link_to_file -> link_target.txt
    lc = root / "link_chain"
    try:
        lc.symlink_to("link_to_file")
        links["chain"] = lc
    except (OSError, NotImplementedError):
        pass

    return links


def create_hardlinks(root: Path) -> dict[str, list[Path]]:
    """Create hardlink groups. Returns dict of group_name -> [paths].

    Skips on platforms without hardlink support (e.g. Windows).
    """
    groups: dict[str, list[Path]] = {}
    content = b"hardlink shared content\n"

    def _make_group(name: str, paths: list[Path]):
        paths[0].write_bytes(content)
        linked = [paths[0]]
        for p in paths[1:]:
            try:
                p.hardlink_to(paths[0])
                linked.append(p)
            except (OSError, NotImplementedError):
                pass
        if len(linked) > 1:
            groups[name] = linked

    # pair in same dir
    _make_group("pair", [root / "hl_a.txt", root / "hl_a_link.txt"])

    # triple across dirs
    (root / "hl_sub").mkdir(exist_ok=True)
    _make_group("triple", [
        root / "hl_b.txt",
        root / "hl_sub" / "hl_b_link1.txt",
        root / "hl_sub" / "hl_b_link2.txt",
    ])

    # deep nested
    deep = root / "hl_deep" / "level1"
    deep.mkdir(parents=True, exist_ok=True)
    _make_group("deep", [root / "hl_c.txt", deep / "hl_c_link.txt"])

    return groups


def create_sparse_files(root: Path) -> dict[str, Path]:
    """Create sparse files. Returns dict of description -> path.

    Falls back to regular files if sparse creation is unsupported.
    """
    sparse: dict[str, Path] = {}

    def _make_sparse(name: str, apparent_size: int, data_regions: list[tuple[int, int, bytes]]):
        """Create file with holes via seek+truncate."""
        p = root / name
        try:
            with open(p, "wb") as f:
                for offset, length, pattern in data_regions:
                    f.seek(offset)
                    f.write((pattern * (length // len(pattern) + 1))[:length])
                f.seek(apparent_size)
                f.truncate()
            sparse[name] = p
        except OSError:
            pass

    # 100MB with data at head and tail
    _make_sparse("sparse_ends.dat", 100 * 1024 * 1024, [
        (0, 4096, b"A"),
        (100 * 1024 * 1024 - 4096, 4096, b"Z"),
    ])

    # 50MB with hole in the middle
    _make_sparse("sparse_middle.dat", 50 * 1024 * 1024, [
        (0, 8192, b"B"),
        (50 * 1024 * 1024 - 8192, 8192, b"Y"),
    ])

    # 10MB small sparse
    _make_sparse("sparse_small.dat", 10 * 1024 * 1024, [
        (0, 4096, b"S"),
        (10 * 1024 * 1024 - 4096, 4096, b"E"),
    ])

    # 20MB with 10 holes (data at every 2MB mark)
    multi_regions = []
    for i in range(10):
        offset = i * 2 * 1024 * 1024
        multi_regions.append((offset, 4096, b"M"))
    _make_sparse("sparse_multi.dat", 20 * 1024 * 1024, multi_regions)

    return sparse


def create_special_filenames(root: Path) -> list[Path]:
    """Create files with unusual but valid filenames."""
    root.mkdir(parents=True, exist_ok=True)
    names = [
        "file with spaces.txt",
        "file-with-dashes.txt",
        "file.with.dots.txt",
        "UPPERCASE.TXT",
        "file@at.txt",
        "file+plus.txt",
        "file_under_score.txt",
    ]
    created = []
    for n in names:
        p = root / n
        try:
            p.write_text(f"content of {n}")
            created.append(p)
        except OSError:
            pass  # some FS may reject certain names
    return created


def create_permission_files(root: Path) -> dict[str, Path]:
    """Create files with various permission modes (Unix only)."""
    import stat as statmod
    files: dict[str, Path] = {}
    modes = {
        "mode_644": 0o644,
        "mode_600": 0o600,
        "mode_755": 0o755,
        "mode_777": 0o777,
        "mode_400": 0o400,
    }
    for name, mode in modes.items():
        p = root / name
        p.write_text(f"permission test {name}")
        try:
            os.chmod(p, mode)
            files[name] = p
        except OSError:
            pass
    return files


def count_files(root: Path) -> int:
    """Count regular files recursively."""
    return sum(1 for _ in root.rglob("*") if _.is_file())


def count_dirs(root: Path) -> int:
    """Count directories recursively."""
    return sum(1 for _ in root.rglob("*") if _.is_dir())


def file_hash(path: Path) -> str:
    """SHA256 hex digest of a file."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def is_sparse(path: Path) -> bool:
    """Check if file occupies fewer blocks than its apparent size suggests."""
    try:
        st = os.stat(path)
        if st.st_size == 0:
            return False
        actual = st.st_blocks * 512
        return actual < st.st_size
    except (AttributeError, OSError):
        return False


def find_copy_dir(backup_root: Path) -> Optional[Path]:
    """Find the COPY_* directory created by fptcli backup."""
    copies = [p for p in backup_root.iterdir() if p.is_dir() and p.name.startswith("COPY_")]
    return copies[0] if copies else None


# ---------------------------------------------------------------------------
# Platform helpers
# ---------------------------------------------------------------------------

IS_LINUX = platform.system() == "Linux"
IS_WINDOWS = platform.system() == "Windows"
IS_UNIX = os.name == "posix"

skip_unless_linux = pytest.mark.skipif(not IS_LINUX, reason="Linux only")
skip_unless_unix = pytest.mark.skipif(not IS_UNIX, reason="Unix/POSIX only")
