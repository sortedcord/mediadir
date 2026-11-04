use super::{json_col, to_json, Repo};
use crate::error::{AppError, AppResult};
use crate::model::{sort_files, MediaFile};
use rusqlite::{params, OptionalExtension, Row};

const COLS: &str = "id, item_id, hash, store_path, original_name, label, container, mime, size, duration, \
    bitrate, video, audio, subtitles, added_at, location, hash_state, fingerprint, src_mtime";

fn row_to_file(r: &Row) -> rusqlite::Result<MediaFile> {
    let video: Option<String> = r.get(11)?;
    Ok(MediaFile {
        id: r.get(0)?,
        item_id: r.get(1)?,
        hash: r.get(2)?,
        store_path: r.get(3)?,
        original_name: r.get(4)?,
        label: r.get(5)?,
        container: r.get(6)?,
        mime: r.get(7)?,
        size: r.get::<_, i64>(8)? as u64,
        duration: r.get(9)?,
        bitrate: r.get::<_, Option<i64>>(10)?.map(|b| b as u64),
        video: video.and_then(|v| serde_json::from_str(&v).ok()),
        audio: json_col(r.get(12)?),
        subtitles: json_col(r.get(13)?),
        added_at: r.get(14)?,
        location: r.get(15)?,
        hash_state: r.get(16)?,
        fingerprint: r.get(17)?,
        src_mtime: r.get(18)?,
    })
}

/// A stored file with its sync bookkeeping (sidecar and integrity tasks).
pub struct FileState {
    pub file: MediaFile,
    pub sidecar_at: i64,
}

impl Repo<'_> {
    pub fn get_file(&self, id: &str) -> AppResult<MediaFile> {
        self.find_file(id)?.ok_or_else(|| AppError::not_found("file"))
    }

    pub fn find_file(&self, id: &str) -> AppResult<Option<MediaFile>> {
        Ok(self.conn.query_row(&format!("SELECT {COLS} FROM media_files WHERE id = ?1"), [id], row_to_file).optional()?)
    }

    pub fn find_file_by_hash(&self, hash: &str) -> AppResult<Option<MediaFile>> {
        Ok(self.conn.query_row(&format!("SELECT {COLS} FROM media_files WHERE hash = ?1"), [hash], row_to_file).optional()?)
    }

    /// Files of an item, ordered by height desc.
    pub fn files_for_item(&self, item_id: &str) -> AppResult<Vec<MediaFile>> {
        let mut st = self.conn.prepare(&format!("SELECT {COLS} FROM media_files WHERE item_id = ?1"))?;
        let mut rows = st.query_map([item_id], row_to_file)?.collect::<Result<Vec<_>, _>>()?;
        sort_files(&mut rows);
        Ok(rows)
    }

    pub fn insert_file(&self, f: &MediaFile) -> AppResult<()> {
        self.conn.execute(
            &format!("INSERT INTO media_files ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)"),
            params![
                f.id,
                f.item_id,
                f.hash,
                f.store_path,
                f.original_name,
                f.label,
                f.container,
                f.mime,
                f.size as i64,
                f.duration,
                f.bitrate.map(|b| b as i64),
                f.video.as_ref().map(to_json),
                to_json(&f.audio),
                to_json(&f.subtitles),
                f.added_at,
                f.location,
                f.hash_state,
                f.fingerprint,
                f.src_mtime,
            ],
        )?;
        Ok(())
    }

    pub fn delete_file_row(&self, id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM media_files WHERE id = ?1", [id])?;
        Ok(())
    }

    #[cfg(test)]
    pub fn file_count(&self) -> AppResult<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM media_files", [], |r| r.get(0))?)
    }

    pub fn all_file_states(&self) -> AppResult<Vec<FileState>> {
        let mut st = self.conn.prepare(&format!("SELECT {COLS}, sidecar_at FROM media_files ORDER BY id"))?;
        let rows = st
            .query_map([], |r| Ok(FileState { file: row_to_file(r)?, sidecar_at: r.get(19)? }))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_sidecar_at(&self, id: &str, at: i64) -> AppResult<()> {
        self.conn.execute("UPDATE media_files SET sidecar_at = ?2 WHERE id = ?1", params![id, at])?;
        Ok(())
    }

    /// The `limit` files whose content was re-hashed longest ago.
    pub fn files_due_for_rehash(&self, limit: i64) -> AppResult<Vec<MediaFile>> {
        let mut st = self.conn.prepare(&format!("SELECT {COLS} FROM media_files WHERE hash_state = 'full' ORDER BY verified_at, id LIMIT ?1"))?;
        let rows = st.query_map([limit], row_to_file)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_verified_at(&self, id: &str, at: i64) -> AppResult<()> {
        self.conn.execute("UPDATE media_files SET verified_at = ?2 WHERE id = ?1", params![id, at])?;
        Ok(())
    }

    /// Referenced files whose full hash is still unknown, oldest first.
    pub fn pending_hash_files(&self, limit: i64) -> AppResult<Vec<MediaFile>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {COLS} FROM media_files WHERE hash_state = 'pending' ORDER BY added_at, id LIMIT ?1"))?;
        let rows = st.query_map([limit], row_to_file)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn count_pending_hashes(&self) -> AppResult<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM media_files WHERE hash_state = 'pending'", [], |r| r.get(0))?)
    }

    /// Replace a pending `fp:` hash by the full hash. `Ok(false)` = another file already has that
    /// hash (unique violation): the caller resolves the duplicate. Also false when the row is no
    /// longer pending (deleted / re-fingerprinted meanwhile).
    pub fn set_full_hash(&self, id: &str, hash: &str, verified_at: i64) -> AppResult<bool> {
        match self.conn.execute(
            "UPDATE media_files SET hash = ?2, hash_state = 'full', verified_at = ?3 WHERE id = ?1 AND hash_state = 'pending'",
            params![id, hash, verified_at],
        ) {
            Ok(n) => Ok(n == 1),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// An external file changed on disk: new size/mtime/fingerprint, hash pending again.
    /// `Ok(false)` = the new fingerprint belongs to another file already.
    pub fn set_refingerprinted(&self, id: &str, size: u64, mtime: i64, fp: &str) -> AppResult<bool> {
        match self.conn.execute(
            "UPDATE media_files SET hash = ?2, hash_state = 'pending', fingerprint = ?3, size = ?4, src_mtime = ?5 WHERE id = ?1",
            params![id, format!("fp:{fp}"), fp, size as i64, mtime],
        ) {
            Ok(n) => Ok(n == 1),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The store entry was renamed (pending `fp:` path -> final hash path) or migrated.
    pub fn set_store_path(&self, id: &str, store_path: &str) -> AppResult<()> {
        self.conn.execute("UPDATE media_files SET store_path = ?2 WHERE id = ?1", params![id, store_path])?;
        Ok(())
    }

    /// Ingest-dir paths that produced this file (newest first), for the old-layout migration.
    pub fn ingest_sources_for_file(&self, file_id: &str) -> AppResult<Vec<String>> {
        let mut st = self.conn.prepare("SELECT source FROM ingests WHERE file_id = ?1 AND origin = 'ingest' AND state = 'done' ORDER BY ingested_at DESC")?;
        let rows = st.query_map([file_id], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Ingest records that produced `old_file` now describe a duplicate of `file`.
    pub fn repoint_ingests(&self, old_file: &str, item_id: &str, file_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE ingests SET state = 'duplicate', item_id = ?2, file_id = ?3, updated_at = ?4 WHERE file_id = ?1",
            params![old_file, item_id, file_id, super::now()],
        )?;
        Ok(())
    }
}
