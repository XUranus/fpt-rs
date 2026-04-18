# Meta Storage Format Specification  
**Version 1.0**  
For the Scan-Backup Project (Rust)

---

## 📌 Overview

This document specifies a **binary, append-only, index-free** storage format for filesystem metadata (`DirMeta` and `FileMeta`) that enables **efficient direct retrieval** using a `(meta_file_id, offset)` pair.

The format is designed for:
- High-throughput ingestion from multiple scanner threads
- Compact, versionable serialization
- Fast random access by consumers using precomputed location hints
- Splitting into multiple files to avoid OS file-size limits

No global index is required—metadata location is managed externally (e.g., by a higher-level catalog or database that stores `(meta_file_id, offset)` per item).

---

## 🗂️ File Layout

All metadata is stored under a single base directory:

```
<base_dir>/
├── meta_0.dat
├── meta_1.dat
├── meta_2.dat
└── ...
```

- Each file is named `meta_<N>.dat`, where `N` is a zero-based, monotonically increasing **32-bit unsigned integer** (`meta_file_id`).
- Maximum file size: **512 MiB** (configurable at compile time).
- Files are **append-only** and **never modified after writing**.

---

## 🔢 Record Format

Each metadata record is stored as a **self-delimiting binary blob** with the following structure:

| Field               | Type     | Size (bytes) | Endianness | Description                         |
|---------------------|----------|--------------|------------|-------------------------------------|
| **Tag**             | `u8`     | 1            | —          | `1` = `DirMeta`, `2` = `FileMeta`  |
| **Payload Length**  | `u32`    | 4            | **Little** | Number of bytes in payload          |
| **Payload**         | `[u8]`   | `L`          | —          | `bincode`-serialized struct         |

> **Total record size** = `1 + 4 + L` bytes.

### Example
```rust
// Storing a DirMeta
[0x01][0x34 0x12 0x00 0x00][<32-bit-bincode-of-DirMeta>]
// Tag=1, Len=0x1234 = 4660 bytes
```

---

## 🧾 Supported Types

| Tag | Value | Rust Type    | Description                |
|-----|-------|--------------|----------------------------|
| DIR | `1`   | `DirMeta`    | Directory metadata         |
| FILE| `2`   | `FileMeta`   | File metadata              |

> Unknown tags must be treated as **corruption or unsupported version**.

---

## 📥 Writing Records

1. Compute serialized payload using `bincode::serialize` with **standard configuration** (default Rust struct layout, little-endian, no varint).
2. Choose next available `meta_<N>.dat` file (create new if current would exceed 512 MiB).
3. Append `[tag][len_le][payload]` to the file.
4. Return `(meta_file_id = N, offset = file_position_before_write)` as the **location key**.

> **Note**: `offset` is a **32-bit unsigned integer**, limiting each `.dat` file to **≤ 4 GiB** of logical records (but physical limit is 512 MiB, so safe).

---

## 🔍 Reading Records

Given `(meta_file_id, offset)`:

1. Open (or reuse) file `meta_<meta_file_id>.dat`.
2. Seek to `offset`.
3. Read 1 byte → `tag`.
4. Read 4 bytes → `len` (interpret as little-endian `u32`).
5. Read `len` bytes → `payload`.
6. Deserialize `payload` with `bincode::deserialize` into the appropriate type based on `tag`.

> **No validation beyond structure is performed**—caller must ensure `(meta_file_id, offset)` points to a valid record start.

---

## ⚙️ Serialization Details

- **Format**: `bincode` (v1.x or v2.x compatible with `standard()` config)
- **Endianness**: Little-endian (for length field and all multi-byte integers in payload)
- **Alignment/Padding**: None (packed)
- **Extensibility**: Backward-compatible if new optional fields are added with `#[serde(default)]`

> The exact binary layout of `DirMeta` and `FileMeta` is defined by their `#[derive(Serialize, Deserialize)]` implementations.

---

## 📦 Struct Definitions (Rust)

```rust
// Common metadata (embedded)
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MetaCommon { /* ... */ }

// File-specific metadata
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FileMeta {
    pub common: MetaCommon,
    pub size: u64,
    pub links: u64,
    pub sparse_range: Option<Vec<(u64, u64)>>,
}

// Directory-specific metadata
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DirMeta {
    pub common: MetaCommon,
}
```

---

## 🧪 Example Workflow

1. **Scanner thread**:
   - Scans `/home/user/docs/`
   - Serializes `DirMeta` → writes to `meta_3.dat` at offset `125000`
   - Returns `MetaIndexEntry { meta_file_id: 3, offset: 125000 }`
   - This entry is stored in a higher-level index (e.g., SQLite, LSM-tree)

2. **Consumer thread**:
   - Looks up `/home/user/docs/` → gets `(3, 125000)`
   - Calls `reader.get_meta(3, 125000)`
   - Receives `MetaVariant::Dir(...)` in **one disk seek + read**

---

## ✅ Guarantees & Limitations

| Property                     | Status        |
|------------------------------|---------------|
| **Atomic record writes**     | ✅ (if ≤ FS block size; else partial writes possible) |
| **Crash safety**             | ✅ (append-only; incomplete records ignored) |
| **Cross-platform**           | ❌ (endianness and `bincode` assumptions; use on same architecture) |
| **Compression**              | ❌ (raw binary; add externally if needed) |
| **Maximum entries per file** | ~10⁷–10⁸ (limited by 512 MiB file cap) |
| **Offset precision**         | 32-bit → max 4 GiB/file (but cap is 512 MiB, so safe) |

---

## 🛠️ Recommended Usage

- **Store `(meta_file_id, offset)`** in your primary index (e.g., B-tree, hash map, or embedded DB).
- **Do not parse `.dat` files directly**—always use the official reader.
- **Rotate files** at 512 MiB to ensure portability and filesystem compatibility.
- **Validate tag/length** on read to detect corruption early.

---

## 📜 License

This format is **open and royalty-free** for use in the Scan-Backup project and derived works.

---

*Document generated on: Monday, January 05, 2026*  
*Implementation in Rust using `bincode`, `serde`, and standard I/O*