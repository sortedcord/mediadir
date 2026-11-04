//! Runtime configuration from environment variables (see docs/STORAGE.md).

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    /// The only watched directory: media dropped anywhere inside is ingested.
    pub ingest_dir: PathBuf,
    /// MediaDir's private content-addressed store.
    pub media_dir: PathBuf,
    /// Content-addressed image assets (`image` field values). Separate from MEDIA_DIR.
    pub asset_dir: PathBuf,
    /// Output width for transform requests that give neither `w` nor `h`.
    pub asset_default_width: u32,
    /// Seconds between unreferenced-asset GC runs, and the minimum age before an asset is collected.
    pub asset_gc_interval: Option<Duration>,
    pub asset_gc_grace: Duration,
    /// Database, thumbnails, frame cache, backups.
    pub data_dir: PathBuf,
    /// Poll interval in addition to inotify (`None` = inotify only).
    pub poll: Option<Duration>,
    /// A file must be unchanged this long before it is ingested.
    pub settle: Duration,
    /// Periodic task intervals (`None` = disabled).
    pub sidecar_interval: Option<Duration>,
    pub verify_interval: Option<Duration>,
    pub backup_interval: Option<Duration>,
    pub backup_keep: usize,
    /// Background full-hash rate for referenced files, bytes/second (`0` = unlimited).
    pub lazy_hash_rate: u64,
    /// Library snapshot cadence (`None` = only the one at startup).
    pub analytics_snapshot: Option<Duration>,
    /// Raw request rows are deleted after this many days (hourly aggregates are kept forever).
    pub analytics_raw_days: u64,
    /// Seconds of /stream inactivity before a playback session closes.
    pub analytics_play_idle: u64,
    /// Seconds of request inactivity before a device session closes.
    pub analytics_device_idle: u64,
}

fn absolute(p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
    }
}

impl Config {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let secs = |k: &str, d: u64| -> u64 {
            env(k).map(|v| v.trim().parse().unwrap_or_else(|_| panic!("{k} must be a number of seconds"))).unwrap_or(d)
        };
        let every = |k: &str, d: u64| Some(secs(k, d)).filter(|s| *s > 0).map(Duration::from_secs);
        Config {
            bind: env("BIND").unwrap_or_else(|| "0.0.0.0:8096".into()),
            ingest_dir: absolute(PathBuf::from(env("INGEST_DIR").unwrap_or_else(|| "./ingest".into()))),
            media_dir: absolute(PathBuf::from(env("MEDIA_DIR").unwrap_or_else(|| "./store".into()))),
            asset_dir: absolute(PathBuf::from(env("ASSET_DIR").unwrap_or_else(|| "./assets".into()))),
            asset_default_width: secs("ASSET_DEFAULT_WIDTH", 1280).clamp(1, 20000) as u32,
            asset_gc_interval: every("ASSET_GC_INTERVAL", 3600),
            asset_gc_grace: Duration::from_secs(secs("ASSET_GC_GRACE_SECS", 86400)),
            data_dir: absolute(PathBuf::from(env("DATA_DIR").unwrap_or_else(|| "./data".into()))),
            poll: every("INGEST_POLL_SECS", 60),
            settle: Duration::from_secs(secs("INGEST_SETTLE_SECS", 10)),
            sidecar_interval: every("SIDECAR_INTERVAL", 3600),
            verify_interval: every("VERIFY_INTERVAL", 86400),
            backup_interval: every("BACKUP_INTERVAL", 21600),
            backup_keep: secs("BACKUP_KEEP", 14).max(1) as usize,
            lazy_hash_rate: secs("LAZY_HASH_MB_S", 64) * 1024 * 1024,
            analytics_snapshot: every("ANALYTICS_SNAPSHOT_SECS", 3600),
            analytics_raw_days: secs("ANALYTICS_RAW_DAYS", 30),
            analytics_play_idle: secs("ANALYTICS_PLAYBACK_IDLE_SECS", 300).max(1),
            analytics_device_idle: secs("ANALYTICS_DEVICE_IDLE_SECS", 1800).max(1),
        }
    }

    /// The ingest dir and the store must be separate trees (the store is never watched).
    pub fn validate(&self) -> Result<(), String> {
        let (i, m) = (&self.ingest_dir, &self.media_dir);
        if i.starts_with(m) || m.starts_with(i) {
            return Err(format!("INGEST_DIR ({}) and MEDIA_DIR ({}) must not contain each other", i.display(), m.display()));
        }
        let a = &self.asset_dir;
        for (name, d) in [("INGEST_DIR", i), ("MEDIA_DIR", m)] {
            if a.starts_with(d) || d.starts_with(a) {
                return Err(format!("ASSET_DIR ({}) and {name} ({}) must not contain each other", a.display(), d.display()));
            }
        }
        Ok(())
    }

    /// INGEST_DIR and MEDIA_DIR must be on one filesystem: placement is a single rename(2).
    pub fn check_same_fs(&self) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        // a directory that does not exist yet will be created on its nearest existing ancestor's filesystem
        let dev = |d: &PathBuf| -> Result<u64, String> {
            let existing = d.ancestors().find(|a| a.exists()).ok_or_else(|| format!("{} has no existing parent", d.display()))?;
            std::fs::metadata(existing).map(|m| m.dev()).map_err(|e| format!("cannot stat {}: {e}", existing.display()))
        };
        let (i, m) = (dev(&self.ingest_dir)?, dev(&self.media_dir)?);
        if i != m {
            return Err(format!(
                "INGEST_DIR ({}) and MEDIA_DIR ({}) are on different filesystems; ingest moves entries into the store with rename(2), so they must share one filesystem",
                self.ingest_dir.display(),
                self.media_dir.display()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_dirs_rejected() {
        let mut c = Config::from_env();
        c.ingest_dir = "/a/ingest".into();
        c.media_dir = "/a/ingest/store".into();
        assert!(c.validate().is_err());
        c.media_dir = "/a/store".into();
        c.asset_dir = "/a/assets".into();
        assert!(c.validate().is_ok());
        c.asset_dir = "/a/store/assets".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn cross_filesystem_is_refused() {
        use std::os::unix::fs::MetadataExt;
        let a = std::env::temp_dir().join(format!("mediadir-cfg-a-{}", std::process::id()));
        let b = std::path::PathBuf::from("/dev/shm").join(format!("mediadir-cfg-b-{}", std::process::id()));
        std::fs::create_dir_all(&a).unwrap();
        let mut c = Config::from_env();
        c.ingest_dir = a.join("ingest");
        c.media_dir = a.join("store");
        assert!(c.check_same_fs().is_ok());
        if std::fs::create_dir_all(&b).is_ok() && std::fs::metadata(&b).unwrap().dev() != std::fs::metadata(&a).unwrap().dev() {
            c.media_dir = b.join("store");
            let e = c.check_same_fs().unwrap_err();
            assert!(e.contains("different filesystems"), "{e}");
        } else {
            eprintln!("skipped: /dev/shm is not a separate filesystem here");
        }
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
