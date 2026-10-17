//! Where received files are written.
//!
//! The receiver never touches the filesystem directly. On a computer that is
//! [`FsStorage`]; on Android it is the media store or a folder the user picked,
//! implemented in Kotlin. Every implementation hands back a real open file, so the
//! verified data is written by Rust at full speed and only creating, publishing and
//! naming go through the platform.
//!
//! Paths are the manifest's relative paths (`Album/Disc 1/track01.flac`), already
//! checked by [`crate::manifest::Manifest::validate`].

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

/// A file still arriving. Only [`ReceiveStorage::finish`] makes it visible under its name.
pub struct Partial {
    pub file: File,
    /// The relative path it will be published under.
    pub rel: String,
    /// Implementation-defined handle: a path, a content URI.
    pub token: String,
}

pub trait ReceiveStorage: Send + Sync {
    /// Whether something already exists at `rel`, for `Album (2)` naming. Counts files
    /// still arriving, if the platform shows them under their final name.
    fn exists(&self, rel: &str) -> io::Result<bool>;
    /// Whether a finished, published file is at `rel`. Files still arriving never count:
    /// resuming relies on this to tell a file finished just before an interruption from
    /// one that is only partly here.
    fn published(&self, rel: &str) -> io::Result<bool>;
    /// Creates the folder `rel` and any parents.
    fn make_dir(&self, rel: &str) -> io::Result<()>;
    /// Creates an empty, not-yet-visible file that will become `rel`.
    fn create_partial(&self, rel: &str) -> io::Result<Partial>;
    /// Opens a partial left by an interrupted transfer, if it is still there. `token` is
    /// the one [`Self::create_partial`] returned.
    fn reopen_partial(&self, rel: &str, token: &str) -> io::Result<Option<Partial>>;
    /// Publishes a verified partial under its name. Must not replace an existing file.
    /// `hash` is its BLAKE3 fingerprint, kept as a verified mark where the platform can.
    fn finish(&self, partial: Partial, hash: &[u8; 32]) -> io::Result<()>;
    /// Removes a partial that will not be finished.
    fn discard(&self, partial: Partial) -> io::Result<()>;
    /// Removes a partial left by a transfer abandoned long ago, known only by its token.
    fn discard_abandoned(&self, token: &str) -> io::Result<()>;
    /// How the folder holding `rel` reads to a person: `~/Downloads/glft/Album`.
    fn describe(&self, rel: &str) -> String;
}

/// Extended attribute holding a received file's BLAKE3 fingerprint, hex.
pub const VERIFIED_XATTR: &str = "user.glft.blake3";

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Plain files under a root folder. Partials sit next to their final place as
/// `.<name>.glft-part`, so publishing is a rename within one folder.
pub struct FsStorage {
    root: PathBuf,
}

impl FsStorage {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, seg| p.join(seg))
    }

    fn partial_path(&self, rel: &str) -> PathBuf {
        let path = self.path(rel);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        path.with_file_name(format!(".{name}.glft-part"))
    }
}

impl ReceiveStorage for FsStorage {
    fn exists(&self, rel: &str) -> io::Result<bool> {
        match fs::symlink_metadata(self.path(rel)) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn published(&self, rel: &str) -> io::Result<bool> {
        // Partials have their own names here, so anything at `rel` is published.
        self.exists(rel)
    }

    fn make_dir(&self, rel: &str) -> io::Result<()> {
        fs::create_dir_all(self.path(rel))
    }

    fn create_partial(&self, rel: &str) -> io::Result<Partial> {
        let path = self.partial_path(rel);
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
        Ok(Partial { file, rel: rel.to_string(), token: path.to_string_lossy().into_owned() })
    }

    fn reopen_partial(&self, rel: &str, token: &str) -> io::Result<Option<Partial>> {
        // Only a partial in the expected place is reused.
        if Path::new(token) != self.partial_path(rel) {
            return Ok(None);
        }
        match OpenOptions::new().read(true).write(true).open(token) {
            Ok(file) => Ok(Some(Partial { file, rel: rel.to_string(), token: token.to_string() })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn finish(&self, partial: Partial, hash: &[u8; 32]) -> io::Result<()> {
        partial.file.sync_all()?;
        // The verified mark; filesystems without extended attributes simply go without.
        let _ = xattr::set(&partial.token, VERIFIED_XATTR, hex(hash).as_bytes());
        let target = self.path(&partial.rel);
        // A hard link fails if the name is taken, so an existing file is never replaced.
        match fs::hard_link(&partial.token, &target) {
            Ok(()) => fs::remove_file(&partial.token),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            // Android's shared storage has no hard links (it answers EACCES or EPERM): check,
            // then rename. The window between the two is tiny, and only this app writes here.
            Err(_) => {
                if fs::symlink_metadata(&target).is_ok() {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", target.display())));
                }
                fs::rename(&partial.token, &target)
            }
        }
    }

    fn discard(&self, partial: Partial) -> io::Result<()> {
        drop(partial.file);
        fs::remove_file(&partial.token)
    }

    fn discard_abandoned(&self, token: &str) -> io::Result<()> {
        // Only ever a partial file of ours, inside the receive folder.
        let path = Path::new(token);
        let ours = path.starts_with(&self.root)
            && path.file_name().is_some_and(|n| n.to_string_lossy().ends_with(crate::manifest::PARTIAL_SUFFIX));
        if ours { fs::remove_file(path) } else { Ok(()) }
    }

    fn describe(&self, rel: &str) -> String {
        let dir = Path::new(rel).parent().map(|p| self.path(&p.to_string_lossy())).unwrap_or_else(|| self.root.clone());
        let dir = dir.to_string_lossy().into_owned();
        match std::env::var("HOME") {
            Ok(home) if !home.is_empty() && dir.starts_with(&home) => format!("~{}", &dir[home.len()..]),
            _ => dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn partial_is_hidden_until_finished_and_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsStorage::new(dir.path().to_path_buf()).unwrap();
        s.make_dir("Album").unwrap();
        let mut p = s.create_partial("Album/a.flac").unwrap();
        p.file.write_all(b"data").unwrap();
        assert!(dir.path().join("Album/.a.flac.glft-part").exists());
        assert!(!s.exists("Album/a.flac").unwrap());
        s.finish(p, &[1; 32]).unwrap();
        assert_eq!(fs::read(dir.path().join("Album/a.flac")).unwrap(), b"data");
        assert!(!dir.path().join("Album/.a.flac.glft-part").exists());

        let p = s.create_partial("Album/a.flac").unwrap();
        assert!(s.finish(p, &[1; 32]).is_err(), "an existing file must not be replaced");
        assert_eq!(fs::read(dir.path().join("Album/a.flac")).unwrap(), b"data");
    }
}
