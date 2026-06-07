---
sidebar_position: 1
title: Transport Engine Overview
description: How fpt-rs transports data between local, NFS, and SMB storage systems
---

# Transport Engine Overview

fpt-rs supports three **transport engines** -- Native/Local, NFS, and SMB -- that
abstract away the differences between storage systems. Each engine implements a
common set of traits so the backup and restore pipelines work identically
regardless of where the source or target data lives.

## The `DataLocation` Enum

All source and target paths in fpt-rs are represented by the `DataLocation` enum
(defined in `src/frame/location.rs`):

```rust
pub enum DataLocation {
    Local(PathBuf),
    #[cfg(feature = "nfs")]
    Nfs(NfsLocation),
    #[cfg(feature = "smb")]
    Smb(SmbLocation),
}
```

Each variant carries the information needed to connect and operate on its
respective storage system:

| Variant    | Payload        | Connection model                          |
|------------|----------------|-------------------------------------------|
| `Local`    | `PathBuf`      | Direct `std::fs` / `libc` syscalls        |
| `Nfs`      | `NfsLocation`  | NFSv3 RPC via `nfs3_client` (no kernel mount) |
| `Smb`      | `SmbLocation`  | SMB2/3 async client via `smb_client`      |

The enum provides constructors and parsers for each transport:

```text
DataLocation::local("/opt/data")
DataLocation::nfs(NfsLocation::from_url("nfs://192.168.1.10/export?sub=/ds1")?)
DataLocation::smb(SmbLocation::from_url("smb://server/share?username=u&password=p")?)
```

## Architecture Diagram

```mermaid
graph TB
    subgraph "CLI Layer"
        CLI[fptcli / fptserver]
    end

    subgraph "Frame Layer"
        BJ[BackupJob]
        RJ[RestoreJob]
        DL[DataLocation Enum]
    end

    subgraph "Transport Engines"
        subgraph "Native / Local"
            NS[Native Scanner]
            NB[Local Copy Engine]
            NPH[Local PostCopyPhases]
            NRO[LocalRestoreOps]
        end
        subgraph "NFS"
            NFS[NfsScanner]
            NFB[NfsSource / NfsTarget]
            NPH2[NfsPostCopyPhases]
        end
        subgraph "SMB"
            SMBS[SmbScanner]
            SMBB[SmbTarget]
            SMBPH[SmbPostCopyPhases]
        end
    end

    subgraph "Backup AIO Pipeline"
        SRC[SourceReader trait]
        TGT[TargetWriter trait]
        PCP[PostCopyPhases trait]
    end

    CLI --> BJ
    CLI --> RJ
    BJ --> DL
    RJ --> DL
    DL -->|Local| NS
    DL -->|NFS| NFS
    DL -->|SMB| SMBS

    NS --> NB
    NFS --> NFB
    SMBS --> SMBB

    NB -->|impl| SRC
    NB -->|impl| TGT
    NFB -->|impl| SRC
    NFB -->|impl| TGT
    SMBB -->|impl| TGT

    NPH -->|impl| PCP
    NPH2 -->|impl| PCP
    SMBPH -->|impl| PCP
```

## Pluggable Engine Design

Each transport is organized as a self-contained module with two submodules:

```text
src/
  native/
    scanner.rs          # Blocking I/O directory traversal (bio)
    backup/
      local_copy.rs     # Read/write via std::fs
      hardlink.rs       # Hardlink creation
      delete.rs         # File/dir deletion
      mtime.rs          # Mtime restoration
      phases_impl.rs    # PostCopyPhases impl
      restore_ops.rs    # RestoreOps impl
    fstat.rs            # File stat helpers
    fwrite_meta.rs      # Metadata write helpers

  nfs/
    connection.rs       # NfsConnectionPool (TCP + AUTH_UNIX)
    scanner.rs          # Async readdirplus scanner
    backup/
      transport.rs      # NfsSource (SourceReader) / NfsTarget (TargetWriter)
      reader.rs         # NFS READ RPCs
      writer.rs         # NFS WRITE RPCs + mkdir
      hardlink.rs       # NFS LINK RPCs
      delete.rs         # NFS REMOVE/RMDIR RPCs
      mtime.rs          # NFS SETATTR RPCs
      phases_impl.rs    # PostCopyPhases impl
    error.rs            # NfsError type

  smb/
    connection.rs       # SmbClientPool + DirCache
    scanner.rs          # Async FileIdBothDirectoryInformation scanner
    backup/
      transport.rs      # SmbTarget (TargetWriter)
      writer.rs         # SMB write + mkdir
      executor.rs       # SMB streaming copy orchestrator
      hardlink.rs       # SMB hardlink operations
      delete.rs         # SMB delete operations
      mtime.rs          # SMB mtime restoration
      phases_impl.rs    # PostCopyPhases impl
    fstat.rs            # SMB file stat helpers
```

## Source/Target Matrix

Any transport can serve as source, target, or both:

| Source \ Target | Local | NFS | SMB |
|-----------------|-------|-----|-----|
| **Local**       | Yes   | Yes | Yes |
| **NFS**         | Yes   | Yes | Yes |
| **SMB**         | Yes   | Yes | Yes |

The frame layer selects the correct `FileBackup` / `FileRestore` implementation
based on the `DataLocation` variants for source and target. For example:

- `LocalFileBackup` -- local source, local target (BIO pipeline)
- `NfsSourceTargetFileBackup` -- NFS source, NFS target (AIO pipeline)
- `NfsSourceSmbTargetFileBackup` -- NFS source, SMB target (AIO pipeline)
- `SmbSourceNfsTargetFileBackup` -- SMB source, NFS target (AIO pipeline)

## Feature Flags

NFS and SMB transports are gated behind Cargo feature flags:

```toml
[features]
default = []
nfs = ["nfs3_client"]
smb = ["smb_client"]
```

When a feature is not enabled, the corresponding `DataLocation` variant and all
associated code are compiled out. CLI tools return a clear error message if a
URL is provided but the feature is not compiled in.

## Scanning: Bio vs Aio

The scanner engine has two modes:

- **BIO (Blocking I/O)** -- used by the native/local transport. Spawns OS
  threads that call `std::fs` to traverse directories. Results flow through a
  `BlockingQueue` to metadata writer threads.

- **AIO (Async I/O)** -- used by NFS and SMB transports. An `AsyncDirScanner`
  implementation runs inside a Tokio runtime, pushing `DirBatchScanResult`
  batches into a tokio mpsc channel. A bridge task forwards these into the
  shared `BlockingQueue` for metadata writing.

Both paths converge on the same metadata writer and control-file generation
logic, so the output format is transport-independent.
