//! The demo user's home folder: the usual XDG folders and a few documents,
//! from `home/` beside this crate's manifest.

use std::time::Duration;

use super::{Node, Tree, insert, now};

pub(crate) const HOME: &str = "/home/demo";

/// Files under `home/`, each with how many hours ago it last changed, so
/// sorting by date means something.
const FILES: [(&str, &[u8], u64); 8] = [
    (
        "Desktop/Readme.txt",
        include_bytes!("../../home/Desktop/Readme.txt"),
        2,
    ),
    (
        "Documents/Welcome.md",
        include_bytes!("../../home/Documents/Welcome.md"),
        1,
    ),
    (
        "Documents/Notes.txt",
        include_bytes!("../../home/Documents/Notes.txt"),
        5,
    ),
    (
        "Documents/Projects/derisk.md",
        include_bytes!("../../home/Documents/Projects/derisk.md"),
        30,
    ),
    (
        "Documents/Projects/losos.md",
        include_bytes!("../../home/Documents/Projects/losos.md"),
        52,
    ),
    (
        "Documents/Recipes/Salmon with dill.txt",
        include_bytes!("../../home/Documents/Recipes/Salmon with dill.txt"),
        200,
    ),
    (
        "Downloads/release-notes.txt",
        include_bytes!("../../home/Downloads/release-notes.txt"),
        20,
    ),
    (
        "Templates/Empty document.txt",
        include_bytes!("../../home/Templates/Empty document.txt"),
        700,
    ),
];

/// Folders that start empty, which git cannot keep under `home/`.
const EMPTY: [&str; 4] = ["Music", "Pictures", "Videos", ".config"];

pub(crate) fn seed(tree: &mut Tree) {
    let week_ago = now() - Duration::from_secs(7 * 24 * 3600);
    let mut home = Node::dir(0o700);
    home.modified = week_ago;
    insert(tree, HOME, home);
    for dir in EMPTY {
        let mut empty = Node::dir(0o755);
        empty.modified = week_ago;
        insert(tree, &format!("{HOME}/{dir}"), empty);
    }
    for (path, data, hours) in FILES {
        let mut node = Node::file(data, 0o644);
        node.modified = now() - Duration::from_secs(hours * 3600);
        insert(tree, &format!("{HOME}/{path}"), node);
    }
}
