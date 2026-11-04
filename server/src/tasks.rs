//! Durability tasks: sidecar manifests, integrity verification (media files and image assets),
//! database backups, unreferenced-asset garbage collection. They run as jobs (`sidecars`,
//! `verify`, `backup`, `asset_gc`; see src/jobs/handlers.rs) on their schedules.
//! Results are reported at `GET /api/ingest/status`.

use crate::assets::AssetService;
use crate::config::Config;
use crate::db::{now, now_ms, Db, Repo};
use crate::error::AppResult;
use crate::ingest::store::Store;
use crate::model::Item;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const WEEK: u64 = 7 * 86400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Problem {
    /// `missing`, `corrupt`, or `changed` (a symlinked file's size/mtime changed; re-fingerprinted)
    pub problem: String,
    pub file_id: String,
    pub item_id: String,
    pub hash: String,
    pub path: String,
    pub detail: String,
    pub detected_at: i64,
}

#[derive(Debug, Serialize)]
pub struct TaskStatus {
    pub last_sidecar_sync: Option<i64>,
    pub last_verify: Option<i64>,
    pub last_backup: Option<i64>,
    pub last_asset_gc: Option<i64>,
    pub problems: Vec<Problem>,
}

pub struct Maintenance {
    db: Db,
    store: Store,
    assets: Arc<AssetService>,
    analytics: Arc<crate::analytics::Analytics>,
    backups: PathBuf,
    cfg: Config,
}

/// Meta keys holding the last successful run of each task (unix seconds).
pub fn meta_key(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "sidecars" => "last_sidecar_sync",
        "verify" => "last_verify",
        "backup" => "last_backup",
        "asset_gc" => "last_asset_gc",
        _ => return None,
    })
}

/// How a long task is driven from a job: stop requests, checkpoints and progress.
pub struct Hooks<'a> {
    pub stop: &'a dyn Fn() -> bool,
    pub checkpoint: Option<Value>,
    pub save: &'a dyn Fn(Value),
    pub progress: &'a dyn Fn(u64, u64),
}


/// Where an interrupted verify run resumes: the rotating re-hash slices that are left, and what
/// the finished part of them found.
#[derive(Debug, Default, Serialize, Deserialize)]
struct VerifyCheckpoint {
    files_left: usize,
    assets_left: usize,
    rehashed: Vec<String>,
    problems: Vec<Problem>,
}

impl Maintenance {
    pub fn new(db: Db, store: Store, assets: Arc<AssetService>, analytics: Arc<crate::analytics::Analytics>, cfg: &Config) -> Arc<Self> {
        Arc::new(Maintenance { db, store, assets, analytics, backups: cfg.data_dir.join("backups"), cfg: cfg.clone() })
    }

    pub fn mark_done(&self, kind: &str) -> AppResult<()> {
        if let Some(k) = meta_key(kind) {
            let conn = self.db.conn()?;
            Repo::new(&conn).meta_set(k, &now().to_string())?;
        }
        Ok(())
    }

    pub fn asset_gc_grace(&self) -> Duration {
        self.cfg.asset_gc_grace
    }

    pub fn status(&self, r: &Repo) -> AppResult<TaskStatus> {
        let t = |k: &str| -> AppResult<Option<i64>> { Ok(r.meta_get(k)?.and_then(|v| v.parse().ok())) };
        Ok(TaskStatus {
            last_sidecar_sync: t("last_sidecar_sync")?,
            last_verify: t("last_verify")?,
            last_backup: t("last_backup")?,
            last_asset_gc: t("last_asset_gc")?,
            problems: r.meta_get("problems")?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default(),
        })
    }

    /// Write `<hash>.json` next to every stored file whose item (or an ancestor) changed since
    /// its last sidecar. Enough to rebuild titles and structure without the database.
    /// `(written, interrupted)`; progress is kept per file (`sidecar_at`), so a re-run continues.
    pub fn sync_sidecars_ctl(&self, stop: &dyn Fn() -> bool) -> AppResult<(usize, bool)> {
        let started = now_ms();
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let items = repo.all_items()?;
        let chain = |id: &str| -> Vec<&Item> {
            let mut out = vec![];
            let mut cur = items.get(id);
            while let Some(i) = cur {
                out.push(i);
                if out.len() > 6 {
                    break;
                }
                cur = i.parent_id.as_deref().and_then(|p| items.get(p));
            }
            out
        };
        let mut written = 0;
        for st in repo.all_file_states()? {
            if stop() {
                return Ok((written, true));
            }
            let f = &st.file;
            if f.is_external() {
                continue; // never write next to (or about) symlinked entries
            }
            let ch = chain(&f.item_id);
            let Some(item) = ch.first() else { continue };
            let changed = ch.iter().map(|i| i.updated_at).max().unwrap_or(0).max(f.added_at * 1000);
            if st.sidecar_at > 0 && changed <= st.sidecar_at {
                continue;
            }
            let parents: Vec<_> =
                ch[1..].iter().rev().map(|p| json!({ "id": p.id, "kind": p.kind, "title": p.title, "year": p.year, "index": p.index })).collect();
            let doc = json!({
                "hash": f.hash,
                "size": f.size,
                "original_name": f.original_name,
                "ingested_at": f.added_at,
                "label": f.label,
                "item": {
                    "id": item.id, "kind": item.kind, "title": item.title, "sort_title": item.sort_title,
                    "subtitle": item.subtitle, "year": item.year, "index": item.index,
                    "parent_index": item.parent_index, "index_end": item.index_end,
                    "overview": item.overview, "genres": item.genres, "fields": item.fields,
                    "parents": parents,
                },
            });
            let path = self.store.sidecar_path(&f.store_path);
            if let Err(e) = write_atomic(&path, &serde_json::to_vec_pretty(&doc)?) {
                eprintln!("sidecars: {}: {e}", path.display());
                continue;
            }
            repo.set_sidecar_at(&f.id, started)?;
            written += 1;
        }
        Ok((written, false))
    }

    /// The store entry of a file (a regular file, or a symlink to the referenced target).
    fn path_of(&self, f: &crate::model::MediaFile) -> PathBuf {
        self.store.abs(&f.store_path)
    }

    /// Every stored file must exist with the right size; a rotating slice is re-hashed so the
    /// whole store is re-hashed about once a week.
    /// The verify interval: the `verify` schedule's (it sets the re-hash slice size), else VERIFY_INTERVAL.
    fn verify_interval(&self, repo: &Repo) -> u64 {
        repo.find_schedule("verify").ok().flatten().and_then(|s| s.interval_secs).map(|i| i as u64)
            .or(self.cfg.verify_interval.map(|d| d.as_secs()))
            .unwrap_or(86400)
            .max(1)
    }

    /// [`Maintenance::verify`] as a job: `Ok(None)` = stopped; the checkpoint saved through
    /// `hooks.save` lets the next run finish the re-hash slices instead of starting over (the
    /// existence checks are cheap and simply run again).
    pub fn verify_ctl(&self, hooks: Hooks) -> AppResult<Option<Vec<Problem>>> {
        let cp: Option<VerifyCheckpoint> = hooks.checkpoint.clone().and_then(|v| serde_json::from_value(v).ok());
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let files = repo.all_file_states()?;
        let mut problems = vec![];
        let mut bad = HashSet::new();
        let problem = |kind: &str, f: &crate::model::MediaFile, detail: String| Problem {
            problem: kind.into(),
            file_id: f.id.clone(),
            item_id: f.item_id.clone(),
            hash: f.hash.clone(),
            path: self.path_of(f).to_string_lossy().to_string(),
            detail,
            detected_at: now(),
        };
        for st in &files {
            let f = &st.file;
            if f.is_external() {
                // symlinked entry: the target's existence + size + mtime every run; never deleted or modified
                match std::fs::metadata(self.path_of(f)) {
                    Err(e) => problems.push(problem("missing", f, e.to_string())),
                    Ok(m) => {
                        use std::os::unix::fs::MetadataExt;
                        let mtime = m.mtime() * 1_000_000_000 + m.mtime_nsec();
                        if m.len() != f.size || mtime != f.src_mtime {
                            let detail = format!("size/mtime changed (size {} -> {}); re-fingerprinted, hash pending", f.size, m.len());
                            match crate::ingest::store::fingerprint(&self.path_of(f)) {
                                Ok((fp, size, mt)) => {
                                    if !repo.set_refingerprinted(&f.id, size, mt, &fp)? {
                                        problems.push(problem("changed", f, format!("{detail}; new content duplicates another file")));
                                    } else {
                                        problems.push(problem("changed", f, detail));
                                    }
                                }
                                Err(e) => problems.push(problem("missing", f, e.to_string())),
                            }
                        }
                        continue;
                    }
                }
                bad.insert(f.id.clone());
                continue;
            }
            match std::fs::metadata(self.path_of(f)) {
                Err(e) => problems.push(problem("missing", f, e.to_string())),
                Ok(m) if m.len() != f.size => problems.push(problem("corrupt", f, format!("size is {} bytes, expected {}", m.len(), f.size))),
                Ok(_) => continue,
            }
            bad.insert(f.id.clone());
        }
        let interval = self.verify_interval(&repo);
        let runs_per_week = WEEK.div_ceil(interval).max(1) as usize;
        let assets = repo.all_assets()?;
        let (slice, aslice) = match &cp {
            Some(c) => (c.files_left, c.assets_left),
            None => (files.len().div_ceil(runs_per_week).max(1), assets.len().div_ceil(runs_per_week).max(1)),
        };
        let mut rehashed: HashSet<String> = cp.as_ref().map(|c| c.rehashed.iter().cloned().collect()).unwrap_or_default();
        let mut found: Vec<Problem> = cp.map(|c| c.problems).unwrap_or_default();
        let total = (rehashed.len() + slice + aslice) as u64;
        let (mut files_left, mut assets_left) = (slice, aslice);
        let stopped = |rehashed: &HashSet<String>, found: &Vec<Problem>, files_left: usize, assets_left: usize| {
            let c = VerifyCheckpoint { files_left, assets_left, rehashed: rehashed.iter().cloned().collect(), problems: found.clone() };
            (hooks.save)(serde_json::to_value(&c).unwrap_or_default());
        };
        let hash_ctl = |p: &Path| crate::ingest::store::hash_file_ctl(p, 0, hooks.stop);
        let interrupted = |e: &std::io::Error| e.kind() == std::io::ErrorKind::Interrupted;
        (hooks.progress)(rehashed.len() as u64, total);
        for f in repo.files_due_for_rehash(slice as i64)? {
            if (hooks.stop)() {
                stopped(&rehashed, &found, files_left, assets_left);
                return Ok(None);
            }
            if !bad.contains(&f.id) {
                match hash_ctl(&self.path_of(&f)) {
                    Ok((h, _)) if h == f.hash => {}
                    Ok((h, _)) => found.push(problem("corrupt", &f, format!("content hash is {h}"))),
                    Err(e) if interrupted(&e) => {
                        stopped(&rehashed, &found, files_left, assets_left);
                        return Ok(None);
                    }
                    Err(e) => found.push(problem("missing", &f, e.to_string())),
                }
            }
            rehashed.insert(f.id.clone());
            repo.set_verified_at(&f.id, now_ms())?;
            files_left = files_left.saturating_sub(1);
            (hooks.progress)(rehashed.len() as u64, total);
        }
        // image assets: existence + size for all, a rotating slice re-hashed
        let aproblem = |kind: &str, a: &crate::db::Asset, detail: String| Problem {
            problem: kind.into(),
            file_id: format!("asset:{}", a.hash),
            item_id: String::new(),
            hash: a.hash.clone(),
            path: self.assets.path(a).to_string_lossy().to_string(),
            detail,
            detected_at: now(),
        };
        let mut abad = HashSet::new();
        for a in &assets {
            match std::fs::metadata(self.assets.path(a)) {
                Err(e) => problems.push(aproblem("missing", a, e.to_string())),
                Ok(m) if m.len() != a.size => problems.push(aproblem("corrupt", a, format!("size is {} bytes, expected {}", m.len(), a.size))),
                Ok(_) => continue,
            }
            abad.insert(a.hash.clone());
        }
        for a in repo.assets_due_for_rehash(aslice as i64)? {
            if (hooks.stop)() {
                stopped(&rehashed, &found, files_left, assets_left);
                return Ok(None);
            }
            if !abad.contains(&a.hash) {
                match hash_ctl(&self.assets.path(&a)) {
                    Ok((h, _)) if h == a.hash => {}
                    Ok((h, _)) => found.push(aproblem("corrupt", &a, format!("content hash is {h}"))),
                    Err(e) if interrupted(&e) => {
                        stopped(&rehashed, &found, files_left, assets_left);
                        return Ok(None);
                    }
                    Err(e) => found.push(aproblem("missing", &a, e.to_string())),
                }
            }
            rehashed.insert(format!("asset:{}", a.hash));
            repo.set_asset_verified(&a.hash)?;
            assets_left = assets_left.saturating_sub(1);
            (hooks.progress)(rehashed.len() as u64, total);
        }
        // re-hash findings of this run (including a resumed part) count unless the existence check already reported that file
        let reported: HashSet<String> = problems.iter().map(|p| p.file_id.clone()).collect();
        problems.extend(found.into_iter().filter(|p| !reported.contains(&p.file_id)));
        // hash mismatches found by earlier runs stay reported until that file is re-hashed again
        let mut current: HashSet<String> = files.iter().map(|s| s.file.id.clone()).collect();
        current.extend(assets.iter().map(|a| format!("asset:{}", a.hash)));
        let previous: Vec<Problem> = repo.meta_get("problems")?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
        let reported: HashSet<String> = problems.iter().map(|p| p.file_id.clone()).collect();
        for p in previous {
            if p.problem == "corrupt" && current.contains(&p.file_id) && !rehashed.contains(&p.file_id) && !reported.contains(&p.file_id) {
                problems.push(p);
            }
        }
        for p in &problems {
            eprintln!("verify: {} {} ({}): {}", p.problem, p.path, p.item_id, p.detail);
        }
        repo.meta_set("problems", &serde_json::to_string(&problems)?)?;
        Ok(Some(problems))
    }

    /// Delete assets that no item/person field references and that are older than `grace`
    /// (so a freshly uploaded asset survives until it is attached). Returns how many went.
    pub fn gc_assets(&self, grace: Duration) -> AppResult<usize> {
        let conn = self.db.conn()?;
        let dead = Repo::new(&conn).gc_assets(now() - grace.as_secs() as i64)?;
        for a in &dead {
            self.assets.remove(a);
        }
        Ok(dead.len())
    }

    /// Online backup with `VACUUM INTO`, keeping the newest `BACKUP_KEEP`.
    pub fn backup(&self) -> AppResult<PathBuf> {
        std::fs::create_dir_all(&self.backups)?;
        // written as `.tmp` and renamed, so a run interrupted by a crash never leaves a partial backup
        let path = self.backups.join(format!("mediadir-{}.db", now()));
        if !path.exists() {
            let tmp = path.with_extension("db.tmp");
            let _ = std::fs::remove_file(&tmp);
            let conn = self.db.conn()?;
            conn.execute("VACUUM INTO ?1", [tmp.to_string_lossy()])?;
            std::fs::rename(&tmp, &path)?;
        }
        // analytics.db rides along (a small file; losing it loses only history)
        let apath = self.backups.join(format!("analytics-{}.db", now()));
        if !apath.exists() {
            let tmp = apath.with_extension("db.tmp");
            let _ = std::fs::remove_file(&tmp);
            match self.analytics.backup_to(&tmp) {
                Ok(()) => std::fs::rename(&tmp, &apath)?,
                Err(e) => eprintln!("backup: analytics.db: {e}"),
            }
        }
        if let Ok(rd) = std::fs::read_dir(&self.backups) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(".db.tmp") {
                    let _ = std::fs::remove_file(e.path()); // leftovers of a crashed run
                }
            }
        }
        for prefix in ["mediadir-", "analytics-"] {
            let mut all: Vec<PathBuf> = std::fs::read_dir(&self.backups)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().map(|n| n.to_string_lossy()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(".db")))
                .collect();
            all.sort();
            let extra = all.len().saturating_sub(self.cfg.backup_keep);
            for old in &all[..extra] {
                let _ = std::fs::remove_file(old);
            }
        }
        Ok(path)
    }
}

/// Atomically replace `path` with `bytes`.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
