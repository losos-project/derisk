//! The project setup's answers and the prompt they add up to: five questions
//! about the app a new project builds, kept as plain data ([`AppSetup`]) so
//! any UI can ask them. The Agent's agent window asks them with mcsapi's egui
//! components and the editor with GPUI's, both from the same [`SetupForm`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What a project is set up to build, saved on the project so the wizard can
/// reopen with the same answers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppSetup {
    /// A LosOS app: one pm recipe, `build.rhai`.
    pub losos: bool,
    pub flatpak: bool,
    pub wasm: bool,
    /// A plugin for derisk's command palette, from [`PluginTemplate::Palette`].
    pub palette_plugin: bool,
    /// A plugin for derisk's overview widgets, from [`PluginTemplate::Widget`].
    pub widget_plugin: bool,
    pub language: Language,
    pub metadata: Metadata,
    /// What the app should do, in the user's words.
    pub description: String,
}

impl Default for AppSetup {
    fn default() -> Self {
        Self {
            losos: true,
            flatpak: false,
            wasm: false,
            palette_plugin: false,
            widget_plugin: false,
            language: Language::Rust,
            metadata: Metadata::default(),
            description: String::new(),
        }
    }
}

/// The languages mcsapi supports an app in. Only Rust gets mcsapi's own
/// widgets, the Agent's live preview and WASM builds; the rest are themed from the
/// outside by x2mcsapi.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[default]
    Rust,
    TypeScript,
    Web,
    CGtk,
    CppQt,
    PythonGtk,
}

impl Language {
    pub const ALL: [Self; 6] = [
        Self::Rust,
        Self::TypeScript,
        Self::Web,
        Self::CGtk,
        Self::CppQt,
        Self::PythonGtk,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust (recommended)",
            Self::TypeScript => "TypeScript",
            Self::Web => "HTML, CSS and JavaScript",
            Self::CGtk => "C with GTK 4",
            Self::CppQt => "C++ with Qt 6",
            Self::PythonGtk => "Python with GTK 4",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::Web => "HTML, CSS and JavaScript",
            Self::CGtk => "C with GTK 4",
            Self::CppQt => "C++ with Qt 6 Widgets",
            Self::PythonGtk => "Python with GTK 4 (PyGObject)",
        }
    }

    /// What the user gets, or gives up, by picking this language.
    pub fn note(self) -> &'static str {
        match self {
            Self::Rust => {
                "mcsapi is written in Rust: the app uses its widgets and theme directly, \
                 shows live in the Agent's preview pane, and builds for WASM."
            }
            Self::TypeScript => {
                "mcsapi's Node bindings cover desktop policy (workspaces, layouts); the UI is a \
                 web page that x2mcsapi restyles. No live preview and no WASM build."
            }
            Self::Web => {
                "x2mcsapi restyles the page from the desktop theme. No mcsapi widgets, no live \
                 preview, no WASM build."
            }
            Self::CGtk | Self::PythonGtk => {
                "x2mcsapi restyles GTK from the desktop theme. No mcsapi widgets, no live \
                 preview, no WASM build."
            }
            Self::CppQt => {
                "x2mcsapi restyles Qt Widgets from the desktop theme. No mcsapi widgets, no live \
                 preview, no WASM build."
            }
        }
    }

    /// The Agent's WASM previews are Rust guests of `derisk_agent_preview`.
    pub fn builds_wasm(self) -> bool {
        self == Self::Rust
    }

    pub fn toolkit(self) -> &'static str {
        match self {
            Self::Rust => {
                "Draw the UI with egui. For the desktop build, make it an mcsapi app: \
                 `mcsapi_ui::App` with the widgets from `mcsapi-components` \
                 (both from https://github.com/dasmatus/mcsapi), so it follows the desktop \
                 theme. Start with the Agent's `preview_new` tool so the app shows in the preview \
                 pane while you work, and check it with `preview_build` and `preview_status`."
            }
            Self::TypeScript => {
                "Run it on Node.js. Use mcsapi's napi-rs bindings (`bindings/node` in \
                 https://github.com/dasmatus/mcsapi) for anything that touches workspaces or \
                 window layout. Build the UI as a web page; x2mcsapi restyles it from the \
                 desktop theme, so keep its styles plain and semantic."
            }
            Self::Web => {
                "Build it as a web page; x2mcsapi restyles it from the desktop theme, so keep \
                 its styles plain and semantic."
            }
            Self::CGtk | Self::PythonGtk => {
                "Use stock GTK 4 widgets without custom CSS: x2mcsapi restyles GTK from the \
                 desktop theme, and LosOS's GTK carries the adaptive patches for phones."
            }
            Self::CppQt => {
                "Use stock Qt Widgets without a custom style sheet: x2mcsapi restyles Qt from \
                 the desktop theme."
            }
        }
    }

    /// The Flatpak runtime that ships this language's toolkit.
    pub fn runtime(self) -> FlatpakRuntime {
        match self {
            Self::CGtk | Self::PythonGtk => FlatpakRuntime::Gnome,
            Self::CppQt => FlatpakRuntime::Kde,
            Self::Rust | Self::TypeScript | Self::Web => FlatpakRuntime::Freedesktop,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FlatpakRuntime {
    #[default]
    Freedesktop,
    Gnome,
    Kde,
}

impl FlatpakRuntime {
    pub const ALL: [Self; 3] = [Self::Freedesktop, Self::Gnome, Self::Kde];

    pub fn id(self) -> &'static str {
        match self {
            Self::Freedesktop => "org.freedesktop.Platform",
            Self::Gnome => "org.gnome.Platform",
            Self::Kde => "org.kde.Platform",
        }
    }

    pub fn sdk(self) -> &'static str {
        match self {
            Self::Freedesktop => "org.freedesktop.Sdk",
            Self::Gnome => "org.gnome.Sdk",
            Self::Kde => "org.kde.Sdk",
        }
    }

    /// A version that exists; the user can type a newer one.
    pub fn default_version(self) -> &'static str {
        match self {
            Self::Freedesktop => "25.08",
            Self::Gnome => "49",
            Self::Kde => "6.9",
        }
    }
}

/// What a WASM build of the app is.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmKind {
    /// Draws through `derisk_agent_preview::serve`, so the Agent can show it.
    #[default]
    Preview,
    /// A `wasi:cli` command run by any WASI runtime.
    Command,
}

impl WasmKind {
    pub const ALL: [Self; 2] = [Self::Preview, Self::Command];

    pub fn label(self) -> &'static str {
        match self {
            Self::Preview => "A window the Agent previews (derisk_agent_preview)",
            Self::Command => "A command-line program (wasi:cli)",
        }
    }
}

/// A web app manifest's `display`.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PwaDisplay {
    #[default]
    Standalone,
    Fullscreen,
    MinimalUi,
    Browser,
}

impl PwaDisplay {
    pub const ALL: [Self; 4] = [
        Self::Standalone,
        Self::Fullscreen,
        Self::MinimalUi,
        Self::Browser,
    ];

    pub fn value(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Fullscreen => "fullscreen",
            Self::MinimalUi => "minimal-ui",
            Self::Browser => "browser",
        }
    }
}

/// The manifest file a PWA build ships, beside its `index.html`.
pub const PWA_MANIFEST: &str = "manifest.webmanifest";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Metadata {
    /// The package name every format shares: cargo, pm and the WASM component.
    pub name: String,
    pub display_name: String,
    pub summary: String,
    pub version: String,
    pub license: String,
    pub homepage: String,
    /// Where the app's source goes; empty means `~/src/<name>`.
    pub folder: String,
    /// pm packages the recipe depends on, comma separated.
    pub pm_dependencies: String,
    pub flatpak_id: String,
    pub flatpak_runtime: FlatpakRuntime,
    pub flatpak_runtime_version: String,
    pub flatpak_network: bool,
    pub flatpak_home: bool,
    pub flatpak_gpu: bool,
    pub flatpak_audio: bool,
    pub wasm_kind: WasmKind,
    /// The WASM app also runs in a browser as an installable web app.
    pub pwa: bool,
    /// Empty means the display name.
    pub pwa_short_name: String,
    pub pwa_start_url: String,
    pub pwa_display: PwaDisplay,
    pub pwa_theme_color: String,
    pub pwa_background_color: String,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: String::new(),
            summary: String::new(),
            version: "0.1.0".into(),
            license: "AGPL-3.0-or-later".into(),
            homepage: String::new(),
            folder: String::new(),
            pm_dependencies: String::new(),
            flatpak_id: String::new(),
            flatpak_runtime: FlatpakRuntime::Freedesktop,
            flatpak_runtime_version: FlatpakRuntime::Freedesktop.default_version().into(),
            flatpak_network: false,
            flatpak_home: false,
            flatpak_gpu: true,
            flatpak_audio: false,
            wasm_kind: WasmKind::Preview,
            pwa: true,
            pwa_short_name: String::new(),
            pwa_start_url: "/".into(),
            pwa_display: PwaDisplay::Standalone,
            pwa_theme_color: "#111827".into(),
            pwa_background_color: "#111827".into(),
        }
    }
}

/// A derisk plugin the project builds: a WebAssembly component derisk
/// loads into the desktop shell, built against the WIT interface derisk
/// ships. The Agent writes a starter crate for each from `templates/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginTemplate {
    /// Rows in the command palette (`palette.wit`).
    Palette,
    /// Cards on the overview (`widget.wit`).
    Widget,
}

impl PluginTemplate {
    pub const ALL: [Self; 2] = [Self::Palette, Self::Widget];

    /// What the plugin adds to the desktop, for the prompt.
    pub fn name(self) -> &'static str {
        match self {
            Self::Palette => "derisk command palette plugin",
            Self::Widget => "derisk overview widget plugin",
        }
    }

    /// The folder its crate goes in, under the project's.
    pub fn folder(self) -> &'static str {
        match self {
            Self::Palette => "palette-plugin",
            Self::Widget => "widget-plugin",
        }
    }

    /// The folder derisk loads this kind of plugin from, under
    /// `$XDG_DATA_HOME`.
    pub fn install_dir(self) -> &'static str {
        match self {
            Self::Palette => "derisk/palette-plugins",
            Self::Widget => "derisk/widget-plugins",
        }
    }

    fn wit(self) -> &'static str {
        match self {
            Self::Palette => "crates/derisk-plugin/wit/palette/palette.wit",
            Self::Widget => "crates/derisk-plugin/wit/widget/widget.wit",
        }
    }

    fn examples(self) -> &'static str {
        match self {
            Self::Palette => "plugins/palette/",
            Self::Widget => "plugins/widgets/",
        }
    }

    fn templates(self) -> [(&'static str, &'static str); 2] {
        match self {
            Self::Palette => [
                (
                    "Cargo.toml",
                    include_str!("../templates/palette-plugin/Cargo.toml"),
                ),
                (
                    "src/lib.rs",
                    include_str!("../templates/palette-plugin/lib.rs"),
                ),
            ],
            Self::Widget => [
                (
                    "Cargo.toml",
                    include_str!("../templates/widget-plugin/Cargo.toml"),
                ),
                (
                    "src/lib.rs",
                    include_str!("../templates/widget-plugin/lib.rs"),
                ),
            ],
        }
    }
}

/// The longest name derisk takes for a plugin.
const MAX_PLUGIN_NAME: usize = 32;

/// `text` inside a Rust or TOML double-quoted string, which escape the same
/// two characters.
fn quoted(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The wizard's steps, in order.
pub const STEPS: [&str; 5] = ["App kind", "Language", "Package", "What it does", "Prompt"];

impl AppSetup {
    pub fn display_name(&self) -> String {
        let display = self.metadata.display_name.trim();
        if !display.is_empty() {
            return display.to_owned();
        }
        self.metadata
            .name
            .split(['-', '_'])
            .filter(|word| !word.is_empty())
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn folder(&self) -> PathBuf {
        let folder = self.metadata.folder.trim();
        if let Some(rest) = folder.strip_prefix("~/")
            && let Some(home) = dirs::home_dir()
        {
            return home.join(rest);
        }
        if !folder.is_empty() {
            return PathBuf::from(folder);
        }
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("src")
            .join(self.metadata.name.trim())
    }

    /// The first thing wrong with the answers up to and including `step`, said
    /// so the user knows what to change.
    pub fn problem(&self, step: usize) -> Option<String> {
        if !self.losos && !self.flatpak && !self.wasm && self.plugins().next().is_none() {
            return Some("Pick at least one kind of app.".into());
        }
        if step < 1 {
            return None;
        }
        if self.wasm && !self.language.builds_wasm() {
            return Some(format!(
                "A WASM app has to be written in Rust; {} cannot build one in the Agent.",
                self.language.name()
            ));
        }
        if self.plugins().next().is_some() && self.language != Language::Rust {
            return Some(format!(
                "A derisk plugin has to be written in Rust; {} cannot build one.",
                self.language.name()
            ));
        }
        if step < 2 {
            return None;
        }
        let metadata = &self.metadata;
        let name = metadata.name.trim();
        if name.is_empty() {
            return Some("Give the package a name.".into());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            || name.starts_with('-')
            || name.starts_with(|c: char| c.is_ascii_digit())
        {
            return Some(
                "The package name takes lowercase letters, digits and dashes, starting with a letter."
                    .into(),
            );
        }
        if self.plugins().next().is_some() && name.len() > MAX_PLUGIN_NAME {
            return Some(format!(
                "A derisk plugin's name is at most {MAX_PLUGIN_NAME} characters."
            ));
        }
        if !is_version(&metadata.version) {
            return Some("The version is numbers separated by dots, such as 0.1.0.".into());
        }
        if self.flatpak {
            let id = metadata.flatpak_id.trim();
            let segments: Vec<&str> = id.split('.').collect();
            if segments.len() < 3
                || segments.iter().any(|segment| {
                    segment.is_empty()
                        || !segment
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                })
            {
                return Some(
                    "A Flatpak app ID is a reverse domain name with at least three parts, such as org.example.TodoApp."
                        .into(),
                );
            }
            if metadata.flatpak_runtime_version.trim().is_empty() {
                return Some("Say which version of the Flatpak runtime to use.".into());
            }
        }
        if self.wasm && metadata.pwa {
            if metadata.pwa_start_url.trim().is_empty() {
                return Some("Give the web app a start URL, such as /.".into());
            }
            for (what, color) in [
                ("theme", &metadata.pwa_theme_color),
                ("background", &metadata.pwa_background_color),
            ] {
                if !is_hex_color(color) {
                    return Some(format!(
                        "The web app's {what} colour is a hex colour such as #111827."
                    ));
                }
            }
        }
        if self.folder().is_file() {
            return Some(format!("{} is a file.", self.folder().display()));
        }
        if step < 3 {
            return None;
        }
        if self.description.trim().is_empty() {
            return Some("Say what the app should do.".into());
        }
        None
    }

    /// Sets the language, and the Flatpak runtime with it unless the user
    /// changed the runtime themselves.
    pub fn set_language(&mut self, language: Language) {
        let old = self.language.runtime();
        self.language = language;
        let metadata = &mut self.metadata;
        if metadata.flatpak_runtime == old
            && metadata.flatpak_runtime_version == old.default_version()
        {
            metadata.flatpak_runtime = language.runtime();
            metadata.flatpak_runtime_version = language.runtime().default_version().into();
        }
    }

    /// The derisk plugins picked, in the order the prompt names them.
    pub fn plugins(&self) -> impl Iterator<Item = PluginTemplate> {
        let picked = [self.palette_plugin, self.widget_plugin];
        PluginTemplate::ALL
            .into_iter()
            .zip(picked)
            .filter_map(|(plugin, picked)| picked.then_some(plugin))
    }

    /// The starter files for `plugin`, with the answers filled in, by path
    /// under the project's folder.
    pub fn plugin_files(&self, plugin: PluginTemplate) -> Vec<(PathBuf, String)> {
        let metadata = &self.metadata;
        let name = metadata.name.trim();
        let summary = match metadata.summary.trim() {
            "" => format!("{} for derisk", self.display_name()),
            summary => summary.to_owned(),
        };
        let license = match metadata.license.trim() {
            "" => "AGPL-3.0-or-later",
            license => license,
        };
        let values = [
            ("{{crate}}", format!("{name}-{}", plugin.folder())),
            ("{{name}}", name.to_owned()),
            ("{{display_name}}", quoted(&self.display_name())),
            ("{{summary}}", quoted(&summary)),
            ("{{version}}", metadata.version.trim().to_owned()),
            ("{{license}}", quoted(license)),
        ];
        plugin
            .templates()
            .into_iter()
            .map(|(path, template)| {
                let text = values
                    .iter()
                    .fold(template.to_owned(), |text, (key, value)| {
                        text.replace(key, value)
                    });
                (PathBuf::from(plugin.folder()).join(path), text)
            })
            .collect()
    }

    /// What every later chat in the project is told about the app.
    pub fn instructions(&self) -> String {
        let mut kinds = Vec::new();
        if self.losos {
            kinds.push("a LosOS app with a pm recipe".to_owned());
        }
        if self.flatpak {
            kinds.push(format!("a Flatpak ({})", self.metadata.flatpak_id.trim()));
        }
        if self.wasm {
            kinds.push("a WASM app (wasm32-wasip2)".to_owned());
        }
        for plugin in self.plugins() {
            kinds.push(format!("a {} in {}/", plugin.name(), plugin.folder()));
        }
        format!(
            "This project builds {} (package `{}`), in {}, written in {}. It ships as {}.",
            self.display_name(),
            self.metadata.name.trim(),
            self.folder().display(),
            self.language.name(),
            join_list(&kinds),
        )
    }

    /// The web app manifest for the PWA build, when there is one.
    pub fn pwa_manifest(&self) -> Option<serde_json::Value> {
        let metadata = &self.metadata;
        if !self.wasm || !metadata.pwa {
            return None;
        }
        let short_name = match metadata.pwa_short_name.trim() {
            "" => self.display_name(),
            short => short.to_owned(),
        };
        let start_url = metadata.pwa_start_url.trim();
        let mut manifest = serde_json::json!({
            "name": self.display_name(),
            "short_name": short_name,
            "start_url": start_url,
            "scope": start_url,
            "display": metadata.pwa_display.value(),
            "theme_color": metadata.pwa_theme_color.trim(),
            "background_color": metadata.pwa_background_color.trim(),
            "icons": [
                {"src": "icons/icon-192.png", "sizes": "192x192", "type": "image/png"},
                {"src": "icons/icon-512.png", "sizes": "512x512", "type": "image/png"},
                {"src": "icons/icon-512-maskable.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable"}
            ],
        });
        if !metadata.summary.trim().is_empty() {
            manifest["description"] = metadata.summary.trim().into();
        }
        Some(manifest)
    }

    /// Creates the app's folder and writes the PWA's manifest and each
    /// plugin's starter crate into it, leaving any file that is there
    /// already: the agent may have edited it since, and the prompt tells it
    /// what to keep.
    pub fn write_files(&self) -> std::io::Result<()> {
        let folder = self.folder();
        std::fs::create_dir_all(&folder)?;
        for plugin in self.plugins() {
            for (path, text) in self.plugin_files(plugin) {
                let path = folder.join(path);
                if path.exists() {
                    continue;
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, text)?;
            }
        }
        let Some(manifest) = self.pwa_manifest() else {
            return Ok(());
        };
        let path = folder.join(PWA_MANIFEST);
        if path.exists() {
            return Ok(());
        }
        let text = serde_json::to_string_pretty(&manifest).map_err(std::io::Error::other)?;
        std::fs::write(path, text + "\n")
    }

    /// The prompt the answers add up to: everything the agent needs to start,
    /// in the order it will do it.
    pub fn prompt(&self) -> String {
        let metadata = &self.metadata;
        let name = metadata.name.trim();
        let version = metadata.version.trim();
        let mut prompt = format!(
            "Build a new app, {}, in {}.\n\n",
            self.display_name(),
            self.folder().display()
        );
        prompt.push_str("## What it should do\n\n");
        prompt.push_str(self.description.trim());
        prompt.push_str("\n\n## Language\n\n");
        prompt.push_str(&format!(
            "Write it in {}. {}\n\n",
            self.language.name(),
            self.language.toolkit()
        ));

        prompt.push_str("## Package metadata\n\n");
        prompt.push_str(&format!("- Package name: `{name}`\n"));
        prompt.push_str(&format!("- Display name: {}\n", self.display_name()));
        if !metadata.summary.trim().is_empty() {
            prompt.push_str(&format!("- Summary: {}\n", metadata.summary.trim()));
        }
        prompt.push_str(&format!("- Version: {version}\n"));
        if !metadata.license.trim().is_empty() {
            prompt.push_str(&format!("- Licence: {}\n", metadata.license.trim()));
        }
        if !metadata.homepage.trim().is_empty() {
            prompt.push_str(&format!("- Homepage: {}\n", metadata.homepage.trim()));
        }
        prompt.push('\n');

        prompt.push_str("## Packaging\n\n");
        if self.losos {
            let dependencies: Vec<String> = metadata
                .pm_dependencies
                .split(',')
                .map(str::trim)
                .filter(|dependency| !dependency.is_empty())
                .map(|dependency| format!("\"{dependency}\""))
                .collect();
            prompt.push_str(&format!(
                "- **LosOS app.** Write a pm recipe, `build.rhai`, in the app's folder. Recipes \
                 are Rhai scripts that call `package(#{{ ... }})` once:\n\n\
                 ```rhai\n\
                 package(#{{\n    name: \"{name}\",\n    version: \"{version}\",\n    \
                 dependencies: [{}],\n    steps: [\n        \
                 step(Build, \"compile\", [/* compile the app */]),\n        \
                 step(Install, \"stage\", [/* install under /dest/usr */]),\n    ],\n}});\n\
                 ```\n\n  \
                 A dependency is the path of the other package's `build.rhai`, so find where \
                 each one's recipe lives. The `Install` step puts the app under `/dest/usr` \
                 (`usr/bin/{name}`, plus a `.desktop` file and icon under `usr/share`). pm runs \
                 each step in a jail with no shell, `/nix/store` read-only and network only for \
                 recipes that need it, so add `--offline` wherever a step can work without it. \
                 Check the recipe with `pm_explain`, then build it with `pm_build`.\n",
                dependencies.join(", "),
            ));
        }
        if self.flatpak {
            let runtime = metadata.flatpak_runtime;
            let id = metadata.flatpak_id.trim();
            let mut finish_args = vec!["--socket=wayland", "--socket=fallback-x11", "--share=ipc"];
            if metadata.flatpak_gpu {
                finish_args.push("--device=dri");
            }
            if metadata.flatpak_network {
                finish_args.push("--share=network");
            }
            if metadata.flatpak_home {
                finish_args.push("--filesystem=home");
            }
            if metadata.flatpak_audio {
                finish_args.push("--socket=pulseaudio");
            }
            prompt.push_str(&format!(
                "- **Flatpak.** Write a flatpak-builder manifest, `{id}.yml`, with app ID `{id}`, \
                 runtime `{}` version `{}`, SDK `{}` and whatever SDK extension {} needs, and \
                 exactly these finish-args: {}. Ask before adding any other permission. Add \
                 `{id}.desktop` and `{id}.metainfo.xml` (summary, licence, version {version}) \
                 so Bazaar and Flathub can list it.\n",
                runtime.id(),
                metadata.flatpak_runtime_version.trim(),
                runtime.sdk(),
                self.language.name(),
                finish_args
                    .iter()
                    .map(|arg| format!("`{arg}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        if self.wasm {
            let what = match metadata.wasm_kind {
                WasmKind::Preview => {
                    "a window drawn through `derisk_agent_preview::serve`, so the Agent's preview pane \
                     can run it: build it with `preview_build` and target `wasm`"
                }
                WasmKind::Command => {
                    "a `wasi:cli` command any WASI runtime can run, built with \
                     `cargo build --target wasm32-wasip2`"
                }
            };
            prompt.push_str(&format!(
                "- **WASM app.** Build `{name}` for `wasm32-wasip2` as {what}. It runs \
                 sandboxed with no file or network access, so keep everything it needs in \
                 memory.\n"
            ));
            if let Some(manifest) = self.pwa_manifest() {
                let manifest = serde_json::to_string_pretty(&manifest).unwrap_or_default();
                prompt.push_str(&format!(
                    "- **PWA.** The WASM app also runs in a browser as an installable web app. \
                     The Agent wrote `{PWA_MANIFEST}` in the app's folder; keep it as it is unless \
                     I ask, and if it is missing, write it with exactly this:\n\n```json\n\
                     {manifest}\n```\n\n  Build a browser version of the same UI with \
                     eframe for `wasm32-unknown-unknown` (trunk), with an `index.html` that \
                     links the manifest, a service worker that caches the app so it works \
                     offline, and the three icons the manifest names.\n"
                ));
            }
        }
        for plugin in self.plugins() {
            let crate_name = format!("{name}-{}", plugin.folder());
            let module = crate_name.replace('-', "_");
            let folder = plugin.folder();
            prompt.push_str(&format!(
                "- **{}.** The Agent wrote a starter crate, `{crate_name}`, in `{folder}/`. It \
                 depends on derisk's SDK by git, so it builds against the interface derisk \
                 ships: read `{}` and the bundled plugins under `{}` in \
                 https://github.com/losos-project/derisk before changing it. Keep the plugin's \
                 manifest name `{name}`, and list only the inputs and action kinds it uses: \
                 derisk shows a plugin only what its manifest asks for and drops anything \
                 outside it. Test its logic with `cargo test` in `{folder}/`. Build it with \
                 `cargo build --release --target wasm32-unknown-unknown`, then make it a \
                 component with `wasm-tools component new \
                 target/wasm32-unknown-unknown/release/{module}.wasm -o {name}.wasm` \
                 (`cargo install wasm-tools` if it is missing). Never build it for \
                 `wasm32-wasip2`: derisk gives plugins no WASI and refuses one that imports \
                 it. To install it, copy `{name}.wasm` to `~/.local/share/{}/`, sign it there \
                 with `pm sign`, and trust the key once with `pm trust {name}.wasm.sig \
                 --trust-dir ~/.config/derisk/trusted`; derisk loads it the next time the \
                 session starts.\n",
                capitalized(plugin.name()),
                plugin.wit(),
                plugin.examples(),
                plugin.install_dir(),
            ));
        }
        prompt.push_str(
            "\nWhen it works, tell me what you built, how to run it, and how to install each \
             package.",
        );
        prompt
    }
}

fn is_hex_color(text: &str) -> bool {
    text.trim()
        .strip_prefix('#')
        .is_some_and(|hex| hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Dotted numbers such as `0.1.0`, the versions pm, cargo and Flatpak all accept.
fn is_version(version: &str) -> bool {
    version
        .trim()
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// The wizard while it is open.
pub struct SetupForm {
    pub step: usize,
    pub setup: AppSetup,
    /// The prompt as shown in the last step; the user may have edited it.
    pub prompt: String,
    /// Whether `prompt` was edited since it was composed, so going back and
    /// changing an answer does not throw the edit away unasked.
    pub prompt_edited: bool,
    /// The project being set up again, or `None` for a new one.
    pub project_id: Option<String>,
}

pub enum SetupAction {
    Cancel,
    /// Create or update the project and send the prompt.
    Start,
}

impl SetupForm {
    pub fn new(setup: AppSetup, project_id: Option<String>) -> Self {
        let prompt = setup.prompt();
        Self {
            step: 0,
            setup,
            prompt,
            prompt_edited: false,
            project_id,
        }
    }

    /// Moves to `step`, composing the prompt afresh on the last step unless
    /// the user already edited it.
    pub fn go_to(&mut self, step: usize) {
        if step == STEPS.len() - 1 && !self.prompt_edited {
            self.prompt = self.setup.prompt();
        }
        self.step = step;
    }

    /// The furthest step the answers so far let the user reach.
    pub fn reachable(&self) -> usize {
        (0..STEPS.len() - 1)
            .find(|step| self.setup.problem(*step).is_some())
            .unwrap_or(STEPS.len() - 1)
    }
}

#[cfg(test)]
mod tests {
    use miette::IntoDiagnostic as _;

    use super::*;

    fn todo() -> AppSetup {
        let mut setup = AppSetup::default();
        setup.metadata.name = "todo-app".into();
        setup.metadata.folder = "/tmp/todo-app".into();
        setup.description = "A todo list.".into();
        setup
    }

    #[test]
    fn each_step_names_what_is_missing() {
        let mut setup = AppSetup {
            losos: false,
            ..AppSetup::default()
        };
        assert!(setup.problem(0).is_some());
        setup.wasm = true;
        assert_eq!(setup.problem(0), None);
        setup.set_language(Language::Web);
        assert!(setup.problem(1).unwrap().contains("Rust"));
        setup.set_language(Language::Rust);
        assert!(setup.problem(2).unwrap().contains("name"));
        setup.metadata.name = "Todo App".into();
        assert!(setup.problem(2).unwrap().contains("lowercase"));
        setup.metadata.name = "todo-app".into();
        setup.metadata.version = "1.x".into();
        assert!(setup.problem(2).unwrap().contains("version"));
        setup.metadata.version = "1.0".into();
        assert_eq!(setup.problem(2), None);
        assert!(setup.problem(3).unwrap().contains("do"));
        setup.description = "Lists todos".into();
        assert_eq!(setup.problem(3), None);
    }

    #[test]
    fn flatpak_needs_a_reverse_domain_id() {
        let mut setup = todo();
        setup.flatpak = true;
        assert!(setup.problem(2).unwrap().contains("app ID"));
        setup.metadata.flatpak_id = "org.example".into();
        assert!(setup.problem(2).is_some());
        setup.metadata.flatpak_id = "org.example.TodoApp".into();
        assert_eq!(setup.problem(3), None);
    }

    #[test]
    fn display_name_falls_back_to_the_package_name() {
        let mut setup = todo();
        assert_eq!(setup.display_name(), "Todo App");
        setup.metadata.display_name = "Todos".into();
        assert_eq!(setup.display_name(), "Todos");
    }

    #[test]
    fn language_picks_the_runtime_until_the_user_does() {
        let mut setup = todo();
        setup.set_language(Language::CppQt);
        assert_eq!(setup.metadata.flatpak_runtime, FlatpakRuntime::Kde);
        assert_eq!(setup.metadata.flatpak_runtime_version, "6.9");
        setup.metadata.flatpak_runtime_version = "6.10".into();
        setup.set_language(Language::CGtk);
        assert_eq!(setup.metadata.flatpak_runtime, FlatpakRuntime::Kde);
    }

    #[test]
    fn prompt_carries_every_answer() {
        let mut setup = todo();
        setup.flatpak = true;
        setup.wasm = true;
        setup.metadata.flatpak_id = "org.example.TodoApp".into();
        setup.metadata.flatpak_network = true;
        setup.metadata.pm_dependencies = "ripgrep, fd".into();
        setup.metadata.summary = "Keeps track of things".into();
        let prompt = setup.prompt();
        assert!(prompt.contains("Build a new app, Todo App, in /tmp/todo-app."));
        assert!(prompt.contains("A todo list."));
        assert!(prompt.contains("Write it in Rust."));
        assert!(prompt.contains("`build.rhai`"));
        assert!(prompt.contains("version: \"0.1.0\""));
        assert!(prompt.contains("dependencies: [\"ripgrep\", \"fd\"]"));
        assert!(prompt.contains("`org.example.TodoApp.yml`"));
        assert!(prompt.contains("`--share=network`"));
        assert!(!prompt.contains("--filesystem=home"));
        assert!(prompt.contains("wasm32-wasip2"));
        assert!(prompt.contains("Summary: Keeps track of things"));
    }

    #[test]
    fn wasm_apps_get_a_pwa_manifest_starting_at_root() -> miette::Result<()> {
        let mut setup = todo();
        assert_eq!(setup.pwa_manifest(), None);
        setup.wasm = true;
        setup.metadata.summary = "Keeps track of things".into();
        let manifest = setup
            .pwa_manifest()
            .ok_or_else(|| miette::miette!("no manifest"))?;
        assert_eq!(manifest["name"], "Todo App");
        assert_eq!(manifest["short_name"], "Todo App");
        assert_eq!(manifest["start_url"], "/");
        assert_eq!(manifest["display"], "standalone");
        assert_eq!(manifest["description"], "Keeps track of things");
        assert_eq!(manifest["icons"].as_array().map(Vec::len), Some(3));
        assert!(setup.prompt().contains(PWA_MANIFEST));
        setup.metadata.pwa = false;
        assert_eq!(setup.pwa_manifest(), None);
        assert!(!setup.prompt().contains(PWA_MANIFEST));
        Ok(())
    }

    #[test]
    fn pwa_colours_must_be_hex() {
        let mut setup = todo();
        setup.wasm = true;
        setup.metadata.pwa_theme_color = "navy".into();
        assert!(
            setup
                .problem(2)
                .unwrap_or_default()
                .contains("theme colour")
        );
        setup.metadata.pwa_theme_color = "#1e3a8a".into();
        assert_eq!(setup.problem(3), None);
    }

    #[test]
    fn prompt_leaves_out_kinds_not_picked() {
        let prompt = todo().prompt();
        assert!(prompt.contains("LosOS app"));
        assert!(!prompt.contains("Flatpak"));
        assert!(!prompt.contains("WASM"));
    }

    #[test]
    fn editing_the_prompt_survives_going_back() {
        let mut form = SetupForm::new(todo(), None);
        form.go_to(4);
        form.prompt = "my own words".into();
        form.prompt_edited = true;
        form.go_to(3);
        form.setup.description = "Something else".into();
        form.go_to(4);
        assert_eq!(form.prompt, "my own words");
    }

    #[test]
    fn instructions_name_the_app_and_its_packages() {
        let mut setup = todo();
        setup.flatpak = true;
        setup.metadata.flatpak_id = "org.example.TodoApp".into();
        assert_eq!(
            setup.instructions(),
            "This project builds Todo App (package `todo-app`), in /tmp/todo-app, written in \
             Rust. It ships as a LosOS app with a pm recipe and a Flatpak (org.example.TodoApp)."
        );
    }

    #[test]
    fn a_plugin_is_enough_and_needs_rust_and_a_short_name() {
        let mut setup = AppSetup {
            losos: false,
            ..todo()
        };
        assert!(setup.problem(0).is_some());
        setup.palette_plugin = true;
        assert_eq!(setup.problem(0), None);
        setup.set_language(Language::PythonGtk);
        assert!(setup.problem(1).unwrap_or_default().contains("Rust"));
        setup.set_language(Language::Rust);
        setup.metadata.name = "a".repeat(MAX_PLUGIN_NAME + 1);
        assert!(setup.problem(2).unwrap_or_default().contains("at most"));
        setup.metadata.name = "todo-app".into();
        assert_eq!(setup.problem(3), None);
    }

    #[test]
    fn plugin_templates_take_the_answers() {
        let mut setup = todo();
        setup.palette_plugin = true;
        setup.widget_plugin = true;
        setup.metadata.display_name = r#"Todo "Pro""#.into();
        for plugin in PluginTemplate::ALL {
            let files = setup.plugin_files(plugin);
            let paths: Vec<_> = files.iter().map(|(path, _)| path.clone()).collect();
            assert_eq!(
                paths,
                [
                    PathBuf::from(plugin.folder()).join("Cargo.toml"),
                    PathBuf::from(plugin.folder()).join("src/lib.rs"),
                ]
            );
            for (_, text) in &files {
                assert!(!text.contains("{{"), "{text}");
            }
            let (_, manifest) = &files[0];
            assert!(manifest.contains(&format!("name = \"todo-app-{}\"", plugin.folder())));
            assert!(manifest.contains("git = \"https://github.com/losos-project/derisk\""));
            let (_, source) = &files[1];
            assert!(source.contains(r#"Manifest::new("todo-app","#));
            assert!(source.contains(r#"Todo \"Pro\""#));
        }
        let prompt = setup.prompt();
        assert!(prompt.contains("wasm-tools component new"));
        assert!(prompt.contains("todo_app_palette_plugin.wasm"));
        assert!(prompt.contains("~/.local/share/derisk/widget-plugins/"));
        assert!(
            setup
                .instructions()
                .contains("a derisk overview widget plugin in widget-plugin/")
        );
    }

    #[test]
    fn writing_files_keeps_what_is_there() -> miette::Result<()> {
        let folder =
            std::env::temp_dir().join(format!("derisk-agent-setup-{}", std::process::id()));
        let mut setup = todo();
        setup.metadata.folder = folder.display().to_string();
        setup.widget_plugin = true;
        let lib = folder.join("widget-plugin/src/lib.rs");
        std::fs::create_dir_all(lib.parent().ok_or_else(|| miette::miette!("no parent"))?)
            .into_diagnostic()?;
        std::fs::write(&lib, "// mine").into_diagnostic()?;
        setup.write_files().into_diagnostic()?;
        assert_eq!(std::fs::read_to_string(&lib).into_diagnostic()?, "// mine");
        assert!(folder.join("widget-plugin/Cargo.toml").is_file());
        std::fs::remove_dir_all(&folder).into_diagnostic()?;
        Ok(())
    }

    #[test]
    fn old_projects_without_a_setup_still_load() -> miette::Result<()> {
        let setup: AppSetup = serde_json::from_str(r#"{"flatpak": true}"#).into_diagnostic()?;
        assert!(setup.losos && setup.flatpak);
        assert_eq!(setup.metadata.version, "0.1.0");
        Ok(())
    }
}
