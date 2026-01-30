use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::worker::{FileCache, FileInfo, FileStatus, ProcessedFile};

/// Open a connection to the database.
pub fn connect(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .context("failed to create database directory")?;
    }

    let conn = Connection::open(db_path).context("failed to open SQLite database")?;

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )?;

    Ok(conn)
}

/// Create the file table if it doesn't already exist.
pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS files (
            id        INTEGER PRIMARY KEY,
            src_path  TEXT NOT NULL UNIQUE,
            dst_path  TEXT NOT NULL,
            hash      TEXT NOT NULL,
            mtime     INTEGER NOT NULL,
            size      INTEGER NOT NULL,
            config    TEXT NOT NULL -- e.g. 'opus:192', for change detection
        );
        CREATE INDEX IF NOT EXISTS idx_hash ON files(hash);",
        // ^^^ index for rename detection (finding a hash regardless of path)
    )
    .context("failed to initialize database schema")?;

    Ok(())
}

/// Read the file table into an in-memory cache.
pub fn load_cache(conn: &Connection) -> Result<FileCache> {
    let count: i64 =
        conn.query_row("SELECT count(*) FROM files", [], |r| r.get(0))?;
    let mut cache = HashMap::with_capacity(count as usize);

    let mut stmt = conn
        .prepare("SELECT src_path, dst_path, hash, mtime, size, config FROM files")?;

    let iter = stmt.query_map([], |row| {
        let src_str: String = row.get(0)?;
        let dst_str: String = row.get(1)?;
        let hash = row.get(2)?;
        let mtime = row.get(3)?;
        let size: i64 = row.get(4)?;
        let config = row.get(5)?;
        Ok((
            PathBuf::from(src_str),
            FileInfo {
                dst: PathBuf::from(dst_str),
                hash,
                mtime,
                size: size as u64,
                config,
            },
        ))
    })?;

    for result in iter {
        let (path, entry) = result?;
        cache.insert(path, entry);
    }

    Ok(cache)
}

/// Batch upsert processed file records.
pub fn ingest_results(
    conn: &mut Connection,
    results: impl Iterator<Item = ProcessedFile>,
) -> Result<()> {
    const BATCH_SIZE: usize = 1000;
    let mut buf = Vec::with_capacity(BATCH_SIZE);

    for file in results {
        match file.status {
            FileStatus::PassedThrough
            | FileStatus::Transcoded
            | FileStatus::Reclaimed => buf.push(file),
            _ => {}
        }
        if buf.len() >= BATCH_SIZE {
            flush_batch(conn, &buf)?;
            buf.clear();
        }
    }
    if !buf.is_empty() {
        flush_batch(conn, &buf)?;
    }

    Ok(())
}

fn flush_batch(conn: &mut Connection, files: &[ProcessedFile]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO files (src_path, dst_path, hash, mtime, size, config)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(src_path) DO UPDATE SET
                dst_path = excluded.dst_path,
                hash = excluded.hash,
                mtime = excluded.mtime,
                size = excluded.size,
                config = excluded.config",
        )?;
        for file in files {
            stmt.execute(params![
                file.src.to_string_lossy(),
                file.info.dst.to_string_lossy(),
                file.info.hash,
                file.info.mtime,
                file.info.size as i64,
                file.info.config,
            ])?;
        }
    }
    tx.commit()?;

    Ok(())
}

/// Prune deleted files from the file table.
pub fn prune<'a>(
    conn: &mut Connection,
    to_delete: impl Iterator<Item = &'a PathBuf>,
) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare("DELETE FROM files WHERE src_path = ?")?;
        for path in to_delete {
            stmt.execute(params![path.to_string_lossy()])?;
        }
    }
    tx.commit()?;

    Ok(())
}
