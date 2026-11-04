//! Analytics / observability: library snapshots, request log, playback and device sessions,
//! stored in a separate `DATA_DIR/analytics.db` (see docs/STORAGE.md). Data only, no UI.

pub mod middleware;
pub mod query;
pub mod snapshot;
pub mod store;
pub mod tracker;

use crate::config::Config;
use crate::db::{now, Db};
use crate::error::{AppError, AppResult};
use crate::ingest::Ingest;
use crate::tasks::Maintenance;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use store::{AnalyticsDb, Msg};

const CHANNEL_CAP: usize = 16_384;

pub struct Analytics {
    pub db: AnalyticsDb,
    tx: SyncSender<Msg>,
    /// Log events dropped because the writer could not keep up.
    pub dropped: AtomicU64,
    raw_days: u64,
    pub snapshot_paths: snapshot::Paths,
}

impl Analytics {
    pub fn start(cfg: &Config, main: Db) -> AppResult<Arc<Self>> {
        let db = AnalyticsDb::open(&cfg.data_dir.join("analytics.db"))?;
        let (tx, rx) = sync_channel(CHANNEL_CAP);
        let w = store::Writer::new(db.clone(), main, cfg.analytics_play_idle as i64, cfg.analytics_device_idle as i64);
        std::thread::Builder::new().name("analytics-writer".into()).spawn(move || store::run(rx, w)).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(Arc::new(Analytics {
            db,
            tx,
            dropped: AtomicU64::new(0),
            raw_days: cfg.analytics_raw_days,
            snapshot_paths: snapshot::Paths {
                data: cfg.data_dir.clone(),
                media: cfg.media_dir.clone(),
                assets: cfg.asset_dir.clone(),
            },
        }))
    }

    /// Fire-and-forget; returns whether the message was queued.
    pub fn send(&self, m: Msg) -> bool {
        match self.tx.try_send(m) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Delete raw request rows older than ANALYTICS_RAW_DAYS (the `analytics_rollup` job;
    /// hourly aggregates are written continuously by the writer and kept forever).
    pub fn rollup(&self) -> AppResult<usize> {
        store::prune_raw(&self.db, self.raw_days, now())
    }

    pub fn snapshot_now(&self, main: &Db, ingest: &Ingest, tasks: &Maintenance) -> AppResult<()> {
        let snap = snapshot::collect(main, ingest, tasks, &self.snapshot_paths)?;
        let conn = self.db.conn()?;
        conn.execute("INSERT OR REPLACE INTO snapshots (ts, data) VALUES (?1, ?2)", rusqlite::params![now(), snap.to_string()])?;
        Ok(())
    }

    /// Online copy of analytics.db for the backup task.
    pub fn backup_to(&self, path: &Path) -> AppResult<()> {
        let conn = self.db.conn()?;
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy()])?;
        Ok(())
    }
}
