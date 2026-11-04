//! Library snapshots: counts, sizes, ingest state and disk usage at a point in time.

use crate::db::Db;
use crate::error::AppResult;
use crate::ingest::Ingest;
use crate::tasks::Maintenance;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub struct Paths {
    pub data: PathBuf,
    pub media: PathBuf,
    pub assets: PathBuf,
}

fn grouped(c: &Connection, col: &str) -> AppResult<Value> {
    let mut st = c.prepare(&format!("SELECT {col}, COUNT(*), COALESCE(SUM(size),0) FROM media_files GROUP BY {col} ORDER BY {col}"))?;
    let mut m = Map::new();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let k: String = r.get(0)?;
        m.insert(if k.is_empty() { "unknown".into() } else { k }, json!({ "count": r.get::<_, i64>(1)?, "bytes": r.get::<_, i64>(2)? }));
    }
    Ok(Value::Object(m))
}

fn counts(c: &Connection, sql: &str) -> AppResult<Value> {
    let mut st = c.prepare(sql)?;
    let mut m = Map::new();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        m.insert(r.get::<_, String>(0)?, json!(r.get::<_, i64>(1)?));
    }
    Ok(Value::Object(m))
}

fn one(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

pub fn disk(path: &Path) -> Value {
    use std::os::unix::ffi::OsStrExt;
    let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return Value::Null };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(cpath.as_ptr(), &mut s) } != 0 {
        return json!({ "path": path.to_string_lossy(), "error": "statvfs failed" });
    }
    let fr = s.f_frsize as u64;
    let (total, free, avail) = (s.f_blocks as u64 * fr, s.f_bfree as u64 * fr, s.f_bavail as u64 * fr);
    json!({ "path": path.to_string_lossy(), "total_bytes": total, "free_bytes": free, "avail_bytes": avail, "used_bytes": total.saturating_sub(free) })
}

/// Current library state (blocking; runs on a blocking thread).
pub fn collect(db: &Db, ingest: &Ingest, tasks: &Maintenance, paths: &Paths) -> AppResult<Value> {
    let conn = db.conn()?;
    let c: &Connection = &conn;
    let items_by_kind = counts(c, "SELECT kind, COUNT(*) FROM items GROUP BY kind ORDER BY kind")?;
    let files_total = c.query_row("SELECT COUNT(*), COALESCE(SUM(size),0) FROM media_files", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let assets = c.query_row("SELECT COUNT(*), COALESCE(SUM(size),0) FROM assets", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let q = ingest.status();
    let problems = tasks.status(&crate::db::Repo::new(c))?.problems.len();
    let disks = json!({ "data": disk(&paths.data), "media": disk(&paths.media), "assets": disk(&paths.assets) });
    Ok(json!({
        "items": { "total": one(c, "SELECT COUNT(*) FROM items"), "by_kind": items_by_kind },
        "files": {
            "count": files_total.0, "bytes": files_total.1,
            "by_container": grouped(c, "container")?, "by_mime": grouped(c, "mime")?,
            "by_location": grouped(c, "location")?, "by_hash_state": grouped(c, "hash_state")?,
        },
        "assets": { "count": assets.0, "bytes": assets.1 },
        "people": one(c, "SELECT COUNT(*) FROM people"),
        "relations": { "count": one(c, "SELECT COUNT(*) FROM relations"), "members": one(c, "SELECT COUNT(*) FROM relation_members") },
        "field_defs": one(c, "SELECT COUNT(*) FROM field_defs"),
        "ingest": {
            "queue": q.queue, "settling": q.settling, "active": q.active, "problems": problems,
            "pending_hashes": one(c, "SELECT COUNT(*) FROM media_files WHERE hash_state = 'pending'"),
            "records_by_state": counts(c, "SELECT state, COUNT(*) FROM ingests GROUP BY state ORDER BY state")?,
        },
        "disks": disks,
    }))
}
