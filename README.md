# Bifrost

Bifrost is a Rust backup engine for large filesystems. It combines scanning, metadata generation, file-copy orchestration, and restore flows for both local filesystems and NFS targets/sources. The main user-facing CLI is `fptcli`; the lower-level tools `fsscan`, `fsbackup`, and `fsdiff` remain available for split workflows and debugging.

This `README.md` stays intentionally short. Detailed format, pipeline, and module documentation lives under [docs/](docs/README.md).

## Build

Debug build:

```bash
cargo build
```

Release build:

```bash
cargo build --release
```

Build with NFS support:

```bash
cargo build --release --features nfs
```

## Test

Run the library/unit tests:

```bash
cargo test
```

Run with NFS feature enabled:

```bash
cargo test --features nfs
```

Run the Python integration suite:

```bash
python scripts/test/test_all.py --keep-logs
```

Useful targeted integration tests:

```bash
python scripts/test/test_special_files.py --keep-on-failure
python scripts/test/test_incremental_backup.py --keep-on-failure
```

## Main CLI

`fptcli` is the recommended entry point.

Build it:

```bash
cargo build --release --bin fptcli --features nfs
```

Basic local backup:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format common
```

Local to NFS backup:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target "nfs://127.0.0.1/opt/backup?sub=/copies" \
  --format common \
  --hardlink \
  --delete \
  --mtime
```

Aggregated backup:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format aggregated \
  --aggregate-layout shard \
  --blob-size 64 \
  --threshold 1024
```

Alternative directory-level aggregate layout:

```bash
./target/release/fptcli backup \
  --data /opt/dataset/source \
  --target /backup/root \
  --format aggregated \
  --aggregate-layout dir-level
```

Restore:

```bash
./target/release/fptcli restore \
  --copy /backup/root/COPY_COMMON_FULL_xxx \
  --target /restore/root \
  --policy replace
```

Path handling:

- Local paths are plain filesystem paths such as `/opt/dataset/source`.
- NFS locations are inferred from `nfs://...` URLs such as `nfs://127.0.0.1/opt/backup?sub=/copies`.
- The old split flags like `--data-nfs` and `--target-nfs` are no longer used.

## Legacy Tools

Scan:

```bash
./target/release/fsscan \
  --ctrl-dir ./tmp/ctrl \
  --meta-dir ./tmp/meta \
  /opt/dataset/source
```

Backup:

```bash
./target/release/fsbackup \
  --source /opt/dataset/source \
  --target /backup/root \
  --meta-dir ./tmp/meta \
  --ctrl-dir ./tmp/ctrl \
  --control-file ./tmp/ctrl/copy.txt
```

Diff:

```bash
./target/release/fsdiff \
  --source /opt/dataset/source \
  --target /backup/root
```

## Documentation

Start with [docs/README.md](docs/README.md).

Important docs:

- [docs/fptcli.md](docs/fptcli.md): `fptcli` backup and restore usage
- [docs/bifrost.md](docs/bifrost.md): architecture overview
- [docs/nfs.md](docs/nfs.md): NFS support and module layout
- [docs/smb.md](docs/smb.md): SMB feasibility, design, and rollout plan
- [docs/aggregate.md](docs/aggregate.md): aggregated backup format
- [docs/ctrlfile.md](docs/ctrlfile.md): control-file formats
- [docs/logging.md](docs/logging.md): routed logging behavior
