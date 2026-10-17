//! The manifest: what is being sent, with a BLAKE3 fingerprint for every file.
//!
//! Paths are relative and use `/`, so a folder arrives as that folder. The receiver
//! checks every path before anything is written ([`Manifest::validate`]).

use std::{
    collections::HashSet,
    fs, io,
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
};

use bao_tree::{
    BaoTree, BlockSize,
    io::{
        outboard::{PreOrderMemOutboard, PreOrderOutboard},
        sync::CreateOutboard,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, ErrorKind},
    events::Totals,
};

/// Verification granularity: 16 KiB chunk groups, as iroh-blobs uses.
pub const BLOCK_SIZE: BlockSize = BlockSize::from_chunk_log(4);

/// Files still arriving are named `.<name>` plus this; offered names may not end in it.
pub const PARTIAL_SUFFIX: &str = ".glft-part";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path, `/`-separated, starting with its root's name.
    pub path: String,
    pub size: u64,
    /// BLAKE3 hash of the whole file.
    pub hash: [u8; 32],
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The items the user chose, by name.
    pub roots: Vec<Root>,
    /// Every folder below a folder root, including empty ones, so structure survives.
    pub dirs: Vec<String>,
    pub files: Vec<FileEntry>,
}

/// Sender-side data for one file: where it is and its verification tree.
pub struct Source {
    pub file: SourceRef,
    pub outboard: PreOrderMemOutboard,
}

/// Opens files the core has no path for, such as files another Android app shared.
pub trait SourceFiles: Send + Sync {
    /// Opens file `index` (as numbered in the [`Item`]s) for reading from the start.
    fn open(&self, index: usize) -> io::Result<fs::File>;
}

/// Where a file being sent is read from. It is opened again for every request, so a
/// resumed transfer reads it afresh.
#[derive(Clone)]
pub enum SourceRef {
    Path(PathBuf),
    Opened(Arc<dyn SourceFiles>, usize),
}

impl SourceRef {
    pub fn open(&self) -> io::Result<fs::File> {
        match self {
            Self::Path(path) => fs::File::open(path),
            Self::Opened(files, index) => files.open(*index),
        }
    }

    /// For error details.
    pub fn describe(&self) -> String {
        match self {
            Self::Path(path) => path.display().to_string(),
            Self::Opened(_, index) => format!("shared file {index}"),
        }
    }
}

/// Something chosen to send on a device without paths (the phone): one file, or a folder
/// with everything below it.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: ItemKind,
}

#[derive(Debug, Clone)]
pub enum ItemKind {
    /// The [`SourceFiles`] index of the file.
    File(usize),
    /// Everything below the folder: `/`-separated paths relative to it, each a file (with its
    /// [`SourceFiles`] index) or a folder (`None`, kept so empty folders arrive too).
    Folder(Vec<(String, Option<usize>)>),
}

impl Manifest {
    pub fn totals(&self) -> Totals {
        Totals {
            folders: self.roots.iter().filter(|r| r.is_dir).count(),
            files: self.files.len(),
            bytes: self.files.iter().map(|f| f.size).sum(),
        }
    }

    /// Receiver-side check of everything the sender claims.
    ///
    /// Every path must be relative, free of `..`, `.` and empty segments, and sit
    /// under one of the declared roots; no path may appear twice.
    pub fn validate(&self) -> Result<(), Error> {
        if self.roots.is_empty() {
            return Err(Error::new(ErrorKind::ProtocolError, "offer has no items"));
        }
        let mut roots = HashSet::new();
        for root in &self.roots {
            check_rel_path(&root.name)?;
            if root.name.contains('/') {
                return Err(unsafe_path(&root.name));
            }
            if !roots.insert(root.name.as_str()) {
                return Err(Error::new(ErrorKind::ProtocolError, format!("item {:?} listed twice", root.name)));
            }
        }
        let mut seen = HashSet::new();
        let paths = self.dirs.iter().map(|d| (d, true)).chain(self.files.iter().map(|f| (&f.path, false)));
        for (path, is_dir) in paths {
            check_rel_path(path)?;
            let first = path.split('/').next().unwrap_or_default();
            let root = self.roots.iter().find(|r| r.name == first).ok_or_else(|| unsafe_path(path))?;
            // A file root is exactly one file; anything else must be inside a folder root.
            let fits = if root.is_dir { is_dir || path.len() > first.len() } else { !is_dir && path == first };
            if !fits || !seen.insert(path.as_str()) {
                return Err(unsafe_path(path));
            }
        }
        Ok(())
    }
}

fn unsafe_path(path: &str) -> Error {
    Error::new(ErrorKind::UnsafePath, format!("offered path {path:?}"))
}

/// Checks one relative path as received from the network.
pub fn check_rel_path(path: &str) -> Result<(), Error> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err(unsafe_path(path));
    }
    if path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == ".." || seg.ends_with(PARTIAL_SUFFIX)) {
        return Err(unsafe_path(path));
    }
    Ok(())
}

/// Builds the manifest for the chosen files and folders, hashing every file.
///
/// Blocking: call it from `spawn_blocking`. Symbolic links are skipped.
pub fn build(inputs: &[PathBuf]) -> Result<(Manifest, Vec<Source>), Error> {
    if inputs.is_empty() {
        return Err(Error::new(ErrorKind::NothingToSend, "no paths given"));
    }
    let mut manifest = Manifest::default();
    let mut sources = Vec::new();
    for input in inputs {
        let abs = fs::canonicalize(input).map_err(|e| Error::io(ErrorKind::SourceUnreadable, input, e))?;
        let name = file_name_text(&abs)?;
        if manifest.roots.iter().any(|r| r.name == name) {
            return Err(Error::new(ErrorKind::DuplicateName, name));
        }
        let meta = fs::metadata(&abs).map_err(|e| Error::io(ErrorKind::SourceUnreadable, &abs, e))?;
        if meta.is_dir() {
            manifest.roots.push(Root { name: name.clone(), is_dir: true });
            let mut walk: Vec<_> = walkdir::WalkDir::new(&abs)
                .min_depth(1)
                .sort_by_file_name()
                .into_iter()
                .collect::<Result<_, _>>()
                .map_err(|e| {
                    let path = e.path().unwrap_or(&abs).to_path_buf();
                    Error::new(ErrorKind::SourceUnreadable, format!("{}: {e}", path.display()))
                })?;
            walk.retain(|e| !e.path_is_symlink());
            for entry in walk {
                let rel = entry.path().strip_prefix(&abs).expect("walkdir stays under its root");
                let rel = format!("{name}/{}", rel_text(rel)?);
                if entry.file_type().is_dir() {
                    manifest.dirs.push(rel);
                } else if entry.file_type().is_file() {
                    let (file, source) = hash_file(entry.path(), rel)?;
                    manifest.files.push(file);
                    sources.push(source);
                }
            }
        } else {
            manifest.roots.push(Root { name: name.clone(), is_dir: false });
            let (file, source) = hash_file(&abs, name)?;
            manifest.files.push(file);
            sources.push(source);
        }
    }
    Ok((manifest, sources))
}

fn hash_file(path: &Path, rel: String) -> Result<(FileEntry, Source), Error> {
    hash_source(SourceRef::Path(path.to_path_buf()), rel)
}

fn hash_source(source: SourceRef, rel: String) -> Result<(FileEntry, Source), Error> {
    let err = |e: io::Error| Error::new(ErrorKind::SourceUnreadable, format!("{}: {e}", source.describe()));
    let file = source.open().map_err(err)?;
    let meta = file.metadata().map_err(err)?;
    if !meta.is_file() {
        // A stream rather than a stored file: its size is unknown and it can't be reread.
        return Err(Error::new(ErrorKind::SourceUnreadable, format!("{rel}: not a stored file")));
    }
    let size = meta.len();
    let ob = PreOrderOutboard::<Vec<u8>>::create_sized(BufReader::with_capacity(1 << 20, file), size, BLOCK_SIZE)
        .map_err(err)?;
    let outboard = PreOrderMemOutboard { root: ob.root, tree: BaoTree::new(size, BLOCK_SIZE), data: ob.data };
    let entry = FileEntry { path: rel, size, hash: *ob.root.as_bytes() };
    Ok((entry, Source { file: source, outboard }))
}

/// Builds the manifest for items chosen on a device without paths, hashing every file.
///
/// Blocking. Names that clash (two shared files both called `image.jpg`) are told apart as
/// `image (2).jpg`, since the person sharing can't rename them. Folder contents are sorted,
/// so the same files always give the same manifest, and so resume after "send again".
pub fn build_items(items: &[Item], files: Arc<dyn SourceFiles>) -> Result<(Manifest, Vec<Source>), Error> {
    if items.is_empty() {
        return Err(Error::new(ErrorKind::NothingToSend, "no items given"));
    }
    let mut manifest = Manifest::default();
    let mut sources = Vec::new();
    for item in items {
        let base = item.name.replace(['/', '\\', '\0'], "_");
        let is_dir = matches!(item.kind, ItemKind::Folder(_));
        let name = free_name(&base, is_dir, |n| manifest.roots.iter().any(|r| r.name == n));
        check_rel_path(&name)?;
        manifest.roots.push(Root { name: name.clone(), is_dir });
        match &item.kind {
            ItemKind::File(index) => {
                let (file, source) = hash_source(SourceRef::Opened(files.clone(), *index), name)?;
                manifest.files.push(file);
                sources.push(source);
            }
            ItemKind::Folder(entries) => {
                let mut entries = entries.clone();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                for (rel, index) in entries {
                    let rel = format!("{name}/{rel}");
                    check_rel_path(&rel)?;
                    match index {
                        None => manifest.dirs.push(rel),
                        Some(index) => {
                            let (file, source) = hash_source(SourceRef::Opened(files.clone(), index), rel)?;
                            manifest.files.push(file);
                            sources.push(source);
                        }
                    }
                }
            }
        }
    }
    Ok((manifest, sources))
}

fn file_name_text(path: &Path) -> Result<String, Error> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::NothingToSend, format!("{} has no name", path.display())))?;
    name.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::new(ErrorKind::NameNotText, path.display().to_string()))
}

fn rel_text(rel: &Path) -> Result<String, Error> {
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    parts
        .map(|p| p.join("/"))
        .ok_or_else(|| Error::new(ErrorKind::NameNotText, rel.display().to_string()))
}

/// Picks a name for `name` that `taken` does not report: `Album` → `Album (2)`,
/// `song.flac` → `song (2).flac`.
pub fn free_name(name: &str, is_dir: bool, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if !is_dir && i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !taken(candidate))
        .expect("an unused name exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a/./b", "a//b", "a/", "a\\b", "..", "a\0b"] {
            assert!(check_rel_path(bad).is_err(), "{bad:?} should be refused");
        }
        for good in ["a", "Album/track01.flac", "a b/c.d", "..hidden", "x..y"] {
            assert!(check_rel_path(good).is_ok(), "{good:?} should pass");
        }
    }

    fn offer(roots: &[(&str, bool)], dirs: &[&str], files: &[&str]) -> Manifest {
        Manifest {
            roots: roots.iter().map(|(n, d)| Root { name: n.to_string(), is_dir: *d }).collect(),
            dirs: dirs.iter().map(|s| s.to_string()).collect(),
            files: files.iter().map(|p| FileEntry { path: p.to_string(), size: 0, hash: [0; 32] }).collect(),
        }
    }

    #[test]
    fn validate_ties_paths_to_roots() {
        assert!(offer(&[("Album", true)], &["Album/cd1"], &["Album/cd1/a.flac"]).validate().is_ok());
        assert!(offer(&[("a.txt", false)], &[], &["a.txt"]).validate().is_ok());
        // outside any root
        assert!(offer(&[("Album", true)], &[], &["Other/a"]).validate().is_err());
        // a file root cannot hold more files
        assert!(offer(&[("a.txt", false)], &[], &["a.txt/b"]).validate().is_err());
        // a folder root cannot also be a file
        assert!(offer(&[("Album", true)], &[], &["Album"]).validate().is_err());
        // duplicates and the partial folder
        assert!(offer(&[("Album", true)], &[], &["Album/a", "Album/a"]).validate().is_err());
        assert!(offer(&[("Album", true)], &[], &["Album/.a.glft-part"]).validate().is_err());
        assert!(offer(&[("Album", true)], &[], &["Album/../../.bashrc"]).validate().is_err());
    }

    #[test]
    fn free_names() {
        let taken = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
        assert_eq!(free_name("Album", true, taken(&[])), "Album");
        assert_eq!(free_name("Album", true, taken(&["Album"])), "Album (2)");
        assert_eq!(free_name("Album", true, taken(&["Album", "Album (2)"])), "Album (3)");
        assert_eq!(free_name("song.flac", false, taken(&["song.flac"])), "song (2).flac");
        assert_eq!(free_name("v1.2", true, taken(&["v1.2"])), "v1.2 (2)");
        assert_eq!(free_name(".bashrc", false, taken(&[".bashrc"])), ".bashrc (2)");
    }

    #[test]
    fn manifest_of_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Album");
        fs::create_dir_all(album.join("cd1")).unwrap();
        fs::create_dir_all(album.join("empty")).unwrap();
        fs::write(album.join("cd1/a.flac"), b"hello").unwrap();
        fs::write(album.join("cover.jpg"), vec![7u8; 100_000]).unwrap();
        let (m, sources) = build(&[album]).unwrap();
        assert_eq!(m.roots, vec![Root { name: "Album".into(), is_dir: true }]);
        assert_eq!(m.dirs, vec!["Album/cd1", "Album/empty"]);
        let paths: Vec<_> = m.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["Album/cd1/a.flac", "Album/cover.jpg"]);
        assert_eq!(m.files[0].hash, *blake3::hash(b"hello").as_bytes());
        assert_eq!(m.files[1].hash, *blake3::hash(&[7u8; 100_000]).as_bytes());
        assert_eq!(sources.len(), 2);
        assert!(m.validate().is_ok());
        assert_eq!(m.totals(), Totals { folders: 1, files: 2, bytes: 100_005 });
    }
}
