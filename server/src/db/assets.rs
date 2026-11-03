//! Image assets: rows for the content-addressed files under ASSET_DIR.

use super::{now, now_ms, Repo};
use crate::error::AppResult;
use crate::model::image_refs;
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct Asset {
    pub hash: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    #[serde(skip)]
    pub created_at: i64,
}

const COLS: &str = "hash, mime, width, height, size, created_at";

fn row(r: &Row) -> rusqlite::Result<Asset> {
    Ok(Asset { hash: r.get(0)?, mime: r.get(1)?, width: r.get(2)?, height: r.get(3)?, size: r.get(4)?, created_at: r.get(5)? })
}

impl Repo<'_> {
    pub fn find_asset(&self, hash: &str) -> AppResult<Option<Asset>> {
        Ok(self.conn.query_row(&format!("SELECT {COLS} FROM assets WHERE hash = ?1"), [hash], row).optional()?)
    }

    pub fn asset_exists(&self, hash: &str) -> AppResult<bool> {
        Ok(self.find_asset(hash)?.is_some())
    }

    /// Insert the asset, or (when it exists) refresh `created_at` so the GC grace period restarts.
    /// Returns the stored row and whether it already existed.
    pub fn upsert_asset(&self, a: &Asset) -> AppResult<(Asset, bool)> {
        if let Some(mut old) = self.find_asset(&a.hash)? {
            old.created_at = now();
            self.conn.execute("UPDATE assets SET created_at = ?2 WHERE hash = ?1", params![old.hash, old.created_at])?;
            return Ok((old, true));
        }
        self.conn.execute(
            "INSERT INTO assets (hash, mime, width, height, size, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![a.hash, a.mime, a.width, a.height, a.size, now()],
        )?;
        Ok((self.find_asset(&a.hash)?.unwrap_or_else(|| a.clone()), false))
    }

    pub fn all_assets(&self) -> AppResult<Vec<Asset>> {
        let mut st = self.conn.prepare(&format!("SELECT {COLS} FROM assets ORDER BY hash"))?;
        let rows = st.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The `limit` assets verified longest ago.
    pub fn assets_due_for_rehash(&self, limit: i64) -> AppResult<Vec<Asset>> {
        let mut st = self.conn.prepare(&format!("SELECT {COLS} FROM assets ORDER BY verified_at, hash LIMIT ?1"))?;
        let rows = st.query_map([limit], row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_asset_verified(&self, hash: &str) -> AppResult<()> {
        self.conn.execute("UPDATE assets SET verified_at = ?2 WHERE hash = ?1", params![hash, now_ms()])?;
        Ok(())
    }

    /// Every asset hash referenced by an `image` field value of any item or person.
    pub fn referenced_assets(&self) -> AppResult<HashSet<String>> {
        let mut keys = vec![];
        {
            let mut st = self.conn.prepare("SELECT key FROM field_defs WHERE type = 'image'")?;
            for k in st.query_map([], |r| r.get::<_, String>(0))? {
                keys.push(k?);
            }
        }
        let mut out = HashSet::new();
        if keys.is_empty() {
            return Ok(out);
        }
        for table in ["items", "people"] {
            let mut st = self.conn.prepare(&format!("SELECT fields FROM {table}"))?;
            for f in st.query_map([], |r| r.get::<_, String>(0))? {
                let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&f?) else { continue };
                for k in &keys {
                    if let Some(refs) = m.get(k).and_then(|v| image_refs(v).ok()) {
                        out.extend(refs);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Delete assets older than `cutoff` (unix seconds) that no field references. Returns them.
    /// Runs in one write transaction so it cannot race an attach.
    pub fn gc_assets(&self, cutoff: i64) -> AppResult<Vec<Asset>> {
        self.tx(|r| {
            let referenced = r.referenced_assets()?;
            let mut st = r.conn.prepare(&format!("SELECT {COLS} FROM assets WHERE created_at < ?1"))?;
            let old = st.query_map([cutoff], row)?.collect::<Result<Vec<_>, _>>()?;
            let dead: Vec<Asset> = old.into_iter().filter(|a| !referenced.contains(&a.hash)).collect();
            for a in &dead {
                r.conn.execute("DELETE FROM assets WHERE hash = ?1", [&a.hash])?;
            }
            Ok(dead)
        })
    }
}
