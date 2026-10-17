//! What a derisk command palette plugin is built with.
//!
//! The bindings for `crates/derisk-plugin/wit/palette/palette.wit`, generated once
//! here so every plugin shares them, and a few builders for the rows and
//! actions a plugin returns. A plugin implements [`Guest`] and exports it
//! with [`export!`]:
//!
//! ```ignore
//! use derisk_palette_sdk::{Guest, Manifest, View, Entry, export};
//!
//! struct Hello;
//!
//! impl Guest for Hello {
//!     fn describe() -> Manifest { ... }
//!     fn entries(view: View) -> Vec<Entry> { ... }
//!     fn query(view: View, text: String) -> Vec<Entry> { Vec::new() }
//! }
//!
//! export!(Hello with_types_in derisk_palette_sdk::bindings);
//! ```
//!
//! The plugins compile to `wasm32-unknown-unknown` and `derisk-plugin`'s
//! build script wraps them in components. They also build for the host, so
//! `cargo test` runs their logic natively; there the imports are stubs that
//! must not be called, which is why [`interpret`] and [`log`] are only
//! reached from code the tests do not run, or through a seam.

pub use serde_json::{self, Value, json};

#[allow(missing_docs, clippy::all, clippy::pedantic)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/derisk-plugin/wit/palette",
        world: "palette-plugin",
        pub_export_macro: true,
        export_macro_name: "export",
        default_bindings_module: "derisk_palette_sdk::bindings",
    });
}

pub use bindings::{
    Guest,
    derisk::palette::{
        host::Level,
        types::{
            App, AppAction, Category, Desk, Entry, File, FileMatch, Hook, Input, Manifest,
            MenuCommand, Position, Registration, TrayItem, View, Window, Workspace,
        },
    },
    export,
};

/// Writes a line to derisk's log.
pub fn log(level: Level, message: &str) {
    bindings::derisk::palette::host::log(level, message);
}

/// The built-in assistant's actions for `text`, or why it does not
/// understand it.
pub fn interpret(text: &str) -> Result<Vec<String>, String> {
    bindings::derisk::palette::assistant::interpret(text)
}

impl Entry {
    /// A catalog row running `actions`, with no detail, keywords or
    /// shortcut.
    pub fn new(
        category: Category,
        icon: &str,
        title: impl Into<String>,
        actions: &[Value],
    ) -> Self {
        Self {
            category,
            title: title.into(),
            detail: String::new(),
            keywords: String::new(),
            icon: icon.to_owned(),
            shortcut: None,
            actions: actions.iter().map(Value::to_string).collect(),
            confirm: false,
            position: Position::Bottom,
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
    pub fn shortcut(mut self, shortcut: Option<impl Into<String>>) -> Self {
        self.shortcut = shortcut.map(Into::into);
        self
    }

    /// Asks for a second Enter.
    pub fn confirm(mut self, confirm: bool) -> Self {
        self.confirm = confirm;
        self
    }

    /// Places a `query` row.
    pub fn at(mut self, position: Position) -> Self {
        self.position = position;
        self
    }

    /// The row's actions, parsed back, for tests.
    pub fn parsed_actions(&self) -> Vec<Value> {
        self.actions
            .iter()
            .filter_map(|a| serde_json::from_str(a).ok())
            .collect()
    }
}

impl Manifest {
    /// A manifest for `name`, version 0.1.0, asking for nothing.
    pub fn new(name: &str, summary: &str) -> Self {
        Self {
            name: name.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            summary: summary.to_owned(),
            hooks: Vec::new(),
            inputs: Vec::new(),
            sources: Vec::new(),
            categories: Vec::new(),
            actions: Vec::new(),
        }
    }

    /// Sets the hooks to call.
    pub fn hooks(mut self, hooks: &[Hook]) -> Self {
        self.hooks = hooks.to_vec();
        self
    }

    /// Sets the parts of the view to fill.
    pub fn inputs(mut self, inputs: &[Input]) -> Self {
        self.inputs = inputs.to_vec();
        self
    }

    /// Sets the registered sources to read.
    pub fn sources(mut self, sources: &[&str]) -> Self {
        self.sources = sources.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    /// Sets the categories rows may have.
    pub fn categories(mut self, categories: &[Category]) -> Self {
        self.categories = categories.to_vec();
        self
    }

    /// Sets the action kinds rows may run.
    pub fn actions(mut self, actions: &[&str]) -> Self {
        self.actions = actions.iter().map(|s| (*s).to_owned()).collect();
        self
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
