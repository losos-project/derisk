//! Command palette plugins: every row the palette shows.
//!
//! A plugin answers two questions from a [`View`] of the desktop: its
//! catalog ([`Plugins::entries`]), which the palette searches as the person
//! types, and the rows for one typed query ([`Plugins::query`]). The palette
//! core in `derisk` ranks, remembers and draws; this module holds each
//! plugin to its [`Manifest`]:
//!
//! - a plugin sees only the parts of the view its manifest lists
//!   ([`Plugin::view`]): one that never asked for files never sees a file
//!   name, and one that asked for registered data sees only its sources;
//! - a row is dropped when its category is not in the manifest, or when one
//!   of its actions is not a JSON object whose `action` tag the manifest
//!   lists ([`Plugin::check`]).
//!
//! Apps, windows, menus, commands, workspaces, the session, the tray and
//! failed units, the browser, files, the assistant, Sonne's agent and web
//! search are all bundled plugins, under `plugins/palette/`.

use miette::Result;
use tracing::warn;
use wasmtime::{
    Store,
    component::{Component, HasSelf, Linker},
};

pub use crate::wit::palette::derisk::palette::types::{
    App, AppAction, Category, Desk, Entry, File, FileMatch, Hook, Input, Manifest, MenuCommand,
    Position, Registration, TrayItem, View, Window, Workspace,
};
use crate::{Interpreter, Kind, bundled, check_actions, engine::Runtime, engine::State, wit};

/// Most rows one call may return; past it the rest are dropped. A files
/// index is capped well below this.
const MAX_ROWS: usize = 100_000;

/// The palette plugin kind.
#[derive(Debug)]
pub struct Palette;

impl Kind for Palette {
    const NOUN: &'static str = "palette plugin";
    const WIT: &'static str = "palette.wit";
    const DIR: &'static str = PLUGIN_DIR;
    const BUNDLED: &'static [(&'static str, &'static [u8])] = bundled::PALETTE;
    type Bindings = wit::palette::PalettePlugin;
    type Manifest = Manifest;

    fn add_to_linker(linker: &mut Linker<State>) -> wasmtime::Result<()> {
        wit::palette::PalettePlugin::add_to_linker::<State, HasSelf<State>>(linker, |state| state)
    }

    fn instantiate(
        store: &mut Store<State>,
        component: &Component,
        linker: &Linker<State>,
    ) -> wasmtime::Result<Self::Bindings> {
        wit::palette::PalettePlugin::instantiate(store, component, linker)
    }

    fn describe(bindings: &Self::Bindings, store: &mut Store<State>) -> wasmtime::Result<Manifest> {
        bindings.call_describe(store)
    }

    fn name(manifest: &Manifest) -> &str {
        &manifest.name
    }
}

/// Where the system's palette plugins go under each `$XDG_DATA_DIRS` entry,
/// and the person's under `$XDG_DATA_HOME`.
pub const PLUGIN_DIR: &str = "derisk/palette-plugins";

/// A loaded palette plugin.
pub type Plugin = crate::Plugin<Palette>;

/// The loaded palette plugins.
pub type Plugins = crate::Plugins<Palette>;

impl Category {
    /// Group heading shown above rows of this kind.
    pub fn heading(self) -> &'static str {
        match self {
            Self::App => "Apps",
            Self::AppAction => "App actions",
            Self::Window => "Windows",
            Self::AppCommand => "App commands",
            Self::Command => "Commands",
            Self::Workspace => "Workspaces",
            Self::Setting => "Settings",
            Self::Session => "Session",
            Self::System => "System",
            Self::File => "Files",
            Self::Ask => "Assistant",
            Self::Web => "Web",
            Self::Agent => "Agent",
            Self::Tab => "Tabs",
            Self::Extension => "Extensions",
        }
    }
}

impl Default for View {
    fn default() -> Self {
        Self {
            hour: 12,
            apps: Vec::new(),
            suggested: Vec::new(),
            windows: Vec::new(),
            menus: Vec::new(),
            desk: Desk {
                workspaces: Vec::new(),
                window_focused: false,
                can_add_workspace: false,
            },
            tray: Vec::new(),
            failed_units: Vec::new(),
            files: Vec::new(),
            file_matches: Vec::new(),
            search_engine: None,
            registered: Vec::new(),
        }
    }
}

impl Plugin {
    /// Whether it implements `hook`.
    pub fn has(&self, hook: Hook) -> bool {
        self.manifest.hooks.contains(&hook)
    }

    /// Whether it may see `input`.
    pub fn reads(&self, input: Input) -> bool {
        self.manifest.inputs.contains(&input)
    }

    /// `view` with only what this plugin may see; everything else empty.
    pub fn view(&self, view: &View) -> View {
        let mut out = View {
            hour: view.hour,
            ..View::default()
        };
        if self.reads(Input::Apps) {
            out.apps.clone_from(&view.apps);
            out.suggested.clone_from(&view.suggested);
        }
        if self.reads(Input::Windows) {
            out.windows.clone_from(&view.windows);
        }
        if self.reads(Input::Menus) {
            out.menus.clone_from(&view.menus);
        }
        if self.reads(Input::Workspaces) {
            out.desk.clone_from(&view.desk);
        }
        if self.reads(Input::Tray) {
            out.tray.clone_from(&view.tray);
        }
        if self.reads(Input::Units) {
            out.failed_units.clone_from(&view.failed_units);
        }
        if self.reads(Input::Files) {
            out.files.clone_from(&view.files);
        }
        if self.reads(Input::Contents) {
            out.file_matches.clone_from(&view.file_matches);
        }
        if self.reads(Input::SearchEngine) {
            out.search_engine.clone_from(&view.search_engine);
        }
        if self.reads(Input::Registered) {
            out.registered = view
                .registered
                .iter()
                .filter(|r| self.manifest.sources.contains(&r.source))
                .cloned()
                .collect();
        }
        out
    }

    /// Whether `entry` stays within the manifest: its category is listed,
    /// and every action is a JSON object whose `action` tag is listed.
    pub fn check(&self, entry: &Entry) -> std::result::Result<(), String> {
        if !self.manifest.categories.contains(&entry.category) {
            return Err(format!(
                "category {:?} is not in its manifest",
                entry.category
            ));
        }
        check_actions(&entry.actions, &self.manifest.actions)
    }

    /// The rows of an answer that pass [`Plugin::check`], at most
    /// [`MAX_ROWS`]; the rest are logged and dropped.
    fn keep(&self, rows: Vec<Entry>) -> Vec<Entry> {
        let total = rows.len();
        if total > MAX_ROWS {
            warn!(
                plugin = self.name(),
                total, "palette plugin returned too many rows"
            );
        }
        rows.into_iter()
            .take(MAX_ROWS)
            .filter(|entry| match self.check(entry) {
                Ok(()) => true,
                Err(reason) => {
                    warn!(
                        plugin = self.name(),
                        title = entry.title,
                        "row dropped: {reason}"
                    );
                    false
                }
            })
            .collect()
    }
}

impl Plugins {
    /// derisk's own palette plugins, compiled in parallel.
    ///
    /// # Errors
    ///
    /// Fails if the engine cannot start or a bundled plugin does not load,
    /// both bugs in derisk.
    pub fn bundled(interpreter: Interpreter) -> Result<Self> {
        Self::with_bundled(Runtime::new(Some(interpreter))?)
    }

    /// `plugin`'s catalog for `view`, checked against its manifest.
    ///
    /// # Errors
    ///
    /// Fails if the plugin traps, runs out of fuel or memory, or does not
    /// implement `entries`; the palette shows its other rows without it.
    pub fn entries(&self, plugin: &Plugin, view: &View) -> Result<Vec<Entry>> {
        if !plugin.has(Hook::Entries) {
            return Ok(Vec::new());
        }
        let view = plugin.view(view);
        let rows = self
            .runtime
            .enter(plugin.name(), &plugin.component, |b, store| {
                b.call_entries(store, &view)
            })?;
        Ok(plugin.keep(rows))
    }

    /// `plugin`'s rows for typed `text`, checked against its manifest.
    ///
    /// # Errors
    ///
    /// As [`Plugins::entries`].
    pub fn query(&self, plugin: &Plugin, view: &View, text: &str) -> Result<Vec<Entry>> {
        if !plugin.has(Hook::Query) {
            return Ok(Vec::new());
        }
        let view = plugin.view(view);
        let rows = self
            .runtime
            .enter(plugin.name(), &plugin.component, |b, store| {
                b.call_query(store, &view, text)
            })?;
        Ok(plugin.keep(rows))
    }
}
