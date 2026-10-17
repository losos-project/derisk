//! Files: the files and folders the host indexed under home, opened with
//! their default app, and for typed text the files whose names or text
//! match it, from derisk's file index, with the line that matched. The
//! only bundled plugin that sees file names or what is in the files.

use derisk_palette_sdk::{Category, Entry, File, Guest, Hook, Input, Manifest, View, export, json};

struct Files;

impl Guest for Files {
    fn describe() -> Manifest {
        Manifest::new("files", "Files and folders under home, by name and by text")
            .hooks(&[Hook::Entries, Hook::Query])
            .inputs(&[Input::Files, Input::Contents])
            .categories(&[Category::File])
            .actions(&["open"])
    }

    fn entries(view: View) -> Vec<Entry> {
        entries(&view)
    }

    fn query(view: View, _: String) -> Vec<Entry> {
        query(&view)
    }
}

/// The catalog, apart from the export so tests can call it.
pub fn entries(view: &View) -> Vec<Entry> {
    view.files
        .iter()
        .map(|file| row(file).detail(file.shown.clone()))
        .collect()
}

/// The rows for the typed text: what the index matched, the line that
/// matched beside the folder it is in. The palette lists them with the
/// catalog's file rows and leaves out one the catalog already has.
pub fn query(view: &View) -> Vec<Entry> {
    view.file_matches
        .iter()
        .map(|found| {
            let detail = match &found.snippet {
                Some(snippet) => format!("{} · {snippet}", folder(&found.file.shown)),
                None => found.file.shown.clone(),
            };
            row(&found.file).detail(detail)
        })
        .collect()
}

/// A row opening `file`, named after it.
fn row(file: &File) -> Entry {
    let icon = if file.folder {
        "folder"
    } else {
        "text-x-generic"
    };
    Entry::new(
        Category::File,
        icon,
        name(&file.path),
        &[json!({"action": "open", "path": file.path})],
    )
}

/// The last part of `path`.
fn name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or(path)
}

/// The folder `shown` is in, as shown: `~/notes` for `~/notes/todo.txt`.
fn folder(shown: &str) -> &str {
    match shown.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) => "/",
        Some((parent, _)) => parent,
        None => shown,
    }
}

export!(Files with_types_in derisk_palette_sdk::bindings);

#[cfg(test)]
mod tests {
    use derisk_palette_sdk::FileMatch;

    use super::*;

    fn todo() -> File {
        File {
            path: "/home/me/notes/todo.txt".into(),
            shown: "~/notes/todo.txt".into(),
            folder: false,
        }
    }

    #[test]
    fn rows_are_named_after_the_file() {
        let view = View {
            files: vec![todo()],
            ..View::default()
        };
        let rows = entries(&view);
        assert_eq!(rows[0].title, "todo.txt");
        assert_eq!(rows[0].detail, "~/notes/todo.txt");
        assert_eq!(rows[0].icon, "text-x-generic");
    }

    #[test]
    fn a_match_in_the_text_shows_the_line_and_the_folder() {
        let view = View {
            file_matches: vec![
                FileMatch {
                    file: todo(),
                    line: Some(3),
                    snippet: Some("buy milk".into()),
                },
                FileMatch {
                    file: File {
                        path: "/home/me/milk".into(),
                        shown: "~/milk".into(),
                        folder: true,
                    },
                    line: None,
                    snippet: None,
                },
            ],
            ..View::default()
        };
        let rows = query(&view);
        assert_eq!(rows[0].title, "todo.txt");
        assert_eq!(rows[0].detail, "~/notes · buy milk");
        assert_eq!(
            rows[0].parsed_actions(),
            [json!({"action": "open", "path": "/home/me/notes/todo.txt"})]
        );
        assert_eq!(rows[1].title, "milk");
        assert_eq!(rows[1].detail, "~/milk");
        assert_eq!(rows[1].icon, "folder");
    }

    #[test]
    fn folders_are_shown_as_given() {
        assert_eq!(folder("~/notes/todo.txt"), "~/notes");
        assert_eq!(folder("~/todo.txt"), "~");
        assert_eq!(folder("/srv/todo.txt"), "/srv");
        assert_eq!(folder("/todo.txt"), "/");
    }
}
