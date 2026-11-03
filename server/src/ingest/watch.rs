//! Watching the ingest dir: inotify (via `notify`) plus periodic polling (network mounts don't
//! deliver inotify), a settle wait, and a dispatcher with bounded concurrency.

use super::{ignore_reason, stat_key, Ingest, Pending};
use notify::{EventKind, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn start(ing: Arc<Ingest>) {
    // dispatcher: at most PIPELINE_SLOTS files in the pipeline at once (uploads share the slots)
    let rx = ing.queue_rx.lock().unwrap().take();
    if let Some(mut rx) = rx {
        let me = ing.clone();
        tokio::spawn(async move {
            while let Some(path) = rx.recv().await {
                let Ok(permit) = me.slots.clone().acquire_owned().await else { break };
                let m = me.clone();
                tokio::task::spawn_blocking(move || {
                    m.active.fetch_add(1, Ordering::Relaxed);
                    m.process_queued(path);
                    m.active.fetch_sub(1, Ordering::Relaxed);
                    drop(permit);
                });
            }
        });
    }

    // inotify
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
    let handler = move |res: notify::Result<notify::Event>| match res {
        Ok(ev) => {
            let relevant = matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Access(notify::event::AccessKind::Close(_)));
            if relevant {
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        }
        Err(e) => eprintln!("ingest: watch error: {e}"),
    };
    match notify::recommended_watcher(handler).and_then(|mut w| w.watch(&ing.ingest_dir, RecursiveMode::Recursive).map(|_| w)) {
        Ok(w) => {
            *ing.watcher.lock().unwrap() = Some(w);
            ing.watching.store(true, Ordering::Relaxed);
            println!("ingest: watching {}", ing.ingest_dir.display());
        }
        Err(e) => eprintln!("ingest: cannot watch {} ({e}); relying on polling", ing.ingest_dir.display()),
    }
    let me = ing.clone();
    tokio::spawn(async move {
        while let Some(p) = events.recv().await {
            let m = me.clone();
            // a directory moved/created in: its files produce no events of their own
            let _ = tokio::task::spawn_blocking(move || if p.symlink_metadata().map(|md| md.is_dir()).unwrap_or(false) { m.scan_dir(&p) } else { m.note(&p) }).await;
        }
    });

    // polling (also the initial scan)
    let me = ing.clone();
    tokio::spawn(async move {
        loop {
            let m = me.clone();
            let _ = tokio::task::spawn_blocking(move || m.scan_dir(&m.ingest_dir.clone())).await;
            match me.poll {
                Some(every) => tokio::time::sleep(every).await,
                None => break,
            }
        }
    });

    // settle
    let me = ing;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tick.tick().await;
            let m = me.clone();
            let _ = tokio::task::spawn_blocking(move || m.settle_tick()).await;
        }
    });
}

impl Ingest {
    /// Not inside a hidden directory, and an ingestible file name.
    fn is_candidate(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.ingest_dir) else { return false };
        let hidden_dir = rel.parent().map(|p| p.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'))).unwrap_or(false);
        let name = rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        !hidden_dir && ignore_reason(&name).is_none()
    }

    /// A file appeared or changed: start (or restart) its settle wait.
    pub(super) fn note(&self, path: &Path) {
        if !self.is_candidate(path) {
            return;
        }
        let Some(key) = stat_key(path) else { return };
        if self.is_seen(&key) {
            return;
        }
        let mut rt = self.rt.lock().unwrap();
        if rt.queued.contains(path) {
            return;
        }
        let p = rt.pending.entry(path.to_path_buf()).or_insert(Pending { size: key.size, mtime: key.mtime, since: Instant::now() });
        if p.size != key.size || p.mtime != key.mtime {
            *p = Pending { size: key.size, mtime: key.mtime, since: Instant::now() };
        }
    }

    /// Walk a directory (recursively, skipping hidden entries and symlinked directories; symlinked files are references) and note every file.
    pub fn scan_dir(&self, dir: &Path) {
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                eprintln!("ingest: cannot read {}", d.display());
                continue;
            };
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push(e.path()),
                    Ok(t) if t.is_file() || t.is_symlink() => self.note(&e.path()),
                    _ => {}
                }
            }
        }
    }

    /// Queue files whose size+mtime have been stable for the settle time.
    fn settle_tick(&self) {
        let snapshot: Vec<(PathBuf, u64, i64, Instant)> = {
            let rt = self.rt.lock().unwrap();
            rt.pending.iter().map(|(p, v)| (p.clone(), v.size, v.mtime, v.since)).collect()
        };
        for (path, size, mtime, since) in snapshot {
            let now = stat_key(&path);
            let mut rt = self.rt.lock().unwrap();
            match now {
                None => {
                    rt.pending.remove(&path);
                }
                Some(k) if k.size != size || k.mtime != mtime => {
                    rt.pending.insert(path, Pending { size: k.size, mtime: k.mtime, since: Instant::now() });
                }
                Some(k) if since.elapsed() >= self.settle => {
                    rt.pending.remove(&path);
                    if !self.is_seen(&k) && rt.queued.insert(path.clone()) {
                        let _ = self.queue_tx.send(path);
                    }
                }
                Some(_) => {}
            }
        }
    }
}
