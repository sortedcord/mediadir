//! MediaDir's private, content-addressed store: `MEDIA_DIR/<h[0..2]>/<h[2..4]>/<hash>.<ext>`.
//!
//! Files get in by reflink (FICLONE) → hardlink → move; never by copy or symlink. Stored files
//! are read-only (0444), store directories 0700.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
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

    /// Store-relative path of a blob.
    pub fn rel_path(hash: &str, ext: &str) -> String {
        let ext = if ext.is_empty() { "bin" } else { ext };
        format!("{}/{}/{hash}.{ext}", &hash[0..2], &hash[2..4])
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
pub fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let mut hasher = blake3::Hasher::new();
    let f = File::open(path)?;
    hasher.update_reader(f)?;
    Ok((hasher.finalize().to_hex().to_string(), hasher.count()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceMethod {
    Reflink,
    Hardlink,
    Move,
}

impl PlaceMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaceMethod::Reflink => "reflink",
            PlaceMethod::Hardlink => "hardlink",
            PlaceMethod::Move => "move",
        }
    }
}

/// Which placement methods may be tried, in order reflink → hardlink → move.
#[derive(Debug, Clone, Copy)]
pub struct PlaceOpts {
    pub reflink: bool,
    pub hardlink: bool,
    pub allow_move: bool,
}

impl PlaceOpts {
    /// Ingest-dir files: keep the original in place when the filesystem allows it.
    pub const LINK_OR_MOVE: PlaceOpts = PlaceOpts { reflink: true, hardlink: true, allow_move: true };
    /// Staged uploads: just rename into place.
    pub const MOVE: PlaceOpts = PlaceOpts { reflink: false, hardlink: false, allow_move: true };
}

/// Put `src` at `dst` without copying bytes, then make `dst` read-only.
/// `dst` must not exist (`AlreadyExists` otherwise). Never copies, never symlinks.
pub fn place(src: &Path, dst: &Path, opts: PlaceOpts) -> io::Result<PlaceMethod> {
    if fs::symlink_metadata(dst).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", dst.display())));
    }
    let mut last: Option<io::Error> = None;
    let mut method = None;
    if opts.reflink {
        match reflink(src, dst) {
            Ok(()) => method = Some(PlaceMethod::Reflink),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    if method.is_none() && opts.hardlink {
        match fs::hard_link(src, dst) {
            Ok(()) => method = Some(PlaceMethod::Hardlink),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    if method.is_none() && opts.allow_move {
        // rename never copies: across filesystems it fails with EXDEV
        match fs::rename(src, dst) {
            Ok(()) => method = Some(PlaceMethod::Move),
            Err(e) => last = Some(e),
        }
    }
    let Some(method) = method else {
        return Err(last.unwrap_or_else(|| io::Error::other("no placement method allowed")));
    };
    // a hardlinked original shares the inode, so it becomes read-only too
    if let Err(e) = fs::set_permissions(dst, Permissions::from_mode(0o444)) {
        eprintln!("store: cannot make {} read-only: {e}", dst.display());
    }
    Ok(method)
}

/// Undo a placement after a later pipeline step failed: links are dropped, a move is reversed.
pub fn unplace(src: &Path, dst: &Path, method: PlaceMethod) -> io::Result<()> {
    match method {
        PlaceMethod::Reflink | PlaceMethod::Hardlink => fs::remove_file(dst),
        PlaceMethod::Move => fs::rename(dst, src),
    }
}

/// Copy-on-write clone (btrfs, xfs, bcachefs...). Fails with EOPNOTSUPP/EXDEV/EINVAL elsewhere.
fn reflink(src: &Path, dst: &Path) -> io::Result<()> {
    let s = File::open(src)?;
    let d = OpenOptions::new().write(true).create_new(true).mode(0o600).open(dst)?;
    // SAFETY: both descriptors are open for the duration of the call.
    let r = unsafe { libc::ioctl(d.as_raw_fd(), libc::FICLONE as _, s.as_raw_fd()) };
    if r != 0 {
        let e = io::Error::last_os_error();
        drop(d);
        let _ = fs::remove_file(dst);
        return Err(e);
    }
    Ok(())
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
    fn hardlinks_on_same_filesystem() {
        let d = tmpdir("link");
        let src = d.join("movie.mkv");
        fs::write(&src, b"bytes").unwrap();
        let dst = d.join("blob.mkv");
        // reflink is unsupported on tmpfs/ext4, so this exercises the reflink -> hardlink fallback
        let m = place(&src, &dst, PlaceOpts::LINK_OR_MOVE).unwrap();
        assert!(src.exists(), "original stays in place");
        let (a, b) = (fs::metadata(&src).unwrap(), fs::metadata(&dst).unwrap());
        if m == PlaceMethod::Hardlink {
            assert_eq!(a.ino(), b.ino());
        } else {
            assert_eq!(m, PlaceMethod::Reflink);
        }
        assert_eq!(b.mode() & 0o777, 0o444);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn moves_when_linking_fails() {
        let d = tmpdir("move");
        let src = d.join("upload.mkv");
        fs::write(&src, b"bytes").unwrap();
        let dst = d.join("blob.mkv");
        let ino = fs::metadata(&src).unwrap().ino();
        // linking is unavailable (e.g. EXDEV / EPERM): only a move is left
        let m = place(&src, &dst, PlaceOpts { reflink: true, hardlink: false, allow_move: true });
        let m = m.unwrap();
        assert!(matches!(m, PlaceMethod::Move | PlaceMethod::Reflink));
        if m == PlaceMethod::Move {
            assert!(!src.exists(), "moved away");
            assert_eq!(fs::metadata(&dst).unwrap().ino(), ino, "a rename, not a copy");
        }
        assert_eq!(fs::read(&dst).unwrap(), b"bytes");
        // moving back undoes it
        if m == PlaceMethod::Move {
            unplace(&src, &dst, m).unwrap();
            assert!(src.exists() && !dst.exists());
        }
        // a link failure with no move allowed is an error, never a copy
        let other = d.join("never.mkv");
        let e = place(&src, &other, PlaceOpts { reflink: false, hardlink: false, allow_move: false });
        assert!(e.is_err() && !other.exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn existing_destination_is_a_duplicate() {
        let d = tmpdir("dup");
        let (a, b, dst) = (d.join("a.mkv"), d.join("b.mkv"), d.join("blob.mkv"));
        fs::write(&a, b"same").unwrap();
        fs::write(&b, b"same").unwrap();
        place(&a, &dst, PlaceOpts::LINK_OR_MOVE).unwrap();
        for opts in [PlaceOpts::LINK_OR_MOVE, PlaceOpts::MOVE] {
            let e = place(&b, &dst, opts).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        }
        assert!(b.exists(), "the duplicate is left alone");
        fs::remove_dir_all(&d).unwrap();
    }
}
