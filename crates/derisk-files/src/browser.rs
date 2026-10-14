use std::{
    collections::BTreeSet,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use derisk_sys::fs;

use crate::fs_ops::{self, Entry, Kind, Trash};

/// Column the listing is sorted by. Folders always come first.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    /// Case-insensitive name.
    #[default]
    Name,
    /// Size in bytes.
    Size,
    /// Modification time.
    Modified,
}

/// Whether a paste copies or moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardOp {
    /// Leave the originals in place.
    Copy,
    /// Move the originals; the clipboard empties after pasting.
    Cut,
}

/// The file manager's state, independent of any UI.
#[derive(Debug)]
pub struct Browser {
    cwd: PathBuf,
    entries: Vec<Entry>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Show dot files.
    pub show_hidden: bool,
    sort: SortKey,
    descending: bool,
    /// Case-insensitive substring filter on names.
    pub filter: String,
    selection: BTreeSet<PathBuf>,
    clipboard: Option<(ClipboardOp, Vec<PathBuf>)>,
    trash: Option<Trash>,
}

impl Browser {
    /// Opens `dir`, using the home trash.
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        Self::with_trash(dir, Trash::home())
    }

    /// Opens `dir` with an explicit trash (or none, which disables trashing).
    pub fn with_trash(dir: impl Into<PathBuf>, trash: Option<Trash>) -> io::Result<Self> {
        let cwd = derisk_sys::path::absolute(dir.into())?;
        let entries = fs_ops::list(&cwd)?;
        let mut browser = Self {
            cwd,
            entries,
            back: Vec::new(),
            forward: Vec::new(),
            show_hidden: false,
            sort: SortKey::Name,
            descending: false,
            filter: String::new(),
            selection: BTreeSet::new(),
            clipboard: None,
            trash,
        };
        browser.sort_entries();
        Ok(browser)
    }

    /// The current directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The trash, when available.
    pub fn trash(&self) -> Option<&Trash> {
        self.trash.as_ref()
    }

    /// Entries to show: sorted, filtered, and without dot files unless
    /// [`Browser::show_hidden`] is set.
    pub fn visible(&self) -> impl Iterator<Item = &Entry> {
        let filter = self.filter.to_lowercase();
        self.entries.iter().filter(move |entry| {
            (self.show_hidden || !entry.is_hidden())
                && (filter.is_empty() || entry.name.to_lowercase().contains(&filter))
        })
    }

    /// Current sort column and direction.
    pub fn sort(&self) -> (SortKey, bool) {
        (self.sort, self.descending)
    }

    /// Sorts by `key`; choosing the current key again flips the direction.
    pub fn sort_by(&mut self, key: SortKey) {
        if self.sort == key {
            self.descending = !self.descending;
        } else {
            self.sort = key;
            self.descending = false;
        }
        self.sort_entries();
    }

    fn sort_entries(&mut self) {
        let (key, descending) = (self.sort, self.descending);
        self.entries.sort_by(|a, b| {
            let folders_first = (a.kind != Kind::Directory).cmp(&(b.kind != Kind::Directory));
            let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
            let order = match key {
                SortKey::Name => by_name(),
                SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
                SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
            };
            folders_first.then(if descending { order.reverse() } else { order })
        });
    }

    /// Re-reads the current directory, keeping the selection that still exists.
    pub fn refresh(&mut self) -> io::Result<()> {
        self.entries = fs_ops::list(&self.cwd)?;
        self.sort_entries();
        let entries = &self.entries;
        self.selection
            .retain(|path| entries.iter().any(|entry| &entry.path == path));
        Ok(())
    }

    /// Opens a directory, recording history. On error nothing changes.
    pub fn navigate(&mut self, dir: impl AsRef<Path>) -> io::Result<()> {
        let dir = derisk_sys::path::absolute(dir.as_ref())?;
        self.enter(dir)?;
        self.forward.clear();
        Ok(())
    }

    fn enter(&mut self, dir: PathBuf) -> io::Result<()> {
        let mut entries = fs_ops::list(&dir)?;
        std::mem::swap(&mut self.entries, &mut entries);
        let previous = std::mem::replace(&mut self.cwd, dir);
        self.back.push(previous);
        self.selection.clear();
        self.filter.clear();
        self.sort_entries();
        Ok(())
    }

    /// Whether [`Browser::go_back`] would do anything.
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    /// Whether [`Browser::go_forward`] would do anything.
    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Returns to the previous directory.
    pub fn go_back(&mut self) -> io::Result<()> {
        let Some(dir) = self.back.pop() else {
            return Ok(());
        };
        let here = self.cwd.clone();
        match self.enter(dir.clone()) {
            Ok(()) => {
                // `enter` recorded `here` as back history; it belongs ahead.
                self.back.pop();
                self.forward.push(here);
                Ok(())
            }
            Err(error) => {
                self.back.push(dir);
                Err(error)
            }
        }
    }

    /// Undoes [`Browser::go_back`].
    pub fn go_forward(&mut self) -> io::Result<()> {
        let Some(dir) = self.forward.pop() else {
            return Ok(());
        };
        self.enter(dir.clone())
            .inspect_err(|_| self.forward.push(dir))
    }

    /// Opens the parent directory.
    pub fn go_up(&mut self) -> io::Result<()> {
        match self.cwd.parent().map(Path::to_path_buf) {
            Some(parent) => self.navigate(parent),
            None => Ok(()),
        }
    }

    /// Selected paths.
    pub fn selection(&self) -> &BTreeSet<PathBuf> {
        &self.selection
    }

    /// Whether `path` is selected.
    pub fn is_selected(&self, path: &Path) -> bool {
        self.selection.contains(path)
    }

    /// Selects only `path`, or toggles it when `extend` is set.
    pub fn select(&mut self, path: &Path, extend: bool) {
        if extend {
            if !self.selection.remove(path) {
                self.selection.insert(path.to_owned());
            }
        } else {
            self.selection.clear();
            self.selection.insert(path.to_owned());
        }
    }

    /// Selects every visible entry.
    pub fn select_all(&mut self) {
        self.selection = self.visible().map(|entry| entry.path.clone()).collect();
    }

    /// Clears the selection.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Creates an empty folder in the current directory.
    pub fn create_folder(&mut self, name: &str) -> io::Result<PathBuf> {
        let path = self.cwd.join(fs_ops::validate_name(name).map_err(invalid)?);
        fs::create_dir(&path)?;
        self.refresh()?;
        self.selection = BTreeSet::from([path.clone()]);
        Ok(path)
    }

    /// Creates an empty file in the current directory.
    pub fn create_file(&mut self, name: &str) -> io::Result<PathBuf> {
        let path = self.cwd.join(fs_ops::validate_name(name).map_err(invalid)?);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        self.refresh()?;
        self.selection = BTreeSet::from([path.clone()]);
        Ok(path)
    }

    /// Renames an entry within its directory. Never overwrites.
    pub fn rename(&mut self, path: &Path, new_name: &str) -> io::Result<PathBuf> {
        let dir = path.parent().unwrap_or(&self.cwd);
        let target = dir.join(fs_ops::validate_name(new_name).map_err(invalid)?);
        if target != path {
            fs_ops::move_path(path, &target)?;
        }
        self.refresh()?;
        self.selection = BTreeSet::from([target.clone()]);
        Ok(target)
    }

    /// Moves the selection to the trash. Returns how many entries moved;
    /// stops at the first failure.
    pub fn trash_selection(&mut self) -> io::Result<usize> {
        let trash = self
            .trash
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no trash available"))?;
        let mut moved = 0;
        let result = self.selection.iter().try_for_each(|path| {
            trash.put(path)?;
            moved += 1;
            Ok(())
        });
        self.refresh()?;
        result.map(|()| moved)
    }

    /// Puts the selection on the clipboard.
    pub fn set_clipboard(&mut self, op: ClipboardOp) {
        if !self.selection.is_empty() {
            self.clipboard = Some((op, self.selection.iter().cloned().collect()));
        }
    }

    /// The clipboard's operation and paths.
    pub fn clipboard(&self) -> Option<(ClipboardOp, &[PathBuf])> {
        self.clipboard
            .as_ref()
            .map(|(op, paths)| (*op, paths.as_slice()))
    }

    /// Pastes the clipboard into the current directory, renaming on name
    /// clashes. Cut entries pasted into their own folder stay put and stay on
    /// the clipboard. Returns
    /// the new paths; stops at the first failure.
    pub fn paste(&mut self) -> io::Result<Vec<PathBuf>> {
        let Some((op, paths)) = self.clipboard.clone() else {
            return Ok(Vec::new());
        };
        let mut pasted = Vec::new();
        let mut moved = false;
        let result = paths.iter().try_for_each(|from| {
            let name: OsString = from
                .file_name()
                .ok_or_else(|| invalid("no file name"))?
                .into();
            if op == ClipboardOp::Cut && from.parent() == Some(self.cwd.as_path()) {
                pasted.push(from.clone());
                return Ok(());
            }
            let to = fs_ops::unused_path(&self.cwd, &name);
            match op {
                ClipboardOp::Copy => fs_ops::copy_recursive(from, &to)?,
                ClipboardOp::Cut => {
                    fs_ops::move_path(from, &to)?;
                    moved = true;
                }
            }
            pasted.push(to);
            Ok(())
        });
        // A cut is used up once it moves something; pasting it back into its
        // own folder keeps it for pasting elsewhere.
        if moved && result.is_ok() {
            self.clipboard = None;
        }
        self.refresh()?;
        self.selection = pasted.iter().cloned().collect();
        result.map(|()| pasted)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
