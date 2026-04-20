//! # Delete Control File Format
//!
//! This module defines the control file format for recording files and directories
//! that need to be deleted during incremental backup operations.
//!
//! ## Purpose
//!
//! During incremental backup, files/directories that existed in the previous backup
//! but no longer exist in the current scan need to be deleted from the target.
//! The delete control file records these entries.
//!
//! ## File Format
//!
//! The delete control file is a text-based format.
//!
//! ```text
//! #BIFROST_DELETE_CTRL_FILE V1 FILES=<N> DIRS=<M> TIME=<UNIX_TIMESTAMP>
//!
//! D <PATH_LEN:8HEX> <PATH>
//! F <PATH_LEN:8HEX> <PATH>
//! ```
//!
//! Entry types:
//! - `D`: Directory to delete
//! - `F`: File to delete
//!
//! Example:
//! ```text
//! #BIFROST_DELETE_CTRL_FILE V1 FILES=2 DIRS=1 TIME=1700000000
//!
//! F 00000014 /home/user/old_file.txt
//! F 00000018 /home/user/temp_file.dat
//! D 00000012 /home/user/old_dir
//! ```

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

/// Represents an entry type in the delete control file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeleteEntryType {
    /// Directory entry
    Dir,
    /// File entry
    File,
}

impl DeleteEntryType {
    #[allow(dead_code)]
    fn as_char(&self) -> char {
        match self {
            DeleteEntryType::Dir => 'D',
            DeleteEntryType::File => 'F',
        }
    }

    fn from_char(c: char) -> Option<Self> {
        match c {
            'D' | 'd' => Some(DeleteEntryType::Dir),
            'F' | 'f' => Some(DeleteEntryType::File),
            _ => None,
        }
    }
}

/// Represents an entry in the delete control file.
#[derive(Debug, Clone, PartialEq)]
pub struct DeleteEntry {
    /// Type of entry (file or directory)
    pub entry_type: DeleteEntryType,
    /// Full path to the file/directory
    pub path: String,
}

/// Writer for delete control files.
pub struct DeleteControlFileWriter {
    fwriter: BufWriter<File>,
    file_count: u64,
    dir_count: u64,
}

impl DeleteControlFileWriter {
    /// Creates a new delete control file and writes the header.
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut fwriter = BufWriter::new(file);
        writeln!(
            fwriter,
            "#BIFROST_DELETE_CTRL_FILE V1 FILES=0 DIRS=0 TIME={}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        )?;
        writeln!(fwriter)?; // Empty line after header
        Ok(Self {
            fwriter,
            file_count: 0,
            dir_count: 0,
        })
    }

    /// Writes a file entry to delete.
    pub fn write_file(&mut self, path: &str) -> io::Result<()> {
        let path_len = path.len() as u32;
        writeln!(
            self.fwriter,
            "F {:08X} {}",
            path_len,
            path
        )?;
        self.file_count += 1;
        Ok(())
    }

    /// Writes a directory entry to delete.
    pub fn write_dir(&mut self, path: &str) -> io::Result<()> {
        let path_len = path.len() as u32;
        writeln!(
            self.fwriter,
            "D {:08X} {}",
            path_len,
            path
        )?;
        self.dir_count += 1;
        Ok(())
    }

    /// Writes a generic entry.
    pub fn write_entry(&mut self, entry: &DeleteEntry) -> io::Result<()> {
        match entry.entry_type {
            DeleteEntryType::Dir => self.write_dir(&entry.path),
            DeleteEntryType::File => self.write_file(&entry.path),
        }
    }

    /// Finishes writing and flushes all data to disk.
    pub fn finish(mut self) -> io::Result<()> {
        self.fwriter.flush()
    }

    /// Returns the current file count.
    pub fn file_count(&self) -> u64 {
        self.file_count
    }

    /// Returns the current directory count.
    pub fn dir_count(&self) -> u64 {
        self.dir_count
    }
}

/// Reader for delete control files.
pub struct DeleteControlFileReader {
    freader: BufReader<File>,
    header: String,
}

impl DeleteControlFileReader {
    /// Opens an existing delete control file for reading.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut freader = BufReader::new(file);
        let mut header = String::new();
        freader.read_line(&mut header)?;

        if !header.starts_with("#BIFROST_DELETE_CTRL_FILE") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid or missing delete control file header",
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

impl Iterator for DeleteControlFileReader {
    type Item = io::Result<DeleteEntry>;

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
                    if tokens.len() < 3 {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid line format: insufficient tokens",
                        )));
                    }

                    let kind = tokens[0].chars().next().unwrap_or('\0');
                    let entry_type = match DeleteEntryType::from_char(kind) {
                        Some(t) => t,
                        None => {
                            return Some(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Unknown entry type (expected 'D' or 'F')",
                            )));
                        }
                    };

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

                    return Some(Ok(DeleteEntry {
                        entry_type,
                        path: path.to_string(),
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
    fn test_delete_control_file_roundtrip() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        // Write test data
        {
            let mut writer = DeleteControlFileWriter::new(&path).unwrap();
            
            writer.write_file("/home/user/old_file.txt").unwrap();
            writer.write_file("/home/user/temp.dat").unwrap();
            writer.write_dir("/home/user/old_dir").unwrap();
            
            writer.finish().unwrap();
        }

        // Read and verify
        {
            let reader = DeleteControlFileReader::open(&path).unwrap();
            let entries: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
            
            assert_eq!(entries.len(), 3);
            
            assert_eq!(entries[0].entry_type, DeleteEntryType::File);
            assert_eq!(entries[0].path, "/home/user/old_file.txt");
            
            assert_eq!(entries[1].entry_type, DeleteEntryType::File);
            assert_eq!(entries[1].path, "/home/user/temp.dat");
            
            assert_eq!(entries[2].entry_type, DeleteEntryType::Dir);
            assert_eq!(entries[2].path, "/home/user/old_dir");
        }
    }
}
