use std::io::{self, BufReader, BufWriter};
use std::path::Path;

use crate::scanner::metadata::control_codec::{
    create_record_writer, finish_record_writer, open_record_reader, put_bytes, put_u32, put_u8,
    read_record, take_bytes, take_u32, take_u8, write_record, ControlFileHeader,
};

const DELETE_MAGIC: &str = "#FPT_DELETE_CTRL_FILE";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeleteEntryType {
    Dir,
    File,
}
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteEntry {
    pub entry_type: DeleteEntryType,
    pub path: String,
}

pub struct DeleteControlFileWriter {
    writer: BufWriter<std::fs::File>,
    header: ControlFileHeader,
}

impl DeleteControlFileWriter {
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
            writer: create_record_writer(path, DELETE_MAGIC, &header)?,
            header,
        })
    }

    pub fn write_file(&mut self, path: &str) -> io::Result<()> {
        self.write_entry(&DeleteEntry {
            entry_type: DeleteEntryType::File,
            path: path.to_string(),
        })
    }

    pub fn write_dir(&mut self, path: &str) -> io::Result<()> {
        self.write_entry(&DeleteEntry {
            entry_type: DeleteEntryType::Dir,
            path: path.to_string(),
        })
    }

    pub fn write_entry(&mut self, entry: &DeleteEntry) -> io::Result<()> {
        let path = entry.path.as_bytes();
        let mut payload = Vec::with_capacity(1 + 3 + 4 + path.len());
        put_u8(
            &mut payload,
            match entry.entry_type {
                DeleteEntryType::Dir => 1,
                DeleteEntryType::File => 2,
            },
        );
        put_u8(&mut payload, 0);
        put_u8(&mut payload, 0);
        put_u8(&mut payload, 0);
        put_u32(
            &mut payload,
            u32::try_from(path.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "delete path too long"))?,
        );
        put_bytes(&mut payload, path);
        write_record(&mut self.writer, &payload)?;
        match entry.entry_type {
            DeleteEntryType::Dir => self.header.dir_count += 1,
            DeleteEntryType::File => self.header.file_count += 1,
        }
        self.header.record_count += 1;
        Ok(())
    }

    pub fn finish(self) -> io::Result<()> {
        finish_record_writer(self.writer, DELETE_MAGIC, &self.header)
    }

    pub fn file_count(&self) -> u64 {
        self.header.file_count
    }

    pub fn dir_count(&self) -> u64 {
        self.header.dir_count
    }
}

pub struct DeleteControlFileReader {
    freader: BufReader<std::fs::File>,
    header: ControlFileHeader,
}

impl DeleteControlFileReader {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let (freader, header) = open_record_reader(path, DELETE_MAGIC)?;
        Ok(Self { freader, header })
    }

    pub fn header(&self) -> &ControlFileHeader {
        &self.header
    }
}

impl Iterator for DeleteControlFileReader {
    type Item = io::Result<DeleteEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let payload = match read_record(&mut self.freader) {
            Ok(Some(payload)) => payload,
            Ok(None) => return None,
            Err(err) => return Some(Err(err)),
        };
        let mut cursor = 0usize;
        let entry_type = match take_u8(&payload, &mut cursor) {
            Ok(1) => DeleteEntryType::Dir,
            Ok(2) => DeleteEntryType::File,
            Ok(_) => {
                return Some(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown delete entry type",
                )))
            }
            Err(err) => return Some(Err(err)),
        };
        for _ in 0..3 {
            if let Err(err) = take_u8(&payload, &mut cursor) {
                return Some(Err(err));
            }
        }
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
        Some(Ok(DeleteEntry { entry_type, path }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delete_control_file_roundtrip() {
        let path = std::env::temp_dir().join("fpt_delete_test.dat");
        {
            let mut writer = DeleteControlFileWriter::new(&path).unwrap();
            writer.write_file(" weird\nfile.txt").unwrap();
            writer.write_dir("/tmp/old_dir").unwrap();
            writer.finish().unwrap();
        }

        let reader = DeleteControlFileReader::open(&path).unwrap();
        assert_eq!(reader.header().file_count, 1);
        assert_eq!(reader.header().dir_count, 1);
        let entries: Vec<_> = reader.collect::<io::Result<Vec<_>>>().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, " weird\nfile.txt");
        assert_eq!(entries[1].path, "/tmp/old_dir");
        let _ = std::fs::remove_file(path);
    }
}
