# Hardlink Backup Support

## Overview

Bifrost now supports backing up and restoring hardlinked files. When multiple files share the same inode (hardlinks), Bifrost will:

1. **During Scan**: Detect and record all files that share the same inode
2. **During Backup (Copy Phase)**: Copy the first file in each hardlink group normally
3. **During Backup (Hardlink Phase)**: Create additional files as hardlinks to the first file
4. **During Restore**: Preserve hardlink relationships in the target filesystem

## Benefits

- **Space Efficiency**: Hardlinks in the backup target consume the same space as the source
- **Consistency**: Hardlink relationships are preserved across backup and restore
- **Performance**: Only one copy of data is transferred, subsequent hardlinks are created locally

## Control File Format

The hardlink control file (`hardlink.txt`) uses a text-based format:

```text
#BIFROST_HARDLINK_CTRL_FILE V1 FILES=<N> INODES=<M> TIME=<UNIX_TIMESTAMP>

I <INODE:16HEX> <DEVICE:16HEX> <LINK_COUNT:8HEX>
F <META_FID:8HEX> <META_OFFSET:8HEX> <PATH_LEN:8HEX> <PATH>
```

### Entry Types

- **Inode Entry (`I`)**: Marks the start of a new hardlink group
  - `INODE`: The inode number (hex)
  - `DEVICE`: The device number (hex)
  - `LINK_COUNT`: Number of hardlinks to this inode

- **File Entry (`F`)**: A file belonging to the current inode group
  - `META_FID`: Metadata file ID
  - `META_OFFSET`: Offset in metadata file
  - `PATH_LEN`: Length of the path string
  - `PATH`: Full path to the file

### Example

```text
#BIFROST_HARDLINK_CTRL_FILE V1 FILES=3 INODES=1 TIME=1700000000

I 000000000000ABCD 0000000000000801 00000003
F 00000000 00000100 00000014 /home/user/file1.txt
F 00000000 00000150 00000014 /home/user/file2.txt
F 00000000 000001A0 00000014 /home/user/file3.txt
```

## Usage

### Scanning with Hardlink Detection

Use the `--scan-hardlinks` flag with `fsscan`:

```bash
fsscan --scan-hardlinks -i /source/path -c /output/ctrl -m /output/meta
```

This will:
1. Scan all files in the source path
2. Detect files with `nlink > 1` (multiple hardlinks)
3. Generate `hardlink.txt` in the control directory

### Backing Up with Hardlink Preservation

Use the `--hardlink` flag with `fsbackup`:

```bash
fsbackup --hardlink \
    -s /source/path \
    -t /target/path \
    -m /output/meta \
    -c /output/ctrl/ctrl.txt
```

This will:
1. Run the copy phase (copy all files normally)
2. Run the hardlink phase (create hardlinks for subsequent files in each group)

### Integrated Testing

Use the `bifrost_test.sh` script with hardlink options:

```bash
./scripts/bifrost_test.sh \
    --scan-hardlinks \
    --backup-hardlinks \
    -i /source/path \
    -o /target/path
```

## Implementation Details

### Scanning Phase

1. During filesystem traversal, each file's `nlink` count is checked
2. Files with `nlink > 1` are added to the `HardlinkIndex`
3. The index maps `(device, inode)` pairs to lists of file paths
4. After scanning completes, the index is written to `hardlink.txt`

### Backup Phase

1. **Copy Phase**: All files are copied normally (including hardlinked files)
2. **Hardlink Phase**:
   - Read `hardlink.txt` to get hardlink groups
   - For each group, the first file that exists becomes the target
   - Subsequent files are created as hardlinks using `link()` syscall
   - If the target doesn't exist, the first existing file in the group is used

### Error Handling

- If a hardlink target doesn't exist, the system searches for another existing file in the group
- If no files in a group exist, the entire group is skipped with an error logged
- Individual hardlink creation failures are logged but don't stop the backup

## Limitations

1. **Cross-Device Hardlinks**: Hardlinks cannot span filesystems. If the target directory is on a different filesystem than the source, hardlinks will be created as separate copies.

2. **Permission Requirements**: Creating hardlinks may require appropriate permissions on the target filesystem.

3. **Windows Support**: Hardlink support on Windows is limited compared to Unix systems. The implementation uses platform-specific APIs (`CreateHardLink` on Windows, `link` on Unix).

## Testing

A test script is provided to create hardlink test files:

```bash
./scripts/create_hardlink_files.sh /tmp/hardlink_test
```

This creates various hardlink scenarios:
- Simple hardlink pairs
- Cross-directory hardlinks
- Multiple hardlinks (5+ links to same inode)
- Large files with hardlinks
- Nested directory structures with hardlinks

To verify hardlink preservation after backup:

```bash
# Check source hardlinks
find /source/path -type f -links +1 -ls

# Check target hardlinks (should match)
find /target/path -type f -links +1 -ls
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Hardlink Backup Flow                        │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Scan Phase                                                     │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │  Traverse   │───▶│  Detect     │───▶│  Write      │         │
│  │  Filesystem │    │  Hardlinks  │    │  hardlink.txt│        │
│  └─────────────┘    └─────────────┘    └─────────────┘         │
│                                                                 │
│  Backup Phase                                                   │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │  Copy Phase │───▶│  Read       │───▶│  Create     │         │
│  │  (all files)│    │  hardlink.txt│   │  Hardlinks  │         │
│  └─────────────┘    └─────────────┘    └─────────────┘         │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Future Enhancements

- **Incremental Hardlink Backup**: Track hardlinks across incremental backups
- **Cross-Filesystem Detection**: Warn when target filesystem doesn't support hardlinks
- **Hardlink Deduplication**: During restore, detect and recreate hardlink relationships even if the backup was done without `--hardlink`
