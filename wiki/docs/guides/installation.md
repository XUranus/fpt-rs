---
sidebar_position: 3
title: Installation
---

# Installation

This guide covers build requirements, cargo feature flags, and platform-specific notes for building fpt-rs from source.

## Build Requirements

| Requirement | Minimum Version | Notes |
|---|---|---|
| Rust toolchain | 1.70+ | Install via [rustup](https://rustup.rs/) |
| C compiler | GCC or Clang | Needed by the `rusqlite` bundled SQLite build |
| (Linux only) | libacl headers | For ACL scanning support (`exacl` crate) |
| (NFS) | Network access | NFS v3 server reachable on the network |
| (SMB) | Network access | SMB/CIFS server reachable on the network |

On Debian/Ubuntu, install system dependencies:

```bash
sudo apt update
sudo apt install build-essential libacl1-dev
```

On Fedora/RHEL:

```bash
sudo dnf groupinstall "Development Tools"
sudo dnf install libacl-devel
```

## Cargo Build Variants

fpt-rs uses Cargo feature flags to include optional transport backends. This keeps the default binary small and avoids pulling in NFS/SMB dependencies when they are not needed.

### Default Build (Local Only)

```bash
cargo build --release
```

This produces the standard binaries (`fptcli`, `fsscan`, `fsbackup`, `fsdiff`, `metainspect`) with local filesystem support and SQLite metadata storage.

### With NFS Support

```bash
cargo build --release --features nfs
```

Adds the `nfs3_client` and `async-channel` dependencies. Enables the `nfs://` URL scheme for source and target paths.

### With SMB Support

```bash
cargo build --release --features smb
```

Adds the `smb_client` and `sspi` dependencies. Enables the `smb://` URL scheme and builds the `smbprobe` diagnostic tool.

### All Transports

```bash
cargo build --release --features "nfs smb"
```

Includes both NFS and SMB support in a single binary.

## Feature Flags Reference

| Feature | Default | Dependencies | What It Enables |
|---|---|---|---|
| `sqlite` | Yes | `rusqlite` (bundled) | SQLite-based metadata and cache storage |
| `nfs` | No | `nfs3_client`, `async-channel` | NFS v3 scanner, backup, and restore via `nfs://` URLs |
| `smb` | No | `smb_client`, `sspi` | SMB/CIFS scanner, backup, and restore via `smb://` URLs; builds `smbprobe` |

When a feature is disabled, attempting to use its URL scheme produces a clear error message:

```
NFS support not compiled in. Rebuild with --features nfs
SMB support not compiled in. Rebuild with --features smb
```

## Platform Notes

### Linux

Linux is the primary development platform. All features are fully supported, including:

- POSIX hardlink detection via inode tracking.
- Extended attributes (`xattr` crate).
- ACL scanning via the `exacl` crate.
- File capabilities and SELinux labels (via xattrs).

Resource limits (open file descriptors) are automatically raised at startup when needed.

### Windows

Windows builds use the `windows` crate for native file operations. Key differences:

- Hardlink detection uses `GetFileInformationByHandle`.
- NTFS alternate data streams are not currently tracked.
- SMB support works natively through the SMB client library.
- ACL support uses Win32 security APIs.

Build with the standard toolchain:

```powershell
cargo build --release
```

:::note
SMB support on Windows may require additional configuration for SSPI authentication. Test with `smbprobe` before running production backups.
:::

## Verifying the Build

After building, verify the binaries are present:

```bash
# Check fptcli is built and runnable
./target/release/fptcli --version

# Check available subcommands
./target/release/fptcli --help
```

Expected output:

```
fptcli 0.1.0
File Protection Tool - Backup and Restore CLI
```

## Building the Server (Optional)

fpt-rs also includes `fptserver`, an HTTP-based task scheduler:

```bash
cargo build --release --bin fptserver
```

The server accepts scan, backup, and restore requests via an RPC API and manages worker processes. See the `fptserver` source for API details.
