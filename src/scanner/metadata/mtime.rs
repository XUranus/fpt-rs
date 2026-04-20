//! # Mtime Control File Format
//!
//! This module defines the control file format for recording directory modification
//! times (mtime and atime) during backup operations.
//!
//! ## Purpose
//!
//! The copy and hardlink phases may affect directory modification times. The mtime
//! phase runs after these phases to restore the original directory timestamps.
//!
//! ## File Format
//!
//! The mtime control file is a text-based format that records directory paths
//! and their original timestamps.
//!
//! ```text
//! #BIFROST_MTIME_CTRL_FILE V1 DIRS=<N> TIME=<UNIX_TIMESTAMP>
//!
//! D <PATH_LEN:8HEX> <PATH> <MODE:8HEX> <UID:8HEX> <GID:8HEX> <ATIME:16HEX> <MTIME:16HEX>
//! ```
//!
//! Example:
//! ```text
//! #BIFROST_MTIME_CTRL_FILE V1 DIRS=3 TIME=1700000000
//!
//! D 00000010 /home/user/docs 000041ED 000003E8 000003E8 00000170B5D7A300 00000170B5D7A300
//! D 0000000E /home/user/src 000041ED 000003E8 000003E8 00000170B5D7A400 00000170B5D7A400
//! ```

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

/// Represents a directory entry in the mtime control file.
#[derive(Debug, Clone, PartialEq)]
pub struct MtimeDirEntry {
    /// Full path to the directory
    pub path: String,
    /// File mode (permissions)
    pub mode: u32,
    /// User ID
    pub uid: u32,
    /// Group ID
    pub gid: u32,
    /// Last access time (seconds since Unix epoch)
    pub atime: u64,
    /// Last modification time (seconds since Unix epoch)
    pub mtime: u64,
}

/// Writer for mtime control files.
pub struct MtimeControlFileWriter {
    fwriter: BufWriter<File>,
    dir_count: u64,
}

impl MtimeControlFileWriter {
    /// Creates a new mtime control file and writes the header.
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut fwriter = BufWriter::new(file);
        writeln!(
            fwriter,
            "#BIFROST_MTIME_CTRL_FILE V1 DIRS=0 TIME={}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        )?;
        writeln!(fwriter)?; // Empty line after header
        Ok(Self {
            fwriter,
            dir_count: 0,
        })
    }

    /// Writes a directory entry.
    pub fn write_dir(&mut self, entry: &MtimeDirEntry) -> io::Result<()> {
        let path_len = entry.path.len() as u32;
        writeln!(
            self.fwriter,
            "D {:08X} {} {:08X} {:08X} {:08X} {:016X} {:016X}",
            path_len,
            entry.path,
            entry.mode,
            entry.uid,
            entry.gid,
            entry.atime,
            entry.mtime
        )?;
        self.dir_count += 1;
        Ok(())
    }

    /// Finishes writing and flushes all data to disk.
    pub fn finish(mut self) -> io::Result<()> {
        self.fwriter.flush()
    }

    /// Returns the current directory count.
    pub fn dir_count(&self) -> u64 {
        self.dir_count
    }
}

/// Reader for mtime control files.
pub struct MtimeControlFileReader {
    freader: BufReader<File>,
    header: String,
}

impl MtimeControlFileReader {
    /// Opens an existing mtime control file for reading.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut freader = BufReader::new(file);
        let mut header = String::new();
        freader.read_line(&mut header)?;

        if !header.starts_with("#BIFROST_MTIME_CTRL_FILE") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid or missing mtime control file header",
            ));
        }

        // Skip the empty line after header
        let mut empty_line = String::new();
        freader.read_line(&mut empty_line)?;

        Ok(Self { freader, header })
    }

    /// Returns the header string.
    pub fn header(&self) -> &str {
        &self.header
    }
}

impl Iterator for MtimeControlFileReader {
    type Item = io::Result<MtimeDirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        loop {
            line.clear();
            match self.freader.read_line(&mut line) {
                Ok(0) => return None, // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue; // Skip empty lines and comments
                    }

                    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                    if tokens.len() < 8 {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid line format: insufficient tokens",
                        )));
                    }

                    let kind = tokens[0];
                    if kind != "D" {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Unknown entry type (expected 'D')",
                        )));
                    }

                    // Parse path length
                    let path_len = match u32::from_str_radix(tokens[1], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };

                    // Parse path (token 2)
                    let path = tokens[2];
                    if path.len() != path_len as usize {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Path length mismatch",
                        )));
                    }

                    // Parse mode
                    let mode = match u32::from_str_radix(tokens[3], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };

                    // Parse uid
                    let uid = match u32::from_str_radix(tokens[4], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };

                    // Parse gid
                    let gid = match u32::from_str_radix(tokens[5], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };

                    // Parse atime
                    let atime = match u64::from_str_radix(tokens[6], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };

                    // Parse mtime
                    let mtime = match u64::from_str_radix(tokens[7], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };

                    return Some(Ok(MtimeDirEntry {
                        path: path.to_string(),
                        mode,
                        uid,
                        gid,
                        atime,
                        mtime,
                    }));
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_mtime_control_file_roundtrip() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        // Write test data
        {
            let mut writer = MtimeControlFileWriter::new(&path).unwrap();
            
            writer.write_dir(&MtimeDirEntry {
                path: "/home/user/docs".to_string(),
                mode: 0o40755,
                uid: 1000,
                gid: 1000,
                atime: 1700000000,
                mtime: 1700000000,
            }).unwrap();
            
            writer.write_dir(&MtimeDirEntry {
                path: "/home/user/src".to_string(),
                mode: 0o40755,
                uid: 1000,
                gid: 1000,
                atime: 1700000100,
                mtime: 1700000100,
            }).unwrap();
            
            writer.finish().unwrap();
        }

        // Read and verify
        {
            let reader = MtimeControlFileReader::open(&path).unwrap();
            let entries: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
            
            assert_eq!(entries.len(), 2);
            
            assert_eq!(entries[0].path, "/home/user/docs");
            assert_eq!(entries[0].mode, 0o40755);
            assert_eq!(entries[0].uid, 1000);
            assert_eq!(entries[0].gid, 1000);
            assert_eq!(entries[0].atime, 1700000000);
            assert_eq!(entries[0].mtime, 1700000000);
            
            assert_eq!(entries[1].path, "/home/user/src");
            assert_eq!(entries[1].atime, 1700000100);
            assert_eq!(entries[1].mtime, 1700000100);
        }
    }
}
