//! Unified background jobs: a persistent queue (`jobs` table), a worker pool with per-type
//! concurrency limits and global priority order, cooperative cancel / pause / resume with
//! checkpoints, retries, schedules (interval or cron) and an event stream. See docs/API.md (Jobs)
//! and docs/STORAGE.md (Jobs and schedules).
//!
//! A job runs on its own OS thread (all work here is blocking file / SQLite I/O). Handlers call
//! [`JobCtx::check`] at safe points; when the job was cancelled or paused the handler returns
//! [`JobError::Interrupted`] and the manager records the new state. Long handlers save a
//! checkpoint ([`JobCtx::save_checkpoint`]) so a resumed job continues instead of restarting.

pub mod cron;
pub mod handlers;
pub mod store;

use crate::config::Config;
use crate::db::{now, Db, Repo};
use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Notify};

pub const STATES: &[&str] = &["queued", "running", "paused", "done", "failed", "cancelled"];
pub const FINISHED: &[&str] = &["done", "failed", "cancelled"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub params: Value,
    pub state: String,
    pub priority: i64,
    pub progress: Option<Progress>,
    pub attempts: i64,
    pub max_attempts: i64,
    pub error: Option<String>,
    pub result: Option<Value>,
    #[serde(skip)]
    pub checkpoint: Option<Value>,
    pub created_at: i64,
    pub run_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub parent_id: Option<String>,
    pub schedule_id: Option<String>,
}

impl Job {
    pub fn is_finished(&self) -> bool {
        FINISHED.contains(&self.state.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Schedule {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub params: Value,
    pub interval_secs: Option<i64>,
    pub cron: Option<String>,
    pub enabled: bool,
    pub next_run: Option<i64>,
    pub last_run: Option<i64>,
    pub last_job_id: Option<String>,
    /// Changed through the API: environment defaults no longer apply to it.
    #[serde(skip)]
    pub edited: bool,
}

impl Schedule {
    /// The next run strictly after `t` (cron wins over interval).
    pub fn next_after(&self, t: i64) -> Option<i64> {
        match (&self.cron, self.interval_secs) {
            (Some(c), _) => cron::Cron::parse(c).ok()?.next_after(t),
            (None, Some(i)) if i > 0 => Some(t + i),
            _ => None,
        }
    }
}

/// What happens to a job found `running` at startup (the previous process died mid-run).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Restart {
    Requeue,
    Fail,
}

#[derive(Debug)]
pub enum JobError {
    /// Cancelled or paused at a checkpoint.
    Interrupted,
    Failed(String),
}

impl From<AppError> for JobError {
    fn from(e: AppError) -> Self {
        JobError::Failed(e.to_string())
    }
}
impl From<std::io::Error> for JobError {
    fn from(e: std::io::Error) -> Self {
        JobError::Failed(e.to_string())
    }
}
impl From<String> for JobError {
    fn from(e: String) -> Self {
        JobError::Failed(e)
    }
}

pub type JobResult = Result<Value, JobError>;

/// One job type.
pub trait Handler: Send + Sync + 'static {
    fn run(&self, ctx: &JobCtx) -> JobResult;
    /// Validate `params` of a job created through the API.
    fn validate(&self, _params: &Value) -> Result<(), String> {
        Ok(())
    }
    fn on_restart(&self, _job: &Job) -> Restart {
        Restart::Requeue
    }
    /// Whether `POST /api/jobs/{id}/pause` may park a running job (it always may park a queued one).
    fn pausable_while_running(&self) -> bool {
        true
    }
    fn priority(&self) -> i64 {
        0
    }
    fn max_attempts(&self) -> i64 {
        1
    }
    /// A queued / paused job was cancelled before it ran (the handler never saw it).
    fn cancelled_before_start(&self, _job: &Job) {}
    /// Whether a due schedule should enqueue a job now (e.g. `hash`: only with pending files).
    fn has_work(&self, _params: &Value) -> bool {
        true
    }
}

#[derive(Default)]
pub struct Ctl {
    cancel: AtomicBool,
    pause: AtomicBool,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobEvent {
    /// `created | state | progress | deleted`
    pub event: &'static str,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub state: String,
    pub progress: Option<Progress>,
    pub error: Option<String>,
    pub ts: i64,
}

impl JobEvent {
    fn of(event: &'static str, j: &Job) -> Self {
        JobEvent { event, id: j.id.clone(), kind: j.kind.clone(), state: j.state.clone(), progress: j.progress.clone(), error: j.error.clone(), ts: now() }
    }
}

/// Handed to a running handler.
pub struct JobCtx {
    pub id: String,
    pub kind: String,
    pub params: Value,
    checkpoint: Option<Value>,
    ctl: Arc<Ctl>,
    jobs: Arc<Jobs>,
    last_progress: Mutex<Option<Instant>>,
}

impl JobCtx {
    /// Cancel or pause was requested.
    pub fn stopping(&self) -> bool {
        self.ctl.cancel.load(Ordering::Relaxed) || self.ctl.pause.load(Ordering::Relaxed)
    }

    pub fn cancelled(&self) -> bool {
        self.ctl.cancel.load(Ordering::Relaxed)
    }

    /// A safe point: `Err(Interrupted)` when the job should stop now.
    pub fn check(&self) -> Result<(), JobError> {
        if self.stopping() {
            Err(JobError::Interrupted)
        } else {
            Ok(())
        }
    }

    /// The checkpoint saved by an earlier (paused / interrupted) run of this job.
    pub fn checkpoint(&self) -> Option<&Value> {
        self.checkpoint.as_ref()
    }

    pub fn save_checkpoint(&self, v: Value) {
        if let Err(e) = self.jobs.db.conn().and_then(|c| Repo::new(&c).set_job_checkpoint(&self.id, Some(&v))) {
            eprintln!("jobs: {}: cannot save checkpoint: {e}", self.id);
        }
    }

    /// Persisted and broadcast at most twice a second (and always when done == total).
    pub fn progress(&self, done: u64, total: Option<u64>, unit: &str) {
        let mut last = self.last_progress.lock().unwrap();
        let final_step = total == Some(done);
        if !final_step && last.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
            return;
        }
        *last = Some(Instant::now());
        let p = Progress { done, total, unit: unit.into() };
        if let Ok(c) = self.jobs.db.conn() {
            let _ = Repo::new(&c).set_job_progress(&self.id, &p);
        }
        let _ = self.jobs.events.send(JobEvent {
            event: "progress",
            id: self.id.clone(),
            kind: self.kind.clone(),
            state: "running".into(),
            progress: Some(p),
            error: None,
            ts: now(),
        });
    }

    pub fn log(&self, level: &str, msg: impl AsRef<str>) {
        self.jobs.log(&self.id, level, msg.as_ref());
    }

    pub fn jobs(&self) -> &Arc<Jobs> {
        &self.jobs
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        println!("{}: {}", self.kind, msg.as_ref());
        self.log("info", msg);
    }

    pub fn warn(&self, msg: impl AsRef<str>) {
        eprintln!("{}: {}", self.kind, msg.as_ref());
        self.log("warn", msg);
    }
}

#[derive(Debug, Clone)]
pub struct JobsConfig {
    pub max_workers: usize,
    pub limits: HashMap<String, usize>,
    pub retention_days: u64,
    pub failed_retention_days: u64,
    pub log_keep: i64,
}

pub const DEFAULT_LIMITS: &[(&str, usize)] =
    &[("ingest", 2), ("hash", 1), ("verify", 1), ("backup", 1), ("sidecars", 1), ("asset_gc", 1), ("analytics_snapshot", 1), ("analytics_rollup", 1)];

impl JobsConfig {
    pub fn from_config(cfg: &Config) -> Result<Self, String> {
        let mut limits: HashMap<String, usize> = DEFAULT_LIMITS.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        for part in cfg.jobs_concurrency.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (k, v) = part.split_once('=').ok_or_else(|| format!("JOBS_CONCURRENCY: expected type=n, got {part:?}"))?;
            let n: usize = v.trim().parse().map_err(|_| format!("JOBS_CONCURRENCY: {v:?} is not a number"))?;
            limits.insert(k.trim().to_string(), n.max(1));
        }
        Ok(JobsConfig {
            max_workers: cfg.jobs_max_workers.max(1),
            limits,
            retention_days: cfg.jobs_retention_days,
            failed_retention_days: cfg.jobs_failed_retention_days.max(cfg.jobs_retention_days),
            log_keep: cfg.jobs_log_keep.max(1) as i64,
        })
    }

    pub fn limit(&self, kind: &str) -> usize {
        self.limits.get(kind).copied().unwrap_or(1)
    }
}

/// `POST /api/jobs` body, and the internal enqueue request.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewJob {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub params: Option<Value>,
    pub priority: Option<i64>,
    pub run_at: Option<i64>,
    #[serde(skip)]
    pub parent_id: Option<String>,
    #[serde(skip)]
    pub schedule_id: Option<String>,
}

impl NewJob {
    pub fn of(kind: &str, params: Value) -> Self {
        NewJob { kind: kind.into(), params: Some(params), ..Default::default() }
    }
}

/// Fields of `PUT /api/schedules/{id}`; `cron: ""` clears the cron expression.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulePatch {
    pub interval_secs: Option<i64>,
    pub cron: Option<String>,
    pub enabled: Option<bool>,
    pub params: Option<Value>,
}

/// A schedule seeded from the environment on startup.
pub struct ScheduleSeed {
    pub id: &'static str,
    pub kind: &'static str,
    /// `None` = disabled by its env var (`0`).
    pub interval: Option<Duration>,
    /// Interval used when the env var disabled it (so enabling it through the API has a cadence).
    pub fallback_secs: i64,
    /// First run when the schedule is created (defaults to now + interval).
    pub first_run: Option<i64>,
}

pub struct Jobs {
    pub db: Db,
    pub cfg: JobsConfig,
    handlers: RwLock<HashMap<String, Arc<dyn Handler>>>,
    /// job id -> (type, control)
    running: Mutex<HashMap<String, (String, Arc<Ctl>)>>,
    wake: Notify,
    sched_wake: Notify,
    pub events: broadcast::Sender<JobEvent>,
    counter: AtomicU64,
}

impl Jobs {
    pub fn new(db: Db, cfg: JobsConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        Arc::new(Jobs {
            db,
            cfg,
            handlers: RwLock::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            sched_wake: Notify::new(),
            events,
            counter: AtomicU64::new(0),
        })
    }

    pub fn register(&self, kind: &str, h: impl Handler) {
        self.handlers.write().unwrap().insert(kind.to_string(), Arc::new(h));
    }

    fn handler(&self, kind: &str) -> Option<Arc<dyn Handler>> {
        self.handlers.read().unwrap().get(kind).cloned()
    }

    pub fn types(&self) -> Vec<String> {
        let mut v: Vec<String> = self.handlers.read().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    fn new_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        crate::model::fnv1a(&format!("job\u{0}{nanos}\u{0}{n}\u{0}{}", std::process::id()))
    }

    fn emit(&self, event: &'static str, j: &Job) {
        let _ = self.events.send(JobEvent::of(event, j));
    }

    fn emit_id(&self, event: &'static str, id: &str) {
        if let Ok(Some(j)) = self.db.conn().and_then(|c| Repo::new(&c).find_job(id)) {
            self.emit(event, &j);
        }
    }

    pub fn log(&self, id: &str, level: &str, msg: &str) {
        if let Err(e) = self.db.conn().and_then(|c| Repo::new(&c).add_job_log(id, level, msg, self.cfg.log_keep)) {
            eprintln!("jobs: {id}: cannot write log: {e}");
        }
    }

    // ---------------------------------------------------------------- lifecycle

    /// Startup: jobs left `running` by the previous process are re-queued or failed, per type.
    pub fn recover(&self) -> AppResult<(usize, usize)> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let (mut requeued, mut failed) = (0, 0);
        for j in repo.jobs_in_state("running")? {
            let restart = self.handler(&j.kind).map(|h| h.on_restart(&j)).unwrap_or(Restart::Fail);
            match restart {
                Restart::Requeue => {
                    repo.requeue_job(&j.id, now(), None)?;
                    repo.add_job_log(&j.id, "warn", "interrupted by a server restart; re-queued", self.cfg.log_keep)?;
                    requeued += 1;
                }
                Restart::Fail => {
                    repo.transition_job(&j.id, &["running"], "failed", Some("interrupted (server restarted)"), None)?;
                    repo.add_job_log(&j.id, "error", "interrupted by a server restart; not restartable", self.cfg.log_keep)?;
                    failed += 1;
                }
            }
        }
        Ok((requeued, failed))
    }

    /// Start the dispatcher and the scheduler.
    pub fn start(self: &Arc<Self>) {
        match self.recover() {
            Ok((0, 0)) => {}
            Ok((r, f)) => println!("jobs: {r} interrupted job(s) re-queued, {f} failed"),
            Err(e) => eprintln!("jobs: recovery failed: {e}"),
        }
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let m = me.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || m.dispatch_once()).await.unwrap_or_else(|e| Err(AppError::Internal(e.to_string()))) {
                    eprintln!("jobs: dispatch: {e}");
                }
                tokio::select! {
                    _ = me.wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        });
        let me = self.clone();
        tokio::spawn(async move {
            let mut last_prune: Option<Instant> = None;
            loop {
                let m = me.clone();
                let prune = last_prune.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600));
                if prune {
                    last_prune = Some(Instant::now());
                }
                let res = tokio::task::spawn_blocking(move || -> AppResult<()> {
                    m.schedule_tick()?;
                    if prune {
                        let n = m.prune()?;
                        if n > 0 {
                            println!("jobs: pruned {n} finished job(s)");
                        }
                    }
                    Ok(())
                })
                .await;
                if let Ok(Err(e)) = res {
                    eprintln!("jobs: scheduler: {e}");
                }
                tokio::select! {
                    _ = me.sched_wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        });
    }

    /// Start every due job that fits the limits, in priority order.
    pub fn dispatch_once(self: &Arc<Self>) -> AppResult<usize> {
        let due = {
            let conn = self.db.conn()?;
            Repo::new(&conn).due_jobs(now(), 500)?
        };
        let mut started = 0;
        for job in due {
            let ctl = {
                let mut running = self.running.lock().unwrap();
                if running.len() >= self.cfg.max_workers {
                    break;
                }
                let of_type = running.values().filter(|(k, _)| *k == job.kind).count();
                if of_type >= self.cfg.limit(&job.kind) {
                    continue;
                }
                if self.handler(&job.kind).is_none() {
                    drop(running);
                    let conn = self.db.conn()?;
                    let repo = Repo::new(&conn);
                    repo.claim_job(&job.id)?;
                    repo.transition_job(&job.id, &["running"], "failed", Some(&format!("unknown job type {}", job.kind)), None)?;
                    self.emit_id("state", &job.id);
                    continue;
                }
                let conn = self.db.conn()?;
                if !Repo::new(&conn).claim_job(&job.id)? {
                    continue;
                }
                let ctl = Arc::new(Ctl::default());
                running.insert(job.id.clone(), (job.kind.clone(), ctl.clone()));
                ctl
            };
            started += 1;
            let me = self.clone();
            let name = format!("job-{}", job.kind);
            let id = job.id.clone();
            if let Err(e) = std::thread::Builder::new().name(name).spawn(move || me.execute(job, ctl)) {
                eprintln!("jobs: cannot start a worker thread: {e}");
                self.running.lock().unwrap().remove(&id);
                let conn = self.db.conn()?;
                Repo::new(&conn).requeue_job(&id, now() + 5, Some(&e.to_string()))?;
            }
        }
        Ok(started)
    }

    fn execute(self: Arc<Self>, job: Job, ctl: Arc<Ctl>) {
        let job = match self.db.conn().and_then(|c| Repo::new(&c).get_job(&job.id)) {
            Ok(j) => j,
            Err(_) => job,
        };
        self.emit("state", &job);
        let handler = self.handler(&job.kind).expect("checked at dispatch");
        let ctx = JobCtx {
            id: job.id.clone(),
            kind: job.kind.clone(),
            params: job.params.clone(),
            checkpoint: job.checkpoint.clone(),
            ctl: ctl.clone(),
            jobs: self.clone(),
            last_progress: Mutex::new(None),
        };
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.run(&ctx)));
        drop(ctx);
        let out = match out {
            Ok(r) => r,
            Err(p) => {
                let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                Err(JobError::Failed(format!("panicked: {msg}")))
            }
        };
        if let Err(e) = self.finalize(&job, &ctl, out) {
            eprintln!("jobs: {}: cannot record the outcome: {e}", job.id);
        }
        self.running.lock().unwrap().remove(&job.id);
        self.emit_id("state", &job.id);
        self.wake.notify_one();
    }

    fn finalize(&self, job: &Job, ctl: &Ctl, out: JobResult) -> AppResult<()> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let attempts = repo.find_job(&job.id)?.map(|j| j.attempts).unwrap_or(job.attempts);
        match out {
            Ok(result) => {
                repo.transition_job(&job.id, &["running"], "done", None, Some(&result))?;
                repo.set_job_checkpoint(&job.id, None)?;
            }
            Err(JobError::Interrupted) if ctl.cancel.load(Ordering::Relaxed) => {
                repo.transition_job(&job.id, &["running"], "cancelled", Some("cancelled"), None)?;
                repo.add_job_log(&job.id, "info", "cancelled", self.cfg.log_keep)?;
            }
            Err(JobError::Interrupted) if ctl.pause.load(Ordering::Relaxed) => {
                repo.transition_job(&job.id, &["running"], "paused", None, None)?;
                repo.add_job_log(&job.id, "info", "paused at a checkpoint", self.cfg.log_keep)?;
            }
            Err(JobError::Interrupted) => repo.requeue_job(&job.id, now(), None)?,
            Err(JobError::Failed(e)) => {
                repo.add_job_log(&job.id, "error", &e, self.cfg.log_keep)?;
                if attempts < job.max_attempts {
                    let backoff = (10i64 << attempts.clamp(0, 8)).min(3600);
                    repo.requeue_job(&job.id, now() + backoff, Some(&e))?;
                    repo.add_job_log(&job.id, "info", &format!("attempt {attempts}/{} failed; retrying in {backoff}s", job.max_attempts), self.cfg.log_keep)?;
                } else {
                    eprintln!("jobs: {} {} failed: {e}", job.kind, job.id);
                    repo.transition_job(&job.id, &["running"], "failed", Some(&e), None)?;
                }
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- operations

    pub fn enqueue(&self, req: NewJob) -> AppResult<Job> {
        let h = self.handler(&req.kind).ok_or_else(|| AppError::bad(format!("unknown job type {:?} (one of {})", req.kind, self.types().join(", "))))?;
        let params = req.params.unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Err(AppError::bad("params must be an object"));
        }
        h.validate(&params).map_err(AppError::bad)?;
        let t = now();
        let job = Job {
            id: self.new_id(),
            kind: req.kind,
            params,
            state: "queued".into(),
            priority: req.priority.unwrap_or_else(|| h.priority()),
            progress: None,
            attempts: 0,
            max_attempts: h.max_attempts(),
            error: None,
            result: None,
            checkpoint: None,
            created_at: t,
            run_at: req.run_at.unwrap_or(t),
            started_at: None,
            finished_at: None,
            parent_id: req.parent_id,
            schedule_id: req.schedule_id,
        };
        let conn = self.db.conn()?;
        Repo::new(&conn).insert_job(&job)?;
        self.emit("created", &job);
        self.wake.notify_one();
        Ok(job)
    }

    /// Enqueue unless an unfinished job of this type with the same `params.<key>` (or, with no
    /// key, any unfinished job of the type) exists; returns the existing one then.
    pub fn enqueue_unique(&self, req: NewJob, key: Option<(&str, &str)>) -> AppResult<Job> {
        {
            let conn = self.db.conn()?;
            let repo = Repo::new(&conn);
            let existing = match key {
                Some((k, v)) => repo.active_job_with(&req.kind, k, v)?,
                None => repo.active_jobs_of(&req.kind)?.into_iter().find(|j| j.params.get("file_id").is_none()).map(|j| j.id),
            };
            if let Some(id) = existing {
                return repo.get_job(&id);
            }
        }
        self.enqueue(req)
    }

    #[cfg(test)]
    pub fn get(&self, id: &str) -> AppResult<Job> {
        let conn = self.db.conn()?;
        Repo::new(&conn).get_job(id)
    }

    fn control(&self, id: &str) -> Option<Arc<Ctl>> {
        self.running.lock().unwrap().get(id).map(|(_, c)| c.clone())
    }

    /// Queued / paused: cancelled now. Running: cancelled at its next checkpoint.
    pub fn cancel(&self, id: &str) -> AppResult<Job> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let job = repo.get_job(id)?;
        match job.state.as_str() {
            "queued" | "paused" => {
                if repo.transition_job(id, &["queued", "paused"], "cancelled", Some("cancelled"), None)? {
                    repo.add_job_log(id, "info", "cancelled before it ran", self.cfg.log_keep)?;
                    if let Some(h) = self.handler(&job.kind) {
                        h.cancelled_before_start(&job);
                    }
                } else if let Some(c) = self.control(id) {
                    c.cancel.store(true, Ordering::Relaxed); // it started meanwhile
                }
            }
            "running" => {
                if let Some(c) = self.control(id) {
                    c.cancel.store(true, Ordering::Relaxed);
                    repo.add_job_log(id, "info", "cancel requested", self.cfg.log_keep)?;
                }
            }
            s => return Err(AppError::Conflict(format!("job is {s}"))),
        }
        self.emit_id("state", id);
        repo.get_job(id)
    }

    /// Queued: parked (not picked up). Running: parked at its next checkpoint.
    pub fn pause(&self, id: &str) -> AppResult<Job> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let job = repo.get_job(id)?;
        match job.state.as_str() {
            "queued" => {
                if !repo.transition_job(id, &["queued"], "paused", None, None)? {
                    return Err(AppError::Conflict("job started meanwhile; pause it again".into()));
                }
            }
            "running" => {
                if !self.handler(&job.kind).is_some_and(|h| h.pausable_while_running()) {
                    return Err(AppError::Conflict(format!("{} jobs cannot be paused while running (cancel it instead)", job.kind)));
                }
                if let Some(c) = self.control(id) {
                    c.pause.store(true, Ordering::Relaxed);
                    repo.add_job_log(id, "info", "pause requested", self.cfg.log_keep)?;
                }
            }
            "paused" => {}
            s => return Err(AppError::Conflict(format!("job is {s}"))),
        }
        self.emit_id("state", id);
        repo.get_job(id)
    }

    pub fn resume(&self, id: &str) -> AppResult<Job> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let job = repo.get_job(id)?;
        match job.state.as_str() {
            "paused" => {
                repo.transition_job(id, &["paused"], "queued", None, None)?;
                repo.add_job_log(id, "info", "resumed", self.cfg.log_keep)?;
            }
            "running" => {
                if let Some(c) = self.control(id) {
                    c.pause.store(false, Ordering::Relaxed); // a pending pause request is withdrawn
                }
            }
            "queued" => {}
            s => return Err(AppError::Conflict(format!("job is {s}"))),
        }
        self.emit_id("state", id);
        self.wake.notify_one();
        repo.get_job(id)
    }

    /// Failed / cancelled: queued again from scratch (attempts reset, checkpoint dropped).
    pub fn retry(&self, id: &str) -> AppResult<Job> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let job = repo.get_job(id)?;
        if !matches!(job.state.as_str(), "failed" | "cancelled") {
            return Err(AppError::Conflict(format!("only failed or cancelled jobs can be retried (this one is {})", job.state)));
        }
        if let Some(h) = self.handler(&job.kind) {
            h.validate(&job.params).map_err(AppError::bad)?;
        }
        repo.reset_job(id)?;
        repo.add_job_log(id, "info", "retry requested", self.cfg.log_keep)?;
        self.emit_id("state", id);
        self.wake.notify_one();
        repo.get_job(id)
    }

    pub fn delete(&self, id: &str) -> AppResult<()> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let job = repo.get_job(id)?;
        if !job.is_finished() {
            return Err(AppError::Conflict(format!("job is {}; cancel it first", job.state)));
        }
        repo.delete_job(id)?;
        let mut gone = job;
        gone.state = "deleted".into();
        self.emit("deleted", &gone);
        Ok(())
    }

    /// Wait until a job is finished (done / failed / cancelled).
    pub async fn wait(&self, id: &str) -> AppResult<Job> {
        let mut rx = self.events.subscribe();
        loop {
            let i = id.to_string();
            let db = self.db.clone();
            let j = db.call(move |r| r.get_job(&i)).await?;
            if j.is_finished() {
                return Ok(j);
            }
            let deadline = tokio::time::sleep(Duration::from_secs(1));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    ev = rx.recv() => match ev {
                        Ok(e) if e.id == id && FINISHED.contains(&e.state.as_str()) => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    },
                    _ = &mut deadline => break,
                }
            }
        }
    }

    pub fn stats(&self) -> AppResult<Value> {
        let counts = {
            let conn = self.db.conn()?;
            Repo::new(&conn).job_counts()?
        };
        let mut by_state: HashMap<String, i64> = STATES.iter().map(|s| (s.to_string(), 0)).collect();
        let mut by_type: serde_json::Map<String, Value> = serde_json::Map::new();
        for t in self.types() {
            by_type.insert(t, json!({}));
        }
        for (t, s, n) in counts {
            *by_state.entry(s.clone()).or_default() += n;
            let e = by_type.entry(t).or_insert_with(|| json!({}));
            e[s] = json!(n);
        }
        let running = self.running.lock().unwrap();
        let mut workers = serde_json::Map::new();
        for t in self.types() {
            let n = running.values().filter(|(k, _)| *k == t).count();
            workers.insert(t.clone(), json!({ "running": n, "limit": self.cfg.limit(&t) }));
        }
        let busy = running.len();
        Ok(json!({
            "by_state": by_state,
            "by_type": by_type,
            "workers": {
                "max": self.cfg.max_workers,
                "busy": busy,
                "utilization": busy as f64 / self.cfg.max_workers as f64,
                "by_type": workers,
            },
        }))
    }

    pub fn prune(&self) -> AppResult<usize> {
        let t = now();
        let conn = self.db.conn()?;
        Repo::new(&conn).prune_jobs(t - self.cfg.retention_days as i64 * 86400, t - self.cfg.failed_retention_days as i64 * 86400)
    }

    // ---------------------------------------------------------------- schedules

    /// Create missing schedules; schedules never edited through the API follow the environment.
    pub fn seed_schedules(&self, seeds: &[ScheduleSeed]) -> AppResult<()> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let t = now();
        for s in seeds {
            let env_secs = s.interval.map(|d| d.as_secs().max(1) as i64);
            match repo.find_schedule(s.id)? {
                None => {
                    let interval = env_secs.unwrap_or(s.fallback_secs);
                    repo.save_schedule(&Schedule {
                        id: s.id.into(),
                        kind: s.kind.into(),
                        params: json!({}),
                        interval_secs: Some(interval),
                        cron: None,
                        enabled: env_secs.is_some(),
                        next_run: Some(s.first_run.unwrap_or(t + interval)),
                        last_run: None,
                        last_job_id: None,
                        edited: false,
                    })?;
                }
                Some(mut cur) if !cur.edited => {
                    let interval = env_secs.unwrap_or(cur.interval_secs.unwrap_or(s.fallback_secs));
                    let enabled = env_secs.is_some();
                    if cur.interval_secs != Some(interval) || cur.enabled != enabled || cur.next_run.is_none() {
                        cur.interval_secs = Some(interval);
                        cur.cron = None;
                        cur.enabled = enabled;
                        let base = cur.last_run.unwrap_or(t);
                        cur.next_run = Some((base + interval).clamp(t, t + interval));
                        repo.save_schedule(&cur)?;
                    }
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    pub fn schedules(&self) -> AppResult<Vec<Schedule>> {
        let conn = self.db.conn()?;
        Repo::new(&conn).list_schedules()
    }

    pub fn update_schedule(&self, id: &str, p: SchedulePatch) -> AppResult<Schedule> {
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let mut s = repo.get_schedule(id)?;
        if let Some(i) = p.interval_secs {
            if i < 1 {
                return Err(AppError::bad("interval_secs must be >= 1"));
            }
            s.interval_secs = Some(i);
        }
        if let Some(c) = p.cron {
            let c = c.trim().to_string();
            if c.is_empty() {
                s.cron = None;
            } else {
                cron::Cron::parse(&c).map_err(AppError::bad)?;
                s.cron = Some(c);
            }
        }
        if let Some(params) = p.params {
            if !params.is_object() {
                return Err(AppError::bad("params must be an object"));
            }
            if let Some(h) = self.handler(&s.kind) {
                h.validate(&params).map_err(AppError::bad)?;
            }
            s.params = params;
        }
        if let Some(e) = p.enabled {
            s.enabled = e;
        }
        if s.cron.is_none() && s.interval_secs.is_none() {
            return Err(AppError::bad("a schedule needs interval_secs or cron"));
        }
        s.edited = true;
        s.next_run = s.next_after(now());
        repo.save_schedule(&s)?;
        self.sched_wake.notify_one();
        Ok(s)
    }

    fn active(&self, repo: &Repo, job_id: &Option<String>) -> AppResult<bool> {
        Ok(match job_id {
            Some(id) => repo.find_job(id)?.is_some_and(|j| !j.is_finished()),
            None => false,
        })
    }

    /// Run a schedule now (even when disabled). 409 while its previous job is unfinished.
    pub fn run_schedule(&self, id: &str) -> AppResult<Job> {
        let s = {
            let conn = self.db.conn()?;
            let repo = Repo::new(&conn);
            let s = repo.get_schedule(id)?;
            if self.active(&repo, &s.last_job_id)? {
                return Err(AppError::Conflict(format!("job {} of this schedule is still unfinished", s.last_job_id.unwrap_or_default())));
            }
            s
        };
        let job = self.enqueue(NewJob { kind: s.kind.clone(), params: Some(s.params.clone()), schedule_id: Some(s.id.clone()), ..Default::default() })?;
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let mut s = repo.get_schedule(id)?;
        s.last_run = Some(now());
        s.last_job_id = Some(job.id.clone());
        repo.save_schedule(&s)?;
        Ok(job)
    }

    /// Enqueue the jobs of due schedules (skipped while the previous one is unfinished).
    pub fn schedule_tick(&self) -> AppResult<usize> {
        let t = now();
        let due = {
            let conn = self.db.conn()?;
            Repo::new(&conn).due_schedules(t)?
        };
        let mut n = 0;
        for mut s in due {
            let busy = {
                let conn = self.db.conn()?;
                self.active(&Repo::new(&conn), &s.last_job_id)?
            };
            let work = self.handler(&s.kind).is_some_and(|h| h.has_work(&s.params));
            if !busy && work {
                match self.enqueue(NewJob { kind: s.kind.clone(), params: Some(s.params.clone()), schedule_id: Some(s.id.clone()), ..Default::default() }) {
                    Ok(j) => {
                        s.last_job_id = Some(j.id);
                        s.last_run = Some(t);
                        n += 1;
                    }
                    Err(e) => eprintln!("jobs: schedule {}: {e}", s.id),
                }
            }
            s.next_run = s.next_after(t);
            let conn = self.db.conn()?;
            Repo::new(&conn).save_schedule(&s)?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests;
