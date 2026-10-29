//! Installed applications from freedesktop `.desktop` files, with their
//! [Desktop Actions] (such as Firefox's "New Private Window").
//!
//! The command palette lists every app and every action. derisk's own apps
//! ship `.desktop` files too (in `derisk-apps`), so their actions come from
//! the same place.
//!
//! [Desktop Actions]: https://specifications.freedesktop.org/desktop-entry-spec/latest/extra-actions.html
//!
//! ```
//! use derisk_desktop::desktop::DesktopEntry;
//!
//! let entry = DesktopEntry::parse("firefox.desktop", "\
//! [Desktop Entry]
//! Type=Application
//! Name=Firefox
//! Exec=firefox %u
//! Actions=new-private-window;
//!
//! [Desktop Action new-private-window]
//! Name=New Private Window
//! Exec=firefox --private-window %u
//! ").unwrap();
//! assert_eq!(entry.argv(), ["firefox"]);
//! assert_eq!(entry.actions[0].name, "New Private Window");
//! assert_eq!(entry.action_argv("new-private-window").unwrap(), ["firefox", "--private-window"]);
//! ```

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The name derisk matches against `OnlyShowIn` and `NotShowIn`.
pub const DESKTOP_NAME: &str = "derisk";

/// One `[Desktop Action <id>]` group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopAction {
    /// The action ID from the `Actions` key.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The `Exec` line, unparsed.
    pub exec: String,
}

/// An application `.desktop` file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopEntry {
    /// Desktop file ID, for example `org.mozilla.firefox.desktop`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// `GenericName`, for example "Web Browser".
    pub generic_name: String,
    /// `Comment`.
    pub comment: String,
    /// `Keywords`.
    pub keywords: Vec<String>,
    /// `Icon` (a theme icon name or path), empty if unset.
    pub icon: String,
    /// `StartupWMClass`: the app ID its windows use when that differs from
    /// the desktop file ID, empty if unset.
    pub wm_class: String,
    /// The `Exec` line, unparsed.
    pub exec: String,
    /// Actions in `Actions` order, only those with a group and an `Exec`.
    pub actions: Vec<DesktopAction>,
    /// `NoDisplay`: installed but not meant for menus.
    pub no_display: bool,
    /// `TryExec`: the program the app needs, empty when it names none.
    pub try_exec: String,
    /// `Categories`, main and additional alike.
    pub categories: Vec<String>,
}

/// Categories whose programs want the computer to themselves while they
/// are open: games, and editors for 3D, CAD, video and music. Their
/// windows turn on the OS's focus mode, which holds background work (the
/// OS's docs/focus-mode.md). Not `AudioVideo` alone, which media players
/// carry too.
pub const FOCUS_CATEGORIES: &[&str] = &[
    "Game",
    "3DGraphics",
    "Engineering",
    "AudioVideoEditing",
    "Sequencer",
    "Midi",
];

/// Whether `program`, a `TryExec` value, is an executable file: the path
/// itself when it has a slash, else a file of that name in a `PATH`
/// directory.
fn installed(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| {
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return executable(Path::new(program));
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(program))))
}

/// Unescapes a string value (`\s`, `\n`, `\t`, `\r`, `\\`).
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            // Other backslashes belong to the value (for example Exec
            // quoting) and are kept.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a list value (`a;b;c;`), honoring `\;`.
fn list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => current.push(';'),
                Some(other) => {
                    current.push('\\');
                    current.push(other);
                }
                None => {}
            },
            ';' => items.push(unescape(&std::mem::take(&mut current))),
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        items.push(unescape(&current));
    }
    items.retain(|i| !i.is_empty());
    items
}

/// Splits an `Exec` line into arguments and drops field codes.
///
/// Arguments may be double-quoted, with `\"`, `` \` ``, `\$` and `\\`
/// escapes inside quotes. File and URL codes (`%f %F %u %U`) are dropped
/// because the palette launches without files; `%c` becomes the name,
/// `%i` becomes `--icon <icon>`, and `%%` a literal `%`. Returns an empty
/// list for a malformed line.
pub fn exec_argv(exec: &str, name: &str, icon: &str) -> Vec<String> {
    // Values are string-unescaped first, then split.
    let exec = unescape(exec);
    let mut args: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut in_word = false;
    let mut arg_quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' => match chars.next() {
                    Some(e @ ('"' | '`' | '$' | '\\')) => current.push(e),
                    Some(other) => {
                        current.push('\\');
                        current.push(other);
                    }
                    None => return Vec::new(),
                },
                _ => current.push(c),
            }
            continue;
        }
        match c {
            ' ' | '\t' => {
                if in_word {
                    args.push((std::mem::take(&mut current), arg_quoted));
                    in_word = false;
                    arg_quoted = false;
                }
            }
            '"' => {
                // Field codes are not allowed in quoted arguments, so those
                // are kept literally.
                quoted = true;
                in_word = true;
                arg_quoted = true;
            }
            _ => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if quoted {
        return Vec::new();
    }
    if in_word {
        args.push((current, arg_quoted));
    }

    let mut out = Vec::new();
    for (arg, quoted) in args {
        if quoted {
            out.push(arg);
            continue;
        }
        match arg.as_str() {
            "%f" | "%F" | "%u" | "%U" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "%k" => {}
            "%i" => {
                if !icon.is_empty() {
                    out.push("--icon".to_owned());
                    out.push(icon.to_owned());
                }
            }
            _ => {
                let mut expanded = String::new();
                let mut chars = arg.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        expanded.push(c);
                        continue;
                    }
                    match chars.next() {
                        Some('%') => expanded.push('%'),
                        Some('c') => expanded.push_str(name),
                        // Other codes inside an argument expand to nothing.
                        _ => {}
                    }
                }
                if !expanded.is_empty() {
                    out.push(expanded);
                }
            }
        }
    }
    out
}

impl DesktopEntry {
    /// Parses a `.desktop` file. Returns `None` for anything that is not a
    /// visible-in-derisk application: other types, `Hidden=true`, an
    /// `OnlyShowIn` without derisk or a `NotShowIn` with it, or no `Exec`.
    pub fn parse(id: &str, text: &str) -> Option<Self> {
        let mut entry = Self {
            id: id.to_owned(),
            name: String::new(),
            generic_name: String::new(),
            comment: String::new(),
            keywords: Vec::new(),
            icon: String::new(),
            wm_class: String::new(),
            exec: String::new(),
            actions: Vec::new(),
            no_display: false,
            try_exec: String::new(),
            categories: Vec::new(),
        };
        let mut kind = String::new();
        let mut hidden = false;
        let mut shown_here = true;
        let mut action_ids = Vec::new();
        let mut groups: Vec<DesktopAction> = Vec::new();
        // `None` before any group, `Some(None)` in [Desktop Entry],
        // `Some(Some(i))` in action group `i`.
        let mut group: Option<Option<usize>> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                group = if name == "Desktop Entry" {
                    Some(None)
                } else if let Some(action) = name.strip_prefix("Desktop Action ") {
                    groups.push(DesktopAction {
                        id: action.to_owned(),
                        name: String::new(),
                        exec: String::new(),
                    });
                    Some(Some(groups.len() - 1))
                } else {
                    // Unknown groups (X-...) are skipped.
                    Some(Some(usize::MAX))
                };
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match group {
                Some(None) => match key {
                    "Type" => kind = value.to_owned(),
                    "Name" => entry.name = unescape(value),
                    "GenericName" => entry.generic_name = unescape(value),
                    "Comment" => entry.comment = unescape(value),
                    "Keywords" => entry.keywords = list(value),
                    "Icon" => entry.icon = unescape(value),
                    "StartupWMClass" => entry.wm_class = unescape(value),
                    "Exec" => entry.exec = value.to_owned(),
                    "NoDisplay" => entry.no_display = value == "true",
                    "Hidden" => hidden = value == "true",
                    "TryExec" => entry.try_exec = unescape(value),
                    "Categories" => entry.categories = list(value),
                    "Actions" => action_ids = list(value),
                    "OnlyShowIn" => {
                        shown_here &= list(value).iter().any(|d| d == DESKTOP_NAME);
                    }
                    "NotShowIn" => {
                        shown_here &= !list(value).iter().any(|d| d == DESKTOP_NAME);
                    }
                    _ => {}
                },
                Some(Some(i)) if i < groups.len() => match key {
                    "Name" => groups[i].name = unescape(value),
                    "Exec" => groups[i].exec = value.to_owned(),
                    _ => {}
                },
                _ => {}
            }
        }
        if kind != "Application" || hidden || !shown_here || entry.exec.is_empty() {
            return None;
        }
        if entry.name.is_empty() {
            entry.name = id.trim_end_matches(".desktop").to_owned();
        }
        entry.actions = action_ids
            .iter()
            .filter_map(|id| groups.iter().find(|g| &g.id == id))
            .filter(|a| !a.name.is_empty() && !a.exec.is_empty())
            .cloned()
            .collect();
        Some(entry)
    }

    /// Whether the app is one of [`FOCUS_CATEGORIES`].
    pub fn wants_focus(&self) -> bool {
        self.categories
            .iter()
            .any(|c| FOCUS_CATEGORIES.contains(&c.as_str()))
    }

    /// Whether this system has the program the app needs. One that does
    /// not, as a core app only some systems give a backend to (Android,
    /// which needs the OS's android-app-support), is not listed at all.
    pub fn installed(&self) -> bool {
        self.try_exec.is_empty() || installed(&self.try_exec)
    }

    /// The command line that starts the app.
    pub fn argv(&self) -> Vec<String> {
        exec_argv(&self.exec, &self.name, &self.icon)
    }

    /// The command line for one of the app's actions.
    pub fn action_argv(&self, action: &str) -> Option<Vec<String>> {
        let action = self.actions.iter().find(|a| a.id == action)?;
        Some(exec_argv(&action.exec, &self.name, &self.icon)).filter(|a| !a.is_empty())
    }

    /// Words to search by: generic name, comment and keywords.
    pub fn search_terms(&self) -> String {
        let mut terms = vec![self.generic_name.as_str(), self.id.as_str()];
        terms.extend(self.keywords.iter().map(String::as_str));
        terms.join(" ").to_lowercase()
    }
}

/// Whether `id` is a valid desktop action identifier.
pub fn is_action_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `applications` directories in XDG precedence order: `$XDG_DATA_HOME`
/// (default `~/.local/share`), then `$XDG_DATA_DIRS` (default
/// `/usr/local/share:/usr/share`).
pub fn application_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    data_home
        .into_iter()
        .chain(
            data_dirs
                .split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        )
        .map(|d| d.join("applications"))
        .collect()
}

/// Reads every application under `dirs`, earlier directories winning for
/// the same desktop file ID, sorted by name. Hidden entries in an earlier
/// directory still shadow later ones, as the spec requires.
pub fn scan(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let mut files = Vec::new();
        collect(dir, dir, 0, &mut files);
        files.sort();
        for (id, path) in files {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(entry) = DesktopEntry::parse(&id, &text).filter(DesktopEntry::installed) {
                out.push(entry);
            }
        }
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// Collects `(desktop file ID, path)` pairs; subdirectories become `-` in
/// the ID (`kde/foo.desktop` is `kde-foo.desktop`).
fn collect(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() && depth < 4 {
            collect(root, &path, depth + 1, out);
        } else if path.extension().is_some_and(|e| e == "desktop")
            && let Ok(rel) = path.strip_prefix(root)
        {
            let id = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("-");
            out.push((id, path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_try_exec(program: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Android\nExec=derisk launch org.derisk.android\nTryExec={program}\n"
        )
    }

    fn installed(program: &str) -> bool {
        DesktopEntry::parse("a.desktop", &with_try_exec(program))
            .unwrap()
            .installed()
    }

    #[test]
    fn an_app_whose_program_is_missing_is_not_listed() {
        let here = std::env::current_exe().unwrap();
        assert!(installed(&here.display().to_string()));
        assert!(!installed("/nonexistent/android-app-support"));
        assert!(!installed("no-such-program-anywhere"));
    }
}
