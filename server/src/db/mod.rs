//! SQLite persistence: connection pool, schema and the typed [`Repo`] layer.

mod assets;
mod fields;
mod files;
mod ingest;
mod items;
mod people;
mod relations;
mod schema;

pub use assets::Asset;
pub use ingest::{IngestRecord, SeenKey};
pub use items::{ItemQuery, Removed};
pub use people::{default_sort_name, CreditDraft, PeopleQuery};
pub use relations::{new_relation_id, normalize_type, Member, Membership, Relation, RelationQuery};

use crate::error::{AppError, AppResult};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::path::Path;

pub type Pool = r2d2::Pool<SqliteConnectionManager>;

/// Cheaply clonable database handle. All work runs on the blocking thread pool.
#[derive(Clone)]
pub struct Db {
    pool: Pool,
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.pragma_update(None, "synchronous", "NORMAL")?;
            c.pragma_update(None, "foreign_keys", "ON")?;
            c.busy_timeout(std::time::Duration::from_secs(10))?;
            Ok(())
        });
        let pool = r2d2::Pool::builder().max_size(8).build(manager)?;
        let mut conn = pool.get()?;
        schema::migrate(&mut conn).map_err(AppError::Internal)?;
        schema::ensure_relations(&conn).map_err(AppError::Internal)?;
        schema::ensure_assets(&conn).map_err(AppError::Internal)?;
        schema::ensure_external(&conn).map_err(AppError::Internal)?;
        Ok(Db { pool })
    }

    /// Run `f` with a repository on a pooled connection, off the async runtime.
    pub async fn call<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&Repo) -> AppResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            f(&Repo::new(&conn))
        })
        .await?
    }

    /// Synchronous access for code already on a blocking thread (the ingest pipeline).
    pub fn conn(&self) -> AppResult<r2d2::PooledConnection<SqliteConnectionManager>> {
        Ok(self.pool.get()?)
    }
}

/// Typed queries over one connection (or transaction, via deref).
pub struct Repo<'c> {
    pub(crate) conn: &'c Connection,
}

impl<'c> Repo<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Repo { conn }
    }

    /// Run `f` inside a transaction.
    pub fn tx<R>(&self, f: impl FnOnce(&Repo) -> AppResult<R>) -> AppResult<R> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(r) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(r)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}

pub(crate) fn json_col<T: serde::de::DeserializeOwned + Default>(s: Option<String>) -> T {
    s.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Repo<'_> {
    pub fn meta_get(&self, key: &str) -> AppResult<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> AppResult<()> {
        self.conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", [key, value])?;
        Ok(())
    }
}
