use std::path::Path;

use rusqlite::{params, Connection};

use crate::backup::aggregate::{AggregateBlobMeta, AggregateRestoreInfo};

pub const SQLITE_INDEX_FILE_NAME: &str = "AGGREGATE_IDX.sqlite";

pub fn write_dir_index(path: &Path, blobs: &[AggregateBlobMeta]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let tmp = path.with_extension("tmp");
    let conn = Connection::open(&tmp).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=OFF;
         PRAGMA synchronous=OFF;
         CREATE TABLE entries (
             file_name TEXT PRIMARY KEY,
             blob_name TEXT NOT NULL,
             offset INTEGER NOT NULL,
             size INTEGER NOT NULL,
             mtime INTEGER NOT NULL,
             mode INTEGER NOT NULL,
             xattrs TEXT,
             acl TEXT
         );",
    )
    .map_err(|e| e.to_string())?;

    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT OR REPLACE INTO entries
                (file_name, blob_name, offset, size, mtime, mode, xattrs, acl)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(|e| e.to_string())?;
        for blob in blobs {
            let blob_name = Path::new(&blob.blob_path)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("invalid blob path {}", blob.blob_path))?;
            for entry in &blob.files {
                let file_name = Path::new(&entry.relative_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| format!("invalid relative path {}", entry.relative_path))?;
                stmt.execute(params![
                    file_name,
                    blob_name,
                    entry.offset as i64,
                    entry.size as i64,
                    entry.mtime as i64,
                    entry.mode as i64,
                    entry.xattrs,
                    entry.acl,
                ])
                .map_err(|e| e.to_string())?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    drop(conn);
    std::fs::rename(tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn read_dir_index(
    path: &Path,
    file_name: &str,
    blob_dir_rel: &str,
) -> Result<Option<AggregateRestoreInfo>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT blob_name, offset, size, mtime, mode, xattrs, acl
             FROM entries WHERE file_name = ?1",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query(params![file_name]).map_err(|e| e.to_string())?;
    if let Some(row) = rows.next().map_err(|e| e.to_string())? {
        Ok(Some(AggregateRestoreInfo {
            blob_path: Path::new(blob_dir_rel)
                .join(row.get::<_, String>(0).map_err(|e| e.to_string())?)
                .to_string_lossy()
                .replace('\\', "/"),
            offset: row.get::<_, i64>(1).map_err(|e| e.to_string())? as u64,
            size: row.get::<_, i64>(2).map_err(|e| e.to_string())? as u64,
            mtime: row.get::<_, i64>(3).map_err(|e| e.to_string())? as u64,
            mode: row.get::<_, i64>(4).map_err(|e| e.to_string())? as u32,
            xattrs: row.get(5).map_err(|e| e.to_string())?,
            acl: row.get(6).map_err(|e| e.to_string())?,
        }))
    } else {
        Ok(None)
    }
}
