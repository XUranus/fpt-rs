# Documentation Guide

This directory contains the implementation-facing documentation for Bifrost. The top-level project `README.md` is only a quick start; the detailed behavior, formats, and module layout are documented here.

## Reading Order

Start here if you are new to the repository:

1. [bifrost.md](bifrost.md) for the current architecture overview.
2. [fptcli.md](fptcli.md) for user-facing backup and restore usage.
3. [nfs.md](nfs.md) if you are touching NFS-backed scan/backup paths.
4. [aggregate.md](aggregate.md), [incremental.md](incremental.md), and [ctrlfile.md](ctrlfile.md) for format and pipeline details.
5. [logging.md](logging.md) when debugging routed logs or `C_REPO/logs`.

Reference docs:

- [metafile.md](metafile.md)
- [hardlink.md](hardlink.md)
- [mtime.md](mtime.md)
- [bugfix/](bugfix/)

## Current Conventions

- Docs should describe the current implementation, not an aspirational design.
- High-level concepts belong in `README.md` or `bifrost.md`.
- Format details belong in dedicated docs under `docs/`.
- If a behavior differs between local and NFS paths, document both explicitly.

## Build A Wiki

The `docs/` directory includes a small wiki builder that copies the markdown set into a wiki-friendly output tree and generates `Home.md` and `_Sidebar.md`.

Build into the default output directory:

```bash
./docs/build_wiki.sh
```

Build into a custom directory:

```bash
python3 docs/build_wiki.py /tmp/bifrost-wiki
```

The output keeps the markdown files as markdown, preserves the `bugfix/` subtree, and generates:

- `Home.md`
- `_Sidebar.md`
- copied documentation pages

This is intended for lightweight wiki publishing or import into another static-doc process. It is not a full site generator.
