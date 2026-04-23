# Control Files

Bifrost uses text control files to describe what the backup and restore engines should do.

Important rule:

- backup control files are generated from scan output
- restore control files are now generated fresh from `M_REPO/meta` for each restore request
- restore no longer treats the copy's old `C_REPO/ctrl` as authoritative input

This means control files should be treated as disposable execution plans, not durable source-of-truth metadata.

## Main Files

Under `C_REPO/ctrl/` you will typically see:

- `copy.txt`
- `copy_<SHARD>.txt` or `copy_<SHARD>_<ROLLOVER>.txt` when control sharding is enabled
- `hardlink.txt`
- `delete.txt`
- `mtime.txt`

For restore, generated control files may also live in a temporary restore-plan directory under the configured temp root instead of reusing the copy's original `C_REPO/ctrl`.

## `copy.txt`

Source implementation: `src/scanner/metadata/filecontrol.rs`

Header:

```text
#BIFROST_BACKUP_CTRL_FILE V1 FILE=0 DIRS=0 TIME=<UNIX_TIMESTAMP>
```

Important note:

- the current writer initializes `FILE=0 DIRS=0`
- these counts are placeholders and are not rewritten on finish

Entry formats:

```text
F <DIFF> <META_FID:8HEX> <META_OFFSET:8HEX> -------- <NAME_LEN:8HEX> <NAME>
D <DIFF> <META_FID:8HEX> <META_OFFSET:8HEX> <FILES_COUNT:8HEX> <PATH_LEN:8HEX> <PATH>
```

File diff codes:

- `NN`: new
- `DM`: data modified
- `MM`: metadata modified
- `DD`: deleted

Directory diff codes:

- `NN`: new
- `MM`: metadata modified
- `DD`: deleted

Example:

```text
#BIFROST_BACKUP_CTRL_FILE V1 FILE=0 DIRS=0 TIME=1700000000
D NN 00000000 000001A0 00000002 00000005 /home
F NN 00000000 00000200 -------- 00000008 .bashrc
F DM 00000000 00000300 -------- 00000009 notes.txt
```

### Paths With Spaces

The format includes explicit path-length fields. The reader uses those lengths to reconstruct names and paths correctly, including entries containing spaces.

### Batch Markers

Directory entries may include an extra batch suffix when a large directory is split across batches:

```text
D NN ... /data/huge BATCH=0/5
D NN ... /data/huge BATCH=1/5 CONT
D NN ... /data/huge BATCH=2/5 CONT LAST
```

### Sharded Copy Control Files

When scanner control sharding is enabled, the scanner emits multiple copy control
files instead of a single `copy.txt`:

```text
copy_00000000_0000.txt
copy_00000001_0000.txt
copy_0000000A_0001.txt
```

Important properties:

- the shard id is derived from the directory path hash
- one directory and all of its file entries always stay in the same copy shard
- rollover may create multiple files for the same shard id
- backup already discovers and runs all `copy_*.txt` files as parallel subtasks
- restore also discovers generated `copy_*.txt` files

## Restore Plan Generation

Restore now uses metadata-driven control-plan generation with three modes:

- `FULL`: generate a complete restore plan from `M_REPO/meta`
- `DIFF`: generate incremental backup controls from previous and current metadata
- `FINEGRAIN`: generate restore controls only for requested paths

Fine-grain matching rules:

- file request: exact logical-path match
- directory request: subtree prefix match

Example:

- requested file `/d2/d3/f1` restores only that file
- requested directory `/d1` restores `/d1` and everything below it

For fine-grain restore, the generated copy plan includes:

- exact requested files
- requested directory subtrees
- ancestor directories needed to materialize deep file restores

The generated hardlink plan only recreates hardlinks among the selected files. A partially selected hardlink group falls back to ordinary file copy for the remaining selected file members.

## `delete.txt`

Source implementation: `src/scanner/metadata/delete.rs`

Header:

```text
#BIFROST_DELETE_CTRL_FILE V1 FILES=<N> DIRS=<M> TIME=<UNIX_TIMESTAMP>
```

Entries:

```text
F <PATH_LEN:8HEX> <PATH>
D <PATH_LEN:8HEX> <PATH>
```

## `hardlink.txt`

Source implementation: `src/scanner/metadata/hardlink.rs`

Header:

```text
#BIFROST_HARDLINK_CTRL_FILE V1 FILES=<N> INODES=<M> TIME=<UNIX_TIMESTAMP>
```

Entries:

```text
I <INODE:16HEX> <DEVICE:16HEX> <LINK_COUNT:8HEX>
F <META_FID:8HEX> <META_OFFSET:8HEX> <PATH_LEN:8HEX> <PATH>
```

`I` starts a hardlink group and the following `F` lines belong to that group.

## `mtime.txt`

Source implementation: `src/scanner/metadata/mtime.rs`

Header:

```text
#BIFROST_MTIME_CTRL_FILE V1 DIRS=<N> TIME=<UNIX_TIMESTAMP>
```

Entries:

```text
D <PATH_LEN:8HEX> <PATH> <MODE:8HEX> <UID:8HEX> <GID:8HEX> <ATIME:16HEX> <MTIME:16HEX>
```

This phase restores directory metadata after copy, hardlink, and delete have already modified directory timestamps.

## Relationship To Metadata Files

`copy.txt` and `hardlink.txt` reference metadata using:

- `META_FID`
- `META_OFFSET`

Those point into `M_REPO/meta/meta_*.dat`.

With multi-writer scanner output, metadata files are now physically named:

- `meta_<WRITER_SHARD>_<SEGMENT>.dat`

The `META_FID` stored in control files is still a single 32-bit value. Bifrost
encodes the writer shard and segment into that value and resolves the physical
file name internally.

See [metafile.md](metafile.md) for metadata-file format details.
