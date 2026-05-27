"""Smoke test: 3x3 transport matrix (local / NFS / SMB).

Tests fptcli backup/restore across all configured transport combinations.
Each pair (source x target) is tested with a small but representative dataset.

Requirements:
    All transports share the same underlying storage (e.g. /opt/dataset).
    NFS mount:  sudo mount -t nfs 127.0.0.1:/opt/dataset /mnt/nfs
    SMB mount:  sudo mount -t cifs //127.0.0.1/dataset /mnt/smb -o ...

Environment variables:
    FPT_DATA_ROOT   Local data root (default: /opt/fpt_test_data)
    FPT_NFS_MOUNT   NFS mount point (e.g. /mnt/nfs) — enables NFS tests
    FPT_SMB_MOUNT   SMB mount point (e.g. /mnt/smb) — enables SMB tests
    FPT_NFS_HOST    NFS server (default: 127.0.0.1)
    FPT_NFS_EXPORT  NFS export path (default: /opt/dataset)
    FPT_NFS_UID     NFS uid
    FPT_NFS_GID     NFS gid
    FPT_SMB_HOST    SMB server (default: 127.0.0.1)
    FPT_SMB_SHARE   SMB share name (default: dataset)
    FPT_SMB_USER    SMB username
    FPT_SMB_PASSWORD SMB password
"""

import os
import shutil
import uuid
import pytest
from pathlib import Path

from framework import (
    FptCli, Transport,
    transport_available, transport_mount, transport_location,
    create_fileset, create_empty_dirs, create_symlinks, create_hardlinks,
    create_special_filenames, find_copy_dir, file_hash, count_files,
)


def _transport_pair_id(pair: tuple[Transport, Transport]) -> str:
    s, t = pair
    return f"{s.value}_to_{t.value}"


def _all_transport_pairs() -> list[tuple[Transport, Transport]]:
    """Return all (source, target) pairs for configured transports."""
    avail = [t for t in Transport if transport_available(t)]
    return [(s, t) for s in avail for t in avail]


def _create_test_dataset(root: Path):
    """Create a representative dataset covering multiple file types.

    Layout:
        normal/       — 10 regular files x 4KB, depth=2
        empty_dirs/   — 3 empty directories
        special/      — files with unusual names
        (if supported) symlinks and hardlinks
    """
    # normal files
    normal = root / "normal"
    create_fileset(normal, depth=2, files_per_dir=5, dirs_per_dir=2, file_size=4096)

    # empty directories
    create_empty_dirs(root / "empty_dirs", [
        "sub_a", "sub_b/deep", "sub_c/deep/deeper",
    ])

    # special filenames
    create_special_filenames(root / "special")

    # symlinks (if supported)
    try:
        link_target = root / "link_data.txt"
        link_target.write_text("link target")
        link = root / "my_link"
        link.symlink_to("link_data.txt")
    except (OSError, NotImplementedError):
        pass


def _verify_test_dataset(original: Path, restored: Path) -> list[str]:
    """Verify restored data matches original. Returns list of error messages."""
    errors = []

    # check all original files exist in restored with matching content
    for src in sorted(original.rglob("*")):
        if not src.is_file():
            continue
        if src.is_symlink():
            continue  # symlinks may or may not be preserved
        rel = src.relative_to(original)
        dst = restored / rel
        if not dst.exists():
            errors.append(f"missing: {rel}")
        elif src.stat().st_size != dst.stat().st_size:
            errors.append(f"size mismatch: {rel}")
        elif src.stat().st_size > 0 and file_hash(src) != file_hash(dst):
            errors.append(f"content mismatch: {rel}")

    # check empty dirs
    for src in sorted(original.rglob("*")):
        if src.is_dir() and not any(src.iterdir()):
            rel = src.relative_to(original)
            dst = restored / rel
            if not dst.is_dir():
                errors.append(f"missing empty dir: {rel}")
            elif any(dst.iterdir()):
                errors.append(f"empty dir not empty: {rel}")

    return errors


@pytest.mark.parametrize(
    "src_transport,tgt_transport",
    _all_transport_pairs(),
    ids=[_transport_pair_id(p) for p in _all_transport_pairs()],
)
def test_transport_matrix(
    fptbin: Path,
    src_transport: Transport,
    tgt_transport: Transport,
    request,
):
    """Backup from src_transport, restore to tgt_transport, verify."""
    test_id = uuid.uuid4().hex[:10]
    test_name = f"matrix_{src_transport.value}_to_{tgt_transport.value}_{test_id}"

    # workspace for local metadata/logs
    ws = Path(os.environ.get("FPT_DATA_ROOT", "/opt/fpt_test_data")) / "_workspace" / test_name
    ws.mkdir(parents=True, exist_ok=True)

    fpt = FptCli(bin_dir=fptbin, work_dir=ws, verbose=1)

    # write test data to source transport mount
    src_mount = transport_mount(src_transport)
    data_dir = src_mount / f"_test_data_{test_id}"
    data_dir.mkdir(parents=True, exist_ok=True)

    _create_test_dataset(data_dir)
    n_files = count_files(data_dir)
    assert n_files > 0, "No test files created"

    try:
        # build fptcli locations
        src_loc = transport_location(src_transport, f"_test_data_{test_id}")
        tgt_loc = transport_location(tgt_transport, f"_test_backup_{test_id}")

        # pre-create target backup dir on the target transport mount
        # (NFS/SMB require sub_path to exist before fptcli connects)
        tgt_mount = transport_mount(tgt_transport)
        backup_root = tgt_mount / f"_test_backup_{test_id}"
        backup_root.mkdir(parents=True, exist_ok=True)

        # backup
        bk = fpt.backup(src_loc, tgt_loc, timeout=120, log_name="backup")
        fpt.assert_success(bk, f"Backup {src_transport.value}→{tgt_transport.value} failed: ")

        # find copy dir on target mount
        copy_dir = find_copy_dir(backup_root)
        assert copy_dir is not None, f"No COPY_* dir found on {tgt_transport.value}"

        # restore to target mount
        restore_root = tgt_mount / f"_test_restore_{test_id}"
        restore_root.mkdir(parents=True, exist_ok=True)
        rs = fpt.restore(str(copy_dir), str(restore_root), timeout=120, log_name="restore")
        fpt.assert_success(rs, f"Restore {src_transport.value}→{tgt_transport.value} failed: ")

        # verify restored data matches original
        errors = _verify_test_dataset(data_dir, restore_root)
        assert not errors, (
            f"Verification failed for {src_transport.value}→{tgt_transport.value}:\n"
            + "\n".join(errors[:20])
        )

        if os.environ.get("FPT_KEEP_ON_FAILURE", "").lower() in ("1", "true"):
            print(f"\n[{src_transport.value}→{tgt_transport.value}] Kept on success:")
            print(f"  source:  {data_dir}")
            print(f"  backup:  {backup_root}")
            print(f"  restore: {restore_root}")

    finally:
        # cleanup unless FPT_KEEP_ON_FAILURE=1
        keep = os.environ.get("FPT_KEEP_ON_FAILURE", "").lower() in ("1", "true")
        if not keep:
            shutil.rmtree(data_dir, ignore_errors=True)
            for t in [tgt_transport]:
                mount = transport_mount(t)
                for subdir in [f"_test_backup_{test_id}", f"_test_restore_{test_id}"]:
                    shutil.rmtree(mount / subdir, ignore_errors=True)
