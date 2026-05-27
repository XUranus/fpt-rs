# Scanner Path Filters

Fpt scanner can optionally filter traversal and emitted metadata/control entries by logical path pattern.

This applies to:

- local `fsscan`
- NFS `fsscan`
- SMB `fsscan`

When filters are not configured, the scanner stays on the old fast path. The hot traversal loops only pay a single `Option` check and do not compute logical filter paths.

## CLI Flags

`fsscan` exposes four repeatable filter flags:

```bash
./target/release/fsscan /opt/dataset/ds2 \
  -c /tmp/scan \
  -m /tmp/scan \
  --include-dir-pattern '/dir/*/*/dir1' \
  --include-file-pattern '/dir/*/dir1/*.txt' \
  --exclude-dir-pattern '/dir/*/dir1/dir1' \
  --exclude-file-pattern '/dir/*/dir1/1.txt'
```

Flags:

- `--include-dir-pattern <pattern>`
- `--include-file-pattern <pattern>`
- `--exclude-dir-pattern <pattern>`
- `--exclude-file-pattern <pattern>`

All patterns are matched against the scanner's logical path namespace:

- local source `/opt/dataset/ds2` uses logical root `/`
- NFS source `nfs://host/export?sub=/ds2` uses logical root `/ds2`
- SMB source `smb://host/share/ds2?...` uses logical root `/ds2`

## Pattern Syntax

Current syntax supports `*` within one path component.

Examples:

- `/dir/*/*/dir1`
- `/dir/*/dir1/*.txt`
- `/foo/bar*`
- `/foo/*bar*baz`

Notes:

- matching is path-segment aware
- `*` does not cross `/`
- `**` is not supported
- paths are normalized to leading-slash logical form before matching

## Semantics

### Include Directory

`--include-dir-pattern '/dir/*/*/dir1'`

Behavior:

- the matched directory is included
- everything under that directory subtree is included
- ancestor directories needed to reach that directory are still traversed and emitted

This lets the scanner reach the selected subtree without scanning unrelated excluded subtrees.

### Include File

`--include-file-pattern '/dir/*/dir1/*.txt'`

Behavior:

- matching files are included
- ancestor directories needed to reach them are still traversed and emitted
- non-matching sibling files are skipped

### Exclude Directory

`--exclude-dir-pattern '/dir/*/dir1/dir1'`

Behavior:

- the matched directory subtree is pruned from traversal
- no files or subdirectories under it are emitted

### Exclude File

`--exclude-file-pattern '/dir/*/dir1/1.txt'`

Behavior:

- matching files are skipped
- surrounding directory traversal is unchanged

## Traversal Behavior

The scanner uses filters in two places:

1. directory pruning
2. entry emission

Directory filters are checked before descending into a directory. This is what keeps the feature useful for performance instead of only post-filtering results after a full traversal.

File filters are checked before a file or symlink entry is added to the output batch.

If a directory itself is not selected for emission and all of its child files were filtered out, the scanner drops the empty directory batch.

## Performance Notes

The implementation is intentionally structured so that the unfiltered case stays cheap:

- `ScanOption.meta_option.path_filters` is `None` by default
- local/NFS/SMB scanners only build logical paths when filters are present
- no wildcard matching runs when filters are disabled

That means there should be no meaningful traversal regression when path filters are not enabled.
