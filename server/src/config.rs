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
            data_dir: absolute(PathBuf::from(env("DATA_DIR").unwrap_or_else(|| "./data".into()))),
            poll: every("INGEST_POLL_SECS", 60),
            settle: Duration::from_secs(secs("INGEST_SETTLE_SECS", 10)),
            sidecar_interval: every("SIDECAR_INTERVAL", 3600),
            verify_interval: every("VERIFY_INTERVAL", 86400),
            backup_interval: every("BACKUP_INTERVAL", 21600),
            backup_keep: secs("BACKUP_KEEP", 14).max(1) as usize,
        }
    }

    /// The ingest dir and the store must be separate trees (the store is never watched).
    pub fn validate(&self) -> Result<(), String> {
        let (i, m) = (&self.ingest_dir, &self.media_dir);
        if i.starts_with(m) || m.starts_with(i) {
            return Err(format!("INGEST_DIR ({}) and MEDIA_DIR ({}) must not contain each other", i.display(), m.display()));
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
        assert!(c.validate().is_ok());
    }
}
