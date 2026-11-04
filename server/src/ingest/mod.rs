//! Ingest: the two ways media enters MediaDir (the watched INGEST_DIR and uploads) and the one
//! pipeline both use: settle → hash → probe → place → name→item.
//! See docs/STORAGE.md.

pub mod catalog;
pub mod classify;
pub mod filename;
pub mod probe;
pub mod store;
pub mod thumb;
mod lazy;
mod migrate;
mod watch;

use crate::config::Config;
use crate::db::{now, Db, IngestRecord, Repo, SeenKey};
use crate::error::{AppError, AppResult};
use crate::model::{ext_of, fnv1a, is_media_ext, MediaFile};
use catalog::StoredFile;
use classify::{ClassifyInput, Classifier, FilenameClassifier};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use store::Store;
use tokio::sync::{mpsc, Semaphore};

/// Files processed at the same time (hashing is I/O bound; keep it to 1-2).
const PIPELINE_SLOTS: usize = 2;

pub struct Ingest {
    db: Db,
    pub ingest_dir: PathBuf,
    pub store: Store,
    settle: Duration,
    poll: Option<Duration>,
    classifier: Box<dyn Classifier>,
    rt: Mutex<Runtime>,
    /// (dev, ino) -> (size, mtime) of ingest-dir files already handled.
    seen: Mutex<HashMap<(u64, u64), (u64, i64)>>,
    /// Hashes being placed right now (duplicate detection before their DB row exists).
    inflight: Mutex<HashSet<String>>,
    slots: Arc<Semaphore>,
    queue_tx: mpsc::UnboundedSender<PathBuf>,
    queue_rx: Mutex<Option<mpsc::UnboundedReceiver<PathBuf>>>,
    active: AtomicUsize,
    watching: AtomicBool,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    counter: AtomicU64,
    lazy_rate: u64,
}

#[derive(Default)]
struct Runtime {
    /// Waiting for size+mtime to settle.
    pending: HashMap<PathBuf, Pending>,
    /// Queued or being processed.
    queued: HashSet<PathBuf>,
}

struct Pending {
    size: u64,
    mtime: i64,
    since: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueStatus {
    /// inotify watch active (polling runs regardless, unless disabled).
    pub watching: bool,
    pub poll_secs: u64,
    pub settle_secs: u64,
    /// Files queued or being processed.
    pub queue: usize,
    /// Files waiting for their size/mtime to settle.
    pub settling: usize,
    pub active: usize,
}

/// Why a file name is not ingested, if it isn't.
pub fn ignore_reason(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if name.starts_with('.') {
        Some("hidden file")
    } else if [".part", ".crdownload", ".tmp"].iter().any(|s| lower.ends_with(s)) {
        Some("partial file")
    } else if !is_media_ext(&ext_of(Path::new(&lower))) {
        Some("not a video/audio file")
    } else {
        None
    }
}

pub(crate) fn stat_key(path: &Path) -> Option<SeenKey> {
    let ns = |m: &std::fs::Metadata| m.mtime() * 1_000_000_000 + m.mtime_nsec();
    let m = std::fs::symlink_metadata(path).ok()?;
    if m.file_type().is_symlink() {
        // a reference: identity is the link's inode, size/mtime are the target's so a target that
        // is still being copied settles, and a dangling link (size 0) is picked up once it resolves
        let (size, mtime) = match std::fs::metadata(path) {
            Ok(t) if t.is_file() => (t.len(), ns(&t)),
            _ => (0, ns(&m)),
        };
        return Some(SeenKey { dev: m.dev(), ino: m.ino(), size, mtime });
    }
    if !m.file_type().is_file() {
        return None;
    }
    Some(SeenKey { dev: m.dev(), ino: m.ino(), size: m.len(), mtime: ns(&m) })
}

/// A symlink in the ingest dir that resolves to a regular file: what the probe/fingerprint saw.
struct Reference {
    canon: PathBuf,
    fp: String,
    size: u64,
    mtime: i64,
}

/// Removes a hash from the in-flight set when placement + cataloguing is over.
struct Claim<'a> {
    set: &'a Mutex<HashSet<String>>,
    hash: String,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.hash);
    }
}

/// What a delete removed.
#[derive(Debug, Serialize)]
pub struct Deleted {
    pub items: usize,
    pub files: usize,
}

impl Ingest {
    pub fn new(db: Db, cfg: &Config) -> AppResult<Arc<Self>> {
        cfg.check_same_fs().map_err(AppError::Internal)?;
        std::fs::create_dir_all(&cfg.ingest_dir)?;
        let store = Store::open(&cfg.media_dir)?;
        // leftovers of interrupted uploads
        if let Ok(rd) = std::fs::read_dir(store.staging_dir()) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        let conn = db.conn()?;
        let repo = Repo::new(&conn);
        let n = repo.fail_interrupted_ingests()?;
        if n > 0 {
            println!("ingest: {n} records interrupted by a restart marked failed");
        }
        migrate::migrate_old_external(&repo, &store, &cfg.ingest_dir);
        migrate::reconcile_leftovers(&repo, &store, &cfg.ingest_dir);
        let seen = repo.all_seen()?.into_iter().map(|k| ((k.dev, k.ino), (k.size, k.mtime))).collect();
        drop(conn);
        let (tx, rx) = mpsc::unbounded_channel();
        Ok(Arc::new(Ingest {
            db,
            ingest_dir: cfg.ingest_dir.clone(),
            store,
            settle: cfg.settle,
            poll: cfg.poll,
            classifier: Box::new(FilenameClassifier),
            rt: Mutex::new(Runtime::default()),
            seen: Mutex::new(seen),
            inflight: Mutex::new(HashSet::new()),
            slots: Arc::new(Semaphore::new(PIPELINE_SLOTS)),
            queue_tx: tx,
            queue_rx: Mutex::new(Some(rx)),
            active: AtomicUsize::new(0),
            watching: AtomicBool::new(false),
            watcher: Mutex::new(None),
            counter: AtomicU64::new(0),
            lazy_rate: cfg.lazy_hash_rate,
        }))
    }

    /// Start watching, polling and processing the ingest dir.
    pub fn start(self: &Arc<Self>) {
        watch::start(self.clone());
        lazy::start(self.clone());
    }

    pub fn status(&self) -> QueueStatus {
        let rt = self.rt.lock().unwrap();
        QueueStatus {
            watching: self.watching.load(Ordering::Relaxed),
            poll_secs: self.poll.map(|p| p.as_secs()).unwrap_or(0),
            settle_secs: self.settle.as_secs(),
            queue: rt.queued.len(),
            settling: rt.pending.len(),
            active: self.active.load(Ordering::Relaxed),
        }
    }

    fn new_record(&self, origin: &str, source: String, name: String, size: Option<u64>) -> IngestRecord {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        IngestRecord {
            id: fnv1a(&format!("ingest\u{0}{source}\u{0}{nanos}\u{0}{n}")),
            source,
            origin: origin.into(),
            name,
            size,
            hash: None,
            state: "waiting".into(),
            error: None,
            item_id: None,
            file_id: None,
            placement: None,
            ingested_at: now(),
            updated_at: now(),
        }
    }

    fn save(&self, rec: &IngestRecord) {
        let r = self.db.conn().and_then(|c| Repo::new(&c).save_ingest(rec));
        if let Err(e) = r {
            eprintln!("ingest: cannot save record {}: {e}", rec.id);
        }
    }

    fn is_seen(&self, k: &SeenKey) -> bool {
        self.seen.lock().unwrap().get(&(k.dev, k.ino)) == Some(&(k.size, k.mtime))
    }

    fn remember(&self, k: &SeenKey, path: &Path, rec: &IngestRecord) {
        self.seen.lock().unwrap().insert((k.dev, k.ino), (k.size, k.mtime));
        let r = self.db.conn().and_then(|c| Repo::new(&c).mark_seen(k, &path.to_string_lossy(), &rec.state, &rec.id));
        if let Err(e) = r {
            eprintln!("ingest: cannot remember {}: {e}", path.display());
        }
    }

    /// Forget files whose ingest failed, so the next poll retries them.
    pub fn forget_failed(&self) -> AppResult<usize> {
        let conn = self.db.conn()?;
        let keys = Repo::new(&conn).forget_failed_seen()?;
        let mut seen = self.seen.lock().unwrap();
        for k in &keys {
            seen.remove(k);
        }
        Ok(keys.len())
    }

    /// Re-queue one failed ingest-dir record whose entry is still in INGEST_DIR.
    pub fn retry_record(&self, id: &str) -> AppResult<()> {
        let rec = {
            let conn = self.db.conn()?;
            Repo::new(&conn).get_ingest(id)?
        };
        if rec.state != "failed" {
            return Err(AppError::bad(format!("only failed records can be retried (this one is {})", rec.state)));
        }
        self.requeue_source(&rec)
    }

    fn requeue_source(&self, rec: &IngestRecord) -> AppResult<()> {
        if rec.origin != "ingest" {
            return Err(AppError::bad("uploads cannot be retried; upload the file again"));
        }
        let path = PathBuf::from(&rec.source);
        if !path.starts_with(&self.ingest_dir) || std::fs::symlink_metadata(&path).is_err() {
            return Err(AppError::not_found("file in INGEST_DIR (it is no longer there)"));
        }
        if let Some(k) = stat_key(&path) {
            self.seen.lock().unwrap().remove(&(k.dev, k.ino));
            let conn = self.db.conn()?;
            Repo::new(&conn).forget_seen(k.dev, k.ino)?;
        }
        self.note(&path);
        Ok(())
    }

    /// Re-queue every failed ingest-dir record still present. Returns (retried, skipped).
    pub fn retry_failed(&self) -> AppResult<(usize, usize)> {
        let recs = {
            let conn = self.db.conn()?;
            Repo::new(&conn).list_ingests(Some("failed"), 1000)?
        };
        let (mut retried, mut skipped) = (0, 0);
        let mut done = HashSet::new();
        for rec in recs {
            if !done.insert(rec.source.clone()) {
                continue;
            }
            match self.requeue_source(&rec) {
                Ok(()) => retried += 1,
                Err(_) => skipped += 1,
            }
        }
        Ok((retried, skipped))
    }

    /// A settled ingest-dir file, on the blocking pool.
    fn process_queued(&self, path: PathBuf) {
        let retry = (|| {
            let key = stat_key(&path)?;
            if self.is_seen(&key) {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().to_string();
            let mut rec = self.new_record("ingest", path.to_string_lossy().to_string(), name, Some(key.size));
            self.save(&rec);
            let retry = self.run(&mut rec, &path, Some(key));
            if std::fs::symlink_metadata(&path).is_err() {
                // the entry moved into the store: nothing is left in the ingest dir to remember
                self.prune_empty_parents(&path);
            } else if !retry {
                // failed / duplicate / ignored entries stay in place and are never retried endlessly
                self.remember(&key, &path, &rec);
            }
            Some(retry)
        })();
        self.rt.lock().unwrap().queued.remove(&path);
        if retry == Some(true) {
            self.note(&path);
        }
    }

    /// After an entry moved out, drop the (now empty) sub-folders it was dropped in; never INGEST_DIR itself.
    fn prune_empty_parents(&self, path: &Path) {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == self.ingest_dir || !d.starts_with(&self.ingest_dir) || std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }

    /// Stream an upload into `.staging` and run it through the pipeline.
    pub async fn upload(self: &Arc<Self>, raw_name: &str, body: axum::body::Body) -> AppResult<IngestRecord> {
        let name = raw_name.rsplit(['/', '\\']).next().unwrap_or("").trim().to_string();
        if name.is_empty() || name == "." || name == ".." {
            return Err(AppError::bad("X-Filename must be a file name"));
        }
        let mut rec = self.new_record("upload", format!("upload:{name}"), name.clone(), None);
        if let Some(reason) = ignore_reason(&name) {
            rec.state = "ignored".into();
            rec.error = Some(reason.into());
            self.save(&rec);
            return Ok(rec);
        }
        self.save(&rec);
        let staging = self.store.staging_dir().join(format!("{}.upload", rec.id));
        match write_body(&staging, body).await {
            Ok(n) => rec.size = Some(n),
            Err(e) => {
                let _ = tokio::fs::remove_file(&staging).await;
                rec.state = "failed".into();
                rec.error = Some(format!("upload interrupted: {e}"));
                self.save(&rec);
                return Ok(rec);
            }
        }
        let permit = self.slots.clone().acquire_owned().await.map_err(|_| AppError::Internal("ingest closed".into()))?;
        let me = self.clone();
        Ok(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            me.run(&mut rec, &staging, None);
            // duplicate / ignored / failed uploads leave nothing behind
            let _ = std::fs::remove_file(&staging);
            rec
        })
        .await?)
    }

    /// Run the pipeline; the record ends in a final state. Returns true when the source changed
    /// while it was processed (it is retried once it settles again).
    fn run(&self, rec: &mut IngestRecord, src: &Path, key: Option<SeenKey>) -> bool {
        match self.try_run(rec, src, key) {
            Ok(retry) => retry,
            Err(e) => {
                eprintln!("ingest: {} failed: {e}", rec.name);
                rec.state = "failed".into();
                rec.error = Some(e.to_string());
                self.save(rec);
                false
            }
        }
    }

    fn set_state(&self, rec: &mut IngestRecord, state: &str) {
        rec.state = state.into();
        self.save(rec);
    }

    fn finish(&self, rec: &mut IngestRecord, state: &str, error: Option<String>) {
        rec.error = error;
        self.set_state(rec, state);
    }

    fn try_run(&self, rec: &mut IngestRecord, src: &Path, key: Option<SeenKey>) -> AppResult<bool> {
        self.set_state(rec, "hashing");
        let is_link = std::fs::symlink_metadata(src).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        let reference = if is_link {
            match self.resolve_link(src) {
                Ok(r) => Some(r),
                Err(msg) => {
                    self.finish(rec, "failed", Some(msg));
                    return Ok(false);
                }
            }
        } else {
            None
        };
        let (hash, size) = match &reference {
            Some(r) => (format!("fp:{}", r.fp), r.size),
            None => store::hash_file(src)?,
        };
        rec.hash = Some(hash.clone());
        rec.size = Some(size);
        if let Some(k) = key {
            if stat_key(src) != Some(k) {
                self.finish(rec, "failed", Some("file changed while it was hashed; retried once it settles".into()));
                return Ok(true);
            }
        }

        // duplicate: the content is already stored (or being stored right now)
        let conn = self.db.conn()?;
        let repo = Repo::new(&conn);
        let claim = {
            let mut inflight = self.inflight.lock().unwrap();
            if inflight.contains(&hash) {
                None
            } else if let Some(existing) = match repo.find_file_by_hash(&hash)? {
                Some(e) => Some(e),
                // a symlinked file keeps its fingerprint after the lazy hasher swapped in the full hash
                None => match &reference {
                    Some(r) => repo.find_file_by_fingerprint(&r.fp)?,
                    None => None,
                },
            } {
                rec.item_id = Some(existing.item_id);
                rec.file_id = Some(existing.id);
                None
            } else {
                inflight.insert(hash.clone());
                Some(Claim { set: &self.inflight, hash: hash.clone() })
            }
        };
        let Some(_claim) = claim else {
            self.finish(rec, "duplicate", None);
            return Ok(false);
        };

        self.set_state(rec, "probing");
        let probe_src = reference.as_ref().map(|r| r.canon.as_path()).unwrap_or(src);
        let Some(probe) = probe::probe(probe_src).filter(|p| p.is_media()) else {
            self.finish(rec, "ignored", Some("ffprobe found no audio or video stream".into()));
            return Ok(false);
        };

        let ext = ext_of(Path::new(&rec.name));
        self.set_state(rec, "placing");
        let rel = Store::rel_path(&hash, &ext);
        self.store.ensure_parent(&rel)?;
        let dst = self.store.abs(&rel);
        if std::fs::symlink_metadata(&dst).is_ok() {
            // not in the database and not in flight: an orphan from an interrupted run (a link is removed, never its target)
            eprintln!("ingest: replacing orphaned blob {}", dst.display());
            std::fs::remove_file(&dst)?;
        }
        // one rename(2) of the directory entry (a symlink moves as the link; a relative one is re-created absolute)
        let recreated = store::place(src, &dst)?;
        let method = if reference.is_some() { "symlink" } else { "move" };
        rec.placement = Some(method.into());

        let input = ClassifyInput { file_name: &rec.name, has_video: probe.video.is_some(), has_audio: !probe.audio.is_empty() };
        let mapping = self.classifier.classify(&input);
        let (location, hash_state, fingerprint, src_mtime) = match &reference {
            Some(r) => (crate::model::LOC_EXTERNAL, "pending", Some(r.fp.clone()), r.mtime),
            None => (crate::model::LOC_STORE, "full", None, 0),
        };
        let stored = StoredFile {
            hash,
            store_path: rel,
            original_name: rec.name.clone(),
            ext,
            size,
            probe,
            location: location.into(),
            hash_state: hash_state.into(),
            fingerprint,
            src_mtime,
        };
        let attached = match catalog::attach(&repo, &mapping, &stored, now()) {
            Ok(a) => a,
            Err(e) => {
                if let Err(u) = store::unplace(src, &dst, recreated) {
                    eprintln!("ingest: cannot undo placement of {}: {u}", dst.display());
                }
                return Err(e);
            }
        };
        store::finish_place(src, recreated);
        rec.item_id = Some(attached.item_id.clone());
        rec.file_id = Some(attached.file_id.clone());

        println!(
            "ingest: {} -> {} {} ({}{})",
            rec.name,
            repo.get_item(&attached.item_id).map(|i| i.kind.as_str()).unwrap_or("?"),
            attached.item_id,
            method,
            if attached.version_of_existing { ", new version" } else { "" }
        );
        self.finish(rec, "done", None);
        Ok(false)
    }

    /// Resolve a symlink from the ingest dir and fingerprint its target (read-only).
    fn resolve_link(&self, link: &Path) -> Result<Reference, String> {
        let canon = std::fs::canonicalize(link).map_err(|e| format!("symlink target cannot be resolved (dangling symlink?): {e}"))?;
        if !std::fs::metadata(&canon).map(|m| m.is_file()).unwrap_or(false) {
            return Err(format!("symlink target {} is not a regular file", canon.display()));
        }
        let (fp, size, mtime) = store::fingerprint(&canon).map_err(|e| format!("cannot fingerprint {}: {e}", canon.display()))?;
        Ok(Reference { canon, fp, size, mtime })
    }

    /// Where a file's bytes are read from: its store entry (a regular file, or a symlink to the media).
    pub fn file_path(&self, f: &MediaFile) -> AppResult<PathBuf> {
        Ok(self.store.abs(&f.store_path))
    }

    /// Delete an item (with children) and every stored file it referenced.
    pub fn delete_item(&self, id: &str) -> AppResult<Deleted> {
        let conn = self.db.conn()?;
        let removed = Repo::new(&conn).tx(|r| r.remove_item_tree(id))?;
        Ok(self.cleanup(&removed.items, &removed.files))
    }

    /// Delete one file (version). The item goes too when it has no files left.
    pub fn delete_file(&self, id: &str) -> AppResult<Deleted> {
        let conn = self.db.conn()?;
        let (items, files) = Repo::new(&conn).tx(|r| {
            let f = r.get_file(id)?;
            r.delete_file_row(&f.id)?;
            let mut removed = crate::db::Removed::default();
            if r.files_for_item(&f.item_id)?.is_empty() {
                removed = r.remove_item_tree(&f.item_id)?;
            }
            removed.files.push(f);
            Ok((removed.items, removed.files))
        })?;
        Ok(self.cleanup(&items, &files))
    }

    fn cleanup(&self, items: &[crate::model::Item], files: &[MediaFile]) -> Deleted {
        for f in files {
            // removes the store entry only: for a symlink that is the link (never its target),
            // for a hardlinked file just this one link
            if let Err(e) = self.store.remove_blob(&f.store_path) {
                eprintln!("ingest: cannot remove blob {}: {e}", f.store_path);
            }
        }
        Deleted { items: items.len(), files: files.len() }
    }
}

async fn write_body(path: &Path, body: axum::body::Body) -> std::io::Result<u64> {
    use http_body_util::BodyExt;
    use tokio::io::AsyncWriteExt;
    let file = tokio::fs::File::create(path).await?;
    let mut out = tokio::io::BufWriter::with_capacity(1 << 20, file);
    let mut body = body;
    let mut n = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(std::io::Error::other)?;
        if let Ok(data) = frame.into_data() {
            out.write_all(&data).await?;
            n += data.len() as u64;
        }
    }
    out.flush().await?;
    out.into_inner().sync_all().await?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(name: &str) -> (PathBuf, Arc<Ingest>) {
        let root = std::env::temp_dir().join(format!("mediadir-ingest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        let mut cfg = Config::from_env();
        cfg.ingest_dir = root.join("ingest");
        cfg.media_dir = root.join("store");
        cfg.data_dir = root.join("data");
        let db = Db::open(&cfg.data_dir.join("t.db")).unwrap();
        (root, Ingest::new(db, &cfg).unwrap())
    }

    /// A tiny real media file (ffmpeg is a hard dependency of the server anyway).
    fn make_media(path: &Path, video: bool, n: u32) {
        let mut c = std::process::Command::new("ffmpeg");
        c.args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={}:duration=0.5", 200 + n)]);
        if video {
            c.args(["-f", "lavfi", "-i", &format!("testsrc=size={}x{}:rate=10:duration=0.5", 160 * n, 90 * n)]);
            c.args(["-c:v", "mpeg4", "-c:a", "aac", "-f", "matroska"]);
        } else {
            c.args(["-c:a", "libvorbis", "-f", "ogg"]);
        }
        assert!(c.arg(path).status().unwrap().success(), "ffmpeg failed");
    }

    #[test]
    fn ignores_non_media_and_partials() {
        assert_eq!(ignore_reason("movie.nfo"), Some("not a video/audio file"));
        assert_eq!(ignore_reason("Movie (2001).mkv.part"), Some("partial file"));
        assert_eq!(ignore_reason("x.mp4.crdownload"), Some("partial file"));
        assert_eq!(ignore_reason(".hidden.mkv"), Some("hidden file"));
        assert_eq!(ignore_reason("Movie (2001).MKV"), None);
    }

    #[test]
    fn regular_file_is_renamed_into_the_store_and_duplicates_stay() {
        let (root, ing) = setup("dup");
        let a = ing.ingest_dir.join("sub/Band - Song.ogg");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        make_media(&a, false, 1);
        let b = ing.ingest_dir.join("Other Name.ogg");
        std::fs::copy(&a, &b).unwrap();
        let ino_a = std::fs::metadata(&a).unwrap().ino();

        let mut r1 = ing.new_record("ingest", a.to_string_lossy().into(), "Band - Song.ogg".into(), None);
        assert!(!ing.run(&mut r1, &a, stat_key(&a)));
        assert_eq!(r1.state, "done", "{:?}", r1.error);
        assert_eq!(r1.placement.as_deref(), Some("move"));
        assert!(std::fs::symlink_metadata(&a).is_err(), "the entry left the ingest dir");
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);
        let f = repo.get_file(r1.file_id.as_ref().unwrap()).unwrap();
        let blob = ing.store.abs(&f.store_path);
        let m = std::fs::symlink_metadata(&blob).unwrap();
        assert!(m.file_type().is_file() && m.ino() == ino_a, "the very same inode: a rename, no copy");
        assert_eq!(f.location, "store");
        let item = repo.get_item(&f.item_id).unwrap();
        assert_eq!((item.kind.as_str(), item.title.as_str(), item.subtitle.as_deref()), ("track", "Song", Some("Band")));

        // duplicate content: recorded, nothing placed, the file stays in the ingest dir
        let mut r2 = ing.new_record("ingest", b.to_string_lossy().into(), "Other Name.ogg".into(), None);
        ing.run(&mut r2, &b, stat_key(&b));
        assert_eq!(r2.state, "duplicate");
        assert_eq!(r2.item_id, r1.item_id);
        assert!(b.exists() && std::fs::metadata(&b).unwrap().ino() != m.ino());
        assert_eq!(repo.file_count().unwrap(), 1);

        // deleting the item removes the store entry
        ing.delete_item(&f.item_id).unwrap();
        assert!(!blob.exists());
        assert!(repo.find_item(&f.item_id).unwrap().is_none());
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hardlink_entry_is_renamed_other_links_stay() {
        let (root, ing) = setup("hl");
        let other = root.join("library-copy.mkv");
        make_media(&other, true, 2);
        let a = ing.ingest_dir.join("Hard Linked (2014).mkv");
        std::fs::hard_link(&other, &a).unwrap();
        let ino = std::fs::metadata(&other).unwrap().ino();
        let mut r = ing.new_record("ingest", a.to_string_lossy().into(), "Hard Linked (2014).mkv".into(), None);
        ing.run(&mut r, &a, stat_key(&a));
        assert_eq!((r.state.as_str(), r.placement.as_deref()), ("done", Some("move")), "{:?}", r.error);
        assert!(!a.exists() && other.exists());
        let conn = ing.db.conn().unwrap();
        let f = Repo::new(&conn).get_file(r.file_id.as_ref().unwrap()).unwrap();
        let blob = ing.store.abs(&f.store_path);
        let m = std::fs::metadata(&blob).unwrap();
        assert_eq!((m.ino(), m.nlink()), (ino, 2));
        // deleting removes just the store link; the other link and its data survive
        ing.delete_file(&f.id).unwrap();
        assert!(!blob.exists());
        assert_eq!(std::fs::metadata(&other).unwrap().ino(), ino);
        assert_eq!(std::fs::metadata(&other).unwrap().nlink(), 1);
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn upload_style_move_and_versions() {
        let (root, ing) = setup("move");
        let (a, b, c) = (ing.store.staging_dir().join("1.upload"), ing.store.staging_dir().join("2.upload"), ing.store.staging_dir().join("3.upload"));
        make_media(&a, true, 2);
        make_media(&b, true, 1);
        make_media(&c, true, 3);
        let mut r1 = ing.new_record("upload", "upload:a".into(), "Versions Test (2015).mkv".into(), None);
        ing.run(&mut r1, &a, None);
        assert_eq!((r1.state.as_str(), r1.placement.as_deref()), ("done", Some("move")), "{:?}", r1.error);
        assert!(!a.exists(), "staged upload was renamed into the store");
        // a second file with the same title+year becomes another version of the same movie
        let mut r2 = ing.new_record("upload", "upload:b".into(), "Versions Test (2015) - 480p.mkv".into(), None);
        ing.run(&mut r2, &b, None);
        assert_eq!(r2.state, "done");
        assert_eq!(r1.item_id, r2.item_id);
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);
        let item = repo.get_item(r1.item_id.as_ref().unwrap()).unwrap();
        assert_eq!((item.kind.as_str(), item.title.as_str(), item.year), ("movie", "Versions Test", Some(2015)));
        assert!(item.thumb.is_none(), "no thumbnail is generated at ingest");
        let files = repo.files_for_item(&item.id).unwrap();
        assert_eq!(files.iter().map(|f| f.height()).collect::<Vec<_>>(), vec![180, 90]);
        assert_eq!(files[0].label, "180p · MPEG4");
        // the same episode twice -> one episode with two versions, under show > season
        let mut r3 = ing.new_record("upload", "upload:c".into(), "Show - S01E02 - Two.mkv".into(), None);
        ing.run(&mut r3, &c, None);
        let ep = repo.get_item(r3.item_id.as_ref().unwrap()).unwrap();
        assert_eq!((ep.kind.as_str(), ep.index, ep.parent_index), ("episode", Some(2), Some(1)));
        let season = repo.get_item(ep.parent_id.as_ref().unwrap()).unwrap();
        let show = repo.get_item(season.parent_id.as_ref().unwrap()).unwrap();
        assert_eq!((season.title.as_str(), show.title.as_str()), ("Season 1", "Show"));
        // deleting one version keeps the item; deleting the last removes item and empty containers
        ing.delete_file(&files[1].id).unwrap();
        assert!(!ing.store.abs(&files[1].store_path).exists());
        assert_eq!(repo.files_for_item(&item.id).unwrap().len(), 1);
        ing.delete_item(&ep.id).unwrap();
        assert!(repo.find_item(&show.id).unwrap().is_none(), "empty show pruned");
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn setup_ext(name: &str) -> (PathBuf, PathBuf, Arc<Ingest>) {
        let root = std::env::temp_dir().join(format!("mediadir-ingest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("ext/Movies")).unwrap();
        let mut cfg = Config::from_env();
        cfg.ingest_dir = root.join("ingest");
        cfg.media_dir = root.join("store");
        cfg.data_dir = root.join("data");
        cfg.lazy_hash_rate = 0;
        let db = Db::open(&cfg.data_dir.join("t.db")).unwrap();
        (root.clone(), root.join("ext"), Ingest::new(db, &cfg).unwrap())
    }

    fn run_link(ing: &Arc<Ingest>, link: &Path) -> IngestRecord {
        let name = link.file_name().unwrap().to_string_lossy().to_string();
        let mut r = ing.new_record("ingest", link.to_string_lossy().into(), name, None);
        ing.run(&mut r, link, stat_key(link));
        r
    }

    #[test]
    fn symlink_is_renamed_into_the_store_and_hashed_lazily() {
        use std::os::unix::fs::PermissionsExt;
        let (root, ext, ing) = setup_ext("ref");
        let target = ext.join("Movies/Ref Movie (2010).mkv");
        make_media(&target, true, 2);
        // read-only target in a read-only directory: ingest must not need to write anywhere there
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(ext.join("Movies"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let before = std::fs::metadata(&target).unwrap();
        let link = ing.ingest_dir.join("Ref Movie (2010).mkv");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let link_ino = std::fs::symlink_metadata(&link).unwrap().ino();

        let r = run_link(&ing, &link);
        assert_eq!((r.state.as_str(), r.placement.as_deref()), ("done", Some("symlink")), "{:?}", r.error);
        assert!(r.hash.as_deref().unwrap().starts_with("fp:"));
        assert!(std::fs::symlink_metadata(&link).is_err(), "the link left the ingest dir");
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);
        let f = repo.get_file(r.file_id.as_ref().unwrap()).unwrap();
        assert_eq!((f.location.as_str(), f.hash_state.as_str()), ("external", "pending"));
        assert!(f.store_path.starts_with("fp/"), "{}", f.store_path);
        let entry = ing.store.abs(&f.store_path);
        let em = std::fs::symlink_metadata(&entry).unwrap();
        assert!(em.file_type().is_symlink() && em.ino() == link_ino, "the link itself was renamed");
        assert_eq!(std::fs::read_link(&entry).unwrap(), target);
        assert_eq!(ing.file_path(&f).unwrap(), entry);
        assert_eq!(std::fs::read(&entry).unwrap(), std::fs::read(&target).unwrap(), "reads resolve through the link");

        // the same target linked again is a duplicate by fingerprint, and stays in the ingest dir
        let link2 = ing.ingest_dir.join("Same Again.mkv");
        std::os::unix::fs::symlink(&target, &link2).unwrap();
        assert_eq!(run_link(&ing, &link2).state, "duplicate");
        assert!(std::fs::symlink_metadata(&link2).is_ok());

        // lazy hash: pending -> full (equal to the real BLAKE3) and the store symlink moves to its hash path
        assert_eq!(repo.count_pending_hashes().unwrap(), 1);
        assert!(matches!(ing.hash_one_pending(&[]).unwrap(), lazy::Step::Done));
        let f2 = repo.get_file(&f.id).unwrap();
        assert_eq!(f2.hash_state, "full");
        assert_eq!(f2.hash, store::hash_file(&target).unwrap().0);
        assert_eq!(f2.id, f.id);
        assert_eq!(f2.store_path, Store::rel_path(&f2.hash, "mkv"));
        assert!(std::fs::symlink_metadata(&entry).is_err(), "interim path is gone");
        let entry2 = ing.store.abs(&f2.store_path);
        assert!(std::fs::symlink_metadata(&entry2).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&entry2).unwrap(), target);
        assert!(matches!(ing.hash_one_pending(&[]).unwrap(), lazy::Step::Idle));

        // deleting removes the store symlink only
        let d = ing.delete_item(&f.item_id).unwrap();
        assert_eq!(d.files, 1);
        assert!(repo.find_file(&f.id).unwrap().is_none());
        assert!(std::fs::symlink_metadata(&entry2).is_err());
        let after = std::fs::metadata(&target).unwrap();
        assert!(target.exists());
        assert_eq!((before.mtime(), before.mtime_nsec(), before.ino(), before.len()), (after.mtime(), after.mtime_nsec(), after.ino(), after.len()));
        assert_eq!(after.permissions().mode() & 0o777, 0o444, "never chmod'ed through the link");
        assert_eq!(std::fs::metadata(ext.join("Movies")).unwrap().permissions().mode() & 0o777, 0o555);
        drop(conn);
        std::fs::set_permissions(ext.join("Movies"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn run_file(ing: &Arc<Ingest>, path: &Path) -> IngestRecord {
        run_link(ing, path)
    }

    #[test]
    fn same_content_after_lazy_hash_is_a_duplicate_not_an_id_collision() {
        let (root, ext, ing) = setup_ext("lazydup");
        let target = ext.join("Movies/Lazy Dup (2012).mkv");
        make_media(&target, true, 2);
        let copy = ext.join("Movies/Lazy Dup copy.mkv");
        std::fs::copy(&target, &copy).unwrap();
        let regular = ing.ingest_dir.join("Lazy Dup regular.mkv");
        std::fs::create_dir_all(&ing.ingest_dir).unwrap();
        std::fs::copy(&target, &regular).unwrap();

        let la = ing.ingest_dir.join("Lazy Dup (2012).mkv");
        std::os::unix::fs::symlink(&target, &la).unwrap();
        let ra = run_link(&ing, &la);
        assert_eq!(ra.state, "done", "{:?}", ra.error);
        assert!(matches!(ing.hash_one_pending(&[]).unwrap(), lazy::Step::Done));
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);
        let fa = repo.get_file(ra.file_id.as_ref().unwrap()).unwrap();
        assert_eq!(fa.hash_state, "full");

        // another symlink to identical bytes (different path): its fp: matches the fingerprint column
        let lb = ing.ingest_dir.join("Lazy Dup again.mkv");
        std::os::unix::fs::symlink(&copy, &lb).unwrap();
        let rb = run_link(&ing, &lb);
        assert_eq!(rb.state, "duplicate", "{:?}", rb.error);
        assert_eq!((rb.file_id.as_deref(), rb.item_id.as_deref()), (Some(fa.id.as_str()), Some(fa.item_id.as_str())));
        assert!(std::fs::symlink_metadata(&lb).is_ok(), "duplicates stay in the ingest dir");
        // a regular file with the same bytes: full hash matches the lazily hashed row
        let rc = run_file(&ing, &regular);
        assert_eq!(rc.state, "duplicate", "{:?}", rc.error);
        assert_eq!(rc.file_id.as_deref(), Some(fa.id.as_str()));
        assert_eq!(repo.file_count().unwrap(), 1);
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lazy_hash_collision_repoints_every_record() {
        let (root, ext, ing) = setup_ext("lazycol");
        let target = ext.join("Movies/Col (2013).mkv");
        make_media(&target, true, 1);
        let regular = ing.ingest_dir.join("Col regular (2013).mkv");
        std::fs::create_dir_all(&ing.ingest_dir).unwrap();
        std::fs::copy(&target, &regular).unwrap();
        let la = ing.ingest_dir.join("Col (2013).mkv");
        std::os::unix::fs::symlink(&target, &la).unwrap();
        let ra = run_link(&ing, &la); // pending row A
        let lb = ing.ingest_dir.join("Col dup.mkv");
        std::os::unix::fs::symlink(&target, &lb).unwrap();
        let rb = run_link(&ing, &lb); // duplicate of A (fingerprint)
        assert_eq!((ra.state.as_str(), rb.state.as_str()), ("done", "duplicate"));
        let rc = run_file(&ing, &regular); // same bytes as a regular file: its own row (A is still fp:)
        assert_eq!(rc.state, "done", "{:?}", rc.error);
        assert_ne!(ra.file_id, rc.file_id);

        assert!(matches!(ing.hash_one_pending(&[]).unwrap(), lazy::Step::Done));
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);
        assert!(repo.find_file(ra.file_id.as_ref().unwrap()).unwrap().is_none(), "the symlinked row was dropped");
        assert_eq!(repo.file_count().unwrap(), 1);
        for id in [&ra.id, &rb.id] {
            let r = repo.get_ingest(id).unwrap();
            assert_eq!(r.state, "duplicate", "record {id}");
            assert_eq!(r.file_id, rc.file_id);
            assert_eq!(r.item_id, rc.item_id);
        }
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn leftover_ingest_symlinks_are_reconciled() {
        let (root, ext, ing) = setup_ext("leftover");
        let target = ext.join("Movies/Left (2014).mkv");
        make_media(&target, true, 1);
        let link = ing.ingest_dir.join("Left (2014).mkv");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let r = run_link(&ing, &link);
        assert_eq!(r.state, "done", "{:?}", r.error);
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);

        // 1) row exists and its store symlink has the same target: the leftover link goes, the target stays
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(migrate::reconcile_with(&repo, &ing.store, &ing.ingest_dir).unwrap(), 1);
        assert!(std::fs::symlink_metadata(&link).is_err() && target.exists());
        assert_eq!(repo.get_ingest(&r.id).unwrap().state, "done");

        // 2) the row is gone and the content is not catalogued: record failed, link stays for a retry
        ing.delete_file(r.file_id.as_ref().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(migrate::reconcile_with(&repo, &ing.store, &ing.ingest_dir).unwrap(), 1);
        let rec = repo.get_ingest(&r.id).unwrap();
        assert_eq!((rec.state.as_str(), rec.file_id.clone()), ("failed", None));
        assert!(std::fs::symlink_metadata(&link).is_ok());
        assert_eq!(repo.file_count().unwrap(), 0);

        // 3) the row is gone but the same content is catalogued elsewhere: duplicate of it, link stays
        let other = ing.ingest_dir.join("Other Left (2014).mkv");
        std::fs::copy(&target, &other).unwrap();
        let ro = run_file(&ing, &other);
        assert_eq!(ro.state, "done", "{:?}", ro.error);
        let mut back = rec.clone();
        back.state = "done".into();
        back.file_id = Some("0000000000000000".into()); // a row that no longer exists
        // the regular copy has a full hash and no fingerprint, so catalogue a fingerprinted (symlinked) row too
        let lo = ing.ingest_dir.join("Left Ref (2014).mkv");
        std::os::unix::fs::symlink(&target, &lo).unwrap();
        let rl = run_link(&ing, &lo);
        assert!(rl.state == "done" || rl.state == "duplicate", "{:?}", rl.error);
        repo.save_ingest(&back).unwrap();
        std::os::unix::fs::symlink(&target, &link).ok();
        assert_eq!(migrate::reconcile_with(&repo, &ing.store, &ing.ingest_dir).unwrap(), 1);
        let dup = repo.get_ingest(&r.id).unwrap();
        assert_eq!((dup.state.as_str(), dup.file_id), ("duplicate", rl.file_id.clone()));
        assert!(std::fs::symlink_metadata(&link).is_ok(), "link of a duplicate stays");
        assert!(target.exists());
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn failed_records_can_be_retried() {
        let (root, ext, ing) = setup_ext("retry");
        let l = ing.ingest_dir.join("later.mkv");
        std::fs::create_dir_all(&ing.ingest_dir).unwrap();
        std::os::unix::fs::symlink(ext.join("nope.mkv"), &l).unwrap();
        let r = run_link(&ing, &l);
        assert_eq!(r.state, "failed");
        let key = stat_key(&l).unwrap();
        ing.remember(&key, &l, &r);
        assert!(ing.is_seen(&key));
        ing.retry_record(&r.id).unwrap();
        assert!(!ing.is_seen(&key), "forgotten");
        assert_eq!(ing.status().settling, 1, "re-queued for the settle wait");
        // not failed -> 400; entry gone -> 404
        let mut ok = ing.new_record("ingest", l.to_string_lossy().into(), "later.mkv".into(), None);
        ok.state = "done".into();
        ing.save(&ok);
        assert!(matches!(ing.retry_record(&ok.id), Err(AppError::BadRequest(_))));
        assert_eq!(ing.retry_failed().unwrap(), (1, 0));
        std::fs::remove_file(&l).unwrap();
        assert!(matches!(ing.retry_record(&r.id), Err(AppError::NotFound(_))));
        assert_eq!(ing.retry_failed().unwrap(), (0, 1));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn relative_symlink_is_recreated_absolute() {
        let (root, ext, ing) = setup_ext("rel");
        let target = ext.join("Movies/Rel Movie (2011).mkv");
        make_media(&target, true, 1);
        std::fs::create_dir_all(ing.ingest_dir.join("deep")).unwrap();
        let link = ing.ingest_dir.join("deep/Rel Movie (2011).mkv");
        // relative to the link's own directory
        let rel = pathdiff(&target, &ing.ingest_dir.join("deep"));
        std::os::unix::fs::symlink(&rel, &link).unwrap();
        assert!(std::fs::read_link(&link).unwrap().is_relative());
        let r = run_link(&ing, &link);
        assert_eq!(r.state, "done", "{:?}", r.error);
        assert!(std::fs::symlink_metadata(&link).is_err(), "ingest entry removed");
        let conn = ing.db.conn().unwrap();
        let f = Repo::new(&conn).get_file(r.file_id.as_ref().unwrap()).unwrap();
        let entry = ing.store.abs(&f.store_path);
        let t = std::fs::read_link(&entry).unwrap();
        assert!(t.is_absolute() && t == std::fs::canonicalize(&target).unwrap(), "{t:?}");
        assert!(std::fs::metadata(&entry).unwrap().is_file(), "resolves from the store location");
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `to` relative to the directory `from` (both absolute, no symlinks involved).
    fn pathdiff(to: &Path, from: &Path) -> PathBuf {
        let (t, f): (Vec<_>, Vec<_>) = (to.components().collect(), from.components().collect());
        let common = t.iter().zip(&f).take_while(|(a, b)| a == b).count();
        let mut out = PathBuf::new();
        for _ in common..f.len() {
            out.push("..");
        }
        for c in &t[common..] {
            out.push(c);
        }
        out
    }

    #[test]
    fn dangling_and_non_file_symlinks_fail_and_stay() {
        let (root, ext, ing) = setup_ext("refbad");
        let l2 = ing.ingest_dir.join("dangling.mkv");
        std::os::unix::fs::symlink(ext.join("nope.mkv"), &l2).unwrap();
        let r = run_link(&ing, &l2);
        assert_eq!(r.state, "failed");
        assert!(r.error.as_deref().unwrap().contains("dangling"), "{:?}", r.error);
        assert!(std::fs::symlink_metadata(&l2).is_ok(), "entry left in place");
        std::fs::create_dir_all(ext.join("dir.mkv")).unwrap();
        let l3 = ing.ingest_dir.join("dir.mkv");
        std::os::unix::fs::symlink(ext.join("dir.mkv"), &l3).unwrap();
        assert!(run_link(&ing, &l3).error.unwrap().contains("not a regular file"));
        assert!(std::fs::symlink_metadata(&l3).is_ok());
        let conn = ing.db.conn().unwrap();
        assert_eq!(Repo::new(&conn).file_count().unwrap(), 0, "nothing catalogued");
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn old_external_layout_is_migrated() {
        let (root, ext, ing) = setup_ext("mig");
        let target = ext.join("Movies/Old Ref (2009).mkv");
        make_media(&target, true, 1);
        let link = ing.ingest_dir.join("Old Ref (2009).mkv");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let r = run_link(&ing, &link);
        assert_eq!(r.state, "done", "{:?}", r.error);
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);
        let f = repo.get_file(r.file_id.as_ref().unwrap()).unwrap();
        let new_entry = ing.store.abs(&f.store_path);

        // recreate the old layout: relative store_path, no store entry, user symlink back in ingest
        let ctx = |f: &crate::model::MediaFile| {
            std::fs::remove_file(ing.store.abs(&f.store_path)).ok();
            repo.set_store_path(&f.id, "Movies/Old Ref (2009).mkv").unwrap();
            std::os::unix::fs::symlink(&target, &link).unwrap_or(());
        };
        ctx(&f);
        assert!(std::fs::symlink_metadata(&new_entry).is_err());
        // 1) no env root: derived from the ingest symlink
        assert_eq!(migrate::migrate_with(&repo, &ing.store, None).unwrap(), 1);
        let f1 = repo.get_file(&f.id).unwrap();
        assert_eq!(f1.store_path, f.store_path);
        assert_eq!(std::fs::read_link(&new_entry).unwrap(), std::fs::canonicalize(&target).unwrap());
        assert!(std::fs::symlink_metadata(&link).is_err(), "leftover ingest symlink removed");
        assert!(target.exists());
        // idempotent
        assert_eq!(migrate::migrate_with(&repo, &ing.store, None).unwrap(), 0);
        // 2) with the old root and no ingest symlink left
        ctx(&f1);
        assert!(std::fs::symlink_metadata(&link).is_ok());
        std::fs::remove_file(&link).unwrap();
        assert_eq!(migrate::migrate_with(&repo, &ing.store, None).unwrap(), 0, "nothing to derive the target from");
        assert_eq!(repo.get_file(&f.id).unwrap().store_path, "Movies/Old Ref (2009).mkv");
        assert_eq!(migrate::migrate_with(&repo, &ing.store, Some(&ext)).unwrap(), 1);
        assert_eq!(repo.get_file(&f.id).unwrap().store_path, f.store_path);
        assert_eq!(std::fs::read_link(&new_entry).unwrap(), std::fs::canonicalize(&target).unwrap());
        assert_eq!(ing.file_path(&repo.get_file(&f.id).unwrap()).unwrap(), new_entry);
        assert!(std::fs::metadata(&new_entry).unwrap().is_file());
        drop(conn);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn fingerprint_covers_size_and_both_ends() {
        let dir = std::env::temp_dir().join(format!("mediadir-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("big.bin");
        let n = 10 * 1024 * 1024usize;
        let mut data: Vec<u8> = (0..n).map(|i| (i * 31 % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        let (a, size, _) = store::fingerprint(&p).unwrap();
        assert_eq!((a.len(), size), (64, n as u64));
        assert_eq!(store::fingerprint(&p).unwrap().0, a, "deterministic");
        data[n - 1] ^= 1; // last byte
        std::fs::write(&p, &data).unwrap();
        assert_ne!(store::fingerprint(&p).unwrap().0, a);
        data[n - 1] ^= 1;
        data[0] ^= 1; // first byte
        std::fs::write(&p, &data).unwrap();
        assert_ne!(store::fingerprint(&p).unwrap().0, a);
        data[0] ^= 1;
        data.push(0); // size
        std::fs::write(&p, &data).unwrap();
        assert_ne!(store::fingerprint(&p).unwrap().0, a);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
