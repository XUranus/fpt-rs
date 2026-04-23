# Control Files

Bifrost control files are execution plans generated from metadata. They are not
the durable source of truth for a copy.

Important rules:

- backup control files are generated from scan output
- restore control files are generated fresh from `M_REPO/meta`
- restore does not treat the copy's old `C_REPO/ctrl` as authoritative input

## Main Files

Under `C_REPO/ctrl/` you will typically see:

- `copy.txt`
- `copy_<SHARD>_<ROLLOVER>.txt` when copy sharding is enabled
- `hardlink.txt`
- `delete.txt`
- `mtime.txt`

## V3 Format

All control files now use the same storage model:

1. a fixed-size `4096` byte header at offset `0`
2. binary length-prefixed records after the header

The header is ASCII text, ends with `\n`, and is padded with `\0` up to 4 KiB.
This makes it:

- human-readable in hex or plain-text viewers
- safe to rewrite in place on `finish()`
- able to store final counts instead of placeholder zeroes

Common header fields:

```text
#BIFROST_<TYPE>_CTRL_FILE V3
HEADER_SIZE=4096
FILE_COUNT=<N>
DIR_COUNT=<N>
INODE_COUNT=<N>
RECORD_COUNT=<N>
TIME=<UNIX_TIMESTAMP>
SOURCE_KIND="<json string>"
SOURCE_ROOT="<json string>"
END
```

Notes:

- `SOURCE_KIND` and `SOURCE_ROOT` are JSON-escaped strings in the header
- path-bearing records are binary, not line-oriented text
- paths are stored as UTF-8 logical paths with explicit byte lengths
- names containing spaces, `\n`, `\r`, tabs, and other special characters are supported

## `copy.txt`

Source implementation: [src/scanner/metadata/filecontrol.rs](/home/xuranus/workspace/bifrost/src/scanner/metadata/filecontrol.rs)

Record types:

- directory record
- file record

Directory record payload:

- diff code
- metadata locator: `meta_fid`, `meta_offset`
- `files_count`
- batch metadata fields
- `path_len`
- UTF-8 path bytes

File record payload:

- diff code
- metadata locator: `meta_fid`, `meta_offset`
- `name_len`
- UTF-8 file-name bytes

File diff codes:

- `New`
- `DataModified`
- `MetaModified`
- `Deleted`

Directory diff codes:

- `New`
- `MetaModified`
- `Deleted`

### Batch Metadata

Directory records still reserve batch metadata so very large directories can be
split across multiple logical batches inside one shard. The current consumers do
not need to inspect that metadata directly.

### Sharded Copy Control Files

When scanner control sharding is enabled, the scanner emits:

```text
copy_00000000_0000.txt
copy_00000001_0000.txt
copy_0000000A_0001.txt
```

Properties:

- shard id is derived from the directory path hash
- one directory and all of its file entries always stay in the same shard
- rollover may create multiple files for the same shard id
- backup discovers and executes all `copy_*.txt` files
- restore also discovers generated `copy_*.txt` files

## `delete.txt`

Source implementation: [src/scanner/metadata/delete.rs](/home/xuranus/workspace/bifrost/src/scanner/metadata/delete.rs)

Record payload:

- entry type: file or directory
- `path_len`
- UTF-8 logical path bytes

## `hardlink.txt`

Source implementation: [src/scanner/metadata/hardlink.rs](/home/xuranus/workspace/bifrost/src/scanner/metadata/hardlink.rs)

Record types:

- inode-group record
- file-in-group record

Inode record payload:

- inode
- device
- link count

File record payload:

- metadata locator: `meta_fid`, `meta_offset`
- `path_len`
- UTF-8 logical path bytes

## `mtime.txt`

Source implementation: [src/scanner/metadata/mtime.rs](/home/xuranus/workspace/bifrost/src/scanner/metadata/mtime.rs)

Record payload:

- mode
- uid
- gid
- `path_len`
- atime
- mtime
- UTF-8 logical path bytes

This phase restores directory metadata after copy, hardlink, and delete have
modified timestamps.

## Restore Plan Generation

Restore uses metadata-driven control-plan generation with three modes:

- `FULL`
- `DIFF`
- `FINEGRAIN`

Fine-grain matching rules:

- file request: exact logical-path match
- directory request: subtree prefix match

For fine-grain restore, the generated plan includes:

- exact requested files
- requested directory subtrees
- ancestor directories needed to materialize deep restores

## Relationship To Metadata Files

`copy.txt` and `hardlink.txt` reference metadata using:

- `META_FID`
- `META_OFFSET`

Those point into `M_REPO/meta/meta_*.dat`.

With multi-writer scanner output, metadata files are physically named:

- `meta_<WRITER_SHARD>_<SEGMENT>.dat`

`META_FID` still stores one 32-bit id. Bifrost encodes `(writer_shard, segment)`
into that id and resolves the physical metadata file internally.
