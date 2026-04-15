//! # Backup Control File Format
//!
//! This module defines a **human-readable, line-oriented control file format**
//! used to describe file and directory changes between backup versions.
//!
//! The control file enables efficient incremental backup and recovery by listing:
//! - Which files/directories are **new**, **modified**, or **deleted**.
//! - Where their full metadata is stored in the metadata repository (`meta_fid`, `meta_offset`).
//!
//! ## File Format
//!
//! The file begins with a header line:
//! ```text
//! #BIFROST_BACKUP_CTRL_FILE V1 FILE=<N> DIRS=<M> TIME=<UNIX_TIMESTAMP>
//! ```
//!
//! Each subsequent line represents one entry:
//! - **File entry**:  
//!   `F <DIFF> <META_FID:8HEX> <META_OFFSET:8HEX> -------- <NAME_LEN:8HEX> <NAME>`
//! - **Directory entry**:  
//!   `D <DIFF> <META_FID:8HEX> <META_OFFSET:8HEX> <FILES_COUNT:8HEX> <PATH_LEN:8HEX> <PATH>`
//!
//! Where:
//! - `<DIFF>` is a 2-character code (e.g., `"NN"` for new, `"DM"` for data modified).
//! - All numeric fields are written as **8-digit uppercase hexadecimal**.
//! - The `--------` placeholder in file entries maintains column alignment.
//!
//! Example:
//! ```text
//! #BIFROST_BACKUP_CTRL_FILE V1 FILE=2 DIRS=1 TIME=1700000000
//! D NN 00000000 000001A0 00000002 00000005 /home
//! F NN 00000000 00000200 -------- 00000008 .bashrc
//! F DM 00000000 00000300 -------- 00000009 notes.txt
//! ```

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

/// Represents the type of change detected for a file.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub enum FileDiff {
    /// The file is new (did not exist in the previous backup).
    New,
    /// The file's data has changed (content hash differs).
    DataModified,
    /// The file's metadata has changed (permissions, timestamps, etc.), but content is unchanged.
    MetaModified,
    /// The file was deleted (exists in previous backup but not current scan).
    Deleted,
}

/// Represents the type of change detected for a directory.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub enum DirDiff {
    /// The directory is new.
    New,
    /// The directory's metadata has changed (e.g., permissions), but its contents are unchanged.
    MetaModified,
    /// The directory was deleted.
    Deleted,
}

/// A control entry for a file, describing its change status and metadata location.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub struct FileControlEntry {
    /// Base name of the file.
    pub name: String,
    /// Type of change detected.
    pub diff: FileDiff,
    /// ID of the metadata file containing the full `FileMeta`.
    pub meta_fid: u32,
    /// Byte offset within the metadata file where `FileMeta` starts.
    pub meta_offset: u32,
}

/// A control entry for a directory, describing its change status and metadata location.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, PartialEq)]
pub struct DirControlEntry {
    /// Full path of the directory.
    pub path: String,
    /// Type of change detected.
    pub diff: DirDiff,
    /// ID of the metadata file containing the full `DirMeta`.
    pub meta_fid: u32,
    /// Byte offset within the metadata file where `DirMeta` starts.
    pub meta_offset: u32,
    /// Number of files directly contained in this directory.
    pub files_count: u32,
}

impl FileDiff {
    /// Returns the short string representation of this diff type.
    ///
    /// Mapping:
    /// - `New` → `"NN"`
    /// - `DataModified` → `"DM"`
    /// - `MetaModified` → `"MM"`
    /// - `Deleted` → `"DD"`
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::New => "NN",
            Self::DataModified => "DM",
            Self::MetaModified => "MM",
            Self::Deleted => "DD",
        }
    }

    /// Parses a short string representation into a `FileDiff`.
    ///
    /// Returns `None` if the input is not one of: `"NN"`, `"DM"`, `"MM"`, `"DD"`.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "NN" => Some(Self::New),
            "DM" => Some(Self::DataModified),
            "MM" => Some(Self::MetaModified),
            "DD" => Some(Self::Deleted),
            _ => None,
        }
    }
}

impl DirDiff {
    /// Returns the short string representation of this diff type.
    ///
    /// Mapping:
    /// - `New` → `"NN"`
    /// - `MetaModified` → `"MM"`
    /// - `Deleted` → `"DD"`
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::New => "NN",
            Self::MetaModified => "MM",
            Self::Deleted => "DD",
        }
    }

    /// Parses a short string representation into a `DirDiff`.
    ///
    /// Returns `None` if the input is not one of: `"NN"`, `"MM"`, `"DD"`.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "NN" => Some(Self::New),
            "MM" => Some(Self::MetaModified),
            "DD" => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// Writer for backup control files.
///
/// Appends file and directory entries in human-readable text format.
/// Not thread-safe. Use external synchronization if needed.
pub struct ControlFileWriter {
    fwriter: BufWriter<File>,
    file_count: u64,
    dir_count: u64,
}

impl ControlFileWriter {
    /// Creates a new control file and writes the header.
    ///
    /// The file is created or truncated. The header includes placeholder counts
    /// (`FILE=0 DIRS=0`) since final counts are not known at creation time.
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::create(path)?; // ✅ Use create(), not open()
        let mut fwriter = BufWriter::new(file);
        writeln!(
            fwriter,
            "#BIFROST_BACKUP_CTRL_FILE V1 FILE=0 DIRS=0 TIME={}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        )?;
        Ok(Self {
            fwriter,
            file_count: 0,
            dir_count: 0,
        })
    }

    /// Writes a file control entry.
    pub fn write_file(&mut self, entry: &FileControlEntry) -> io::Result<()> {
        let path_len = entry.name.len() as u32;
        writeln!(
            self.fwriter,
            "F {} {:08X} {:08X} -------- {:08X} {}",
            entry.diff.as_str(),
            entry.meta_fid,
            entry.meta_offset,
            path_len,
            entry.name
        )?;
        self.file_count += 1;
        Ok(())
    }

    /// Writes a directory control entry.
    pub fn write_dir(&mut self, entry: &DirControlEntry) -> io::Result<()> {
        let path_len = entry.path.len() as u32;
        writeln!(
            self.fwriter,
            "D {} {:08X} {:08X} {:08X} {:08X} {}",
            entry.diff.as_str(),
            entry.meta_fid,
            entry.meta_offset,
            entry.files_count,
            path_len,
            entry.path
        )?;
        self.dir_count += 1;
        Ok(())
    }

    /// Writes a directory control entry with batch information.
    ///
    /// For large directories split across multiple batches:
    /// ```text
    /// D NN 00000000 00000100 00000064 00000010 /data/huge BATCH=0/5
    /// D NN 00000000 00000100 00000064 00000010 /data/huge BATCH=1/5 CONT
    /// D NN 00000000 00000100 00000064 00000010 /data/huge BATCH=2/5 CONT LAST
    /// ```
    pub fn write_dir_with_batch(
        &mut self,
        entry: &DirControlEntry,
        batch: crate::scanner::metadata::sharded_control::BatchInfo,
    ) -> io::Result<()> {
        let path_len = entry.path.len() as u32;
        let batch_marker = format!(
            "BATCH={}/{}{}{}",
            batch.batch_num,
            batch.total_batches,
            if batch.is_continuation { " CONT" } else { "" },
            if batch.is_last { " LAST" } else { "" }
        );
        writeln!(
            self.fwriter,
            "D {} {:08X} {:08X} {:08X} {:08X} {} {}",
            entry.diff.as_str(),
            entry.meta_fid,
            entry.meta_offset,
            entry.files_count,
            path_len,
            entry.path,
            batch_marker
        )?;
        self.dir_count += 1;
        Ok(())
    }

    /// Finishes writing and flushes all data to disk.
    ///
    /// Note: The header is **not updated** with final counts. For accurate counts,
    /// consider post-processing the file or using a different format.
    pub fn finish(mut self) -> io::Result<()> {
        self.fwriter.flush()
    }
}

/// A parsed control entry from the file.
#[derive(Debug)]
pub enum ControlEntry {
    /// A file entry.
    File(FileControlEntry),
    /// A directory entry.
    Dir(DirControlEntry),
}

/// Reader for backup control files.
///
/// Parses the control file line by line, skipping empty lines and comments.
/// Not thread-safe.
pub struct ControlFileReader {
    freader: BufReader<File>,
    header: String,
}

impl ControlFileReader {
    /// Opens an existing control file for reading.
    ///
    /// Validates that the first line is a valid header.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut freader = BufReader::new(file);
        let mut header = String::new();
        freader.read_line(&mut header)?;

        if !header.starts_with("#BIFROST_BACKUP_CTRL_FILE") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid or missing header",
            ));
        }

        Ok(Self { freader, header })
    }
}

impl Iterator for ControlFileReader {
    type Item = io::Result<ControlEntry>;

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
                    // F requires at least 7 tokens, D requires at least 7
                    if tokens.len() < 7 {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid line format: insufficient tokens",
                        )));
                    }

                    let kind = tokens[0];
                    let diff_code = tokens[1];

                    // Parse common fields
                    let meta_fid = match u32::from_str_radix(tokens[2], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };
                    let meta_offset = match u32::from_str_radix(tokens[3], 16) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                    };

                    return match kind {
                        "F" => {
                            let path_len = match u32::from_str_radix(tokens[5], 16) {
                                Ok(v) => v,
                                Err(e) => {
                                    return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e)))
                                }
                            };
                            let path = tokens[6];
                            // Optional: validate path_len == path.len()
                            let diff = match FileDiff::from_str(diff_code) {
                                Some(d) => d,
                                None => {
                                    return Some(Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Unknown file diff code",
                                    )))
                                }
                            };
                            Some(Ok(ControlEntry::File(FileControlEntry {
                                name: path.to_string(),
                                diff,
                                meta_fid,
                                meta_offset,
                            })))
                        }
                        "D" => {
                            let files_count = match u32::from_str_radix(tokens[4], 16) {
                                Ok(v) => v,
                                Err(e) => {
                                    return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e)))
                                }
                            };
                            let path_len = match u32::from_str_radix(tokens[5], 16) {
                                Ok(v) => v,
                                Err(e) => {
                                    return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e)))
                                }
                            };
                            let path = tokens[6];
                            let diff = match DirDiff::from_str(diff_code) {
                                Some(d) => d,
                                None => {
                                    return Some(Err(io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        "Unknown directory diff code",
                                    )))
                                }
                            };
                            Some(Ok(ControlEntry::Dir(DirControlEntry {
                                path: path.to_string(),
                                diff,
                                meta_fid,
                                meta_offset,
                                files_count,
                            })))
                        }
                        _ => Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Unknown entry type",
                        ))),
                    };
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}