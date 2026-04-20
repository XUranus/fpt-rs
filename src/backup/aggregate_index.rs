//! # Aggregate Index Module
//!
//! This module provides indexing for aggregate blob files.
//! The index maps original filenames to their locations within blob files,
//! enabling efficient restore operations.
//!
//! When the `sqlite` feature is enabled, uses SQLite for persistent storage.
//! Otherwise, uses an in-memory HashMap.

use log::{debug, info};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::backup::aggregate::{AggregateBlobMeta, AggregateFileEntry, AggregateRestoreInfo};

/// SQLite schema for the aggregate index.
pub const INDEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS aggregate_index (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_name TEXT NOT NULL,
    dir_path TEXT NOT NULL,
    blob_name TEXT NOT NULL,
    offset INTEGER NOT NULL,
    size INTEGER NOT NULL,
    ctime INTEGER,
    mtime INTEGER,
    mode INTEGER,
    xattrs TEXT,
    acl TEXT,
    UNIQUE(file_name, dir_path)
);

CREATE INDEX IF NOT EXISTS idx_file_name ON aggregate_index(file_name);
CREATE INDEX IF NOT EXISTS idx_blob_name ON aggregate_index(blob_name);
CREATE INDEX IF NOT EXISTS idx_dir_path ON aggregate_index(dir_path);
"#;

/// Manages the index for aggregate backup/restore.
pub struct AggregateIndex {
    db_path: PathBuf,
    /// In-memory storage for when SQLite is not available
    memory_index: Mutex<HashMap<String, AggregateRestoreInfo>>,
}

impl AggregateIndex {
    /// Creates or opens an aggregate index at the specified path.
    pub fn open(db_path: &Path) -> Result<Self, AggregateIndexError> {
        let db_path = db_path.to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Initialize the database schema (opens and closes connection)
        #[cfg(feature = "sqlite")]
        {
            let conn = rusqlite::Connection::open(&db_path)?;
            conn.execute_batch(INDEX_SCHEMA)?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
            // Connection is dropped here, closing it
        }

        let index = Self {
            db_path,
            memory_index: Mutex::new(HashMap::new()),
        };

        info!("Aggregate index opened at {:?}", index.db_path);
        Ok(index)
    }

    /// Opens a connection to the SQLite database.
    /// Note: We open/close connections per operation to avoid file descriptor exhaustion
    /// when many directories are being processed concurrently.
    #[cfg(feature = "sqlite")]
    fn open_connection(&self) -> Result<rusqlite::Connection, AggregateIndexError> {
        let conn = rusqlite::Connection::open(&self.db_path)?;
        Ok(conn)
    }

    /// Adds a blob's metadata to the index.
    pub fn add_blob(&self, blob_meta: &AggregateBlobMeta) -> Result<(), AggregateIndexError> {
        #[cfg(feature = "sqlite")]
        {
            self.add_blob_sqlite(blob_meta)?;
        }

        // Also add to in-memory index as fallback
        let mut index = self.memory_index.lock().unwrap();
        for entry in &blob_meta.files {
            let key = format!("{}/{}", blob_meta.dir_path, entry.file_name);
            let info = AggregateRestoreInfo {
                blob_name: blob_meta.blob_name.clone(),
                offset: entry.offset,
                size: entry.size,
                mtime: entry.mtime,
                mode: entry.mode,
                xattrs: entry.xattrs.clone(),
                acl: entry.acl.clone(),
            };
            index.insert(key, info);
        }

        debug!(
            "Added {} files to index for blob {}",
            blob_meta.files.len(),
            blob_meta.blob_name
        );
        Ok(())
    }

    /// Adds a blob to the SQLite index.
    #[cfg(feature = "sqlite")]
    fn add_blob_sqlite(&self, blob_meta: &AggregateBlobMeta) -> Result<(), AggregateIndexError> {
        use rusqlite::params;

        let mut conn = self.open_connection()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO aggregate_index 
                 (file_name, dir_path, blob_name, offset, size, ctime, mtime, mode, xattrs, acl)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;

            for entry in &blob_meta.files {
                stmt.execute(params![
                    &entry.file_name,
                    &blob_meta.dir_path,
                    &blob_meta.blob_name,
                    entry.offset as i64,
                    entry.size as i64,
                    entry.ctime as i64,
                    entry.mtime as i64,
                    entry.mode as i64,
                    entry.xattrs.as_ref().map(|s| s.as_str()),
                    entry.acl.as_ref().map(|s| s.as_str()),
                ])?;
            }
        }
        tx.commit()?;

        Ok(())
    }

    /// Queries the index for a file's restore information.
    pub fn query_file(
        &self,
        file_name: &str,
        dir_path: &str,
    ) -> Result<Option<AggregateRestoreInfo>, AggregateIndexError> {
        // First try in-memory index
        let key = format!("{}/{}", dir_path, file_name);
        let index = self.memory_index.lock().unwrap();
        if let Some(info) = index.get(&key) {
            return Ok(Some(info.clone()));
        }
        drop(index);

        #[cfg(feature = "sqlite")]
        {
            self.query_file_sqlite(file_name, dir_path)
        }

        #[cfg(not(feature = "sqlite"))]
        {
            Ok(None)
        }
    }

    /// Queries the SQLite index for a file.
    #[cfg(feature = "sqlite")]
    fn query_file_sqlite(
        &self,
        file_name: &str,
        dir_path: &str,
    ) -> Result<Option<AggregateRestoreInfo>, AggregateIndexError> {
        use rusqlite::params;

        let conn = self.open_connection()?;
        let mut stmt = conn.prepare(
            "SELECT blob_name, offset, size, mtime, mode, xattrs, acl 
             FROM aggregate_index 
             WHERE file_name = ?1 AND dir_path = ?2",
        )?;

        let result = stmt.query_row(params![file_name, dir_path], |row| {
            Ok(AggregateRestoreInfo {
                blob_name: row.get(0)?,
                offset: row.get::<_, i64>(1)? as u64,
                size: row.get::<_, i64>(2)? as u64,
                mtime: row.get::<_, i64>(3)? as u64,
                mode: row.get::<_, i64>(4)? as u32,
                xattrs: row.get(5)?,
                acl: row.get(6)?,
            })
        });

        match result {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Gets all files in a specific blob.
    pub fn get_blob_files(
        &self,
        blob_name: &str,
    ) -> Result<Vec<AggregateFileEntry>, AggregateIndexError> {
        #[cfg(feature = "sqlite")]
        {
            self.get_blob_files_sqlite(blob_name)
        }

        #[cfg(not(feature = "sqlite"))]
        {
            // Search in-memory index for files in this blob
            let index = self.memory_index.lock().unwrap();
            let entries: Vec<AggregateFileEntry> = index
                .iter()
                .filter(|(_, info)| info.blob_name == blob_name)
                .map(|(key, info)| {
                    // Extract filename from key (format: "dir_path/file_name")
                    let file_name = key
                        .rfind('/')
                        .map(|i| &key[i + 1..])
                        .unwrap_or(key.as_str())
                        .to_string();
                    AggregateFileEntry {
                        file_name,
                        offset: info.offset,
                        size: info.size,
                        ctime: 0, // Not stored in restore info
                        mtime: info.mtime,
                        mode: info.mode,
                        xattrs: info.xattrs.clone(),
                        acl: info.acl.clone(),
                    }
                })
                .collect();
            Ok(entries)
        }
    }

    /// Gets all files in a specific blob from SQLite.
    #[cfg(feature = "sqlite")]
    fn get_blob_files_sqlite(
        &self,
        blob_name: &str,
    ) -> Result<Vec<AggregateFileEntry>, AggregateIndexError> {
        use rusqlite::params;

        let conn = self.open_connection()?;
        let mut stmt = conn.prepare(
            "SELECT file_name, offset, size, ctime, mtime, mode, xattrs, acl 
             FROM aggregate_index 
             WHERE blob_name = ?1",
        )?;

        let entries = stmt.query_map(params![blob_name], |row| {
            Ok(AggregateFileEntry {
                file_name: row.get(0)?,
                offset: row.get::<_, i64>(1)? as u64,
                size: row.get::<_, i64>(2)? as u64,
                ctime: row.get::<_, i64>(3)? as u64,
                mtime: row.get::<_, i64>(4)? as u64,
                mode: row.get::<_, i64>(5)? as u32,
                xattrs: row.get(6)?,
                acl: row.get(7)?,
            })
        })?;

        let mut result = Vec::new();
        for entry in entries {
            result.push(entry?);
        }

        Ok(result)
    }

    /// Checks if a file is in the index (i.e., was aggregated).
    pub fn is_aggregated(
        &self,
        file_name: &str,
        dir_path: &str,
    ) -> Result<bool, AggregateIndexError> {
        // Check in-memory index first
        let key = format!("{}/{}", dir_path, file_name);
        let index = self.memory_index.lock().unwrap();
        if index.contains_key(&key) {
            return Ok(true);
        }
        drop(index);

        #[cfg(feature = "sqlite")]
        {
            use rusqlite::params;

            let conn = self.open_connection()?;
            let mut stmt = conn.prepare(
                "SELECT 1 FROM aggregate_index WHERE file_name = ?1 AND dir_path = ?2 LIMIT 1",
            )?;

            let result: Result<i64, _> =
                stmt.query_row(params![file_name, dir_path], |row| row.get(0));

            match result {
                Ok(_) => Ok(true),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
                Err(e) => Err(e.into()),
            }
        }

        #[cfg(not(feature = "sqlite"))]
        {
            Ok(false)
        }
    }

    /// Deletes index entries for a specific blob (used for cleanup).
    #[cfg(feature = "sqlite")]
    pub fn delete_blob_entries(&self, blob_name: &str) -> Result<usize, AggregateIndexError> {
        use rusqlite::params;

        let conn = self.open_connection()?;
        let count = conn.execute(
            "DELETE FROM aggregate_index WHERE blob_name = ?1",
            params![blob_name],
        )?;

        info!("Deleted {} entries for blob {}", count, blob_name);
        Ok(count)
    }

    #[cfg(not(feature = "sqlite"))]
    pub fn delete_blob_entries(&self, _blob_name: &str) -> Result<usize, AggregateIndexError> {
        Ok(0)
    }

    /// Gets statistics about the index.
    pub fn get_stats(&self) -> Result<IndexStats, AggregateIndexError> {
        let memory_count = self.memory_index.lock().unwrap().len() as u64;

        #[cfg(feature = "sqlite")]
        {
            let conn = self.open_connection()?;

            let total_files: i64 =
                conn.query_row("SELECT COUNT(*) FROM aggregate_index", [], |row| row.get(0))?;

            let total_blobs: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT blob_name) FROM aggregate_index",
                [],
                |row| row.get(0),
            )?;

            let total_size: i64 = conn.query_row(
                "SELECT COALESCE(SUM(size), 0) FROM aggregate_index",
                [],
                |row| row.get(0),
            )?;

            Ok(IndexStats {
                total_files: total_files as u64,
                total_blobs: total_blobs as u64,
                total_size: total_size as u64,
                memory_entries: memory_count,
            })
        }

        #[cfg(not(feature = "sqlite"))]
        {
            Ok(IndexStats {
                total_files: memory_count,
                total_blobs: 0,
                total_size: 0,
                memory_entries: memory_count,
            })
        }
    }
}

/// Statistics about the aggregate index.
#[derive(Debug, Default)]
pub struct IndexStats {
    pub total_files: u64,
    pub total_blobs: u64,
    pub total_size: u64,
    pub memory_entries: u64,
}

/// Errors that can occur when working with the aggregate index.
#[derive(Debug)]
pub enum AggregateIndexError {
    Io(std::io::Error),
    #[cfg(feature = "sqlite")]
    Sqlite(rusqlite::Error),
    Other(String),
}

impl std::fmt::Display for AggregateIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregateIndexError::Io(e) => write!(f, "IO error: {}", e),
            #[cfg(feature = "sqlite")]
            AggregateIndexError::Sqlite(e) => write!(f, "SQLite error: {}", e),
            AggregateIndexError::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for AggregateIndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AggregateIndexError::Io(e) => Some(e),
            #[cfg(feature = "sqlite")]
            AggregateIndexError::Sqlite(e) => Some(e),
            AggregateIndexError::Other(_) => None,
        }
    }
}

impl From<std::io::Error> for AggregateIndexError {
    fn from(e: std::io::Error) -> Self {
        AggregateIndexError::Io(e)
    }
}

#[cfg(feature = "sqlite")]
impl From<rusqlite::Error> for AggregateIndexError {
    fn from(e: rusqlite::Error) -> Self {
        AggregateIndexError::Sqlite(e)
    }
}
