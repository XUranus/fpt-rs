use std::io::{self, BufReader, BufWriter};
use std::path::Path;

use crate::scanner::metadata::control_codec::{
    create_record_writer, finish_record_writer, open_record_reader, put_bytes, put_u16, put_u32,
    put_u8, read_record, take_bytes, take_u16, take_u32, take_u8, write_record, ControlFileHeader,
};

const COPY_MAGIC: &str = "#BIFROST_BACKUP_CTRL_FILE";
const RECORD_TYPE_DIR: u8 = 1;
const RECORD_TYPE_FILE: u8 = 2;

/// Represents the type of change detected for a file.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub enum FileDiff {
    New,
    DataModified,
    MetaModified,
    Deleted,
}
/// Represents the type of change detected for a directory.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub enum DirDiff {
    New,
    MetaModified,
    Deleted,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub struct FileControlEntry {
    pub name: String,
    pub diff: FileDiff,
    pub meta_fid: u32,
    pub meta_offset: u32,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub struct DirControlEntry {
    pub path: String,
    pub diff: DirDiff,
    pub meta_fid: u32,
    pub meta_offset: u32,
    pub files_count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlEntry {
    Dir(DirControlEntry),
    File(FileControlEntry),
}

pub struct ControlFileWriter {
    writer: BufWriter<std::fs::File>,
    header: ControlFileHeader,
}

impl ControlFileWriter {
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Self::new_with_header(path, &ControlFileHeader::default())
    }

    pub fn new_with_header<P: AsRef<Path>>(
        path: P,
        header: &ControlFileHeader,
    ) -> io::Result<Self> {
        Ok(Self {
            writer: create_record_writer(path, COPY_MAGIC, header)?,
            header: header.clone(),
        })
    }

    pub fn write_file(&mut self, entry: &FileControlEntry) -> io::Result<()> {
        let name = entry.name.as_bytes();
        let mut payload = Vec::with_capacity(1 + 1 + 2 + 4 + 4 + 4 + name.len());
        put_u8(&mut payload, RECORD_TYPE_FILE);
        put_u8(&mut payload, encode_file_diff(&entry.diff));
        put_u16(&mut payload, 0);
        put_u32(&mut payload, entry.meta_fid);
        put_u32(&mut payload, entry.meta_offset);
        put_u32(
            &mut payload,
            u32::try_from(name.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name too long"))?,
        );
        put_bytes(&mut payload, name);
        write_record(&mut self.writer, &payload)?;
        self.header.file_count += 1;
        self.header.record_count += 1;
        Ok(())
    }

    pub fn write_dir(&mut self, entry: &DirControlEntry) -> io::Result<()> {
        self.write_dir_record(entry, 0, 1, false, true)
    }

    pub fn write_dir_with_batch(
        &mut self,
        entry: &DirControlEntry,
        batch: crate::scanner::metadata::sharded_control::BatchInfo,
    ) -> io::Result<()> {
        self.write_dir_record(
            entry,
            batch.batch_num,
            batch.total_batches,
            batch.is_continuation,
            batch.is_last,
        )
    }

    fn write_dir_record(
        &mut self,
        entry: &DirControlEntry,
        batch_num: u32,
        batch_total: u32,
        is_continuation: bool,
        is_last: bool,
    ) -> io::Result<()> {
        let path = entry.path.as_bytes();
        let mut payload = Vec::with_capacity(1 + 1 + 2 + 4 * 6 + path.len());
        let mut flags = 0u8;
        if is_continuation {
            flags |= 0x01;
        }
        if is_last {
            flags |= 0x02;
        }
        if batch_total > 1 {
            flags |= 0x04;
        }
        put_u8(&mut payload, RECORD_TYPE_DIR);
        put_u8(&mut payload, encode_dir_diff(&entry.diff));
        put_u16(&mut payload, flags as u16);
        put_u32(&mut payload, entry.meta_fid);
        put_u32(&mut payload, entry.meta_offset);
        put_u32(&mut payload, entry.files_count);
        put_u32(&mut payload, batch_num);
        put_u32(&mut payload, batch_total);
        put_u32(
            &mut payload,
            u32::try_from(path.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory path too long"))?,
        );
        put_bytes(&mut payload, path);
        write_record(&mut self.writer, &payload)?;
        self.header.dir_count += 1;
        self.header.record_count += 1;
        Ok(())
    }

    pub fn finish(self) -> io::Result<()> {
        finish_record_writer(self.writer, COPY_MAGIC, &self.header)
    }

    pub fn file_count(&self) -> u64 {
        self.header.file_count
    }

    pub fn dir_count(&self) -> u64 {
        self.header.dir_count
    }
}

pub struct ControlFileReader {
    reader: BufReader<std::fs::File>,
    header: ControlFileHeader,
}

impl ControlFileReader {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let (reader, header) = open_record_reader(path, COPY_MAGIC)?;
        Ok(Self { reader, header })
    }

    pub fn header(&self) -> &ControlFileHeader {
        &self.header
    }
}

impl Iterator for ControlFileReader {
    type Item = io::Result<ControlEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let payload = match read_record(&mut self.reader) {
            Ok(Some(payload)) => payload,
            Ok(None) => return None,
            Err(err) => return Some(Err(err)),
        };
        let mut cursor = 0usize;
        let record_type = match take_u8(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let diff = match take_u8(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let flags = match take_u16(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let meta_fid = match take_u32(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let meta_offset = match take_u32(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };

        let entry = match record_type {
            RECORD_TYPE_FILE => {
                let path_len = match take_u32(&payload, &mut cursor) {
                    Ok(v) => v as usize,
                    Err(err) => return Some(Err(err)),
                };
                let name = match take_bytes(&payload, &mut cursor, path_len)
                    .and_then(|bytes| {
                        std::str::from_utf8(bytes)
                            .map(|s| s.to_string())
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                    }) {
                    Ok(name) => name,
                    Err(err) => return Some(Err(err)),
                };
                let diff = match decode_file_diff(diff) {
                    Ok(d) => d,
                    Err(err) => return Some(Err(err)),
                };
                ControlEntry::File(FileControlEntry {
                    name,
                    diff,
                    meta_fid,
                    meta_offset,
                })
            }
            RECORD_TYPE_DIR => {
                let files_count = match take_u32(&payload, &mut cursor) {
                    Ok(v) => v,
                    Err(err) => return Some(Err(err)),
                };
                if flags & 0x04 != 0 {
                    if let Err(err) = take_u32(&payload, &mut cursor) {
                        return Some(Err(err));
                    }
                    if let Err(err) = take_u32(&payload, &mut cursor) {
                        return Some(Err(err));
                    }
                } else {
                    if let Err(err) = take_u32(&payload, &mut cursor) {
                        return Some(Err(err));
                    }
                    if let Err(err) = take_u32(&payload, &mut cursor) {
                        return Some(Err(err));
                    }
                }
                let path_len = match take_u32(&payload, &mut cursor) {
                    Ok(v) => v as usize,
                    Err(err) => return Some(Err(err)),
                };
                let path = match take_bytes(&payload, &mut cursor, path_len)
                    .and_then(|bytes| {
                        std::str::from_utf8(bytes)
                            .map(|s| s.to_string())
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                    }) {
                    Ok(path) => path,
                    Err(err) => return Some(Err(err)),
                };
                let diff = match decode_dir_diff(diff) {
                    Ok(d) => d,
                    Err(err) => return Some(Err(err)),
                };
                ControlEntry::Dir(DirControlEntry {
                    path,
                    diff,
                    meta_fid,
                    meta_offset,
                    files_count,
                })
            }
            _ => {
                return Some(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown copy control record type",
                )))
            }
        };

        Some(Ok(entry))
    }
}

fn encode_file_diff(diff: &FileDiff) -> u8 {
    match diff {
        FileDiff::New => 1,
        FileDiff::DataModified => 2,
        FileDiff::MetaModified => 3,
        FileDiff::Deleted => 4,
    }
}

fn decode_file_diff(value: u8) -> io::Result<FileDiff> {
    match value {
        1 => Ok(FileDiff::New),
        2 => Ok(FileDiff::DataModified),
        3 => Ok(FileDiff::MetaModified),
        4 => Ok(FileDiff::Deleted),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown file diff code",
        )),
    }
}

fn encode_dir_diff(diff: &DirDiff) -> u8 {
    match diff {
        DirDiff::New => 1,
        DirDiff::MetaModified => 2,
        DirDiff::Deleted => 3,
    }
}

fn decode_dir_diff(value: u8) -> io::Result<DirDiff> {
    match value {
        1 => Ok(DirDiff::New),
        2 => Ok(DirDiff::MetaModified),
        3 => Ok(DirDiff::Deleted),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown dir diff code",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn control_file_reader_roundtrip_weird_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("bifrost_ctrl_{unique}.txt"));

        let mut writer = ControlFileWriter::new(&path).unwrap();
        writer
            .write_dir(&DirControlEntry {
                path: " /tmp/source\n dir".to_string(),
                diff: DirDiff::MetaModified,
                meta_fid: 1,
                meta_offset: 2,
                files_count: 3,
            })
            .unwrap();
        writer
            .write_file(&FileControlEntry {
                name: " leading\nfile.txt".to_string(),
                diff: FileDiff::New,
                meta_fid: 4,
                meta_offset: 5,
            })
            .unwrap();
        writer.finish().unwrap();

        let reader = ControlFileReader::open(&path).unwrap();
        assert_eq!(reader.header().dir_count, 1);
        assert_eq!(reader.header().file_count, 1);
        let entries: Vec<_> = reader.collect::<io::Result<Vec<_>>>().unwrap();

        assert!(matches!(
            &entries[0],
            ControlEntry::Dir(DirControlEntry { path, .. }) if path == " /tmp/source\n dir"
        ));
        assert!(matches!(
            &entries[1],
            ControlEntry::File(FileControlEntry { name, .. }) if name == " leading\nfile.txt"
        ));

        let _ = std::fs::remove_file(path);
    }
}
