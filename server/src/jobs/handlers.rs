//! The job types of MediaDir and their default schedules.
//!
//! | type | params | restart (found `running` at startup) |
//! |---|---|---|
//! | `ingest` | `{path}` (ingest-dir entry) or `{upload, staging}` (internal) | path: re-queued (a fresh record; the interrupted one is failed) · upload: failed (staged bytes are wiped at startup) |
//! | `hash` | `{}` or `{file_id}` | re-queued (pending rows are the state) |
//! | `verify` | `{}` | re-queued, resumes from its checkpoint |
//! | `sidecars` | `{}` | re-queued (per-file `sidecar_at` is the state) |
//! | `backup` | `{}` | re-queued (backups are written as `.tmp` + rename) |
//! | `asset_gc` | `{grace_secs?}` | re-queued |
//! | `analytics_snapshot` | `{}` | re-queued |
//! | `analytics_rollup` | `{}` | re-queued |

use super::{Handler, JobCtx, JobError, JobResult, Jobs, Restart, ScheduleSeed};
use crate::analytics::Analytics;
use crate::config::Config;
use crate::db::{now, Db, Repo};
use crate::error::AppResult;
use crate::ingest::Ingest;
use crate::tasks::{Hooks, Maintenance};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Reject keys other than `allowed`.
pub fn only_keys(p: &Value, allowed: &[&str]) -> Result<(), String> {
    if let Some(o) = p.as_object() {
        for k in o.keys() {
            if !allowed.contains(&k.as_str()) {
                return Err(if allowed.is_empty() {
                    format!("this job type takes no params (got {k:?})")
                } else {
                    format!("unknown param {k:?} (allowed: {})", allowed.join(", "))
                });
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- ingest

pub struct IngestHandler(pub Arc<Ingest>);

impl IngestHandler {
    fn path(p: &Value) -> Option<PathBuf> {
        p.get("path").and_then(|v| v.as_str()).map(PathBuf::from)
    }
    fn upload(p: &Value) -> Option<(String, PathBuf)> {
        Some((p.get("upload")?.as_str()?.to_string(), PathBuf::from(p.get("staging")?.as_str()?)))
    }
}

impl Handler for IngestHandler {
    fn run(&self, ctx: &JobCtx) -> JobResult {
        let stop = || ctx.stopping();
        let rec = if let Some((rid, staging)) = Self::upload(&ctx.params) {
            Some(self.0.process_upload(&rid, &staging, &ctx.id, &stop)?)
        } else {
            let path = Self::path(&ctx.params).ok_or_else(|| JobError::Failed("params.path is missing".into()))?;
            self.0.process_path(&path, Some(&ctx.id), &stop)
        };
        let Some(rec) = rec else {
            ctx.info("nothing to do (entry gone or already handled)");
            return Ok(json!({ "record_id": null, "state": "skipped" }));
        };
        if rec.error.as_deref() == Some("cancelled") && ctx.cancelled() {
            return Err(JobError::Interrupted);
        }
        let msg = format!("{} -> {}{}", rec.name, rec.state, rec.error.as_deref().map(|e| format!(" ({e})")).unwrap_or_default());
        if rec.state == "failed" {
            ctx.warn(&msg);
        } else {
            ctx.log("info", &msg);
        }
        // the record carries the outcome; a failed ingest is a finished job (retry it through /api/ingest/{id}/retry)
        Ok(json!({ "record_id": rec.id, "state": rec.state, "error": rec.error, "item_id": rec.item_id, "file_id": rec.file_id }))
    }

    fn validate(&self, p: &Value) -> Result<(), String> {
        if let Some((_, staging)) = Self::upload(p) {
            only_keys(p, &["upload", "staging"])?;
            if !staging.starts_with(self.0.store.staging_dir()) {
                return Err("staging must be inside the store's staging directory".into());
            }
            return Ok(());
        }
        only_keys(p, &["path"])?;
        let path = Self::path(p).ok_or("params.path (an entry inside INGEST_DIR) is required")?;
        if !path.is_absolute() || !path.starts_with(&self.0.ingest_dir) || path.components().any(|c| c.as_os_str() == "..") {
            return Err(format!("path must be an absolute path inside INGEST_DIR ({})", self.0.ingest_dir.display()));
        }
        if std::fs::symlink_metadata(&path).is_err() {
            return Err(format!("{} does not exist", path.display()));
        }
        Ok(())
    }

    fn on_restart(&self, job: &super::Job) -> Restart {
        if Self::upload(&job.params).is_some() {
            Restart::Fail
        } else {
            Restart::Requeue
        }
    }

    fn pausable_while_running(&self) -> bool {
        false
    }

    fn priority(&self) -> i64 {
        10
    }

    fn cancelled_before_start(&self, job: &super::Job) {
        if let Some((rid, staging)) = Self::upload(&job.params) {
            self.0.cancel_upload(&rid, &staging);
        } else if let Some(p) = Self::path(&job.params) {
            self.0.cancel_path(&p, &job.id);
        }
    }
}

// ------------------------------------------------------------------- maintenance

pub struct TaskHandler {
    pub kind: &'static str,
    pub tasks: Arc<Maintenance>,
}

impl Handler for TaskHandler {
    fn run(&self, ctx: &JobCtx) -> JobResult {
        let t = &self.tasks;
        let stop = || ctx.stopping();
        let result = match self.kind {
            "sidecars" => {
                let (n, interrupted) = t.sync_sidecars_ctl(&stop)?;
                if interrupted {
                    return Err(JobError::Interrupted);
                }
                ctx.info(format!("wrote {n} sidecar(s)"));
                json!({ "written": n })
            }
            "verify" => {
                let save = |v: Value| ctx.save_checkpoint(v);
                let progress = |d: u64, total: u64| ctx.progress(d, Some(total), "files");
                if ctx.checkpoint().is_some() {
                    ctx.info("resuming from checkpoint");
                }
                let hooks = Hooks { stop: &stop, checkpoint: ctx.checkpoint().cloned(), save: &save, progress: &progress };
                let Some(problems) = t.verify_ctl(hooks)? else { return Err(JobError::Interrupted) };
                for p in &problems {
                    ctx.log("warn", format!("{} {} ({}): {}", p.problem, p.path, p.item_id, p.detail));
                }
                if problems.iter().any(|p| p.problem == "changed") {
                    // re-fingerprinted symlinked files are pending a full hash again
                    ctx.jobs().enqueue_unique(super::NewJob::of("hash", json!({})), None)?;
                }
                ctx.info(format!("{} problem(s)", problems.len()));
                json!({ "problems": problems.len() })
            }
            "backup" => {
                ctx.check()?;
                let p = t.backup()?;
                ctx.info(format!("wrote {}", p.display()));
                json!({ "path": p.to_string_lossy() })
            }
            "asset_gc" => {
                ctx.check()?;
                let grace = ctx.params.get("grace_secs").and_then(|v| v.as_u64()).map(Duration::from_secs).unwrap_or(t.asset_gc_grace());
                let n = t.gc_assets(grace)?;
                ctx.info(format!("removed {n} unreferenced asset(s)"));
                json!({ "removed": n })
            }
            k => return Err(JobError::Failed(format!("unknown task {k}"))),
        };
        t.mark_done(self.kind)?;
        Ok(result)
    }

    fn validate(&self, p: &Value) -> Result<(), String> {
        if self.kind == "asset_gc" {
            only_keys(p, &["grace_secs"])?;
            if p.get("grace_secs").is_some_and(|v| !v.is_u64()) {
                return Err("grace_secs must be a non-negative integer".into());
            }
            return Ok(());
        }
        only_keys(p, &[])
    }

    fn priority(&self) -> i64 {
        match self.kind {
            "backup" => 5,
            "sidecars" => 2,
            "asset_gc" => 1,
            _ => -5, // verify: long and least urgent
        }
    }
}

// ------------------------------------------------------------------- analytics

pub struct AnalyticsHandler {
    pub kind: &'static str,
    pub analytics: Arc<Analytics>,
    pub db: Db,
    pub ingest: Arc<Ingest>,
    pub tasks: Arc<Maintenance>,
}

impl Handler for AnalyticsHandler {
    fn run(&self, ctx: &JobCtx) -> JobResult {
        ctx.check()?;
        match self.kind {
            "analytics_snapshot" => {
                self.analytics.snapshot_now(&self.db, &self.ingest, &self.tasks)?;
                Ok(json!({ "ts": now() }))
            }
            _ => {
                let n = self.analytics.rollup()?;
                ctx.log("info", format!("pruned {n} raw request row(s)"));
                Ok(json!({ "pruned": n }))
            }
        }
    }

    fn validate(&self, p: &Value) -> Result<(), String> {
        only_keys(p, &[])
    }

    fn priority(&self) -> i64 {
        3
    }
}

// ------------------------------------------------------------------- wiring

pub fn register_all(jobs: &Jobs, db: &Db, ingest: &Arc<Ingest>, tasks: &Arc<Maintenance>, analytics: &Arc<Analytics>) {
    jobs.register("ingest", IngestHandler(ingest.clone()));
    jobs.register("hash", crate::ingest::HashHandler(ingest.clone()));
    for kind in ["sidecars", "verify", "backup", "asset_gc"] {
        jobs.register(kind, TaskHandler { kind, tasks: tasks.clone() });
    }
    for kind in ["analytics_snapshot", "analytics_rollup"] {
        jobs.register(kind, AnalyticsHandler { kind, analytics: analytics.clone(), db: db.clone(), ingest: ingest.clone(), tasks: tasks.clone() });
    }
}

/// Default schedules from the environment (the old timer variables). A task that ran before
/// keeps its cadence: its first run is `last run + interval`.
pub fn seeds(cfg: &Config, db: &Db) -> AppResult<Vec<ScheduleSeed>> {
    let conn = db.conn()?;
    let repo = Repo::new(&conn);
    let t = now();
    let cadence = |kind: &str, every: Option<Duration>| -> AppResult<Option<i64>> {
        let (Some(k), Some(every)) = (crate::tasks::meta_key(kind), every) else { return Ok(None) };
        let every = every.as_secs().max(1) as i64;
        Ok(repo.meta_get(k)?.and_then(|v| v.parse::<i64>().ok()).map(|last| (last + every).clamp(t + 1, t + every)))
    };
    Ok(vec![
        ScheduleSeed { id: "sidecars", kind: "sidecars", interval: cfg.sidecar_interval, fallback_secs: 3600, first_run: cadence("sidecars", cfg.sidecar_interval)? },
        ScheduleSeed { id: "verify", kind: "verify", interval: cfg.verify_interval, fallback_secs: 86400, first_run: cadence("verify", cfg.verify_interval)? },
        ScheduleSeed { id: "backup", kind: "backup", interval: cfg.backup_interval, fallback_secs: 21600, first_run: cadence("backup", cfg.backup_interval)? },
        ScheduleSeed { id: "asset_gc", kind: "asset_gc", interval: cfg.asset_gc_interval, fallback_secs: 3600, first_run: cadence("asset_gc", cfg.asset_gc_interval)? },
        ScheduleSeed { id: "hash", kind: "hash", interval: cfg.hash_interval, fallback_secs: 60, first_run: Some(t) },
        // a snapshot is also taken at every startup (a one-off job), so the schedule starts one interval later
        ScheduleSeed { id: "analytics_snapshot", kind: "analytics_snapshot", interval: cfg.analytics_snapshot, fallback_secs: 3600, first_run: None },
        ScheduleSeed { id: "analytics_rollup", kind: "analytics_rollup", interval: cfg.analytics_rollup, fallback_secs: 3600, first_run: Some(t) },
    ])
}
