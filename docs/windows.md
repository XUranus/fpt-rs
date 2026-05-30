# Windows Platform Support

This document describes the Windows-specific implementation details for Fpt's scanner and backup engine.

## Supported Features

| Feature | Scan | Backup | Restore | Notes |
|---------|------|--------|---------|-------|
| Regular files | Yes | Yes | Yes | Full content + metadata |
| Directories | Yes | Yes | Yes | Including empty dirs |
| Hardlinks | Yes | Yes | Yes | NTFS; via `NumberOfLinks` query |
| Symlinks (file) | Yes | Yes | Yes | Requires admin or Developer Mode |
| Symlinks (directory) | Yes | Yes | Yes | Uses `symlink_dir` on restore |
| Junctions | Yes | Yes | Yes | Via `read_link` / `mklink /J` |
| File attributes | Yes | Yes | Yes | READONLY, HIDDEN, SYSTEM etc. stored in `attr` field, restored via SetFileAttributesW |
| Security descriptors | Yes | Partial | Partial | SDDL captured via GetKernelObjectSecurity; restored via ConvertStringSecurityDescriptorToSecurityDescriptorW + SetSecurityInfo |
| Sparse files | Yes | Yes | Yes | Detected via GetCompressedFileSizeW; restored via FSCTL_SET_SPARSE + FSCTL_SET_ZERO_DATA |
| Long filenames | Yes | Yes | Yes | Up to 255 chars per component |
| Long paths | Yes | Yes | Yes | `\\?\` prefix applied for paths > 240 chars; `longPathAware` manifest embedded |
| Incremental backup | Yes | Yes | Yes | File index via `FILE_ID_INFO` |
| Local → SMB backup | — | Yes | — | Tested on Windows loopback share |
| SMB → Local backup | — | Yes | — | query_info fallback for deserialization failure |
| SMB → SMB backup | — | Yes | — | Tested on Windows loopback share |
| SMB target restore | — | — | Yes | Via local mount path access to SMB share |

## Architecture

### Path Representation

All metadata stores paths with forward-slash (`/`) separators regardless of platform. The `path_util` module provides conversion:

- `to_logical_string(Path) -> String` — native path to forward-slash
- `normalize_logical(&str) -> String` — normalize any path string to `/` form
- `make_relative_and_join(source, target, path, logical)` — cross-platform prefix stripping

### Scanner

The scanner (`src/scanner/`) uses `std::fs::read_dir` for traversal which returns native `\`-separated paths on Windows. `DirMeta.path` is stored via `to_logical_string()` to ensure forward slashes in metadata.

Hidden file detection checks both dot-prefix (Unix convention) and `FILE_ATTRIBUTE_HIDDEN` (Windows convention).

### Native Stat (`src/native/fstat.rs`)

Windows file metadata is retrieved via Win32 APIs:

- `CreateFileW` with `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`
- `GetFileInformationByHandleEx(FileBasicInfo)` — timestamps and attributes
- `GetFileInformationByHandleEx(FileIdInfo)` — 128-bit file ID (folded to u64)
- `GetFileInformationByHandleEx(FileStandardInfo)` — `NumberOfLinks` for hardlink detection
- `GetKernelObjectSecurity` + `ConvertSecurityDescriptorToStringSecurityDescriptorW` — SDDL

### Backup Engine (`src/backup/`)

- **Copy phase**: Standard file copy preserving content.
- **Hardlink phase**: Uses `std::fs::hard_link()` which maps to `CreateHardLinkW`.
- **Delete phase**: Path matching uses logical (forward-slash) string comparison.
- **Mtime phase**: Uses `FileTimes::set_accessed/set_modified`.

### Restore

- `create_symlink()` uses `symlink_dir` for directory targets, `symlink_file` for files.
- `restore_windows_attrs()` calls `SetFileAttributesW` to restore file attributes.
- `restore_windows_sd()` converts SDDL string back via `ConvertStringSecurityDescriptorToSecurityDescriptorW` and applies via `SetSecurityInfo`.
- `restore_sparse()` marks files sparse via `DeviceIoControl(FSCTL_SET_SPARSE)` and punches holes via `FSCTL_SET_ZERO_DATA`.

## SMB Transport Matrix (Windows)

All 4 local↔SMB directions tested and verified on Windows 10 loopback SMB share:

| Direction | Status | MD5 Verified |
|-----------|--------|--------------|
| Local → SMB | ✅ | Yes |
| SMB → Local (backup) | ✅ | Yes |
| SMB → SMB | ✅ | Yes |
| SMB target restore | ✅ | Yes |

**Known SMB issue**: `FileAllInformation` query deserialization fails on Windows
local loopback SMB shares due to smb-rs library binary layout mismatch. The scanner
falls back to a minimal DirMeta and continues enumeration successfully.
See [smb.md](smb.md) for analysis.

## Known Limitations

1. **Security Descriptor restore scope**: Only DACL is restored (no SACL — requires `SE_SECURITY_NAME` privilege). Owner/Group are set when present in SDDL.

2. **Sparse hole ranges**: The scanner detects that a file is sparse (via `GetCompressedFileSizeW`) but does not record exact hole locations. During restore, the entire file content is written first, then trailing zero regions are punched. Intermediate holes are not restored.

3. **File index collision**: The 128-bit `FILE_ID_INFO` is XOR-folded to u64, which can theoretically cause collisions. The `devno` is 0 on Windows, so cross-volume collisions are possible.

4. **SMB→SMB restore via SMB URL**: The `fptcli restore --copy smb://...` is not yet implemented. SMB backups must be restored via local mount path access to the SMB share.

## Testing

Windows tests are in `tests/smoke/test_windows.py` (marked `@pytest.mark.skipif(not IS_WINDOWS)`).

Additional tests that now run on Windows:
- `test_hardlinks` — hardlink backup/restore with content verification
- `test_symlinks_backup_succeeds` — symlink backup/restore (runtime privilege check)

Run all Windows-compatible tests:
```bash
python -m pytest tests/smoke/ -v
```
