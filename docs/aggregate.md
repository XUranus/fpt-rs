# Aggregate Backup

Aggregated backup packs small files into blob files instead of writing each file as an individual target object. Bifrost now supports two aggregate layouts.

## Layouts

### `DIR_LEVEL`

- Aggregates files directory by directory.
- Each populated data directory gets its own `.AGGR_DIR/`.
- Each `.AGGR_DIR/` contains one or more blob files plus one SQLite index:
  - `AGGREGATE_IDX.sqlite`
- Restore looks up a file in the SQLite index for that file's parent directory.

Example:

```text
D_REPO/
├── .AGGR_DIR/
│   ├── 0876ed2b6a013000.bifrost.blob
│   └── AGGREGATE_IDX.sqlite
└── a/
    └── .AGGR_DIR/
        ├── 0876ed2b6a023000.bifrost.blob
        └── AGGREGATE_IDX.sqlite
```

### `SHARD`

- Aggregates files into a shared `D_REPO/.AGGR/` area.
- Files are assigned to shard buckets.
- The copy writes:
  - shard blob files under `D_REPO/.AGGR/shard-xxx/`
  - one shared binary index `D_REPO/.AGGR/AGGREGATE_INDEX.bidx`

Example:

```text
D_REPO/
└── .AGGR/
    ├── AGGREGATE_INDEX.bidx
    ├── shard-000/
    │   └── 0876ed8f6b003000.bifrost.blob
    └── shard-001/
        └── 0876ed8f6b013000.bifrost.blob
```

## Config

Relevant fields in `AggregateConfig`:

```rust
pub struct AggregateConfig {
    pub enabled: bool,
    pub layout: AggregateLayout, // DIR_LEVEL or SHARD
    pub max_blob_size: u64,
    pub file_threshold: u64,
    pub shard_count: u16,        // used by SHARD
}
```

Files smaller than `file_threshold` are aggregate candidates. Larger files are copied normally to their original logical paths.

## CLI

`fptcli`:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format aggregated \
  --aggregate-layout shard \
  --blob-size 64 \
  --threshold 1024
```

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format aggregated \
  --aggregate-layout dir-level
```

`fsbackup`:

```bash
./target/release/fsbackup \
  --source /opt/dataset/source \
  --target /backup/root \
  --meta-dir ./backup/meta \
  --control-file ./backup/ctrl/copy.txt \
  --aggregate \
  --aggregate-layout shard
```

## Manifest

Aggregated copies record their layout in `manifest.json`:

```json
"aggregation": {
  "layout": "SHARD",
  "max_blob_size": 4194304,
  "file_threshold": 1048576,
  "shard_count": 16
}
```

Restore uses the manifest to select the correct aggregate lookup path.

## Behavior

- Aggregated backup only runs the copy phase.
- `--hardlink`, `--delete`, and `--mtime` are ignored during aggregated backup.
- Aggregated restore still replays copy, hardlink, delete, and mtime phases from the copy metadata/control files.

## Tradeoffs

`DIR_LEVEL`:

- Better locality within one source directory.
- Creates many small indexes and `.AGGR_DIR` directories on deep trees.
- Useful when you explicitly want per-directory aggregate isolation.

`SHARD`:

- Lower metadata overhead on trees with many small directories.
- Uses one shared index and a bounded shard fanout.
- Usually the better default.
