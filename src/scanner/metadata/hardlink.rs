//! # Hardlink Control File Format
//!
//! This module defines the control file format for tracking hardlink relationships
//! during backup and restore operations.
//!
//! ## File Format
//!
//! The hardlink control file is a text-based format that records inode-to-path mappings
//! for files with multiple hard links (nlink > 1).
//!
//! ```text
//! #BIFROST_HARDLINK_CTRL_FILE V1 FILES=<N> INODES=<M> TIME=<UNIX_TIMESTAMP>
//!
//! # Inode entry (marks the start of a new inode group)
//! I <INODE:16HEX> <DEVICE:16HEX> <LINK_COUNT:8HEX>
//!
//! # File entry (belongs to the preceding inode entry)
//! F <META_FID:8HEX> <META_OFFSET:8HEX> <PATH_LEN:8HEX> <PATH>
//! ```
//!
//! Example:
//! ```text
//! #BIFROST_HARDLINK_CTRL_FILE V1 FILES=3 INODES=1 TIME=1700000000
//!
//! I 000000000000ABCD 0000000000000801 00000003
//! F 00000000 00000100 00000014 /home/user/file1.txt
//! F 00000000 00000150 00000014 /home/user/file2.txt
//! F 00000000 000001A0 00000014 /home/user/file3.txt
//! ```
//!
//! The backup process uses this file to:
//! 1. Copy the first file in each inode group normally
//! 2. Create subsequent files as hard links to the first file

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

/// Represents an inode entry in the hardlink control file.
/// Marks the start of a new group of hardlinked files.
#[derive(Debug, Clone, PartialEq)]
pub struct HardlinkInodeEntry {
    /// Inode number (unique identifier within a device)
    pub inode: u64,
    /// Device number (major/minor combined)
    pub device: u64,
    /// Number of hard links to this inode
    pub link_count: u32,
}

/// Represents a file entry belonging to an inode group.
#[derive(Debug, Clone, PartialEq)]
pub struct HardlinkFileEntry {
    /// ID of the metadata file containing the full file metadata
    pub meta_fid: u32,
    /// Byte offset within the metadata file where metadata starts
    pub meta_offset: u32,
    /// Full path to the file
    pub path: String,
}

/// A parsed entry from the hardlink control file.
#[derive(Debug)]
pub enum HardlinkEntry {
    /// Inode entry marking a new group
    Inode(HardlinkInodeEntry),
    /// File entry belonging to the current group
    File(HardlinkFileEntry),
}

/// Writer for hardlink control files.
pub struct HardlinkControlFileWriter {
    fwriter: BufWriter<File>,
    file_count: u64,
    inode_count: u64,
}

impl HardlinkControlFileWriter {
    /// Creates a new hardlink control file and writes the header.
    pub fn new<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut fwriter = BufWriter::new(file);
        writeln!(
            fwriter,
            "#BIFROST_HARDLINK_CTRL_FILE V1 FILES=0 INODES=0 TIME={}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        )?;
        writeln!(fwriter)?; // Empty line after header
        Ok(Self {
            fwriter,
            file_count: 0,
            inode_count: 0,
        })
    }

    /// Writes an inode entry to mark the start of a new hardlink group.
    pub fn write_inode(&mut self, entry: &HardlinkInodeEntry) -> io::Result<()> {
        writeln!(
            self.fwriter,
            "I {:016X} {:016X} {:08X}",
            entry.inode, entry.device, entry.link_count
        )?;
        self.inode_count += 1;
        Ok(())
    }

    /// Writes a file entry belonging to the current inode group.
    pub fn write_file(&mut self, entry: &HardlinkFileEntry) -> io::Result<()> {
        let path_len = entry.path.len() as u32;
        writeln!(
            self.fwriter,
            "F {:08X} {:08X} {:08X} {}",
            entry.meta_fid, entry.meta_offset, path_len, entry.path
        )?;
        self.file_count += 1;
        Ok(())
    }

    /// Finishes writing and flushes all data to disk.
    pub fn finish(mut self) -> io::Result<()> {
        self.fwriter.flush()
    }

    /// Returns the current file count.
    pub fn file_count(&self) -> u64 {
        self.file_count
    }

    /// Returns the current inode count.
    pub fn inode_count(&self) -> u64 {
        self.inode_count
    }
}

/// Reader for hardlink control files.
pub struct HardlinkControlFileReader {
    freader: BufReader<File>,
    header: String,
}

impl HardlinkControlFileReader {
    /// Opens an existing hardlink control file for reading.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut freader = BufReader::new(file);
        let mut header = String::new();
        freader.read_line(&mut header)?;

        if !header.starts_with("#BIFROST_HARDLINK_CTRL_FILE") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid or missing hardlink control file header",
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

impl Iterator for HardlinkControlFileReader {
    type Item = io::Result<HardlinkEntry>;

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
                    if tokens.len() < 2 {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid line format: insufficient tokens",
                        )));
                    }

                    let kind = tokens[0];

                    return match kind {
                        "I" => {
                            if tokens.len() < 4 {
                                return Some(Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "Invalid inode entry format",
                                )));
                            }
                            let inode = match u64::from_str_radix(tokens[1], 16) {
                                Ok(v) => v,
                                Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                            };
                            let device = match u64::from_str_radix(tokens[2], 16) {
                                Ok(v) => v,
                                Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                            };
                            let link_count = match u32::from_str_radix(tokens[3], 16) {
                                Ok(v) => v,
                                Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                            };
                            Some(Ok(HardlinkEntry::Inode(HardlinkInodeEntry {
                                inode,
                                device,
                                link_count,
                            })))
                        }
                        "F" => {
                            if tokens.len() < 5 {
                                return Some(Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "Invalid file entry format",
                                )));
                            }
                            let meta_fid = match u32::from_str_radix(tokens[1], 16) {
                                Ok(v) => v,
                                Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                            };
                            let meta_offset = match u32::from_str_radix(tokens[2], 16) {
                                Ok(v) => v,
                                Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                            };
                            let path_len = match u32::from_str_radix(tokens[3], 16) {
                                Ok(v) => v,
                                Err(e) => return Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
                            };
                            let path = tokens[4];
                            // Validate path length
                            if path.len() != path_len as usize {
                                return Some(Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "Path length mismatch",
                                )));
                            }
                            Some(Ok(HardlinkEntry::File(HardlinkFileEntry {
                                meta_fid,
                                meta_offset,
                                path: path.to_string(),
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

/// Hardlink group information collected during scanning.
/// Maps an inode to all its file paths.
#[derive(Debug, Default)]
pub struct HardlinkGroup {
    /// Inode number
    pub inode: u64,
    /// Device number
    pub device: u64,
    /// Number of hard links
    pub link_count: u32,
    /// List of (meta_fid, meta_offset, path) for each file
    pub files: Vec<(u32, u32, String)>,
}

/// In-memory index for tracking hardlinks during scanning.
#[derive(Debug, Default)]
pub struct HardlinkIndex {
    /// Maps (device, inode) to the index in the groups vector
    inode_map: std::collections::HashMap<(u64, u64), usize>,
    /// List of hardlink groups
    groups: Vec<HardlinkGroup>,
}

impl HardlinkIndex {
    /// Creates a new empty hardlink index.
    pub fn new() -> Self {
        Self {
            inode_map: std::collections::HashMap::new(),
            groups: Vec::new(),
        }
    }

    /// Adds a file to the hardlink index.
    /// Returns true if this is a hardlink (nlink > 1), false otherwise.
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
            // Existing group - add file to it
            self.groups[idx].files.push((meta_fid, meta_offset, path));
        } else {
            // New group
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

    /// Returns all hardlink groups.
    pub fn groups(&self) -> &[HardlinkGroup] {
        &self.groups
    }

    /// Returns the number of hardlink groups.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Returns the total number of files in all groups.
    pub fn total_file_count(&self) -> usize {
        self.groups.iter().map(|g| g.files.len()).sum()
    }

    /// Writes all hardlink groups to a control file.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_hardlink_control_file_roundtrip() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        // Write test data
        {
            let mut writer = HardlinkControlFileWriter::new(&path).unwrap();
            
            writer.write_inode(&HardlinkInodeEntry {
                inode: 0xABCD,
                device: 0x0801,
                link_count: 3,
            }).unwrap();
            
            writer.write_file(&HardlinkFileEntry {
                meta_fid: 0,
                meta_offset: 0x100,
                path: "/home/user/file1.txt".to_string(),
            }).unwrap();
            
            writer.write_file(&HardlinkFileEntry {
                meta_fid: 0,
                meta_offset: 0x150,
                path: "/home/user/file2.txt".to_string(),
            }).unwrap();
            
            writer.finish().unwrap();
        }

        // Read and verify
        {
            let reader = HardlinkControlFileReader::open(&path).unwrap();
            let entries: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
            
            assert_eq!(entries.len(), 3);
            
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
                    assert_eq!(file.path, "/home/user/file1.txt");
                }
                _ => panic!("Expected file entry"),
            }
        }
    }

    #[test]
    fn test_hardlink_index() {
        let mut index = HardlinkIndex::new();
        
        // Add hardlinked files
        assert!(index.add_file(12345, 0x0801, 3, 0, 0x100, "/path/file1".to_string()));
        assert!(index.add_file(12345, 0x0801, 3, 0, 0x150, "/path/file2".to_string()));
        assert!(index.add_file(12345, 0x0801, 3, 0, 0x1A0, "/path/file3".to_string()));
        
        // Add non-hardlinked file
        assert!(!index.add_file(99999, 0x0801, 1, 0, 0x200, "/path/single".to_string()));
        
        assert_eq!(index.group_count(), 1);
        assert_eq!(index.total_file_count(), 3);
        
        let groups = index.groups();
        assert_eq!(groups[0].files.len(), 3);
    }
}
