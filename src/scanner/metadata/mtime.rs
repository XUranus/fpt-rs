use std::io::{self, BufReader, BufWriter};
use std::path::Path;

use crate::scanner::metadata::control_codec::{
    create_record_writer, finish_record_writer, open_record_reader, put_bytes, put_u32, put_u64,
    read_record, take_bytes, take_u32, take_u64, write_record, ControlFileHeader,
};

const MTIME_MAGIC: &str = "#BIFROST_MTIME_CTRL_FILE";

#[derive(Debug, Clone, PartialEq)]
pub struct MtimeDirEntry {
    pub path: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub atime: u64,
    pub mtime: u64,
}
pub struct MtimeControlFileWriter {
    fwriter: BufWriter<std::fs::File>,
    header: ControlFileHeader,
}

impl MtimeControlFileWriter {
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
            fwriter: create_record_writer(path, MTIME_MAGIC, &header)?,
            header,
        })
    }

    pub fn write_dir(&mut self, entry: &MtimeDirEntry) -> io::Result<()> {
        let path = entry.path.as_bytes();
        let mut payload = Vec::with_capacity(4 * 4 + 8 * 2 + path.len());
        put_u32(&mut payload, entry.mode);
        put_u32(&mut payload, entry.uid);
        put_u32(&mut payload, entry.gid);
        put_u32(
            &mut payload,
            u32::try_from(path.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mtime path too long"))?,
        );
        put_u64(&mut payload, entry.atime);
        put_u64(&mut payload, entry.mtime);
        put_bytes(&mut payload, path);
        write_record(&mut self.fwriter, &payload)?;
        self.header.dir_count += 1;
        self.header.record_count += 1;
        Ok(())
    }

    pub fn finish(self) -> io::Result<()> {
        finish_record_writer(self.fwriter, MTIME_MAGIC, &self.header)
    }

    pub fn dir_count(&self) -> u64 {
        self.header.dir_count
    }
}

pub struct MtimeControlFileReader {
    freader: BufReader<std::fs::File>,
    header: ControlFileHeader,
}

impl MtimeControlFileReader {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let (freader, header) = open_record_reader(path, MTIME_MAGIC)?;
        Ok(Self { freader, header })
    }

    pub fn header(&self) -> &ControlFileHeader {
        &self.header
    }
}

impl Iterator for MtimeControlFileReader {
    type Item = io::Result<MtimeDirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let payload = match read_record(&mut self.freader) {
            Ok(Some(payload)) => payload,
            Ok(None) => return None,
            Err(err) => return Some(Err(err)),
        };
        let mut cursor = 0usize;
        let mode = match take_u32(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let uid = match take_u32(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let gid = match take_u32(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let path_len = match take_u32(&payload, &mut cursor) {
            Ok(v) => v as usize,
            Err(err) => return Some(Err(err)),
        };
        let atime = match take_u64(&payload, &mut cursor) {
            Ok(v) => v,
            Err(err) => return Some(Err(err)),
        };
        let mtime = match take_u64(&payload, &mut cursor) {
            Ok(v) => v,
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
        Some(Ok(MtimeDirEntry {
            path,
            mode,
            uid,
            gid,
            atime,
            mtime,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mtime_control_file_roundtrip() {
        let path = std::env::temp_dir().join("bifrost_mtime_test.dat");
        {
            let mut writer = MtimeControlFileWriter::new(&path).unwrap();
            writer
                .write_dir(&MtimeDirEntry {
                    path: " weird\npath".to_string(),
                    mode: 0o755,
                    uid: 1000,
                    gid: 1000,
                    atime: 123,
                    mtime: 456,
                })
                .unwrap();
            writer.finish().unwrap();
        }

        let reader = MtimeControlFileReader::open(&path).unwrap();
        assert_eq!(reader.header().dir_count, 1);
        let entries: Vec<_> = reader.collect::<io::Result<Vec<_>>>().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, " weird\npath");
        assert_eq!(entries[0].mtime, 456);
        let _ = std::fs::remove_file(path);
    }
}
