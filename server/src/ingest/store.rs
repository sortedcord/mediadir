//! MediaDir's private, content-addressed store: `MEDIA_DIR/<h[0..2]>/<h[2..4]>/<hash>.<ext>`.
//!
//! Entries get in by a single rename(2) of the directory entry (regular file, hardlink or symlink
//! alike), never by copy; INGEST_DIR is on the same filesystem as the store. Stored files are never
//! chmod'ed; store directories are 0700.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Create (or open) the store at `root`, including its `.staging` dir for uploads.
    pub fn open(root: &Path) -> io::Result<Self> {
        let s = Store { root: root.to_path_buf() };
        mkdir_private(&s.root)?;
        mkdir_private(&s.staging_dir())?;
        Ok(s)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Uploads are streamed here (same filesystem as the store, so placing them is a rename).
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(".staging")
    }

    /// Store-relative path of a blob. A pending (`fp:<hex>`) hash gets `fp/<hex[0..2]>/<hex>.<ext>`
    /// until its full hash is known; that never collides with a hash directory (`p` is not hex).
    pub fn rel_path(hash: &str, ext: &str) -> String {
        let ext = if ext.is_empty() { "bin" } else { ext };
        match hash.strip_prefix("fp:") {
            Some(fp) => format!("fp/{}/{fp}.{ext}", &fp[0..2]),
            None => format!("{}/{}/{hash}.{ext}", &hash[0..2], &hash[2..4]),
        }
    }

    pub fn abs(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// `<hash>.json` next to the blob.
    pub fn sidecar_path(&self, rel: &str) -> PathBuf {
        self.abs(rel).with_extension("json")
    }

    /// Create the blob's parent directories (0700).
    pub fn ensure_parent(&self, rel: &str) -> io::Result<()> {
        let mut dir = self.root.clone();
        let parts: Vec<&str> = rel.split('/').collect();
        for p in &parts[..parts.len().saturating_sub(1)] {
            dir.push(p);
            mkdir_private(&dir)?;
        }
        Ok(())
    }

    /// Remove a blob and its sidecar. Missing files are not an error.
    pub fn remove_blob(&self, rel: &str) -> io::Result<()> {
        for p in [self.abs(rel), self.sidecar_path(rel)] {
            match fs::remove_file(&p) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

fn mkdir_private(dir: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

/// BLAKE3 of the full content, streamed. Returns (hex hash, bytes read).
#[cfg(test)]
pub fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let mut hasher = blake3::Hasher::new();
    let f = File::open(path)?;
    hasher.update_reader(f)?;
    Ok((hasher.finalize().to_hex().to_string(), hasher.count()))
}

/// Bytes read from each end of a referenced file for its quick fingerprint.
pub const FP_EDGE: u64 = 4 * 1024 * 1024;

/// Quick identity of a (possibly huge, possibly remote) file: BLAKE3 over a domain tag, the size,
/// the first 4 MiB and the last 4 MiB (no overlap; files <= 8 MiB are hashed whole).
/// Read-only. Returns (hex, size, mtime in ns) from the opened handle.
pub fn fingerprint(path: &Path) -> io::Result<(String, u64, i64)> {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    let mut f = File::open(path)?;
    let m = f.metadata()?;
    if !m.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let size = m.len();
    let mut h = blake3::Hasher::new();
    h.update(b"mediadir-fp1\0");
    h.update(&size.to_le_bytes());
    let head = size.min(FP_EDGE);
    let mut buf = vec![0u8; head as usize];
    f.read_exact(&mut buf)?;
    h.update(&buf);
    let tail_start = size.saturating_sub(FP_EDGE).max(head);
    if tail_start < size {
        let mut buf = vec![0u8; (size - tail_start) as usize];
        f.seek(SeekFrom::Start(tail_start))?;
        f.read_exact(&mut buf)?;
        h.update(&buf);
    }
    Ok((h.finalize().to_hex().to_string(), size, m.mtime() * 1_000_000_000 + m.mtime_nsec()))
}

/// Full BLAKE3, read at no more than `rate` bytes/second (`0` = unlimited). Read-only.
/// Full BLAKE3 of a file read at most `rate` bytes/second (`0` = unlimited), that gives up (`ErrorKind::Interrupted`) as soon as `stop()` is true
/// (checked every MiB): a cancelled / paused job stops mid-file.
pub fn hash_file_ctl(path: &Path, rate: u64, stop: &dyn Fn() -> bool) -> io::Result<(String, u64)> {
    use std::io::Read;
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let started = std::time::Instant::now();
    let mut n = 0u64;
    loop {
        if stop() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        let r = f.read(&mut buf)?;
        if r == 0 {
            break;
        }
        h.update(&buf[..r]);
        n += r as u64;
        if rate > 0 {
            let due = std::time::Duration::from_secs_f64(n as f64 / rate as f64);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait);
            }
        }
    }
    Ok((h.finalize().to_hex().to_string(), n))
}

/// Put the directory entry `src` at `dst` with one rename(2) (`dst` must not exist). A regular file
/// or hardlink moves as is; a symlink moves as the link itself (its target is never followed). A
/// symlink with a relative target would dangle from the store, so it is re-created at `dst` with the
/// absolute target and the caller removes `src` once the placement is committed ([`finish_place`]).
/// Returns whether `src` still has to be removed by [`finish_place`].
pub fn place(src: &Path, dst: &Path) -> io::Result<bool> {
    if fs::symlink_metadata(dst).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", dst.display())));
    }
    if let Ok(target) = fs::read_link(src) {
        if target.is_relative() {
            let abs = fs::canonicalize(src)?; // fully resolved; dangling links were rejected before
            std::os::unix::fs::symlink(abs, dst)?;
            return Ok(true);
        }
    }
    fs::rename(src, dst)?;
    Ok(false)
}

/// Complete a [`place`] that re-created a link: drop the original ingest entry (only the link).
pub fn finish_place(src: &Path, recreated: bool) {
    if recreated {
        if let Err(e) = fs::remove_file(src) {
            eprintln!("store: cannot remove ingest link {}: {e}", src.display());
        }
    }
}

/// Undo a placement after a later pipeline step failed.
pub fn unplace(src: &Path, dst: &Path, recreated: bool) -> io::Result<()> {
    if recreated {
        fs::remove_file(dst)
    } else {
        fs::rename(dst, src)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mediadir-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn layout_and_hash() {
        assert_eq!(Store::rel_path("abcdef0123", "mkv"), "ab/cd/abcdef0123.mkv");
        let d = tmpdir("hash");
        let f = d.join("x.bin");
        fs::write(&f, b"hello").unwrap();
        let (h, n) = hash_file(&f).unwrap();
        assert_eq!(n, 5);
        assert_eq!(h, blake3::hash(b"hello").to_hex().to_string());
        let s = Store::open(&d.join("store")).unwrap();
        assert_eq!(fs::metadata(s.root()).unwrap().mode() & 0o777, 0o700);
        assert!(s.staging_dir().is_dir());
        s.ensure_parent("ab/cd/abcd.mkv").unwrap();
        assert_eq!(fs::metadata(s.abs("ab/cd")).unwrap().mode() & 0o777, 0o700);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rename_keeps_inode_and_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("rename");
        let src = d.join("movie.mkv");
        fs::write(&src, b"bytes").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
        let other = d.join("other-link.mkv");
        fs::hard_link(&src, &other).unwrap();
        let dst = d.join("blob.mkv");
        let ino = fs::metadata(&src).unwrap().ino();
        assert!(!place(&src, &dst).unwrap());
        assert!(!src.exists() && other.exists(), "only the dropped entry moved; the other link stays");
        let m = fs::metadata(&dst).unwrap();
        assert_eq!((m.ino(), m.mode() & 0o777, m.nlink()), (ino, 0o640, 2), "a rename: same inode, no chmod");
        unplace(&src, &dst, false).unwrap();
        assert!(src.exists() && !dst.exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn symlinks_move_as_links() {
        let d = tmpdir("symlink");
        let target = d.join("real.mkv");
        fs::write(&target, b"bytes").unwrap();
        // absolute link: the link itself is renamed
        let (l1, s1) = (d.join("abs.mkv"), d.join("store-abs.mkv"));
        std::os::unix::fs::symlink(&target, &l1).unwrap();
        let ino = fs::symlink_metadata(&l1).unwrap().ino();
        assert!(!place(&l1, &s1).unwrap());
        assert!(fs::symlink_metadata(&l1).is_err());
        assert_eq!(fs::symlink_metadata(&s1).unwrap().ino(), ino);
        assert_eq!(fs::read_link(&s1).unwrap(), target);
        // relative link: re-created with the absolute canonical target, original removed on finish
        fs::create_dir_all(d.join("in/sub")).unwrap();
        let l2 = d.join("in/sub/rel.mkv");
        std::os::unix::fs::symlink("../../real.mkv", &l2).unwrap();
        let s2 = d.join("store-rel.mkv");
        let recreated = place(&l2, &s2).unwrap();
        assert!(recreated && fs::symlink_metadata(&l2).is_ok());
        assert_eq!(fs::read_link(&s2).unwrap(), fs::canonicalize(&target).unwrap());
        assert_eq!(fs::read(&s2).unwrap(), b"bytes");
        finish_place(&l2, recreated);
        assert!(fs::symlink_metadata(&l2).is_err());
        assert!(target.exists(), "the target is never touched");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn existing_destination_is_refused() {
        let d = tmpdir("dup");
        let (a, b, dst) = (d.join("a.mkv"), d.join("b.mkv"), d.join("blob.mkv"));
        fs::write(&a, b"same").unwrap();
        fs::write(&b, b"same").unwrap();
        place(&a, &dst).unwrap();
        let e = place(&b, &dst).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert!(b.exists(), "the source is left alone");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn pending_hash_paths() {
        assert_eq!(Store::rel_path(&format!("fp:{}", "ab".repeat(32)), "mkv"), format!("fp/ab/{}.mkv", "ab".repeat(32)));
    }
}
