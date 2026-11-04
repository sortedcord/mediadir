//! One-time startup migration of the old "referenced media" layout.
//!
//! Before the rename model, `EXTERNAL_ROOT` held the media and `media_files.store_path` of an
//! `external` row was relative to it, while the user's symlink stayed in INGEST_DIR. Now an external
//! file is a symlink *inside the store*. For every `external` row whose store entry is not a symlink
//! yet, create that symlink (absolute target = old root + old store_path; the root comes from the
//! `EXTERNAL_ROOT` env var if still set, else from the symlink left in INGEST_DIR), point `store_path`
//! at it, and remove the leftover ingest symlink. Idempotent; targets are never touched.

use super::store::Store;
use crate::db::Repo;
use crate::model::ext_of;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn migrate_old_external(repo: &Repo, store: &Store, _ingest_dir: &Path) {
    let old_root = std::env::var("EXTERNAL_ROOT").ok().filter(|v| !v.trim().is_empty()).map(|v| PathBuf::from(v.trim()));
    match migrate_with(repo, store, old_root.as_deref()) {
        Ok(0) => {}
        Ok(n) => println!("migrate: {n} referenced file(s) moved to the new layout (store symlinks)"),
        Err(e) => eprintln!("migrate: {e}"),
    }
}

pub(super) fn migrate_with(repo: &Repo, store: &Store, old_root: Option<&Path>) -> Result<usize, crate::error::AppError> {
    let mut migrated = 0;
    for st in repo.all_file_states()? {
        let f = st.file;
        if !f.is_external() {
            continue;
        }
        let entry = store.abs(&f.store_path);
        match fs::symlink_metadata(&entry) {
            Ok(m) if m.file_type().is_symlink() => continue, // already in the new layout
            Ok(_) => {
                eprintln!("migrate: {} exists in the store and is not a symlink; {} skipped", entry.display(), f.original_name);
                continue;
            }
            Err(_) => {}
        }
        // the symlink(s) the user dropped in INGEST_DIR for this file, if still there
        let links: Vec<PathBuf> = repo
            .ingest_sources_for_file(&f.id)?
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false))
            .collect();
        let from_root = old_root.map(|r| fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()).join(&f.store_path)).filter(|t| fs::symlink_metadata(t).is_ok());
        let from_link = links.iter().find_map(|l| fs::canonicalize(l).ok());
        let Some(target) = from_root.or(from_link) else {
            eprintln!(
                "migrate: cannot place {} ({}): set EXTERNAL_ROOT (old value) for one more start, or restore its symlink in INGEST_DIR",
                f.original_name, f.store_path
            );
            continue;
        };
        let rel = Store::rel_path(&f.hash, &ext_of(Path::new(&f.original_name)));
        let dst = store.abs(&rel);
        if let Err(e) = store.ensure_parent(&rel) {
            eprintln!("migrate: {}: {e}", dst.display());
            continue;
        }
        match fs::symlink_metadata(&dst) {
            Ok(m) if m.file_type().is_symlink() => {
                let _ = fs::remove_file(&dst); // a leftover of an interrupted earlier run
            }
            Ok(_) => {
                eprintln!("migrate: {} exists and is not a symlink; {} skipped", dst.display(), f.original_name);
                continue;
            }
            Err(_) => {}
        }
        if let Err(e) = std::os::unix::fs::symlink(&target, &dst) {
            eprintln!("migrate: cannot create {}: {e}", dst.display());
            continue;
        }
        if let Err(e) = repo.set_store_path(&f.id, &rel) {
            eprintln!("migrate: cannot update {}: {e}", f.original_name);
            let _ = fs::remove_file(&dst);
            continue;
        }
        for l in &links {
            match fs::remove_file(l) {
                Ok(()) => println!("migrate: removed ingest symlink {}", l.display()),
                Err(e) => eprintln!("migrate: cannot remove ingest symlink {}: {e}", l.display()),
            }
        }
        println!("migrate: {} -> store symlink {} -> {}", f.original_name, rel, target.display());
        migrated += 1;
    }
    Ok(migrated)
}
