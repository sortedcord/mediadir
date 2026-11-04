use super::{now, Repo};
use crate::error::{AppError, AppResult};
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

/// One file's trip through the ingest pipeline (`GET /api/ingest`).
#[derive(Debug, Clone, Serialize)]
pub struct IngestRecord {
    pub id: String,
    /// Where the file came from: its absolute path in the ingest dir, or `upload:<name>`.
    pub source: String,
    /// `ingest` (watched directory) or `upload`.
    pub origin: String,
    /// Original file name.
    pub name: String,
    pub size: Option<u64>,
    pub hash: Option<String>,
    /// `waiting | hashing | placing | probing | done | duplicate | failed | ignored`
    pub state: String,
    pub error: Option<String>,
    pub item_id: Option<String>,
    pub file_id: Option<String>,
    /// How the file entered the store: `reflink`, `hardlink` or `move`.
    pub placement: Option<String>,
    pub ingested_at: i64,
    pub updated_at: i64,
    /// The `ingest` job that processed it (`GET /api/jobs/{id}`); null for records from before jobs.
    pub job_id: Option<String>,
}

const COLS: &str = "id, source, origin, name, size, hash, state, error, item_id, file_id, placement, ingested_at, updated_at, job_id";

fn row_to_record(r: &Row) -> rusqlite::Result<IngestRecord> {
    Ok(IngestRecord {
        id: r.get(0)?,
        source: r.get(1)?,
        origin: r.get(2)?,
        name: r.get(3)?,
        size: r.get::<_, Option<i64>>(4)?.map(|s| s as u64),
        hash: r.get(5)?,
        state: r.get(6)?,
        error: r.get(7)?,
        item_id: r.get(8)?,
        file_id: r.get(9)?,
        placement: r.get(10)?,
        ingested_at: r.get(11)?,
        updated_at: r.get(12)?,
        job_id: r.get(13)?,
    })
}

/// Identity of a file in the ingest dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SeenKey {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    /// Nanoseconds since the epoch.
    pub mtime: i64,
}

impl Repo<'_> {
    pub fn save_ingest(&self, rec: &IngestRecord) -> AppResult<()> {
        self.conn.execute(
            &format!("INSERT OR REPLACE INTO ingests ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"),
            params![
                rec.id,
                rec.source,
                rec.origin,
                rec.name,
                rec.size.map(|s| s as i64),
                rec.hash,
                rec.state,
                rec.error,
                rec.item_id,
                rec.file_id,
                rec.placement,
                rec.ingested_at,
                now(),
                rec.job_id,
            ],
        )?;
        Ok(())
    }

    pub fn get_ingest(&self, id: &str) -> AppResult<IngestRecord> {
        self.conn
            .query_row(&format!("SELECT {COLS} FROM ingests WHERE id = ?1"), [id], row_to_record)
            .optional()?
            .ok_or_else(|| AppError::not_found("ingest record"))
    }

    /// Newest first.
    pub fn list_ingests(&self, state: Option<&str>, limit: i64) -> AppResult<Vec<IngestRecord>> {
        let limit = limit.clamp(1, 1000);
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLS} FROM ingests WHERE (?1 IS NULL OR state = ?1) ORDER BY ingested_at DESC, rowid DESC LIMIT ?2"
        ))?;
        let rows = st.query_map(params![state, limit], row_to_record)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Records left in a non-final state by a previous run are marked failed.
    pub fn fail_interrupted_ingests(&self) -> AppResult<usize> {
        Ok(self.conn.execute(
            "UPDATE ingests SET state = 'failed', error = 'interrupted (server restarted)', updated_at = ?1
             WHERE state IN ('waiting', 'hashing', 'placing', 'probing')",
            [now()],
        )?)
    }

    pub fn all_seen(&self) -> AppResult<Vec<SeenKey>> {
        let mut st = self.conn.prepare("SELECT dev, ino, size, mtime FROM seen_files")?;
        let rows = st
            .query_map([], |r| {
                Ok(SeenKey {
                    dev: r.get::<_, i64>(0)? as u64,
                    ino: r.get::<_, i64>(1)? as u64,
                    size: r.get::<_, i64>(2)? as u64,
                    mtime: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn mark_seen(&self, k: &SeenKey, path: &str, state: &str, ingest_id: &str) -> AppResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO seen_files (dev, ino, size, mtime, path, state, ingest_id, seen_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![k.dev as i64, k.ino as i64, k.size as i64, k.mtime, path, state, ingest_id, now()],
        )?;
        Ok(())
    }

    /// Forget one file (by inode) so the next poll processes it again.
    pub fn forget_seen(&self, dev: u64, ino: u64) -> AppResult<()> {
        self.conn.execute("DELETE FROM seen_files WHERE dev = ?1 AND ino = ?2", params![dev as i64, ino as i64])?;
        Ok(())
    }

    /// `(record id, source, file_id)` of finished ingest-dir records.
    pub fn done_ingest_sources(&self) -> AppResult<Vec<(String, String, Option<String>)>> {
        let mut st = self.conn.prepare("SELECT id, source, file_id FROM ingests WHERE origin = 'ingest' AND state = 'done'")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Forget failed files so the next poll retries them. Returns the forgotten keys.
    pub fn forget_failed_seen(&self) -> AppResult<Vec<(u64, u64)>> {
        let mut st = self.conn.prepare("SELECT dev, ino FROM seen_files WHERE state = 'failed'")?;
        let keys = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)))?
            .collect::<Result<Vec<_>, _>>()?;
        self.conn.execute("DELETE FROM seen_files WHERE state = 'failed'", [])?;
        Ok(keys)
    }
}
