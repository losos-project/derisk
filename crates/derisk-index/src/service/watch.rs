//! Keeping the index in step with home and the other folders indexed: one
//! walk at the start, then a shallow walk of each folder inotify says
//! something changed in.
//!
//! inotify only says which folder to look at again. Looking again compares
//! each entry's stamp with the one it was indexed with, so a missed or
//! merged event costs nothing but a later look, a move is a removal from
//! one folder and an addition to another, and when the kernel's queue
//! overflows the whole of home is walked again, which skips everything
//! unchanged.

use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use ignore::WalkBuilder;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask, Watches};
use tantivy::IndexWriter;
use tracing::{debug, info, warn};

use super::{
    store::{Error, Stamp, Store},
    text,
};

/// What each folder is watched for: entries made, written, removed or
/// moved, and the folder itself going.
fn mask() -> WatchMask {
    WatchMask::CREATE
        | WatchMask::CLOSE_WRITE
        | WatchMask::DELETE
        | WatchMask::MOVED_FROM
        | WatchMask::MOVED_TO
        | WatchMask::DELETE_SELF
        | WatchMask::MOVE_SELF
        | WatchMask::ONLYDIR
        | WatchMask::DONT_FOLLOW
        | WatchMask::EXCL_UNLINK
}

/// One inotify event, as the reading thread passes it on.
#[derive(Debug)]
pub struct Change {
    wd: i32,
    mask: EventMask,
    name: Option<OsString>,
}

/// Reads inotify's events and passes them on until `changes` is dropped.
/// On its own thread, since reading blocks.
pub fn read(mut inotify: Inotify, changes: &Sender<Change>) {
    let mut buffer = [0; 64 << 10];
    loop {
        let events = match inotify.read_events_blocking(&mut buffer) {
            Ok(events) => events,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                warn!(error = %e, "cannot read what changed under home");
                return;
            }
        };
        for event in events {
            let change = Change {
                wd: event.wd.get_watch_descriptor_id(),
                mask: event.mask,
                name: event.name.map(ToOwned::to_owned),
            };
            if changes.send(change).is_err() {
                return;
            }
        }
    }
}

/// How long the folders that changed are gathered after the first change,
/// so a program writing many files is looked at once.
const SETTLE: Duration = Duration::from_millis(300);

/// The most a stream of changes is gathered before it is looked at anyway.
const GATHER: Duration = Duration::from_secs(2);

/// Entries indexed between commits on a long walk, so the first one is
/// searchable as it goes rather than at its end.
const BATCH: usize = 2000;

/// The ignore files a walk reads. When one changes, everything under its
/// folder is looked at again, not only the folder.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// The one writer of the index.
pub struct Indexer<'a> {
    store: &'a Store,
    writer: IndexWriter,
    watches: Watches,
    /// Each watched folder by its watch, and the watch by the folder.
    folders: HashMap<i32, (WatchDescriptor, PathBuf)>,
    watched: HashMap<PathBuf, i32>,
    /// Whether the last watch could not be added, said once.
    full: bool,
    /// Entries written since the last commit.
    pending: usize,
}

impl<'a> Indexer<'a> {
    /// The indexer of `store`, adding its watches to `watches`.
    ///
    /// # Errors
    ///
    /// When the index's writer cannot be had.
    pub fn new(store: &'a Store, watches: Watches) -> Result<Self, Error> {
        Ok(Self {
            store,
            writer: store.writer()?,
            watches,
            folders: HashMap::new(),
            watched: HashMap::new(),
            full: false,
            pending: 0,
        })
    }

    /// Walks the whole of home and the other folders indexed, indexing
    /// what is new or changed since the index was last written and
    /// forgetting what is gone, and what is under a folder Settings no
    /// longer names.
    ///
    /// # Errors
    ///
    /// When the index cannot be read or written.
    pub fn walk(&mut self) -> Result<(), Error> {
        let started = Instant::now();
        for dropped in self.store.dropped() {
            info!(folder = %dropped.display(), "no longer indexed");
            for (path, stamp) in self.store.indexed(&dropped, true)? {
                self.store.remove(&self.writer, &path, stamp.folder);
                self.written()?;
            }
        }
        for root in self.store.roots().to_vec() {
            // A disk not mounted yet keeps what was indexed from it until
            // it is, rather than losing it at every login without it.
            if !root.is_dir() {
                info!(folder = %root.display(), "not there to index");
                continue;
            }
            self.watch(&root);
            self.sync(&root, true)?;
        }
        self.commit()?;
        self.store.walked()?;
        info!(
            folders = self.folders.len(),
            seconds = started.elapsed().as_secs_f32(),
            "files are indexed"
        );
        Ok(())
    }

    /// Follows `changes` until the reading thread ends.
    ///
    /// # Errors
    ///
    /// When the index cannot be read or written.
    pub fn follow(&mut self, changes: &Receiver<Change>) -> Result<(), Error> {
        while let Ok(first) = changes.recv() {
            let started = Instant::now();
            // Each folder to look at, and whether to look under it too.
            let mut dirty = BTreeMap::new();
            let mut again = self.note(first, &mut dirty);
            while started.elapsed() < GATHER {
                match changes.recv_timeout(SETTLE) {
                    Ok(change) => again |= self.note(change, &mut dirty),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return self.commit(),
                }
            }
            if again {
                info!("inotify's queue overflowed: walking home again");
                self.walk()?;
                continue;
            }
            for (dir, tree) in dirty {
                self.sync(&dir, tree)?;
            }
            self.commit()?;
        }
        self.commit()
    }

    /// Takes in one change: the folder it happened in goes into `dirty`.
    /// Returns whether changes were lost and home has to be walked again.
    fn note(&mut self, change: Change, dirty: &mut BTreeMap<PathBuf, bool>) -> bool {
        if change.mask.contains(EventMask::Q_OVERFLOW) {
            return true;
        }
        if change.mask.contains(EventMask::IGNORED) {
            // The watch is gone with its folder; the folder's parent says
            // so too, and is looked at for it.
            if let Some((_, dir)) = self.folders.remove(&change.wd) {
                self.watched.remove(&dir);
            }
            return false;
        }
        let Some((_, dir)) = self.folders.get(&change.wd) else {
            return false;
        };
        if change
            .mask
            .intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF)
        {
            if let Some(parent) = dir.parent().filter(|p| self.indexed(p)) {
                dirty.entry(parent.to_owned()).or_insert(false);
            }
            return false;
        }
        let tree = change
            .name
            .as_deref()
            .is_some_and(|n| IGNORE_FILES.iter().any(|i| n == *i));
        *dirty.entry(dir.clone()).or_insert(false) |= tree;
        false
    }

    /// Watches `dir`. A folder that was moved keeps its watch, which then
    /// names it at its new path.
    fn watch(&mut self, dir: &Path) {
        match self.watches.add(dir, mask()) {
            Ok(wd) => {
                let id = wd.get_watch_descriptor_id();
                if let Some((_, old)) = self.folders.insert(id, (wd, dir.to_owned())) {
                    self.watched.remove(&old);
                }
                self.watched.insert(dir.to_owned(), id);
            }
            // fs.inotify.max_user_watches: what is past it is indexed but
            // only looked at again on the next walk.
            Err(e) if e.raw_os_error() == Some(28) => {
                if !self.full {
                    self.full = true;
                    warn!(
                        watched = self.folders.len(),
                        "out of inotify watches: changes in further folders show at the next login"
                    );
                }
            }
            Err(e) => debug!(error = %e, dir = %dir.display(), "folder not watched"),
        }
    }

    /// Stops watching `dir` and every folder under it.
    fn unwatch(&mut self, dir: &Path) {
        let gone: Vec<PathBuf> = self
            .watched
            .keys()
            .filter(|d| d.starts_with(dir))
            .cloned()
            .collect();
        for d in gone {
            if let Some(id) = self.watched.remove(&d)
                && let Some((wd, _)) = self.folders.remove(&id)
            {
                // Fails when the folder is already gone, which removed the
                // watch with it.
                let _ = self.watches.remove(wd);
            }
        }
    }

    /// Brings `dir`'s entries in the index up to date: its direct entries,
    /// or with `tree` everything under it. Hidden entries, symlinks and
    /// what ignore files leave out are not indexed, and are forgotten if
    /// they were.
    fn sync(&mut self, dir: &Path, tree: bool) -> Result<(), Error> {
        let mut indexed = self.store.indexed(dir, tree)?;
        let mut walk = WalkBuilder::new(dir);
        walk.max_depth((!tree).then_some(1))
            // Ignore files in the folders above apply, as they do on the
            // first walk, which started above this one.
            .parents(true)
            .follow_links(false);
        let mut new_folders = Vec::new();
        for entry in walk.build() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    debug!(error = %e, "not indexed");
                    continue;
                }
            };
            if entry.depth() == 0 {
                continue;
            }
            let path = entry.path();
            let Some(kind) = entry.file_type() else {
                continue;
            };
            if !(kind.is_dir() || kind.is_file()) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let stamp = Stamp::of(&metadata);
            let was = indexed.remove(path);
            if stamp.folder {
                self.watch(path);
                // A folder new to a shallow look has never been walked.
                if was.is_none() && !tree {
                    new_folders.push(path.to_owned());
                }
            }
            if was == Some(stamp) {
                continue;
            }
            let text = if stamp.folder || stamp.size == 0 {
                None
            } else {
                text::read(path).unwrap_or_else(|e| {
                    debug!(error = %e, path = %path.display(), "text not read");
                    None
                })
            };
            self.store.put(&self.writer, path, stamp, text.as_deref())?;
            self.written()?;
        }
        // What is indexed and was not walked is gone, hidden, a symlink now
        // or ignored, unless another folder indexed holds it: a hidden one
        // in home that Settings names is walked from there.
        for (path, stamp) in indexed {
            if self.claimed(dir, &path) {
                continue;
            }
            self.store.remove(&self.writer, &path, stamp.folder);
            if stamp.folder {
                self.unwatch(&path);
            }
            self.written()?;
        }
        for folder in new_folders {
            self.sync(&folder, true)?;
        }
        Ok(())
    }

    /// Whether `dir` is in a folder indexed.
    fn indexed(&self, dir: &Path) -> bool {
        self.store.roots().iter().any(|root| dir.starts_with(root))
    }

    /// Whether `path` is under a folder indexed in its own right inside
    /// `dir`, whose own walk keeps it.
    fn claimed(&self, dir: &Path, path: &Path) -> bool {
        self.store
            .roots()
            .iter()
            .any(|root| root != dir && root.starts_with(dir) && path.starts_with(root))
    }

    /// Counts one entry written, committing every [`BATCH`].
    fn written(&mut self) -> Result<(), Error> {
        self.pending += 1;
        if self.pending >= BATCH {
            self.commit()?;
        }
        Ok(())
    }

    /// Commits what was written and makes it searchable.
    fn commit(&mut self) -> Result<(), Error> {
        if self.pending == 0 {
            return Ok(());
        }
        debug!(entries = self.pending, "committing");
        self.writer.commit()?;
        self.store.reload()?;
        self.pending = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fs};

    use crossbeam_channel::bounded;

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> (Scratch, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("derisk-index-watch-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let home = dir.join("home");
        fs::create_dir_all(home.join("notes")).unwrap();
        fs::create_dir_all(home.join(".secret")).unwrap();
        fs::create_dir_all(home.join("code/target")).unwrap();
        fs::write(home.join("notes/a.txt"), "alpha").unwrap();
        fs::write(home.join(".secret/key"), "beta").unwrap();
        fs::write(home.join("code/.ignore"), "target/\n").unwrap();
        fs::write(home.join("code/target/out.txt"), "gamma").unwrap();
        fs::write(home.join("code/main.rs"), "fn main() {}").unwrap();
        (Scratch(dir), home)
    }

    /// What the index holds, relative to home.
    fn indexed(store: &Store) -> HashSet<PathBuf> {
        store
            .indexed(store.home(), true)
            .unwrap()
            .into_keys()
            .map(|p| p.strip_prefix(store.home()).unwrap().to_owned())
            .collect()
    }

    fn set(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn the_walk_skips_hidden_and_ignored_entries() {
        let (dir, home) = scratch("walk");
        let store = Store::open(&dir.0.join("index"), &home).unwrap();
        let inotify = Inotify::init().unwrap();
        let mut indexer = Indexer::new(&store, inotify.watches()).unwrap();
        indexer.walk().unwrap();
        assert_eq!(
            indexed(&store),
            set(&["notes", "notes/a.txt", "code", "code/main.rs"])
        );
        assert_eq!(indexer.folders.len(), 3, "home, notes and code are watched");
    }

    #[test]
    fn other_folders_are_indexed_until_dropped() {
        let (dir, home) = scratch("roots");
        let index = dir.0.join("index");
        let data = dir.0.join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("report.txt"), "zeta").unwrap();
        // Hidden, so home's own walk leaves it out and would forget it.
        let secret = home.join(".secret");
        let paths = |store: &Store, text| -> Vec<PathBuf> {
            let found = store.search(text, 5).unwrap();
            found.into_iter().map(|m| m.path).collect()
        };
        {
            let store = Store::open_with(&index, &home, &[data.clone(), secret.clone()]).unwrap();
            let inotify = Inotify::init().unwrap();
            let mut indexer = Indexer::new(&store, inotify.watches()).unwrap();
            indexer.walk().unwrap();
            assert_eq!(paths(&store, "zeta"), [data.join("report.txt")]);
            assert_eq!(paths(&store, "data report"), [data.join("report.txt")]);
            indexer.walk().unwrap();
            assert_eq!(paths(&store, "beta"), [secret.join("key")]);
        }
        let store = Store::open(&index, &home).unwrap();
        let inotify = Inotify::init().unwrap();
        Indexer::new(&store, inotify.watches())
            .unwrap()
            .walk()
            .unwrap();
        assert!(paths(&store, "zeta").is_empty());
        assert!(paths(&store, "beta").is_empty());
    }

    #[test]
    fn changes_are_followed() {
        let (dir, home) = scratch("follow");
        let store = Store::open(&dir.0.join("index"), &home).unwrap();
        let mut inotify = Inotify::init().unwrap();
        let mut indexer = Indexer::new(&store, inotify.watches()).unwrap();
        indexer.walk().unwrap();

        let (tx, rx) = bounded(1024);
        std::thread::scope(|s| {
            s.spawn(|| indexer.follow(&rx).unwrap());
            fs::write(home.join("notes/b.txt"), "delta").unwrap();
            fs::create_dir_all(home.join("new/deep")).unwrap();
            fs::write(home.join("new/deep/c.txt"), "epsilon").unwrap();
            fs::rename(home.join("code"), home.join("work")).unwrap();
            fs::remove_file(home.join("notes/a.txt")).unwrap();
            let want = set(&[
                "notes",
                "notes/b.txt",
                "new",
                "new/deep",
                "new/deep/c.txt",
                "work",
                "work/main.rs",
            ]);
            // What `read` does on its thread in the service, here without
            // blocking so the test can stop.
            let mut buffer = [0; 4096];
            let deadline = Instant::now() + Duration::from_secs(10);
            while indexed(&store) != want {
                assert!(Instant::now() < deadline, "index has {:?}", indexed(&store));
                if let Ok(events) = inotify.read_events(&mut buffer) {
                    for event in events {
                        tx.send(Change {
                            wd: event.wd.get_watch_descriptor_id(),
                            mask: event.mask,
                            name: event.name.map(ToOwned::to_owned),
                        })
                        .unwrap();
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            drop(tx);
        });
        let found = store.search("epsilon", 5).unwrap();
        assert_eq!(found[0].path, home.join("new/deep/c.txt"));
    }
}
