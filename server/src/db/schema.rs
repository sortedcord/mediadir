//! The v3 schema. Fresh databases only: there is no migration from the v2 (library) schema.

use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = r#"
CREATE TABLE items (
    id TEXT PRIMARY KEY,
    library_id TEXT NOT NULL,
    kind TEXT NOT NULL, parent_id TEXT, title TEXT NOT NULL, sort_title TEXT, subtitle TEXT,
    year INTEGER, idx INTEGER, parent_index INTEGER, index_end INTEGER, overview TEXT,
    genres TEXT NOT NULL DEFAULT '[]', duration REAL,
    added_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    locked TEXT NOT NULL DEFAULT '[]', thumb TEXT,
    extra TEXT NOT NULL DEFAULT '{}', fields TEXT NOT NULL DEFAULT '{}',
    group_key TEXT
);
CREATE INDEX items_library_parent ON items(library_id, parent_id);
CREATE INDEX items_parent ON items(parent_id);
CREATE UNIQUE INDEX items_group ON items(group_key) WHERE group_key IS NOT NULL;

CREATE TABLE media_files (
    id TEXT PRIMARY KEY, item_id TEXT NOT NULL, hash TEXT NOT NULL UNIQUE, store_path TEXT NOT NULL,
    original_name TEXT NOT NULL, label TEXT NOT NULL DEFAULT '',
    container TEXT NOT NULL DEFAULT '', mime TEXT NOT NULL DEFAULT '', size INTEGER NOT NULL DEFAULT 0,
    duration REAL, bitrate INTEGER, video TEXT,
    audio TEXT NOT NULL DEFAULT '[]', subtitles TEXT NOT NULL DEFAULT '[]',
    added_at INTEGER NOT NULL,
    sidecar_at INTEGER NOT NULL DEFAULT 0,
    verified_at INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX media_files_item ON media_files(item_id);

CREATE TABLE people (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, sort_name TEXT, bio TEXT, birth_date TEXT,
    image TEXT, fields TEXT NOT NULL DEFAULT '{}', created_at INTEGER NOT NULL
);
CREATE TABLE credits (
    id TEXT PRIMARY KEY, item_id TEXT NOT NULL, person_id TEXT NOT NULL, role TEXT NOT NULL,
    character TEXT, ord INTEGER NOT NULL DEFAULT 0, source TEXT NOT NULL
);
CREATE INDEX credits_item ON credits(item_id);
CREATE INDEX credits_person ON credits(person_id);
CREATE TABLE field_defs (
    key TEXT PRIMARY KEY, label TEXT NOT NULL, type TEXT NOT NULL, applies_to TEXT NOT NULL DEFAULT '[]',
    display TEXT NOT NULL, builtin INTEGER NOT NULL DEFAULT 0, auto INTEGER NOT NULL DEFAULT 0
);
CREATE TRIGGER items_cleanup AFTER DELETE ON items BEGIN
    DELETE FROM credits WHERE item_id = old.id;
END;
CREATE TRIGGER people_cleanup AFTER DELETE ON people BEGIN
    DELETE FROM credits WHERE person_id = old.id;
END;

-- one row per file that entered the pipeline (ingest dir or upload)
CREATE TABLE ingests (
    id TEXT PRIMARY KEY, origin TEXT NOT NULL, source TEXT NOT NULL, name TEXT NOT NULL,
    size INTEGER, hash TEXT, state TEXT NOT NULL, error TEXT, item_id TEXT, file_id TEXT, placement TEXT,
    ingested_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX ingests_time ON ingests(ingested_at);

-- ingest-dir files already handled, by inode, so linked originals are never ingested twice
CREATE TABLE seen_files (
    dev INTEGER NOT NULL, ino INTEGER NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL,
    path TEXT NOT NULL, state TEXT NOT NULL, ingest_id TEXT, seen_at INTEGER NOT NULL,
    PRIMARY KEY (dev, ino)
);

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

pub fn migrate(conn: &mut Connection) -> Result<(), String> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tables: i64 = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version != 0 || tables > 0 {
        return Err(format!(
            "database has schema v{version}, this server needs a fresh v{SCHEMA_VERSION} database (point DATA_DIR at a new directory)"
        ));
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    println!("db: created schema v{SCHEMA_VERSION}");
    Ok(())
}
