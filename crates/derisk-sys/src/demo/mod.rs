//! The demo system a browser build runs on: one in-memory tree of files,
//! seeded on first use, plus the environment and clock std lacks there.
//!
//! The page has one thread, so the tree lives in a thread-local and every
//! call borrows it for as long as the call takes, never across calls.

pub mod fs;
mod home;
mod proc;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// What a path holds.
pub(crate) enum Body {
    Dir,
    File(Vec<u8>),
    /// A `/proc` or `/dev` file, written out from the seconds since the
    /// page loaded each time it is read.
    Proc(Box<dyn Fn(f64) -> Vec<u8>>),
}

pub(crate) struct Node {
    pub body: Body,
    pub mode: u32,
    pub modified: SystemTime,
}

impl Node {
    pub fn dir(mode: u32) -> Self {
        Self {
            body: Body::Dir,
            mode,
            modified: now(),
        }
    }

    pub fn file(data: impl Into<Vec<u8>>, mode: u32) -> Self {
        Self {
            body: Body::File(data.into()),
            mode,
            modified: now(),
        }
    }

    pub fn generated(write: impl Fn(f64) -> Vec<u8> + 'static) -> Self {
        Self {
            body: Body::Proc(Box::new(write)),
            mode: 0o444,
            modified: now(),
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.body, Body::Dir)
    }
}

/// Every path in the demo system, absolute and normalized, so a folder's
/// contents are the keys that follow it with it as their parent.
pub(crate) type Tree = BTreeMap<PathBuf, Node>;

thread_local! {
    static TREE: RefCell<Tree> = RefCell::new(seed());
    static LOADED: web_time::Instant = web_time::Instant::now();
}

/// Runs `f` with the tree borrowed.
pub(crate) fn with<R>(f: impl FnOnce(&mut Tree) -> R) -> R {
    TREE.with(|tree| f(&mut tree.borrow_mut()))
}

fn seed() -> Tree {
    let mut tree = Tree::new();
    tree.insert(PathBuf::from("/"), Node::dir(0o755));
    home::seed(&mut tree);
    proc::seed(&mut tree);
    insert(
        &mut tree,
        "/etc/hostname",
        Node::file("losos-demo\n", 0o644),
    );
    insert(
        &mut tree,
        "/dev/urandom",
        Node::generated(|_| {
            (0..64)
                .map(|_| (js_sys::Math::random() * 256.0) as u8)
                .collect()
        }),
    );
    tree
}

/// Adds `path` and any folders above it that are missing, which date from
/// a week ago, when the demo user was made.
pub(crate) fn insert(tree: &mut Tree, path: &str, node: Node) {
    let path = PathBuf::from(path);
    for parent in path.ancestors().skip(1) {
        tree.entry(parent.to_owned()).or_insert_with(|| {
            let mut dir = Node::dir(0o755);
            dir.modified = now() - Duration::from_secs(7 * 24 * 3600);
            dir
        });
    }
    tree.insert(path, node);
}

/// `path` made absolute against `/` with `.` and `..` resolved, the form
/// the tree's keys take.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// The page's clock.
pub(crate) fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs_f64(js_sys::Date::now() / 1000.0)
}

/// Seconds since the page loaded.
pub(crate) fn elapsed() -> f64 {
    LOADED.with(|loaded| loaded.elapsed().as_secs_f64())
}

/// The demo system's environment.
pub(crate) fn env(key: &str) -> Option<&'static str> {
    Some(match key {
        "HOME" => home::HOME,
        "USER" | "LOGNAME" => "demo",
        "LANG" => "en_US.UTF-8",
        "XDG_DATA_DIRS" => "/usr/share",
        _ => return None,
    })
}
