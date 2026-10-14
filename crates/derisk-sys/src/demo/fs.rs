//! The demo system's `std::fs`: the same names and signatures, as far as
//! the apps use them, over the in-memory tree.
//!
//! There are no links, owners or devices: a rename never crosses
//! filesystems, and modes are kept but never checked.

use std::{
    ffi::OsString,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use super::{Body, Node, Tree, elapsed, normalize, now, with};

fn not_found() -> io::Error {
    io::ErrorKind::NotFound.into()
}

/// The parent of `path` must exist and be a folder for anything to be made
/// in it.
fn check_parent(tree: &Tree, path: &Path) -> io::Result<()> {
    match path.parent().map(|parent| tree.get(parent)) {
        // `/` itself.
        None => Ok(()),
        Some(Some(node)) if node.is_dir() => Ok(()),
        Some(Some(_)) => Err(io::ErrorKind::NotADirectory.into()),
        Some(None) => Err(not_found()),
    }
}

/// The paths directly inside `dir`.
fn children(tree: &Tree, dir: &Path) -> Vec<PathBuf> {
    tree.range(dir.to_owned()..)
        .skip(1)
        .take_while(|(path, _)| path.starts_with(dir))
        .filter(|(path, _)| path.parent() == Some(dir))
        .map(|(path, _)| path.clone())
        .collect()
}

/// `dir` and everything under it.
fn subtree(tree: &Tree, dir: &Path) -> Vec<PathBuf> {
    tree.range(dir.to_owned()..)
        .take_while(|(path, _)| path.starts_with(dir))
        .map(|(path, _)| path.clone())
        .collect()
}

fn contents(node: &Node) -> io::Result<Vec<u8>> {
    match &node.body {
        Body::Dir => Err(io::ErrorKind::IsADirectory.into()),
        Body::File(data) => Ok(data.clone()),
        Body::Proc(write) => Ok(write(elapsed())),
    }
}

/// What a path is. There are no links, so `is_symlink` is always false.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileType {
    dir: bool,
}

impl FileType {
    /// A folder.
    pub fn is_dir(&self) -> bool {
        self.dir
    }

    /// A regular file.
    pub fn is_file(&self) -> bool {
        !self.dir
    }

    /// Never: the demo system has no links.
    pub fn is_symlink(&self) -> bool {
        false
    }
}

/// A file's mode bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Permissions {
    mode: u32,
}

impl Permissions {
    /// Whether nobody may write it.
    pub fn readonly(&self) -> bool {
        self.mode & 0o222 == 0
    }

    /// Takes write permission away from everyone, or gives the owner it.
    pub fn set_readonly(&mut self, readonly: bool) {
        if readonly {
            self.mode &= !0o222;
        } else {
            self.mode |= 0o200;
        }
    }
}

/// `std::os::unix::fs::PermissionsExt` for the demo system.
pub trait PermissionsExt {
    /// The mode bits.
    fn mode(&self) -> u32;
    /// Replaces the mode bits.
    fn set_mode(&mut self, mode: u32);
    /// Permissions with `mode`.
    fn from_mode(mode: u32) -> Self;
}

impl PermissionsExt for Permissions {
    fn mode(&self) -> u32 {
        self.mode
    }

    fn set_mode(&mut self, mode: u32) {
        self.mode = mode;
    }

    fn from_mode(mode: u32) -> Self {
        Self { mode }
    }
}

/// What [`metadata`] reports.
#[derive(Clone, Debug)]
pub struct Metadata {
    file_type: FileType,
    len: u64,
    mode: u32,
    modified: SystemTime,
}

impl Metadata {
    fn of(node: &Node) -> Self {
        let len = match &node.body {
            Body::Dir => 0,
            Body::File(data) => data.len() as u64,
            // As on Linux, where /proc files report no size.
            Body::Proc(_) => 0,
        };
        Self {
            file_type: FileType { dir: node.is_dir() },
            len,
            mode: node.mode,
            modified: node.modified,
        }
    }

    /// What the path is.
    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    /// A folder.
    pub fn is_dir(&self) -> bool {
        self.file_type.is_dir()
    }

    /// A regular file.
    pub fn is_file(&self) -> bool {
        self.file_type.is_file()
    }

    /// Never: the demo system has no links.
    pub fn is_symlink(&self) -> bool {
        false
    }

    /// Size in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the size is zero.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The mode bits.
    pub fn permissions(&self) -> Permissions {
        Permissions { mode: self.mode }
    }

    /// When it last changed.
    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }
}

/// What is at `path`.
pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let path = normalize(path.as_ref());
    with(|tree| tree.get(&path).map(Metadata::of).ok_or_else(not_found))
}

/// [`metadata`]: there are no links not to follow.
pub fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    metadata(path)
}

/// Always an error: the demo system has no links.
pub fn read_link(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    metadata(path)?;
    Err(io::Error::new(io::ErrorKind::InvalidInput, "not a link"))
}

/// `path` made absolute, when something is there.
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = normalize(path.as_ref());
    metadata(&path).map(|_| path)
}

/// One entry of [`read_dir`].
#[derive(Debug)]
pub struct DirEntry {
    path: PathBuf,
}

impl DirEntry {
    /// The entry's full path.
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    /// The entry's name.
    pub fn file_name(&self) -> OsString {
        self.path.file_name().unwrap_or_default().to_owned()
    }

    /// What the entry is.
    pub fn metadata(&self) -> io::Result<Metadata> {
        metadata(&self.path)
    }

    /// What kind of entry it is.
    pub fn file_type(&self) -> io::Result<FileType> {
        self.metadata().map(|m| m.file_type())
    }
}

/// The entries of a folder as it was when [`read_dir`] was called.
#[derive(Debug)]
pub struct ReadDir(std::vec::IntoIter<PathBuf>);

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|path| Ok(DirEntry { path }))
    }
}

/// Lists a folder.
pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
    let path = normalize(path.as_ref());
    with(|tree| match tree.get(&path) {
        Some(node) if node.is_dir() => Ok(ReadDir(children(tree, &path).into_iter())),
        Some(_) => Err(io::ErrorKind::NotADirectory.into()),
        None => Err(not_found()),
    })
}

/// A file's bytes.
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = normalize(path.as_ref());
    with(|tree| contents(tree.get(&path).ok_or_else(not_found)?))
}

/// A file's text.
pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Replaces a file's contents, making it if it is missing.
pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?
        .write_all(contents.as_ref())
}

/// Makes one folder.
pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    DirBuilder::new().create(path)
}

/// Makes a folder and any missing above it.
pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    DirBuilder::new().recursive(true).create(path)
}

/// Removes a file.
pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref());
    with(|tree| match tree.get(&path) {
        Some(node) if node.is_dir() => Err(io::ErrorKind::IsADirectory.into()),
        Some(_) => {
            tree.remove(&path);
            Ok(())
        }
        None => Err(not_found()),
    })
}

/// Removes an empty folder.
pub fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref());
    with(|tree| match tree.get(&path) {
        Some(node) if !node.is_dir() => Err(io::ErrorKind::NotADirectory.into()),
        Some(_) if !children(tree, &path).is_empty() => {
            Err(io::ErrorKind::DirectoryNotEmpty.into())
        }
        Some(_) => {
            tree.remove(&path);
            Ok(())
        }
        None => Err(not_found()),
    })
}

/// Removes a folder and everything in it.
pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref());
    with(|tree| match tree.get(&path) {
        Some(node) if !node.is_dir() => Err(io::ErrorKind::NotADirectory.into()),
        Some(_) => {
            for gone in subtree(tree, &path) {
                tree.remove(&gone);
            }
            Ok(())
        }
        None => Err(not_found()),
    })
}

/// Moves `from` to `to`, replacing a file or an empty folder there, as
/// `rename(2)` does.
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (from, to) = (normalize(from.as_ref()), normalize(to.as_ref()));
    with(|tree| {
        let moving_dir = tree.get(&from).ok_or_else(not_found)?.is_dir();
        check_parent(tree, &to)?;
        if from == to {
            return Ok(());
        }
        if to.starts_with(&from) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match tree.get(&to) {
            Some(there) if there.is_dir() && !moving_dir => {
                return Err(io::ErrorKind::IsADirectory.into());
            }
            Some(there) if !there.is_dir() && moving_dir => {
                return Err(io::ErrorKind::NotADirectory.into());
            }
            Some(_) if moving_dir && !children(tree, &to).is_empty() => {
                return Err(io::ErrorKind::DirectoryNotEmpty.into());
            }
            _ => {}
        }
        tree.remove(&to);
        for old in subtree(tree, &from) {
            let node = tree.remove(&old).expect("listed just now");
            let new = to.join(old.strip_prefix(&from).expect("under `from`"));
            tree.insert(new, node);
        }
        Ok(())
    })
}

/// Copies a file's contents and mode, returning how many bytes it copied.
pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let (data, mode) = {
        let from = normalize(from.as_ref());
        with(|tree| {
            let node = tree.get(&from).ok_or_else(not_found)?;
            contents(node).map(|data| (data, node.mode))
        })?
    };
    write(&to, &data)?;
    set_permissions(&to, Permissions { mode })?;
    Ok(data.len() as u64)
}

/// Changes a path's mode bits.
pub fn set_permissions(path: impl AsRef<Path>, permissions: Permissions) -> io::Result<()> {
    let path = normalize(path.as_ref());
    with(|tree| {
        tree.get_mut(&path).ok_or_else(not_found)?.mode = permissions.mode;
        Ok(())
    })
}

/// Makes folders, as `std::fs::DirBuilder` does.
#[derive(Clone, Debug)]
pub struct DirBuilder {
    recursive: bool,
    mode: u32,
}

impl Default for DirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DirBuilder {
    /// Makes one folder, mode `0o755`.
    pub fn new() -> Self {
        Self {
            recursive: false,
            mode: 0o755,
        }
    }

    /// Also makes missing folders above, and accepts one already there.
    pub fn recursive(&mut self, recursive: bool) -> &mut Self {
        self.recursive = recursive;
        self
    }

    /// Makes the folder.
    pub fn create(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = normalize(path.as_ref());
        with(|tree| {
            if self.recursive {
                let missing: Vec<PathBuf> = path
                    .ancestors()
                    .take_while(|p| !tree.contains_key(*p))
                    .map(Path::to_owned)
                    .collect();
                if let Some(top) = missing.last() {
                    check_parent(tree, top)?;
                }
                if tree.get(&path).is_some_and(|node| !node.is_dir()) {
                    return Err(io::ErrorKind::AlreadyExists.into());
                }
                for dir in missing.into_iter().rev() {
                    tree.insert(dir, Node::dir(self.mode));
                }
                Ok(())
            } else if tree.contains_key(&path) {
                Err(io::ErrorKind::AlreadyExists.into())
            } else {
                check_parent(tree, &path)?;
                tree.insert(path, Node::dir(self.mode));
                Ok(())
            }
        })
    }
}

/// `std::os::unix::fs::DirBuilderExt` for the demo system.
pub trait DirBuilderExt {
    /// The mode new folders get.
    fn mode(&mut self, mode: u32) -> &mut Self;
}

impl DirBuilderExt for DirBuilder {
    fn mode(&mut self, mode: u32) -> &mut Self {
        self.mode = mode;
        self
    }
}

/// How to open a file, as `std::fs::OpenOptions` says it.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    mode: Option<u32>,
}

impl OpenOptions {
    /// Nothing allowed yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allows reading.
    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    /// Allows writing.
    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    /// Writes go to the end.
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    /// Empties the file on opening.
    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    /// Makes the file if it is missing.
    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    /// Makes the file, failing if something is there.
    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.create_new = create_new;
        self
    }

    /// Opens `path`.
    pub fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        let path = normalize(path.as_ref());
        let writes = self.write || self.append;
        if !self.read && !writes {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        with(|tree| {
            match tree.get_mut(&path) {
                Some(_) if self.create_new => return Err(io::ErrorKind::AlreadyExists.into()),
                Some(node) if node.is_dir() && writes => {
                    return Err(io::ErrorKind::IsADirectory.into());
                }
                Some(node) => {
                    if self.truncate && writes {
                        node.body = Body::File(Vec::new());
                        node.modified = now();
                    }
                }
                None if self.create || self.create_new => {
                    check_parent(tree, &path)?;
                    // umask 022, as a plain create would get.
                    let mode = self.mode.unwrap_or(0o666) & !0o022;
                    tree.insert(path.clone(), Node::file(Vec::new(), mode));
                }
                None => return Err(not_found()),
            }
            Ok(())
        })?;
        Ok(File {
            path,
            position: 0,
            read: self.read,
            write: writes,
            append: self.append,
        })
    }
}

/// `std::os::unix::fs::OpenOptionsExt` for the demo system.
pub trait OpenOptionsExt {
    /// The mode a new file gets.
    fn mode(&mut self, mode: u32) -> &mut Self;
    /// Ignored: no `O_*` flag changes anything here.
    fn custom_flags(&mut self, flags: i32) -> &mut Self;
}

impl OpenOptionsExt for OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        self.mode = Some(mode);
        self
    }

    fn custom_flags(&mut self, _: i32) -> &mut Self {
        self
    }
}

/// An open file. It names its path rather than holding the contents, so
/// what it reads and writes is always the tree's, and a file renamed or
/// removed while open stops working.
#[derive(Debug)]
pub struct File {
    path: PathBuf,
    position: u64,
    read: bool,
    write: bool,
    append: bool,
}

impl File {
    /// Opens `path` for reading.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        OpenOptions::new().read(true).open(path)
    }

    /// Opens `path` for writing, emptying or making it.
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    }

    /// What the file is.
    pub fn metadata(&self) -> io::Result<Metadata> {
        metadata(&self.path)
    }

    /// Changes the file's mode bits.
    pub fn set_permissions(&self, permissions: Permissions) -> io::Result<()> {
        set_permissions(&self.path, permissions)
    }

    /// Nothing to flush: writes land in the tree as they are made.
    pub fn sync_all(&self) -> io::Result<()> {
        self.metadata().map(drop)
    }

    /// [`File::sync_all`].
    pub fn sync_data(&self) -> io::Result<()> {
        self.sync_all()
    }

    /// Cuts or zero-extends the file to `size` bytes.
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        self.with_data(|data| {
            data.resize(size as usize, 0);
            Ok(())
        })
    }

    fn with_data<R>(&self, f: impl FnOnce(&mut Vec<u8>) -> io::Result<R>) -> io::Result<R> {
        if !self.write {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not opened for writing",
            ));
        }
        with(|tree| {
            let node = tree.get_mut(&self.path).ok_or_else(not_found)?;
            let Body::File(data) = &mut node.body else {
                return Err(io::ErrorKind::PermissionDenied.into());
            };
            node.modified = now();
            f(data)
        })
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.read {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not opened for reading",
            ));
        }
        let data = read(&self.path)?;
        let start = (self.position as usize).min(data.len());
        let n = (&data[start..]).read(buf)?;
        self.position += n as u64;
        Ok(n)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (append, position) = (self.append, self.position as usize);
        let end = self.with_data(|data| {
            let start = if append { data.len() } else { position };
            if data.len() < start {
                data.resize(start, 0);
            }
            let overlap = (data.len() - start).min(buf.len());
            data[start..start + overlap].copy_from_slice(&buf[..overlap]);
            data.extend_from_slice(&buf[overlap..]);
            Ok(start + buf.len())
        })?;
        self.position = end as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for File {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.metadata()?.len();
        let position = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => len.checked_add_signed(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
        };
        self.position = position.ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}
