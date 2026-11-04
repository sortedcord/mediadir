//! Background full hashing of symlinked (external) store entries.
//!
//! At ingest a referenced file only gets a quick fingerprint (`hash = "fp:<hex>"`,
//! `hash_state = pending`). This task, one file at a time and rate-limited (`LAZY_HASH_MB_S`),
//! computes the full BLAKE3 and swaps it into `hash`. If another file already has that hash the
//! row is a duplicate: its row (and its item, if that was its only file) is dropped from the
//! database, its store symlink is removed and its ingest records become `duplicate` of the existing
//! file. The target file itself is never touched. When the hash is known the store symlink is
//! renamed from its interim `fp/…` path to the hash-based path. See docs/STORAGE.md.

use super::{store, Ingest, Store};
use crate::model::ext_of;
use std::path::Path;
use crate::db::{now_ms, Repo};
use crate::error::AppResult;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const IDLE: Duration = Duration::from_secs(3);
const RETRY_AFTER: Duration = Duration::from_secs(600);

pub fn start(ing: Arc<Ingest>) {
    tokio::spawn(async move {
        let mut failed: HashMap<String, Instant> = HashMap::new();
        loop {
            let me = ing.clone();
            let skip: Vec<String> = failed.iter().filter(|(_, t)| t.elapsed() < RETRY_AFTER).map(|(k, _)| k.clone()).collect();
            failed.retain(|_, t| t.elapsed() < RETRY_AFTER);
            let res = tokio::task::spawn_blocking(move || me.hash_one_pending(&skip)).await;
            match res {
                Ok(Ok(Step::Idle)) => tokio::time::sleep(IDLE).await,
                Ok(Ok(Step::Done)) => {}
                Ok(Ok(Step::Failed(id))) => {
                    failed.insert(id, Instant::now());
                }
                Ok(Err(e)) => {
                    eprintln!("lazy hash: {e}");
                    tokio::time::sleep(IDLE).await;
                }
                Err(e) => {
                    eprintln!("lazy hash panicked: {e}");
                    tokio::time::sleep(IDLE).await;
                }
            }
        }
    });
}

pub(super) enum Step {
    Idle,
    Done,
    Failed(String),
}

impl Ingest {
    pub(super) fn hash_one_pending(&self, skip: &[String]) -> AppResult<Step> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let Some(f) = repo.pending_hash_files(skip.len() as i64 + 1)?.into_iter().find(|f| !skip.contains(&f.id)) else {
            return Ok(Step::Idle);
        };
        drop(conn);
        let path = match self.file_path(&f) {
            Ok(p) => p,
            Err(_) => return Ok(Step::Failed(f.id)),
        };
        // the file must still be the one that was fingerprinted
        let before = store::fingerprint(&path);
        match before {
            Ok((fp, size, mtime)) if Some(&fp) == f.fingerprint.as_ref() && size == f.size => {
                match store::hash_file_throttled(&path, self.lazy_rate) {
                    Ok((hash, n)) => {
                        let unchanged = matches!(store::fingerprint(&path), Ok((fp2, _, m2)) if fp2 == fp && m2 == mtime) && n == size;
                        if !unchanged {
                            return Ok(Step::Failed(f.id)); // changed while hashing; verify re-fingerprints it
                        }
                        self.finish_hash(&f.id, &hash)?;
                        Ok(Step::Done)
                    }
                    Err(e) => {
                        eprintln!("lazy hash: {}: {e}", path.display());
                        Ok(Step::Failed(f.id))
                    }
                }
            }
            // missing / changed: leave it for the integrity task, which reports and re-fingerprints
            _ => Ok(Step::Failed(f.id)),
        }
    }

    fn finish_hash(&self, id: &str, hash: &str) -> AppResult<()> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        repo.tx(|r| {
            if r.set_full_hash(id, hash, now_ms())? {
                println!("lazy hash: {id} -> {hash}");
                if let Some(f) = r.find_file(id)? {
                    let rel = Store::rel_path(hash, &ext_of(Path::new(&f.original_name)));
                    if rel != f.store_path && self.rename_entry(&f.store_path, &rel)? {
                        r.set_store_path(id, &rel)?;
                    }
                }
                return Ok(());
            }
            // unique violation: the same content is already catalogued
            let Some(existing) = r.find_file_by_hash(hash)? else { return Ok(()) }; // gone or no longer pending
            if existing.id == id {
                return Ok(());
            }
            let Some(mine) = r.find_file(id)? else { return Ok(()) };
            println!("lazy hash: {} duplicates file {} ({}); dropping the reference", mine.original_name, existing.id, existing.original_name);
            r.repoint_ingests(&mine.id, &existing.item_id, &existing.id)?;
            r.delete_file_row(&mine.id)?;
            if let Err(e) = self.store.remove_blob(&mine.store_path) {
                eprintln!("lazy hash: cannot remove store link {}: {e}", mine.store_path); // only the link; never the target
            }
            if mine.item_id != existing.item_id && r.files_for_item(&mine.item_id)?.is_empty() {
                r.remove_item_tree(&mine.item_id)?; // database rows only
            }
            Ok(())
        })
    }

    /// Rename a store entry (a symlink) to its final path. `Ok(false)` = the entry is missing
    /// (verify reports that), so the recorded path is kept.
    fn rename_entry(&self, from: &str, to: &str) -> AppResult<bool> {
        let (src, dst) = (self.store.abs(from), self.store.abs(to));
        if std::fs::symlink_metadata(&src).is_err() {
            eprintln!("lazy hash: store entry {} is missing; path not renamed", src.display());
            return Ok(false);
        }
        self.store.ensure_parent(to)?;
        if std::fs::symlink_metadata(&dst).is_ok() {
            std::fs::remove_file(&dst)?; // an orphan (not in the database)
        }
        std::fs::rename(&src, &dst)?;
        // drop the now-empty interim directory
        if let Some(d) = src.parent() {
            let _ = std::fs::remove_dir(d);
        }
        Ok(true)
    }
}
