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
| File attributes | Yes | Yes | Partial | READONLY, HIDDEN, SYSTEM etc. stored in `attr` field |
| Security descriptors | Yes | No | No | SDDL string captured during scan; restore not yet implemented |
| Sparse files | Partial | Yes | No | Size preserved; sparseness not detected or recreated |
| Long filenames | Yes | Yes | Yes | Up to 255 chars per component |
| Long paths | Partial | Partial | Partial | Limited by MAX_PATH without `\\?\` prefix |
| Incremental backup | Yes | Yes | Yes | File index via `FILE_ID_INFO` |

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

## Known Limitations

1. **Security Descriptor restore**: SDs are read during scan (stored as SDDL) but not written back during restore. The `restore_common_metadata` function does not call `SetSecurityInfo`.

2. **Attribute restore**: `restore_windows_attrs` is defined but may not be called in all restore code paths (the async restore pipeline in `restore_pipeline.rs` does not call `restore_common_metadata`).

3. **Sparse file sparseness**: The scanner does not detect sparse ranges on Windows (`detect_sparse_ranges` returns `None`). During restore, sparse files are created as regular files — `FSCTL_SET_SPARSE` is not called.

4. **Long paths**: Paths exceeding MAX_PATH (260) require the `\\?\` prefix which is not currently applied. The Cargo.toml does not include a `longPathAware` manifest.

5. **File index collision**: The 128-bit `FILE_ID_INFO` is XOR-folded to u64, which can theoretically cause collisions. The `devno` is 0 on Windows, so cross-volume collisions are possible.

## Testing

Windows tests are in `tests/smoke/test_windows.py` (marked `@pytest.mark.skipif(not IS_WINDOWS)`).

Additional tests that now run on Windows:
- `test_hardlinks` — hardlink backup/restore with content verification
- `test_symlinks_backup_succeeds` — symlink backup/restore (runtime privilege check)

Run all Windows-compatible tests:
```bash
python -m pytest tests/smoke/ -v
```
