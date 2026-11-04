use super::*;
use std::sync::atomic::AtomicUsize;

fn setup(name: &str, max_workers: usize, limits: &[(&str, usize)]) -> (std::path::PathBuf, Arc<Jobs>) {
    let root = std::env::temp_dir().join(format!("mediadir-jobs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let db = Db::open(&root.join("t.db")).unwrap();
    let cfg = JobsConfig {
        max_workers,
        limits: limits.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        retention_days: 14,
        failed_retention_days: 30,
        log_keep: 5,
    };
    (root, Jobs::new(db, cfg))
}

/// Dispatch until nothing is queued (and due) or running.
fn drain(jobs: &Arc<Jobs>) {
    let t0 = Instant::now();
    loop {
        jobs.dispatch_once().unwrap();
        let busy = !jobs.running.lock().unwrap().is_empty();
        let queued = jobs.db.conn().unwrap().query_row("SELECT COUNT(*) FROM jobs WHERE state = 'queued' AND run_at <= ?1", [now()], |r| r.get::<_, i64>(0)).unwrap();
        if !busy && queued == 0 {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "jobs did not drain");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_state(jobs: &Arc<Jobs>, id: &str, state: &str) -> Job {
    let t0 = Instant::now();
    loop {
        jobs.dispatch_once().unwrap();
        let j = jobs.get(id).unwrap();
        if j.state == state && !jobs.running.lock().unwrap().contains_key(id) {
            return j;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "job {id} never became {state} (is {})", j.state);
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Record(Arc<Mutex<Vec<String>>>);
impl Handler for Record {
    fn run(&self, ctx: &JobCtx) -> JobResult {
        self.0.lock().unwrap().push(ctx.params["name"].as_str().unwrap().to_string());
        Ok(json!({}))
    }
}

#[test]
fn priority_then_age_order() {
    let (root, jobs) = setup("order", 1, &[]);
    let seen = Arc::new(Mutex::new(vec![]));
    jobs.register("rec", Record(seen.clone()));
    for (name, prio) in [("low", -1), ("a", 0), ("high", 5), ("b", 0), ("top", 9)] {
        jobs.enqueue(NewJob { priority: Some(prio), ..NewJob::of("rec", json!({ "name": name })) }).unwrap();
    }
    // a future run_at waits even with a high priority
    jobs.enqueue(NewJob { priority: Some(100), run_at: Some(now() + 3600), ..NewJob::of("rec", json!({ "name": "later" })) }).unwrap();
    drain(&jobs);
    assert_eq!(*seen.lock().unwrap(), vec!["top", "high", "a", "b", "low"]);
    std::fs::remove_dir_all(&root).unwrap();
}

struct Slow {
    now: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
}
impl Handler for Slow {
    fn run(&self, _: &JobCtx) -> JobResult {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(n, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        self.now.fetch_sub(1, Ordering::SeqCst);
        Ok(json!({}))
    }
}

#[test]
fn per_type_and_global_limits() {
    let (root, jobs) = setup("limits", 3, &[("slow", 2), ("other", 5)]);
    let (n1, m1, n2, m2) = (Arc::default(), Arc::<AtomicUsize>::default(), Arc::default(), Arc::<AtomicUsize>::default());
    jobs.register("slow", Slow { now: n1, max: m1.clone() });
    jobs.register("other", Slow { now: n2, max: m2.clone() });
    for _ in 0..6 {
        jobs.enqueue(NewJob::of("slow", json!({}))).unwrap();
        jobs.enqueue(NewJob::of("other", json!({}))).unwrap();
    }
    let (max_total, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
    let watcher = {
        let (j, mt, st) = (jobs.clone(), max_total.clone(), stop.clone());
        std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                mt.fetch_max(j.running.lock().unwrap().len(), Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };
    drain(&jobs);
    stop.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    assert_eq!(m1.load(Ordering::SeqCst), 2, "slow is limited to 2");
    assert!(max_total.load(Ordering::SeqCst) <= 3, "never more than JOBS_MAX_WORKERS");
    assert_eq!(max_total.load(Ordering::SeqCst), 3);
    let s = jobs.stats().unwrap();
    assert_eq!(s["by_state"]["done"], 12);
    assert_eq!(s["workers"]["by_type"]["slow"]["limit"], 2);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Counts to `params.n`, checkpointing every step; resumes from the checkpoint.
struct Counter;
impl Handler for Counter {
    fn run(&self, ctx: &JobCtx) -> JobResult {
        let n = ctx.params["n"].as_u64().unwrap();
        let start = ctx.checkpoint().and_then(|c| c["i"].as_u64()).unwrap_or(0);
        for i in start..n {
            ctx.check()?;
            std::thread::sleep(Duration::from_millis(10));
            ctx.save_checkpoint(json!({ "i": i + 1 }));
            ctx.progress(i + 1, Some(n), "steps");
        }
        ctx.log("info", "finished");
        Ok(json!({ "started_from": start }))
    }
}

#[test]
fn cancel_queued_and_running() {
    let (root, jobs) = setup("cancel", 4, &[]);
    jobs.register("count", Counter);
    let q = jobs.enqueue(NewJob { run_at: Some(now() + 3600), ..NewJob::of("count", json!({ "n": 5 })) }).unwrap();
    assert_eq!(jobs.cancel(&q.id).unwrap().state, "cancelled");
    assert!(matches!(jobs.cancel(&q.id), Err(AppError::Conflict(_))));
    let r = jobs.enqueue(NewJob::of("count", json!({ "n": 1000 }))).unwrap();
    jobs.dispatch_once().unwrap();
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(jobs.cancel(&r.id).unwrap().state, "running", "running jobs stop at their next checkpoint");
    let j = wait_state(&jobs, &r.id, "cancelled");
    assert!(j.finished_at.is_some() && j.progress.unwrap().done < 1000);
    // retry: from scratch
    let j = jobs.retry(&r.id).unwrap();
    assert_eq!((j.state.as_str(), j.attempts), ("queued", 0));
    jobs.cancel(&r.id).unwrap();
    // only finished jobs can be deleted
    let live = jobs.enqueue(NewJob { run_at: Some(now() + 3600), ..NewJob::of("count", json!({ "n": 1 })) }).unwrap();
    assert!(matches!(jobs.delete(&live.id), Err(AppError::Conflict(_))));
    jobs.delete(&q.id).unwrap();
    assert!(matches!(jobs.get(&q.id), Err(AppError::NotFound(_))));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn pause_parks_at_a_checkpoint_and_resume_continues() {
    let (root, jobs) = setup("pause", 4, &[]);
    jobs.register("count", Counter);
    let j = jobs.enqueue(NewJob::of("count", json!({ "n": 60 }))).unwrap();
    jobs.dispatch_once().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    jobs.pause(&j.id).unwrap();
    let p = wait_state(&jobs, &j.id, "paused");
    let at = p.checkpoint.as_ref().unwrap()["i"].as_u64().unwrap();
    assert!(at > 0 && at < 60, "checkpoint {at}");
    // paused jobs are not picked up
    jobs.dispatch_once().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(jobs.get(&j.id).unwrap().state, "paused");
    jobs.resume(&j.id).unwrap();
    let d = wait_state(&jobs, &j.id, "done");
    assert_eq!(d.result.unwrap()["started_from"].as_u64().unwrap(), at);
    assert!(d.checkpoint.is_none(), "checkpoint dropped when done");
    assert_eq!(d.progress.unwrap(), Progress { done: 60, total: Some(60), unit: "steps".into() });
    // a queued job pauses immediately; logs are bounded (log_keep = 5)
    let q = jobs.enqueue(NewJob { run_at: Some(now() + 3600), ..NewJob::of("count", json!({ "n": 1 })) }).unwrap();
    assert_eq!(jobs.pause(&q.id).unwrap().state, "paused");
    let logs = jobs.db.conn().map(|c| Repo::new(&c).job_logs(&j.id).unwrap()).unwrap();
    assert!(logs.len() <= 5 && logs.last().unwrap()["msg"] == "finished", "{logs:?}");
    std::fs::remove_dir_all(&root).unwrap();
}

struct Flaky(Restart);
impl Handler for Flaky {
    fn run(&self, _: &JobCtx) -> JobResult {
        Err(JobError::Failed("boom".into()))
    }
    fn max_attempts(&self) -> i64 {
        2
    }
    fn on_restart(&self, _: &Job) -> Restart {
        self.0
    }
}

#[test]
fn failures_retry_with_backoff_then_fail() {
    let (root, jobs) = setup("retry", 4, &[]);
    jobs.register("flaky", Flaky(Restart::Requeue));
    let j = jobs.enqueue(NewJob::of("flaky", json!({}))).unwrap();
    let q = wait_state(&jobs, &j.id, "queued");
    assert_eq!((q.attempts, q.error.as_deref()), (1, Some("boom")));
    assert!(q.run_at > now(), "backed off");
    jobs.db.conn().unwrap().execute("UPDATE jobs SET run_at = 0 WHERE id = ?1", [&j.id]).unwrap();
    let f = wait_state(&jobs, &j.id, "failed");
    assert_eq!(f.attempts, 2);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn restart_recovery_per_type() {
    let (root, jobs) = setup("recover", 4, &[]);
    jobs.register("again", Flaky(Restart::Requeue));
    jobs.register("once", Flaky(Restart::Fail));
    let a = jobs.enqueue(NewJob::of("again", json!({}))).unwrap();
    let b = jobs.enqueue(NewJob::of("once", json!({}))).unwrap();
    let c = jobs.enqueue(NewJob::of("gone", json!({}))).err();
    assert!(c.is_some(), "unknown types are rejected");
    {
        let conn = jobs.db.conn().unwrap();
        let r = Repo::new(&conn);
        assert!(r.claim_job(&a.id).unwrap() && r.claim_job(&b.id).unwrap());
    }
    // a new process over the same database
    let again = Jobs::new(jobs.db.clone(), jobs.cfg.clone());
    again.register("again", Flaky(Restart::Requeue));
    again.register("once", Flaky(Restart::Fail));
    assert_eq!(again.recover().unwrap(), (1, 1));
    assert_eq!(again.get(&a.id).unwrap().state, "queued");
    let fb = again.get(&b.id).unwrap();
    assert_eq!((fb.state.as_str(), fb.error.as_deref()), ("failed", Some("interrupted (server restarted)")));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn schedules_seed_edit_and_tick() {
    let (root, jobs) = setup("sched", 4, &[]);
    let seen = Arc::new(Mutex::new(vec![]));
    jobs.register("rec", Record(seen));
    let t = now();
    let seeds = |secs: Option<u64>| {
        vec![ScheduleSeed { id: "r", kind: "rec", interval: secs.map(Duration::from_secs), fallback_secs: 77, first_run: Some(t - 1) }]
    };
    jobs.seed_schedules(&seeds(Some(30))).unwrap();
    let s = &jobs.schedules().unwrap()[0];
    assert_eq!((s.interval_secs, s.enabled, s.next_run), (Some(30), true, Some(t - 1)));
    // env changes apply while the schedule was never edited
    jobs.seed_schedules(&seeds(None)).unwrap();
    let s = &jobs.schedules().unwrap()[0];
    assert_eq!((s.interval_secs, s.enabled), (Some(30), false));
    jobs.update_schedule("r", SchedulePatch { enabled: Some(true), params: Some(json!({ "name": "x" })), ..Default::default() }).unwrap();
    // due now: one job, not a second while it is unfinished
    jobs.db.conn().unwrap().execute("UPDATE schedules SET next_run = 0", []).unwrap();
    assert_eq!(jobs.schedule_tick().unwrap(), 1);
    let s = jobs.schedules().unwrap().remove(0);
    let first = s.last_job_id.clone().unwrap();
    assert!(s.next_run.unwrap() >= now() + 29 && s.next_run.unwrap() <= now() + 31, "{:?}", s.next_run);
    jobs.db.conn().unwrap().execute("UPDATE schedules SET next_run = 0", []).unwrap();
    assert_eq!(jobs.schedule_tick().unwrap(), 0, "previous job still queued");
    assert!(matches!(jobs.run_schedule("r"), Err(AppError::Conflict(_))));
    let j = jobs.get(&first).unwrap();
    assert_eq!((j.schedule_id.as_deref(), j.params["name"].as_str()), (Some("r"), Some("x")));
    drain(&jobs);
    let j2 = jobs.run_schedule("r").unwrap();
    assert_eq!(jobs.schedules().unwrap()[0].last_job_id.as_deref(), Some(j2.id.as_str()));
    // edited: env no longer applies; cron wins over the interval
    jobs.seed_schedules(&seeds(Some(5))).unwrap();
    assert_eq!(jobs.schedules().unwrap()[0].interval_secs, Some(30));
    let s = jobs.update_schedule("r", SchedulePatch { cron: Some("0 4 * * *".into()), ..Default::default() }).unwrap();
    let n = s.next_run.unwrap();
    assert_eq!(n.rem_euclid(86400), 4 * 3600);
    assert!(n > now() && n <= now() + 86400);
    assert!(jobs.update_schedule("r", SchedulePatch { cron: Some("nope".into()), ..Default::default() }).is_err());
    assert!(jobs.update_schedule("r", SchedulePatch { interval_secs: Some(0), ..Default::default() }).is_err());
    let s = jobs.update_schedule("r", SchedulePatch { cron: Some(String::new()), interval_secs: Some(10), ..Default::default() }).unwrap();
    assert!(s.cron.is_none() && (s.next_run.unwrap() - now() - 10).abs() <= 1);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn retention_prunes_finished_jobs_and_logs() {
    let (root, jobs) = setup("prune", 4, &[]);
    jobs.register("rec", Record(Arc::default()));
    let mk = |state: &str, age_days: i64| {
        let j = jobs.enqueue(NewJob { run_at: Some(now() + 3600), ..NewJob::of("rec", json!({ "name": "x" })) }).unwrap();
        let conn = jobs.db.conn().unwrap();
        conn.execute("UPDATE jobs SET state = ?2, finished_at = ?3 WHERE id = ?1", rusqlite::params![j.id, state, now() - age_days * 86400]).unwrap();
        jobs.log(&j.id, "info", "x");
        j.id
    };
    let old_done = mk("done", 20);
    let old_cancelled = mk("cancelled", 15);
    let new_done = mk("done", 1);
    let old_failed = mk("failed", 20);
    let ancient_failed = mk("failed", 40);
    let queued = jobs.enqueue(NewJob { run_at: Some(now() + 3600), ..NewJob::of("rec", json!({ "name": "x" })) }).unwrap().id;
    assert_eq!(jobs.prune().unwrap(), 3);
    let exists = |id: &str| jobs.get(id).is_ok();
    assert!(!exists(&old_done) && !exists(&old_cancelled) && !exists(&ancient_failed));
    assert!(exists(&new_done) && exists(&old_failed) && exists(&queued));
    let logs: i64 = jobs.db.conn().unwrap().query_row("SELECT COUNT(*) FROM job_logs", [], |r| r.get(0)).unwrap();
    assert_eq!(logs, 2);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn concurrency_env_parsing() {
    let mut cfg = Config::from_env();
    cfg.jobs_concurrency = "ingest=4, verify=2".into();
    let c = JobsConfig::from_config(&cfg).unwrap();
    assert_eq!((c.limit("ingest"), c.limit("verify"), c.limit("hash"), c.limit("unknown")), (4, 2, 1, 1));
    cfg.jobs_concurrency = "ingest".into();
    assert!(JobsConfig::from_config(&cfg).is_err());
}
