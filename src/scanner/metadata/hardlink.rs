use std::io::{self, BufReader, BufWriter};
use std::path::Path;

use crate::scanner::metadata::control_codec::{
    create_record_writer, finish_record_writer, open_record_reader, put_bytes, put_u32, put_u64,
    put_u8, read_record, take_bytes, take_u32, take_u64, take_u8, write_record, ControlFileHeader,
};

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

#[derive(Debug)]
pub enum HardlinkEntry {
    Inode(HardlinkInodeEntry),
    File(HardlinkFileEntry),
}

pub struct HardlinkControlFileWriter {
    fwriter: BufWriter<std::fs::File>,
    header: ControlFileHeader,
}

impl HardlinkControlFileWriter {
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Self::new_with_source(path, "local", "/")
    }

    pub fn new_with_source<P: AsRef<Path>>(
        path: P,
        source_kind: &str,
        source_root: &str,
    ) -> io::Result<Self> {
        let header = ControlFileHeader {
            source_kind: source_kind.to_string(),
            source_root: source_root.to_string(),
            ..ControlFileHeader::default()
        };
        Ok(Self {
            fwriter: create_record_writer(path, HARDLINK_MAGIC, &header)?,
            header,
        })
    }

    pub fn write_inode(&mut self, entry: &HardlinkInodeEntry) -> io::Result<()> {
        let mut payload = Vec::with_capacity(1 + 8 + 8 + 4);
        put_u8(&mut payload, RECORD_TYPE_INODE);
        put_u64(&mut payload, entry.inode);
        put_u64(&mut payload, entry.device);
        put_u32(&mut payload, entry.link_count);
        write_record(&mut self.fwriter, &payload)?;
        self.header.inode_count += 1;
        self.header.record_count += 1;
        Ok(())
    }

    pub fn write_file(&mut self, entry: &HardlinkFileEntry) -> io::Result<()> {
        let path = entry.path.as_bytes();
        let mut payload = Vec::with_capacity(1 + 4 + 4 + 4 + path.len());
        put_u8(&mut payload, RECORD_TYPE_FILE);
        put_u32(&mut payload, entry.meta_fid);
        put_u32(&mut payload, entry.meta_offset);
        put_u32(
            &mut payload,
            u32::try_from(path.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "hardlink path too long"))?,
        );
        put_bytes(&mut payload, path);
        write_record(&mut self.fwriter, &payload)?;
        self.header.file_count += 1;
        self.header.record_count += 1;
        Ok(())
    }

    pub fn finish(self) -> io::Result<()> {
        finish_record_writer(self.fwriter, HARDLINK_MAGIC, &self.header)
    }

    pub fn file_count(&self) -> u64 {
        self.header.file_count
    }

    pub fn inode_count(&self) -> u64 {
        self.header.inode_count
    }
}

pub struct HardlinkControlFileReader {
    freader: BufReader<std::fs::File>,
    header: ControlFileHeader,
}

impl HardlinkControlFileReader {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let (freader, header) = open_record_reader(path, HARDLINK_MAGIC)?;
        Ok(Self { freader, header })
    }

    pub fn header(&self) -> &ControlFileHeader {
        &self.header
    }
}

impl Iterator for HardlinkControlFileReader {
    type Item = io::Result<HardlinkEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let payload = match read_record(&mut self.freader) {
            Ok(Some(payload)) => payload,
            Ok(None) => return None,
            Err(err) => return Some(Err(err)),
        };
        let mut cursor = 0usize;
        let record_type = match take_u8(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        match record_type {
            RECORD_TYPE_INODE => {
                let inode = match take_u64(&payload, &mut cursor) {
                    Ok(v) => v,
                    Err(err) => return Some(Err(err)),
                };
                let device = match take_u64(&payload, &mut cursor) {
                    Ok(v) => v,
                    Err(err) => return Some(Err(err)),
                };
                let link_count = match take_u32(&payload, &mut cursor) {
                    Ok(v) => v,
                    Err(err) => return Some(Err(err)),
                };
                Some(Ok(HardlinkEntry::Inode(HardlinkInodeEntry {
                    inode,
                    device,
                    link_count,
                })))
            }
            RECORD_TYPE_FILE => {
                let meta_fid = match take_u32(&payload, &mut cursor) {
                    Ok(v) => v,
                    Err(err) => return Some(Err(err)),
                };
                let meta_offset = match take_u32(&payload, &mut cursor) {
                    Ok(v) => v,
                    Err(err) => return Some(Err(err)),
                };
                let path_len = match take_u32(&payload, &mut cursor) {
                    Ok(v) => v as usize,
                    Err(err) => return Some(Err(err)),
                };
                let path = match take_bytes(&payload, &mut cursor, path_len).and_then(|bytes| {
                    std::str::from_utf8(bytes)
                        .map(|s| s.to_string())
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                }) {
                    Ok(path) => path,
                    Err(err) => return Some(Err(err)),
                };
                Some(Ok(HardlinkEntry::File(HardlinkFileEntry {
                    meta_fid,
                    meta_offset,
                    path,
                })))
            }
            _ => Some(Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown hardlink record type",
            ))),
        }
    }
}

#[derive(Debug, Default)]
pub struct HardlinkGroup {
    pub inode: u64,
    pub device: u64,
    pub link_count: u32,
    pub files: Vec<(u32, u32, String)>,
}

#[derive(Debug, Default)]
pub struct HardlinkIndex {
    inode_map: std::collections::HashMap<(u64, u64), usize>,
    groups: Vec<HardlinkGroup>,
}

impl HardlinkIndex {
    pub fn new() -> Self {
        Self {
            inode_map: std::collections::HashMap::new(),
            groups: Vec::new(),
        }
    }

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
            return false;
        }

        let key = (device, inode);
        if let Some(&idx) = self.inode_map.get(&key) {
            self.groups[idx].files.push((meta_fid, meta_offset, path));
        } else {
            let idx = self.groups.len();
            let mut group = HardlinkGroup {
                inode,
                device,
                link_count,
                files: Vec::with_capacity(link_count as usize),
            };
            group.files.push((meta_fid, meta_offset, path));
            self.groups.push(group);
            self.inode_map.insert(key, idx);
        }
        true
    }

    pub fn groups(&self) -> &[HardlinkGroup] {
        &self.groups
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn total_file_count(&self) -> usize {
        self.groups.iter().map(|g| g.files.len()).sum()
    }

    pub fn write_to_file<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        self.write_to_file_with_source(path, "local", "/")
    }

    pub fn write_to_file_with_source<P: AsRef<Path>>(
        &self,
        path: P,
        source_kind: &str,
        source_root: &str,
    ) -> io::Result<()> {
        let mut writer =
            HardlinkControlFileWriter::new_with_source(path, source_kind, source_root)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_hardlink_control_file_roundtrip() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        {
            let mut writer = HardlinkControlFileWriter::new(&path).unwrap();

            writer
                .write_inode(&HardlinkInodeEntry {
                    inode: 0xABCD,
                    device: 0x0801,
                    link_count: 3,
                })
                .unwrap();

            writer
                .write_file(&HardlinkFileEntry {
                    meta_fid: 0,
                    meta_offset: 0x100,
                    path: " /home/user/file1\n.txt".to_string(),
                })
                .unwrap();

            writer
                .write_file(&HardlinkFileEntry {
                    meta_fid: 0,
                    meta_offset: 0x150,
                    path: "/home/user/file2.txt".to_string(),
                })
                .unwrap();

            writer.finish().unwrap();
        }

        let reader = HardlinkControlFileReader::open(&path).unwrap();
        assert_eq!(reader.header().inode_count, 1);
        assert_eq!(reader.header().file_count, 2);
        let entries: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();

        match &entries[0] {
            HardlinkEntry::Inode(inode) => {
                assert_eq!(inode.inode, 0xABCD);
                assert_eq!(inode.device, 0x0801);
                assert_eq!(inode.link_count, 3);
            }
            _ => panic!("Expected inode entry"),
        }

        match &entries[1] {
            HardlinkEntry::File(file) => {
                assert_eq!(file.meta_fid, 0);
                assert_eq!(file.meta_offset, 0x100);
                assert_eq!(file.path, " /home/user/file1\n.txt");
            }
            _ => panic!("Expected file entry"),
        }
    }

    #[test]
    fn test_hardlink_index() {
        let mut index = HardlinkIndex::new();
        assert!(index.add_file(12345, 0x0801, 3, 0, 0x100, "/path/file1".to_string()));
        assert!(index.add_file(12345, 0x0801, 3, 0, 0x150, "/path/file2".to_string()));
        assert!(index.add_file(12345, 0x0801, 3, 0, 0x1A0, "/path/file3".to_string()));
        assert!(!index.add_file(99999, 0x0801, 1, 0, 0x200, "/path/single".to_string()));

        assert_eq!(index.group_count(), 1);
        assert_eq!(index.total_file_count(), 3);
        assert_eq!(index.groups()[0].files.len(), 3);
    }
}
