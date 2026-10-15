//! The command palette: one search box for everything derisk can do.
//!
//! Super+Space opens it. Typing searches, in one ranked list:
//!
//! - apps (the core apps and anything installed) and their actions,
//! - open windows on every workspace,
//! - commands for the focused window, including its app's own global-menu
//!   items, so apps get palette commands just by registering menus,
//! - shell commands (overview, layouts, workspaces, moving windows),
//! - session commands (lock, suspend, log out, ...), tray items and failed
//!   user units,
//! - a browser's tabs and extensions, virtual machines and the apps in
//!   them, files, and
//! - for the typed text itself, the assistant, Sonne's agent, an address to
//!   open and a web search.
//!
//! Every one of those rows comes from a plugin: a WebAssembly component
//! run in a sandbox by `derisk-plugin`, bundled in derisk or installed
//! (see that crate). This module is the core around them. It shows each
//! plugin the parts of the desktop it asked for ([`view`]), keeps their
//! answers until what they read changes ([`Catalog`]), turns the actions in
//! them into [`Action`]s, and ranks the rows for the query.
//!
//! Anything that matches nothing (or reads like a sentence the assistant
//! understands, such as "open firefox and snap it left") goes to the
//! built-in assistant as a natural-language request.
//!
//! A leading character narrows the search: `>` commands only, `@` windows,
//! `/` or `~` files, `?` ask the assistant. Picks are remembered, so things
//! you use often rise to the top, and an empty query lists them first.
//!
//! This module is UI-free; [`crate::ui`] draws it.
//!
//! ```
//! use derisk::{geom::rect, palette::{self, History}, shell::Shell};
//!
//! let mut shell = Shell::new(rect(0, 0, 1920, 1080), false);
//! shell.map_window("kitty", "~");
//! let entries = palette::entries(&shell, &[], &[]);
//! let hits = palette::search(&entries, "max", &History::default());
//! assert_eq!(entries[hits[0]].title, "Maximize");
//! ```

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub use derisk_plugin::palette::{App, AppAction, Category, File, Plugins, Position, View};
use derisk_plugin::palette::{
    Desk, Hook, Input, MenuCommand, Registration, TrayItem, Window, Workspace,
};
use tracing::warn;

use crate::{
    action::Action,
    assistant,
    desktop::DesktopEntry,
    menu::{self, MenuEntry},
    shell::Shell,
    systemd::SessionOp,
};

/// Whether rows of `category` are commands, which `>` narrows to.
fn is_command(category: Category) -> bool {
    matches!(
        category,
        Category::AppAction
            | Category::AppCommand
            | Category::Command
            | Category::Workspace
            | Category::Session
            | Category::System
    )
}

/// Where rows of `category` sit in the catalog, which breaks ties between
/// equal scores: apps, windows, menus and commands, workspaces, the
/// session, the system, then the many rows that only show once typed for.
fn catalog_order(category: Category) -> u8 {
    match category {
        Category::App => 0,
        Category::Window => 1,
        Category::AppCommand => 2,
        Category::Command => 3,
        Category::Workspace => 4,
        Category::Session => 5,
        Category::System => 6,
        Category::Tab => 7,
        Category::AppAction | Category::Setting | Category::Extension => 8,
        Category::File => 9,
        Category::Ask | Category::Web | Category::Agent => 10,
    }
}

/// One thing the palette can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Kind of entry.
    pub category: Category,
    /// Main text.
    pub title: String,
    /// Secondary text (app summary, window title, path, ...).
    pub detail: String,
    /// Extra search terms, not shown.
    pub keywords: String,
    /// The symbolic icon (see [`crate::icons`]); for app rows, which show
    /// the app's own icon, the glyph standing in when no theme has it.
    pub icon: String,
    /// Keyboard shortcut hint, if the command has one.
    pub shortcut: Option<String>,
    /// What choosing the entry does.
    pub actions: Vec<Action>,
    /// Ends the session or loses work: needs a second Enter.
    pub confirm: bool,
}

impl Entry {
    /// An entry running `actions`, with no detail, keywords or shortcut.
    pub fn new(
        category: Category,
        icon: &str,
        title: impl Into<String>,
        actions: Vec<Action>,
    ) -> Self {
        Self {
            category,
            title: title.into(),
            detail: String::new(),
            keywords: String::new(),
            icon: icon.to_owned(),
            shortcut: None,
            actions,
            confirm: false,
        }
    }

    /// Sets the secondary text.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// Sets extra search terms.
    pub fn keywords(mut self, keywords: impl Into<String>) -> Self {
        self.keywords = keywords.into();
        self
    }

    /// Sets the shortcut hint.
    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Whether choosing this entry hibernates the machine.
    pub fn hibernates(&self) -> bool {
        self.actions.iter().any(|a| {
            matches!(
                a,
                Action::Session {
                    op: SessionOp::Hibernate,
                    ..
                }
            )
        })
    }

    /// The key [`History`] remembers this entry by.
    pub fn key(&self) -> String {
        format!("{:?}:{}:{}", self.category, self.title, self.detail)
    }

    /// A plugin's row with its actions parsed, or `None` when one does not
    /// parse. A row that logs out, reboots or powers off asks for a second
    /// Enter whatever the plugin said.
    pub fn from_plugin(row: derisk_plugin::palette::Entry) -> Option<Self> {
        let actions = row
            .actions
            .iter()
            .map(|a| serde_json::from_str::<Action>(a))
            .collect::<Result<Vec<_>, _>>()
            .inspect_err(|e| warn!(title = row.title, "palette row dropped: {e}"))
            .ok()?;
        let destructive = actions
            .iter()
            .any(|a| matches!(a, Action::Session { op, .. } if op.is_destructive()));
        Some(Self {
            category: row.category,
            title: row.title,
            detail: row.detail,
            keywords: row.keywords,
            icon: row.icon,
            shortcut: row.shortcut,
            actions,
            confirm: row.confirm || destructive,
        })
    }
}

/// An app as the palette's plugins see it.
pub fn app(id: &str, name: &str, summary: &str, icon: &str, keywords: &[&str]) -> App {
    App {
        id: id.to_owned(),
        name: name.to_owned(),
        summary: summary.to_owned(),
        icon: icon.to_owned(),
        keywords: format!("{id} {}", keywords.join(" ")),
        actions: Vec::new(),
    }
}

/// An app from its `.desktop` file, with its desktop actions.
pub fn desktop_app(entry: &DesktopEntry, icon: &str) -> App {
    let summary = if entry.comment.is_empty() {
        &entry.generic_name
    } else {
        &entry.comment
    };
    App {
        id: entry.id.clone(),
        name: entry.name.clone(),
        summary: summary.clone(),
        icon: icon.to_owned(),
        keywords: entry.search_terms(),
        actions: entry
            .actions
            .iter()
            .map(|a| AppAction {
                id: a.id.clone(),
                name: a.name.clone(),
            })
            .collect(),
    }
}

/// An indexed path as the files plugin sees it, shown under `home` as
/// `~/...`. Asks the filesystem whether it is a folder, so it belongs where
/// the index is built, off the frame.
pub fn file(path: &Path, home: Option<&Path>) -> File {
    let shown = match home.map(|h| path.strip_prefix(h)) {
        Some(Ok(rel)) => format!("~/{}", rel.display()),
        _ => path.display().to_string(),
    };
    File {
        path: path.display().to_string(),
        shown,
        folder: path.is_dir(),
    }
}

/// The desktop as the palette's plugins see it, without the files, which
/// [`Catalog`] adds only for a plugin that reads them.
pub fn view(shell: &Shell, apps: &[App]) -> View {
    let focused = shell.focused();
    let mut windows = Vec::new();
    let mut desk = Desk {
        workspaces: Vec::new(),
        window_focused: focused.is_some(),
        can_add_workspace: shell.can_add_workspace(),
    };
    let active = shell.active_workspace();
    for (i, &ws) in shell.workspaces().iter().enumerate() {
        let number = i as u32 + 1;
        let on = shell.windows_on(ws);
        desk.workspaces.push(Workspace {
            number,
            windows: on.len() as u32,
            active: u64::from(number) == active,
        });
        for w in on {
            let (app, title) = shell.window_label(w).unwrap_or_default();
            windows.push(Window {
                id: w.get(),
                app: shell.apps.look(app).name.into_owned(),
                title: title.to_owned(),
                workspace: number,
                minimized: shell.is_minimized(w),
                focused: Some(w) == focused,
            });
        }
    }

    // The focused app's own menus, then the shell's Window menu.
    let mut menus = Vec::new();
    if let Some(w) = focused {
        let window = Some(w.get());
        let (app, _) = shell.window_label(w).unwrap_or_default();
        let app = shell.apps.look(app).name.into_owned();
        let command =
            |path: &str, shortcut: Option<&str>, shell: bool, action: Action| MenuCommand {
                label: path.rsplit(" › ").next().unwrap_or(path).to_owned(),
                path: path.to_owned(),
                app: app.clone(),
                shortcut: shortcut.map(str::to_owned),
                shell,
                actions: vec![serde_json::to_string(&action).unwrap_or_default()],
            };
        for m in shell.menus.app_menus(w.get()) {
            flatten(&m.entries, &m.title, &mut |id, path, shortcut| {
                let action = Action::ActivateMenu {
                    window,
                    item: id.to_owned(),
                };
                menus.push(command(path, shortcut, false, action));
            });
        }
        let window_menu = menu::window_menu();
        // Keeping a window on top is refused for an untrusted machine's, so
        // the palette does not offer it.
        let untrusted = shell.is_untrusted(w);
        flatten(&window_menu.entries, "Window", &mut |id, path, shortcut| {
            if untrusted
                && matches!(
                    menu::shell_action(id, w.get()),
                    Some(Action::KeepOnTop { .. })
                )
            {
                return;
            }
            if let Some(action) = menu::shell_action(id, w.get()) {
                menus.push(command(path, shortcut, true, action));
            }
        });
    }

    View {
        hour: shell.clock.hour,
        apps: apps.to_vec(),
        suggested: shell.habits.suggestions(shell.clock.hour, 5),
        windows,
        menus,
        desk,
        tray: shell
            .tray
            .items()
            .map(|item| TrayItem {
                id: item.id.clone(),
                title: item.title.clone(),
            })
            .collect(),
        failed_units: shell.failed_units.clone(),
        files: Vec::new(),
        search_engine: shell.effects.search.map(|e| e.name().to_owned()),
        registered: shell
            .palette_sources
            .iter()
            .map(|(source, data)| Registration {
                source: source.to_owned(),
                data: data.to_string(),
            })
            .collect(),
    }
}

/// The assistant as plugins call it: [`assistant::interpret`] with its
/// actions as JSON.
fn interpret(text: &str) -> Result<Vec<String>, String> {
    assistant::interpret(text)
        .map(|actions| {
            actions
                .iter()
                .map(|a| serde_json::to_string(a).unwrap_or_default())
                .collect()
        })
        .map_err(|e| e.to_string())
}

static PLUGINS: OnceLock<Plugins> = OnceLock::new();

/// The palette's plugins: the bundled ones, then the system's, then the
/// person's signed ones (see `derisk-plugin` for where each comes from).
/// Loaded once, on first use; [`preload`] starts that early.
///
/// # Panics
///
/// If the bundled plugins do not load, which is a bug in derisk's build.
pub fn plugins() -> &'static Plugins {
    PLUGINS.get_or_init(|| {
        crate::plugins::with_installed(
            Plugins::bundled(interpret).expect("derisk's bundled palette plugins"),
        )
    })
}

/// Loads [`plugins`] now, so the palette's first opening does not wait for
/// them to compile. For a thread of its own at startup.
pub fn preload() {
    plugins();
}

/// What one plugin last answered, and from what.
#[derive(Debug, Default)]
struct Answer {
    /// The view it was asked with, files left out.
    view: Option<View>,
    /// [`Catalog::files_generation`] then, if it reads files.
    files: Option<u64>,
    rows: Vec<Entry>,
}

/// The palette's rows, kept between frames: each plugin is asked again
/// only when a part of the desktop it reads has changed, and the query
/// rows only when the query has.
#[derive(Debug, Default)]
pub struct Catalog {
    answers: Vec<Answer>,
    entries: Vec<Entry>,
    files_generation: u64,
    built: bool,
    /// The `can_hibernate` the entries were filtered with.
    hibernate: bool,
    query: Option<(String, View)>,
    rows: Rows,
}

/// The rows for the typed text, by where they go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    /// The assistant's reading of the text.
    pub ask: Option<Entry>,
    /// Above the catalog's matches.
    pub top: Vec<Entry>,
    /// Below them.
    pub bottom: Vec<Entry>,
}

impl Catalog {
    /// Tells the catalog the files changed, so the plugins reading them
    /// are asked again.
    pub fn files_changed(&mut self) {
        self.files_generation += 1;
    }

    /// The catalog for `view` and `files`, asking again only the plugins
    /// whose part of it changed. Rows the machine cannot carry out are left
    /// out: Hibernate unless `can_hibernate`.
    pub fn entries(
        &mut self,
        plugins: &Plugins,
        view: &View,
        files: &[File],
        can_hibernate: bool,
    ) -> &[Entry] {
        self.answers.resize_with(plugins.len(), Answer::default);
        let stale: Vec<(usize, View, Option<u64>)> = plugins
            .iter()
            .enumerate()
            .filter(|(_, p)| p.has(Hook::Entries))
            .filter_map(|(i, p)| {
                let seen = p.view(view);
                let files = p.reads(Input::Files).then_some(self.files_generation);
                let answer = &self.answers[i];
                (answer.view.as_ref() != Some(&seen) || answer.files != files)
                    .then_some((i, seen, files))
            })
            .collect();
        if stale.is_empty() && self.built && self.hibernate == can_hibernate {
            return &self.entries;
        }
        let fresh = plugins.each(
            |i, _| stale.iter().any(|(s, ..)| *s == i),
            |p| {
                let mut seen = p.view(view);
                if p.reads(Input::Files) {
                    seen.files = files.to_vec();
                }
                plugins.entries(p, &seen)
            },
        );
        for ((index, rows), (_, seen, files)) in fresh.into_iter().zip(stale) {
            self.answers[index] = Answer {
                view: Some(seen),
                files,
                rows: rows.into_iter().filter_map(Entry::from_plugin).collect(),
            };
        }
        self.built = true;
        self.hibernate = can_hibernate;
        let mut entries: Vec<Entry> = self
            .answers
            .iter()
            .flat_map(|a| a.rows.iter())
            .filter(|e| can_hibernate || !e.hibernates())
            .cloned()
            .collect();
        // Stable: within a category, plugins' own order stands.
        entries.sort_by_key(|e| catalog_order(e.category));
        self.entries = entries;
        &self.entries
    }

    /// The rows for `query`, asked again only when it or the view changed.
    /// Only a query with no scope prefix has any.
    pub fn rows(&mut self, plugins: &Plugins, view: &View, query: &str) -> &Rows {
        let (scope, text) = scope(query);
        if scope != Scope::All || text.is_empty() {
            self.query = None;
            self.rows = Rows::default();
            return &self.rows;
        }
        if self
            .query
            .as_ref()
            .is_some_and(|(q, v)| q == text && v == view)
        {
            return &self.rows;
        }
        let answers = plugins.each(|_, p| p.has(Hook::Query), |p| plugins.query(p, view, text));
        let mut rows = Rows::default();
        for row in answers.into_iter().flat_map(|(_, rows)| rows) {
            let position = row.position;
            let Some(entry) = Entry::from_plugin(row) else {
                continue;
            };
            if entry.category == Category::Ask {
                rows.ask.get_or_insert(entry);
            } else if position == Position::Top {
                rows.top.push(entry);
            } else {
                rows.bottom.push(entry);
            }
        }
        self.query = Some((text.to_owned(), view.clone()));
        self.rows = rows;
        &self.rows
    }
}

/// Everything the palette's catalog holds for `shell`, `apps` and `files`,
/// from the loaded [`plugins`], asked afresh.
pub fn entries(shell: &Shell, apps: &[App], files: &[File]) -> Vec<Entry> {
    Catalog::default()
        .entries(plugins(), &view(shell, apps), files, shell.can_hibernate)
        .to_vec()
}

/// The rows for typed `query` in `shell`, from the loaded [`plugins`].
pub fn rows(shell: &Shell, query: &str) -> Rows {
    Catalog::default()
        .rows(plugins(), &view(shell, &[]), query)
        .clone()
}

/// The assistant's row for `text`: what it would do, or why it can't.
pub fn ask(text: &str) -> Entry {
    let rows = Catalog::default()
        .rows(plugins(), &View::default(), text)
        .clone();
    rows.ask.unwrap_or_else(|| {
        Entry::new(
            Category::Ask,
            "tool-magic",
            format!("Ask derisk: {text}"),
            Vec::new(),
        )
        .detail("No assistant plugin is loaded")
    })
}

/// The rows the palette lists for `query`, in order: the assistant first
/// when the query reads like a request it understands, then the query's
/// top rows, the catalog's `hits`, the assistant otherwise (only when it
/// understood, or nothing else matched, to say why), then the query's
/// bottom rows.
pub fn list<'a>(
    entries: &'a [Entry],
    hits: &[usize],
    rows: &'a Rows,
    query: &str,
) -> Vec<&'a Entry> {
    let ask = rows
        .ask
        .as_ref()
        .filter(|ask| !ask.actions.is_empty() || hits.is_empty());
    let first = ask.filter(|_| prefer_assistant(entries, hits, query));
    // A request that opens apps lists them under the assistant's row, so
    // they can be opened on their own: a sentence matches no title, so the
    // search alone would not.
    let named = first.map_or_else(Vec::new, |ask| named_apps(entries, ask));
    first
        .into_iter()
        .chain(named.iter().map(|&i| &entries[i]))
        .chain(&rows.top)
        .chain(
            hits.iter()
                .filter(|i| !named.contains(i))
                .map(|&i| &entries[i]),
        )
        .chain(ask.filter(|_| first.is_none()))
        .chain(&rows.bottom)
        .collect()
}

/// Indices of the app entries that `ask`'s steps open, in step order.
fn named_apps(entries: &[Entry], ask: &Entry) -> Vec<usize> {
    let mut named = Vec::new();
    for action in &ask.actions {
        let Action::Launch { app } = action else {
            continue;
        };
        let found = entries
            .iter()
            .position(|e| e.category == Category::App && opens(e, app));
        if let Some(i) = found.filter(|i| !named.contains(i)) {
            named.push(i);
        }
    }
    named
}

/// Whether an app entry is the app the assistant calls `name`: its ID, the
/// ID's last segment (`files`), or its title, as the host resolves it.
fn opens(entry: &Entry, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    let title = entry.title.to_lowercase();
    title == name
        || title.replace(' ', "") == name
        || entry.actions.iter().any(|a| match a {
            Action::Launch { app } => {
                let id = app.to_lowercase();
                let id = id.strip_suffix(".desktop").unwrap_or(&id);
                id == name || id.rsplit('.').next() == Some(name.as_str())
            }
            _ => false,
        })
}

/// How often each entry was chosen, so frequent picks rank first.
#[derive(Clone, Debug, Default)]
pub struct History {
    uses: BTreeMap<String, u32>,
    clock: u32,
    last: BTreeMap<String, u32>,
}

impl History {
    /// Remembers that `entry` was chosen.
    pub fn record(&mut self, entry: &Entry) {
        let key = entry.key();
        self.clock += 1;
        *self.uses.entry(key.clone()).or_default() += 1;
        self.last.insert(key, self.clock);
    }

    /// Ranking boost for an entry: grows with use and recency.
    pub fn boost(&self, entry: &Entry) -> u32 {
        let key = entry.key();
        let uses = self.uses.get(&key).copied().unwrap_or(0).min(10);
        let recency = self
            .last
            .get(&key)
            .map_or(0, |&t| 20u32.saturating_sub(self.clock - t));
        uses * 20 + recency * 3
    }
}

/// Scores how well `query` matches `text`, ignoring case; `None` if some
/// query character is missing.
///
/// Contiguous matches beat scattered ones, and matches at the start of the
/// text or of a word beat matches inside a word.
pub fn fuzzy(text: &str, query: &str) -> Option<u32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let query: Vec<char> = query.to_lowercase().chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    let word_start = |i: usize| i == 0 || !text[i - 1].is_alphanumeric();
    // A contiguous substring, best at a word start.
    let substring = (0..text.len())
        .filter(|&i| text[i..].starts_with(&query))
        .map(|i| {
            let mut score = 100 + 10 * query.len() as u32;
            if i == 0 {
                score += 60;
            } else if word_start(i) {
                score += 40;
            }
            score.saturating_sub(i as u32)
        })
        .max();
    if substring.is_some() {
        return substring;
    }
    // Otherwise every character in order, rewarding runs and word starts.
    let mut score = 0u32;
    let mut at = 0usize;
    let mut previous: Option<usize> = None;
    for &c in &query {
        let i = (at..text.len()).find(|&i| text[i] == c)?;
        score += 4;
        if previous == Some(i.wrapping_sub(1)) {
            score += 8;
        }
        if word_start(i) {
            score += 10;
        }
        previous = Some(i);
        at = i + 1;
    }
    Some(score)
}

/// Scores an entry for a (possibly multi-word) query; every word must match
/// the title, detail or keywords. Title matches count double.
fn score(entry: &Entry, query: &str) -> Option<u32> {
    query.split_whitespace().try_fold(0u32, |total, word| {
        let title = fuzzy(&entry.title, word).map(|s| s * 2);
        let detail = fuzzy(&entry.detail, word);
        let keywords = entry
            .keywords
            .split_whitespace()
            .filter_map(|k| fuzzy(k, word))
            .max();
        [title, detail, keywords]
            .into_iter()
            .flatten()
            .max()
            .map(|s| total + s)
    })
}

/// A search prefix that narrows the palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    /// Everything.
    All,
    /// `>`: commands only.
    Commands,
    /// `@`: open windows.
    Windows,
    /// `/` or `~`: files.
    Files,
    /// `?`: straight to the assistant.
    Ask,
}

/// Splits a leading scope character off a query.
pub fn scope(query: &str) -> (Scope, &str) {
    let trimmed = query.trim_start();
    let scope = match trimmed.chars().next() {
        Some('>') => Scope::Commands,
        Some('@') => Scope::Windows,
        Some('/' | '~') => Scope::Files,
        Some('?') => Scope::Ask,
        _ => return (Scope::All, query.trim()),
    };
    // `/` and `~` stay part of a file query ("~/notes" searches for "notes").
    let rest = trimmed[1..].trim_start_matches('/').trim();
    (scope, rest)
}

fn in_scope(scope: Scope, category: Category) -> bool {
    match scope {
        Scope::All => true,
        Scope::Commands => is_command(category),
        Scope::Windows => category == Category::Window,
        Scope::Files => category == Category::File,
        Scope::Ask => false,
    }
}

/// Indices into `entries` matching `query`, best first.
///
/// Files and app actions only show once something is typed (or once
/// picked), so an empty query lists apps, windows and commands, most used
/// first.
pub fn search(entries: &[Entry], query: &str, history: &History) -> Vec<usize> {
    let (scope, query) = scope(query);
    let mut hits: Vec<(u32, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| in_scope(scope, e.category))
        .filter(|(_, e)| {
            // Files and app actions are many; an empty query only lists the
            // ones picked before.
            !(query.is_empty()
                && scope == Scope::All
                && matches!(e.category, Category::File | Category::AppAction)
                && history.boost(e) == 0)
        })
        .filter_map(|(i, e)| score(e, query).map(|s| (s + history.boost(e), i)))
        .collect();
    // Higher score first; ties keep catalog order (apps, windows, commands, ...).
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Whether the assistant should be offered first: the query reads like a
/// request (several words) and the assistant understands it, while the best
/// entry's title does not contain every word.
pub fn prefer_assistant(entries: &[Entry], hits: &[usize], query: &str) -> bool {
    let (scope, text) = scope(query);
    if scope == Scope::Ask {
        return true;
    }
    if scope != Scope::All || text.split_whitespace().count() < 2 {
        return false;
    }
    // Close: the best entry's title contains every typed word.
    let lower = text.to_lowercase();
    let close = hits.first().is_some_and(|&i| {
        let title = entries[i].title.to_lowercase();
        lower.split_whitespace().all(|w| title.contains(w))
    });
    !close && assistant::interpret(text).is_ok()
}

/// Visits every enabled menu item as `(id, "Menu › Submenu › Item", shortcut)`.
fn flatten(entries: &[MenuEntry], path: &str, visit: &mut dyn FnMut(&str, &str, Option<&str>)) {
    for entry in entries {
        match entry {
            MenuEntry::Item {
                id,
                label,
                shortcut,
                enabled: true,
            } => visit(id, &format!("{path} › {label}"), shortcut.as_deref()),
            MenuEntry::Submenu { label, entries } => {
                flatten(entries, &format!("{path} › {label}"), visit);
            }
            MenuEntry::Item { .. } | MenuEntry::Separator => {}
        }
    }
}

/// Lists files and folders under `root` for the palette: at most `depth`
/// levels deep and `limit` paths, skipping hidden entries and symlinks.
///
/// Folders come before the files inside them, shallow before deep.
pub fn index_files(root: &Path, depth: usize, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut level = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for dir in level {
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut children: Vec<_> = read
                .filter_map(Result::ok)
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .filter_map(|e| Some((e.path(), e.file_type().ok()?)))
                .filter(|(_, t)| !t.is_symlink())
                .collect();
            children.sort_by(|a, b| a.0.cmp(&b.0));
            for (path, kind) in children {
                if out.len() >= limit {
                    return out;
                }
                if kind.is_dir() {
                    next.push(path.clone());
                }
                out.push(path);
            }
        }
        level = next;
    }
    out
}

/// Most programs that may register palette data at once.
const MAX_SOURCES: usize = 32;

/// Largest registration, as JSON text: a browser's tabs and extensions fit
/// many times over.
const MAX_SOURCE_BYTES: usize = 256 << 10;

/// Data programs registered for the palette's plugins over the agent socket
/// (`register_palette`), such as a browser's tabs, and which connection, if
/// any, owns each. Plugins that asked for a source see its data; picks from
/// their rows go back to the owner as `palette` events ([`event`]).
#[derive(Clone, Debug, Default)]
pub struct Sources {
    data: BTreeMap<String, serde_json::Value>,
    owners: BTreeMap<String, u64>,
}

impl Sources {
    /// Registers or replaces `source`'s data on behalf of `owner` (none for
    /// a headless agent). A source another connection owns is refused, so
    /// one program cannot speak for another.
    pub fn register(
        &mut self,
        source: String,
        data: serde_json::Value,
        owner: Option<u64>,
    ) -> Result<(), String> {
        let valid = (1..=32).contains(&source.len())
            && source
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid {
            return Err(format!(
                "palette source {source:?} must be 1-32 characters of a-z, 0-9 and -"
            ));
        }
        if let Some(&other) = self.owners.get(&source)
            && Some(other) != owner
        {
            return Err(format!(
                "another connection registered palette source {source}"
            ));
        }
        let size = data.to_string().len();
        if size > MAX_SOURCE_BYTES {
            return Err(format!(
                "palette data for {source} is {size} bytes, more than {MAX_SOURCE_BYTES}"
            ));
        }
        if !self.data.contains_key(&source) && self.data.len() >= MAX_SOURCES {
            return Err(format!(
                "at most {MAX_SOURCES} programs may register palette data"
            ));
        }
        match owner {
            Some(owner) => self.owners.insert(source.clone(), owner),
            None => self.owners.remove(&source),
        };
        self.data.insert(source, data);
        Ok(())
    }

    /// Removes `source`, unless another connection than `owner` owns it.
    pub fn remove(&mut self, source: &str, owner: Option<u64>) -> Result<(), String> {
        if let Some(&other) = self.owners.get(source)
            && Some(other) != owner
        {
            return Err(format!(
                "another connection registered palette source {source}"
            ));
        }
        self.owners.remove(source);
        self.data
            .remove(source)
            .map(drop)
            .ok_or_else(|| format!("no palette source {source}"))
    }

    /// Removes what connection `owner` registered, as it closes.
    pub fn disown(&mut self, owner: u64) {
        let gone: Vec<String> = self
            .owners
            .iter()
            .filter(|&(_, &o)| o == owner)
            .map(|(s, _)| s.clone())
            .collect();
        for source in gone {
            self.owners.remove(&source);
            self.data.remove(&source);
        }
    }

    /// Whether `source` is registered.
    pub fn has(&self, source: &str) -> bool {
        self.data.contains_key(source)
    }

    /// The connection to tell of a pick from `source`'s rows.
    pub fn recipient(&self, source: &str) -> Option<u64> {
        self.owners.get(source).copied()
    }

    /// Every registration, by source.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &serde_json::Value)> {
        self.data.iter().map(|(s, d)| (s.as_str(), d))
    }
}

/// The event a source's owner hears when a row of its is picked:
/// `{"event":"palette","source":"danube","command":{...}}`.
pub fn event(source: &str, command: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"event": "palette", "source": source, "command": command})
}
