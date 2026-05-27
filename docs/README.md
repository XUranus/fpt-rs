# Documentation Guide

This directory contains the implementation-facing documentation for Fpt. The top-level project `README.md` is only a quick start; the detailed behavior, formats, and module layout are documented here.

## Reading Order

Start here if you are new to the repository:

1. [fpt.md](fpt.md) for the current architecture overview.
2. [fptcli.md](fptcli.md) for user-facing backup and restore usage.
3. [nfs.md](nfs.md) if you are touching NFS-backed scan/backup paths.
4. [smb.md](smb.md) for the SMB transport design and rollout plan.
5. [aggregate.md](aggregate.md), [incremental.md](incremental.md), and [ctrlfile.md](ctrlfile.md) for format and pipeline details.
6. [pipeline_refactor.md](pipeline_refactor.md) for the shared copy-plan/block-transfer refactor.
7. [smoke_matrix.md](smoke_matrix.md) for the local/NFS/SMB backup+restore smoke matrix.
8. [task_lifecycle.md](task_lifecycle.md) for the common scanner/backup/restore lifecycle API.
9. [retry_failure.md](retry_failure.md) for structured failure logs and retry policy behavior.
10. [logging.md](logging.md) when debugging routed logs or `C_REPO/logs`.
11. [scanner_optimization.md](scanner_optimization.md) for the current metadata-writer and copy-control sharding changes.
12. [scanner_filter.md](scanner_filter.md) for scanner include/exclude path matching and traversal-pruning behavior.
13. [runtime_memory.md](runtime_memory.md) for scan/backup/restore memory hotspots and runtime knobs.
14. [fptserver.md](fptserver.md) for the RPC server, worker process model, and task APIs.
15. [code_organization.md](code_organization.md) for module layout and parameter-grouping conventions.

Reference docs:

- [metafile.md](metafile.md)
- [hardlink.md](hardlink.md)
- [mtime.md](mtime.md)
- [retry_failure.md](retry_failure.md)
- [bugfix/](bugfix/)

## Current Conventions

- Docs should describe the current implementation, not an aspirational design.
- High-level concepts belong in `README.md` or `fpt.md`.
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
python3 docs/build_wiki.py /tmp/fpt-wiki
```

The output keeps the markdown files as markdown, preserves the `bugfix/` subtree, and generates:

- `Home.md`
- `_Sidebar.md`
- copied documentation pages

This is intended for lightweight wiki publishing or import into another static-doc process. It is not a full site generator.
