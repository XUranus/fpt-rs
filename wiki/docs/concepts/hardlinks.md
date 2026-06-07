---
title: Hardlinks
description: How fpt-rs detects, records, and preserves hardlinked files across backup and restore operations.
---

# Hardlinks

A hardlink is a directory entry that shares the same inode (and therefore the same data) as another file. On a typical Linux filesystem, many files may share an inode -- for example, package managers, build systems, and deduplication tools create hardlinks extensively. fpt-rs detects hardlinks during the scan phase and preserves them during backup so the destination has the same structure.

## Detection During Scan

The scanner detects hardlinks by examining each file's link count (`nlink` on Unix, `GetFileInformationByHandle` on Windows). Files with `nlink > 1` are potential hardlink candidates.

During scanning, the scanner maintains an in-memory `HardlinkIndex` (`src/scanner/metadata/hardlink.rs`) that groups files by `(device, inode)`:

```rust
// src/scanner/metadata/hardlink.rs
#[derive(Debug, Default)]
pub struct HardlinkIndex {
    inode_map: HashMap<(u64, u64), usize>,  // (device, inode) -> group index
    groups: Vec<HardlinkGroup>,
}

#[derive(Debug, Default)]
pub struct HardlinkGroup {
    pub inode: u64,
    pub device: u64,
    pub link_count: u32,
    pub files: Vec<(u32, u32, String)>,  // (meta_fid, meta_offset, path)
}
```

The `add_file()` method groups files by inode:

```rust
// src/scanner/metadata/hardlink.rs
impl HardlinkIndex {
    pub fn add_file(
        &mut self,
        inode: u64,
        device: u64,
        link_count: u32,
        meta_fid: u32,
        meta_offset: u32,
        path: String,
    ) -> bool {
        if link_count <= 1 {
            return false;  // not a hardlink
        }
        let key = (device, inode);
        if let Some(&idx) = self.inode_map.get(&key) {
            self.groups[idx].files.push((meta_fid, meta_offset, path));
        } else {
            let idx = self.groups.len();
            let mut group = HardlinkGroup {
                inode, device, link_count,
                files: Vec::with_capacity(link_count as usize),
            };
            group.files.push((meta_fid, meta_offset, path));
            self.groups.push(group);
            self.inode_map.insert(key, idx);
        }
        true
    }
}
```

```mermaid
flowchart TD
    A[Scanner reads directory] --> B{For each file}
    B --> C[stat / lstat]
    C --> D{links > 1?}
    D -->|No| E[Normal file<br/>Write to copy.txt]
    D -->|Yes| F[HardlinkIndex::add_file<br/>device + inode + meta_fid + path]
    F --> G[Group by<br/>device + inode pair]
```

The `HardlinkIndex` can be serialized to a control file:

```rust
// src/scanner/metadata/hardlink.rs
impl HardlinkIndex {
    pub fn write_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        let mut writer = HardlinkControlFileWriter::new(path)?;
        for group in &self.groups {
            writer.write_inode(&HardlinkInodeEntry {
                inode: group.inode,
                device: group.device,
                link_count: group.link_count,
            })?;
            for (meta_fid, meta_offset, path) in &group.files {
                writer.write_file(&HardlinkFileEntry {
                    meta_fid: *meta_fid,
                    meta_offset: *meta_offset,
                    path: path.clone(),
                })?;
            }
        }
        writer.finish()
    }
}
```

## Control File Format

The `hardlink.txt` binary file uses the standard control file codec (`src/scanner/metadata/hardlink.rs`). The magic identifier is `#FPT_HARDLINK_CTRL_FILE`.

### Data Structures

```rust
// src/scanner/metadata/hardlink.rs
const HARDLINK_MAGIC: &str = "#FPT_HARDLINK_CTRL_FILE";
const RECORD_TYPE_INODE: u8 = 1;
const RECORD_TYPE_FILE: u8 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct HardlinkInodeEntry {
    pub inode: u64,
    pub device: u64,
    pub link_count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HardlinkFileEntry {
    pub meta_fid: u32,
    pub meta_offset: u32,
    pub path: String,
}

pub enum HardlinkEntry {
    Inode(HardlinkInodeEntry),
    File(HardlinkFileEntry),
}
```

### Record Layout

```mermaid
block-beta
    columns 1
    block:header["4 KB Header"]
        A["#FPT_HARDLINK_CTRL_FILE magic"]
        B["version, record_count, file_count, inode_count"]
        C["time, source_kind, source_root"]
        D["... padded to 4096 bytes"]
    end
    block:records["Binary Records"]
        E["[Inode record] inode=12345 dev=2065 nlink=3"]
        F["[File record] path=/data/a.txt meta_fid=0 meta_offset=42"]
        G["[File record] path=/data/b.txt meta_fid=0 meta_offset=99"]
        H["[File record] path=/data/c.txt meta_fid=1 meta_offset=10"]
        I["[Inode record] inode=67890 dev=2065 nlink=2"]
        J["[File record] path=/data/d.txt meta_fid=1 meta_offset=55"]
        K["[File record] path=/data/e.txt meta_fid=2 meta_offset=0"]
    end
```

Each record is length-prefixed (4-byte little-endian length, then payload). The payload starts with a 1-byte record type tag.

### Inode Record Payload (21 bytes)

Written by `write_inode()`:

```rust
// src/scanner/metadata/hardlink.rs
pub fn write_inode(&mut self, entry: &HardlinkInodeEntry) -> io::Result<()> {
    let mut payload = Vec::with_capacity(1 + 8 + 8 + 4);
    put_u8(&mut payload, RECORD_TYPE_INODE);   // 1 byte
    put_u64(&mut payload, entry.inode);         // 8 bytes
    put_u64(&mut payload, entry.device);        // 8 bytes
    put_u32(&mut payload, entry.link_count);    // 4 bytes
    write_record(&mut self.fwriter, &payload)?;
    Ok(())
}
```

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | Type tag (`0x01`) |
| 1 | 8 | Inode number (u64 LE) |
| 9 | 8 | Device number (u64 LE) |
| 17 | 4 | Link count (u32 LE) |

### File Record Payload (variable)

Written by `write_file()`:

```rust
// src/scanner/metadata/hardlink.rs
pub fn write_file(&mut self, entry: &HardlinkFileEntry) -> io::Result<()> {
    let path = entry.path.as_bytes();
    let mut payload = Vec::with_capacity(1 + 4 + 4 + 4 + path.len());
    put_u8(&mut payload, RECORD_TYPE_FILE);     // 1 byte
    put_u32(&mut payload, entry.meta_fid);      // 4 bytes
    put_u32(&mut payload, entry.meta_offset);   // 4 bytes
    put_u32(&mut payload, path.len() as u32);   // 4 bytes
    put_bytes(&mut payload, path);              // N bytes
    write_record(&mut self.fwriter, &payload)?;
    Ok(())
}
```

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | Type tag (`0x02`) |
| 1 | 4 | `meta_fid` -- metadata file ID (u32 LE) |
| 5 | 4 | `meta_offset` -- byte offset in metadata file (u32 LE) |
| 9 | 4 | Path length in bytes (u32 LE) |
| 13 | N | Path (UTF-8) |

### Reading Hardlink Files

The `HardlinkControlFileReader` implements `Iterator` for streaming reads:

```rust
// src/scanner/metadata/hardlink.rs
impl Iterator for HardlinkControlFileReader {
    type Item = io::Result<HardlinkEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let payload = match read_record(&mut self.freader) {
            Ok(Some(payload)) => payload,
            Ok(None) => return None,
            Err(err) => return Some(Err(err)),
        };
        let mut cursor = 0usize;
        let record_type = take_u8(&payload, &mut cursor)?;
        match record_type {
            RECORD_TYPE_INODE => {
                let inode = take_u64(&payload, &mut cursor)?;
                let device = take_u64(&payload, &mut cursor)?;
                let link_count = take_u32(&payload, &mut cursor)?;
                Some(Ok(HardlinkEntry::Inode(HardlinkInodeEntry { inode, device, link_count })))
            }
            RECORD_TYPE_FILE => {
                let meta_fid = take_u32(&payload, &mut cursor)?;
                let meta_offset = take_u32(&payload, &mut cursor)?;
                let path_len = take_u32(&payload, &mut cursor)? as usize;
                let path = take_bytes(&payload, &mut cursor, path_len)?;
                let path = std::str::from_utf8(path)?.to_string();
                Some(Ok(HardlinkEntry::File(HardlinkFileEntry { meta_fid, meta_offset, path })))
            }
            _ => Some(Err(io::Error::new(InvalidData, "unknown hardlink record type"))),
        }
    }
}
```

## Backup Phase -- Creating Hardlinks

During the hardlink phase of backup, the engine reads `hardlink.txt` and processes each inode group:

```mermaid
flowchart TD
    A[Read hardlink.txt] --> B[Accumulate files<br/>per inode group]
    B --> C{Group complete?<br/>next Inode record or EOF}
    C -->|Yes| D[First file = primary<br/>Already exists from copy phase]
    D --> E[For each secondary file]
    E --> F[Create hardlink:<br/>secondary -> primary]
    F --> G[Success?]
    G -->|Yes| H[Increment hardlinks_created]
    G -->|No| I[Increment hardlinks_failed<br/>Log error]
    C -->|No| B
```

The key rule: **only the first file in each group is copied** during the copy phase. All subsequent files are created as hardlinks pointing to the first file's destination path. This means:

- The first file must exist before the hardlink phase runs (guaranteed by copy running first)
- Hardlinks preserve the original filesystem's link structure
- Only one copy of the data is stored, even when N paths reference it

## Transport-Specific Implementation

Each backup transport implements hardlinking differently:

| Transport | Mechanism |
|---|---|
| Local | `std::fs::hard_link(primary, secondary)` |
| NFS v3 | `LINK3` RPC -- `link(file_fh, parent_dir_fh, filename)` |
| SMB | `FSCTL_SET_SPARSE` or SMB2 `SET_INFO` with `FileLinkInformation` |

All transports share the same control file reader (`HardlinkControlFileReader`) and group-processing logic.

## Restore Considerations

During restore, hardlinks are handled implicitly: since the backup stores only one copy of the data (in the primary file's location), restoring the primary file restores the data. The hardlink phase then creates the link structure on the target. If the restore target is a local filesystem, `std::fs::hard_link` is used; for remote targets, the appropriate transport RPC is called.
