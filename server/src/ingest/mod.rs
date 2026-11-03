//! Ingest: the two ways media enters MediaDir (the watched INGEST_DIR and uploads) and the one
//! pipeline both use: settle → hash → probe → place → name→item.
//! See docs/STORAGE.md.

pub mod catalog;
pub mod classify;
pub mod filename;
pub mod probe;
pub mod store;
pub mod thumb;
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
use store::{PlaceOpts, Store};
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
    let m = std::fs::symlink_metadata(path).ok()?;
    if !m.file_type().is_file() {
        return None;
    }
    Some(SeenKey { dev: m.dev(), ino: m.ino(), size: m.len(), mtime: m.mtime() * 1_000_000_000 + m.mtime_nsec() })
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
        }))
    }

    /// Start watching, polling and processing the ingest dir.
    pub fn start(self: &Arc<Self>) {
        watch::start(self.clone());
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
            let retry = self.run(&mut rec, &path, PlaceOpts::LINK_OR_MOVE, Some(key));
            if !retry {
                self.remember(&key, &path, &rec);
            }
            Some(retry)
        })();
        self.rt.lock().unwrap().queued.remove(&path);
        if retry == Some(true) {
            self.note(&path);
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
            me.run(&mut rec, &staging, PlaceOpts::MOVE, None);
            // duplicate / ignored / failed uploads leave nothing behind
            let _ = std::fs::remove_file(&staging);
            rec
        })
        .await?)
    }

    /// Run the pipeline; the record ends in a final state. Returns true when the source changed
    /// while it was processed (it is retried once it settles again).
    fn run(&self, rec: &mut IngestRecord, src: &Path, opts: PlaceOpts, key: Option<SeenKey>) -> bool {
        match self.try_run(rec, src, opts, key) {
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

    fn try_run(&self, rec: &mut IngestRecord, src: &Path, opts: PlaceOpts, key: Option<SeenKey>) -> AppResult<bool> {
        self.set_state(rec, "hashing");
        let (hash, size) = store::hash_file(src)?;
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
            } else if let Some(existing) = repo.find_file_by_hash(&hash)? {
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
        let Some(probe) = probe::probe(src).filter(|p| p.is_media()) else {
            self.finish(rec, "ignored", Some("ffprobe found no audio or video stream".into()));
            return Ok(false);
        };

        self.set_state(rec, "placing");
        let ext = ext_of(Path::new(&rec.name));
        let rel = Store::rel_path(&hash, &ext);
        self.store.ensure_parent(&rel)?;
        let dst = self.store.abs(&rel);
        if std::fs::symlink_metadata(&dst).is_ok() {
            // not in the database and not in flight: an orphan from an interrupted run
            eprintln!("ingest: replacing orphaned blob {}", dst.display());
            std::fs::remove_file(&dst)?;
        }
        let method = store::place(src, &dst, opts)?;
        rec.placement = Some(method.as_str().into());

        let input = ClassifyInput { file_name: &rec.name, has_video: probe.video.is_some(), has_audio: !probe.audio.is_empty() };
        let mapping = self.classifier.classify(&input);
        let stored = StoredFile { hash, store_path: rel, original_name: rec.name.clone(), ext, size, probe };
        let attached = match catalog::attach(&repo, &mapping, &stored, now()) {
            Ok(a) => a,
            Err(e) => {
                if let Err(u) = store::unplace(src, &dst, method) {
                    eprintln!("ingest: cannot undo placement of {}: {u}", dst.display());
                }
                return Err(e);
            }
        };
        rec.item_id = Some(attached.item_id.clone());
        rec.file_id = Some(attached.file_id.clone());

        println!(
            "ingest: {} -> {} {} ({}{})",
            rec.name,
            repo.get_item(&attached.item_id).map(|i| i.kind.as_str()).unwrap_or("?"),
            attached.item_id,
            method.as_str(),
            if attached.version_of_existing { ", new version" } else { "" }
        );
        self.finish(rec, "done", None);
        Ok(false)
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
    fn duplicate_content_is_recorded_not_placed() {
        let (root, ing) = setup("dup");
        let a = ing.ingest_dir.join("Band - Song.ogg");
        make_media(&a, false, 1);
        let b = ing.ingest_dir.join("Other Name.ogg");
        std::fs::copy(&a, &b).unwrap();

        let mut r1 = ing.new_record("ingest", a.to_string_lossy().into(), "Band - Song.ogg".into(), None);
        assert!(!ing.run(&mut r1, &a, PlaceOpts::LINK_OR_MOVE, stat_key(&a)));
        assert_eq!(r1.state, "done", "{:?}", r1.error);
        assert_eq!(r1.placement.as_deref(), Some("hardlink"));
        let conn = ing.db.conn().unwrap();
        let repo = Repo::new(&conn);
        let f = repo.get_file(r1.file_id.as_ref().unwrap()).unwrap();
        let blob = ing.store.abs(&f.store_path);
        assert_eq!(std::fs::metadata(&blob).unwrap().ino(), std::fs::metadata(&a).unwrap().ino());
        let item = repo.get_item(&f.item_id).unwrap();
        assert_eq!((item.kind.as_str(), item.title.as_str(), item.subtitle.as_deref()), ("track", "Song", Some("Band")));

        let mut r2 = ing.new_record("ingest", b.to_string_lossy().into(), "Other Name.ogg".into(), None);
        ing.run(&mut r2, &b, PlaceOpts::LINK_OR_MOVE, stat_key(&b));
        assert_eq!(r2.state, "duplicate");
        assert_eq!(r2.item_id, r1.item_id);
        assert!(b.exists() && std::fs::metadata(&b).unwrap().ino() != std::fs::metadata(&blob).unwrap().ino());
        assert_eq!(repo.file_count().unwrap(), 1);

        // deleting the item removes the blob but never the ingest original
        ing.delete_item(&f.item_id).unwrap();
        assert!(!blob.exists() && a.exists());
        assert!(repo.find_item(&f.item_id).unwrap().is_none());
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
        ing.run(&mut r1, &a, PlaceOpts::MOVE, None);
        assert_eq!((r1.state.as_str(), r1.placement.as_deref()), ("done", Some("move")), "{:?}", r1.error);
        assert!(!a.exists(), "staged upload was renamed into the store");
        // a second file with the same title+year becomes another version of the same movie
        let mut r2 = ing.new_record("upload", "upload:b".into(), "Versions Test (2015) - 480p.mkv".into(), None);
        ing.run(&mut r2, &b, PlaceOpts::MOVE, None);
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
        ing.run(&mut r3, &c, PlaceOpts::MOVE, None);
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
}
