//! Persistence of jobs, job logs and schedules (tables created in `db::schema::ensure_jobs`).

use super::{Job, Progress, Schedule};
use crate::db::{now, Repo};
use crate::error::{AppError, AppResult};
use rusqlite::{params, OptionalExtension, Row};
use serde_json::Value;

pub const JOB_COLS: &str = "id, type, params, state, priority, progress, attempts, max_attempts, error, result, checkpoint, \
     created_at, run_at, started_at, finished_at, parent_id, schedule_id";

fn json_opt(s: Option<String>) -> Option<Value> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

pub fn row_to_job(r: &Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get(0)?,
        kind: r.get(1)?,
        params: json_opt(r.get(2)?).unwrap_or(Value::Object(Default::default())),
        state: r.get(3)?,
        priority: r.get(4)?,
        progress: r.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<Progress>(&s).ok()),
        attempts: r.get(6)?,
        max_attempts: r.get(7)?,
        error: r.get(8)?,
        result: json_opt(r.get(9)?),
        checkpoint: json_opt(r.get(10)?),
        created_at: r.get(11)?,
        run_at: r.get(12)?,
        started_at: r.get(13)?,
        finished_at: r.get(14)?,
        parent_id: r.get(15)?,
        schedule_id: r.get(16)?,
    })
}

const SCHED_COLS: &str = "id, type, params, interval_secs, cron, enabled, next_run, last_run, last_job_id, edited";

fn row_to_schedule(r: &Row) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: r.get(0)?,
        kind: r.get(1)?,
        params: json_opt(r.get(2)?).unwrap_or(Value::Object(Default::default())),
        interval_secs: r.get(3)?,
        cron: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        next_run: r.get(6)?,
        last_run: r.get(7)?,
        last_job_id: r.get(8)?,
        edited: r.get::<_, i64>(9)? != 0,
    })
}

#[derive(Default)]
pub struct JobFilter {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

impl Repo<'_> {
    pub fn insert_job(&self, j: &Job) -> AppResult<()> {
        self.conn.execute(
            &format!("INSERT INTO jobs ({JOB_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)"),
            params![
                j.id,
                j.kind,
                j.params.to_string(),
                j.state,
                j.priority,
                j.progress.as_ref().map(|p| serde_json::to_string(p).unwrap_or_default()),
                j.attempts,
                j.max_attempts,
                j.error,
                j.result.as_ref().map(|v| v.to_string()),
                j.checkpoint.as_ref().map(|v| v.to_string()),
                j.created_at,
                j.run_at,
                j.started_at,
                j.finished_at,
                j.parent_id,
                j.schedule_id
            ],
        )?;
        Ok(())
    }

    pub fn find_job(&self, id: &str) -> AppResult<Option<Job>> {
        Ok(self.conn.query_row(&format!("SELECT {JOB_COLS} FROM jobs WHERE id = ?1"), [id], row_to_job).optional()?)
    }

    pub fn get_job(&self, id: &str) -> AppResult<Job> {
        self.find_job(id)?.ok_or_else(|| AppError::not_found(format!("job {id}")))
    }

    pub fn list_jobs(&self, f: &JobFilter) -> AppResult<(Vec<Job>, i64)> {
        let w = "(?1 IS NULL OR state = ?1) AND (?2 IS NULL OR type = ?2)";
        let total: i64 = self.conn.query_row(&format!("SELECT COUNT(*) FROM jobs WHERE {w}"), params![f.state, f.kind], |r| r.get(0))?;
        let mut st = self.conn.prepare(&format!(
            "SELECT {JOB_COLS} FROM jobs WHERE {w} ORDER BY created_at DESC, rowid DESC LIMIT ?3 OFFSET ?4"
        ))?;
        let rows = st.query_map(params![f.state, f.kind, f.limit, f.offset], row_to_job)?.collect::<Result<Vec<_>, _>>()?;
        Ok((rows, total))
    }

    /// Queued jobs that are due, in dispatch order (priority, then age).
    pub fn due_jobs(&self, at: i64, limit: i64) -> AppResult<Vec<Job>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {JOB_COLS} FROM jobs WHERE state = 'queued' AND run_at <= ?1 ORDER BY priority DESC, run_at, created_at, rowid LIMIT ?2"
        ))?;
        let rows = st.query_map(params![at, limit], row_to_job)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// queued -> running; false when someone else changed it first.
    pub fn claim_job(&self, id: &str) -> AppResult<bool> {
        Ok(self.conn.execute(
            "UPDATE jobs SET state = 'running', attempts = attempts + 1, started_at = ?2, finished_at = NULL WHERE id = ?1 AND state = 'queued'",
            params![id, now()],
        )? == 1)
    }

    /// Set state (and the matching timestamps / error / result) only when the job is in one of `from`.
    pub fn transition_job(&self, id: &str, from: &[&str], to: &str, error: Option<&str>, result: Option<&Value>) -> AppResult<bool> {
        let finished = matches!(to, "done" | "failed" | "cancelled");
        let list = from.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
        let n = self.conn.execute(
            &format!(
                "UPDATE jobs SET state = ?2, error = ?3, result = COALESCE(?4, result), finished_at = CASE WHEN ?5 THEN ?6 ELSE NULL END
                 WHERE id = ?1 AND state IN ({list})"
            ),
            params![id, to, error, result.map(|v| v.to_string()), finished, now()],
        )?;
        Ok(n == 1)
    }

    pub fn requeue_job(&self, id: &str, run_at: i64, error: Option<&str>) -> AppResult<()> {
        self.conn.execute("UPDATE jobs SET state = 'queued', run_at = ?2, error = ?3, finished_at = NULL WHERE id = ?1", params![id, run_at, error])?;
        Ok(())
    }

    /// Manual retry of a finished job: queued again from scratch.
    pub fn reset_job(&self, id: &str) -> AppResult<bool> {
        Ok(self.conn.execute(
            "UPDATE jobs SET state = 'queued', attempts = 0, error = NULL, result = NULL, checkpoint = NULL, progress = NULL,
             run_at = ?2, started_at = NULL, finished_at = NULL WHERE id = ?1 AND state IN ('failed', 'cancelled', 'done')",
            params![id, now()],
        )? == 1)
    }

    pub fn set_job_progress(&self, id: &str, p: &Progress) -> AppResult<()> {
        self.conn.execute("UPDATE jobs SET progress = ?2 WHERE id = ?1", params![id, serde_json::to_string(p)?])?;
        Ok(())
    }

    pub fn set_job_checkpoint(&self, id: &str, v: Option<&Value>) -> AppResult<()> {
        self.conn.execute("UPDATE jobs SET checkpoint = ?2 WHERE id = ?1", params![id, v.map(|v| v.to_string())])?;
        Ok(())
    }

    pub fn delete_job(&self, id: &str) -> AppResult<bool> {
        let n = self.conn.execute("DELETE FROM jobs WHERE id = ?1 AND state IN ('done', 'failed', 'cancelled')", [id])?;
        if n == 1 {
            self.conn.execute("DELETE FROM job_logs WHERE job_id = ?1", [id])?;
        }
        Ok(n == 1)
    }

    /// Append a log line, keeping only the newest `keep` lines of the job.
    pub fn add_job_log(&self, id: &str, level: &str, msg: &str, keep: i64) -> AppResult<()> {
        self.conn.execute("INSERT INTO job_logs (job_id, ts, level, msg) VALUES (?1, ?2, ?3, ?4)", params![id, now(), level, msg])?;
        self.conn.execute(
            "DELETE FROM job_logs WHERE job_id = ?1 AND id <= (SELECT id FROM job_logs WHERE job_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
            params![id, keep],
        )?;
        Ok(())
    }

    pub fn job_logs(&self, id: &str) -> AppResult<Vec<Value>> {
        let mut st = self.conn.prepare("SELECT ts, level, msg FROM job_logs WHERE job_id = ?1 ORDER BY id")?;
        let rows = st
            .query_map([id], |r| Ok(serde_json::json!({ "ts": r.get::<_, i64>(0)?, "level": r.get::<_, String>(1)?, "msg": r.get::<_, String>(2)? })))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn jobs_in_state(&self, state: &str) -> AppResult<Vec<Job>> {
        let mut st = self.conn.prepare(&format!("SELECT {JOB_COLS} FROM jobs WHERE state = ?1"))?;
        let rows = st.query_map([state], row_to_job)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Unfinished (queued / running / paused) jobs of a type whose `params.<key>` equals `value`.
    pub fn active_job_with(&self, kind: &str, key: &str, value: &str) -> AppResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM jobs WHERE type = ?1 AND state IN ('queued', 'running', 'paused') AND json_extract(params, '$.' || ?2) = ?3 LIMIT 1",
                params![kind, key, value],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn active_jobs_of(&self, kind: &str) -> AppResult<Vec<Job>> {
        let mut st = self.conn.prepare(&format!("SELECT {JOB_COLS} FROM jobs WHERE type = ?1 AND state IN ('queued', 'running', 'paused')"))?;
        let rows = st.query_map([kind], row_to_job)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn count_jobs(&self, kind: &str, states: &[&str]) -> AppResult<i64> {
        let list = states.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");
        Ok(self.conn.query_row(&format!("SELECT COUNT(*) FROM jobs WHERE type = ?1 AND state IN ({list})"), [kind], |r| r.get(0))?)
    }

    /// `[(type, state, count)]`
    pub fn job_counts(&self) -> AppResult<Vec<(String, String, i64)>> {
        let mut st = self.conn.prepare("SELECT type, state, COUNT(*) FROM jobs GROUP BY type, state ORDER BY type, state")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete finished jobs (and their logs): done/cancelled before `cutoff`, failed before `failed_cutoff`.
    pub fn prune_jobs(&self, cutoff: i64, failed_cutoff: i64) -> AppResult<usize> {
        let n = self.conn.execute(
            "DELETE FROM jobs WHERE (state IN ('done', 'cancelled') AND finished_at < ?1) OR (state = 'failed' AND finished_at < ?2)",
            params![cutoff, failed_cutoff],
        )?;
        self.conn.execute("DELETE FROM job_logs WHERE job_id NOT IN (SELECT id FROM jobs)", [])?;
        // ingest records keep their (now dangling) job_id; GET /api/jobs/{id} answers 404 for it
        Ok(n)
    }

    // ---- schedules

    pub fn list_schedules(&self) -> AppResult<Vec<Schedule>> {
        let mut st = self.conn.prepare(&format!("SELECT {SCHED_COLS} FROM schedules ORDER BY id"))?;
        let rows = st.query_map([], row_to_schedule)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn find_schedule(&self, id: &str) -> AppResult<Option<Schedule>> {
        Ok(self.conn.query_row(&format!("SELECT {SCHED_COLS} FROM schedules WHERE id = ?1"), [id], row_to_schedule).optional()?)
    }

    pub fn get_schedule(&self, id: &str) -> AppResult<Schedule> {
        self.find_schedule(id)?.ok_or_else(|| AppError::not_found(format!("schedule {id}")))
    }

    pub fn save_schedule(&self, s: &Schedule) -> AppResult<()> {
        self.conn.execute(
            &format!("INSERT OR REPLACE INTO schedules ({SCHED_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"),
            params![s.id, s.kind, s.params.to_string(), s.interval_secs, s.cron, s.enabled as i64, s.next_run, s.last_run, s.last_job_id, s.edited as i64],
        )?;
        Ok(())
    }

    pub fn due_schedules(&self, at: i64) -> AppResult<Vec<Schedule>> {
        let mut st = self.conn.prepare(&format!("SELECT {SCHED_COLS} FROM schedules WHERE enabled = 1 AND next_run IS NOT NULL AND next_run <= ?1"))?;
        let rows = st.query_map([at], row_to_schedule)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_ingest_job(&self, ingest_id: &str, job_id: &str) -> AppResult<()> {
        self.conn.execute("UPDATE ingests SET job_id = ?2 WHERE id = ?1", params![ingest_id, job_id])?;
        Ok(())
    }
}
