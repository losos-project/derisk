//! The derisk core apps in one catalog.
//!
//! Each app lives in its own crate and implements [`mcsapi_ui::App`]. This
//! crate lists them ([`APPS`]), registers them with an
//! [`mcsapi_runtime::Runtime`], and keeps the running [`App`] objects next
//! to their runtime instances ([`Session`]), so a compositor host only has to
//! give each instance a surface and call [`mcsapi_ui::run_frame`].
//!
//! ```
//! use derisk_apps::{SETTINGS, Session};
//! use mcsapi_runtime::AppId;
//!
//! let mut session = Session::new()?;
//! assert_eq!(session.runtime().apps().count(), derisk_apps::APPS.len());
//! let instance = session.launch(&AppId::new(SETTINGS).unwrap())?;
//! assert_eq!(session.app_mut(instance).unwrap().title(), "Settings");
//! session.stop(instance)?;
//! # Ok::<(), mcsapi_runtime::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use mcsapi_runtime::{AppId, Error, InstanceId, Manifest, Runtime};
use mcsapi_ui::{App, Theme, egui};

/// App ID of Settings.
pub const SETTINGS: &str = "org.derisk.settings";
/// App ID of Files.
pub const FILES: &str = "org.derisk.files";
/// App ID of the Text Editor.
pub const EDITOR: &str = "org.derisk.editor";
/// App ID of the System Monitor.
pub const MONITOR: &str = "org.derisk.monitor";
/// App ID of the Calculator.
pub const CALCULATOR: &str = "org.derisk.calculator";
/// App ID of Machines.
pub const MACHINES: &str = derisk_machines::APP_ID;
/// App ID of the Store.
pub const STORE: &str = derisk_store::APP_ID;
/// App ID of Android.
pub const ANDROID: &str = derisk_android::APP_ID;

/// A catalog entry for one core app.
#[derive(Clone, Copy, Debug)]
pub struct AppInfo {
    /// Reverse-DNS app ID.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// One-line description for launchers and search.
    pub summary: &'static str,
    /// An emoji icon from egui's built-in font.
    pub icon: &'static str,
    /// Search keywords.
    pub keywords: &'static [&'static str],
    /// The app's `.desktop` file. Its `[Desktop Action]` groups are the
    /// actions [`AppInfo::create_action`] accepts.
    pub desktop_file: &'static str,
    create: fn() -> Box<dyn App>,
    action: fn(&str) -> Option<Box<dyn App>>,
    /// Opens the app on a file or folder, for the apps whose desktop file
    /// takes one (`%F`): `None` for the others.
    open: Option<Opener>,
}

/// Makes an app open on a file or folder.
type Opener = fn(&Path) -> Box<dyn App>;

impl AppInfo {
    /// The validated app ID.
    pub fn app_id(&self) -> AppId {
        AppId::new(self.id).expect("catalog IDs are valid")
    }

    /// The runtime manifest.
    pub fn manifest(&self) -> Manifest {
        Manifest::new(self.app_id(), self.name)
    }

    /// Creates a fresh app object with its default state.
    pub fn create(&self) -> Box<dyn App> {
        (self.create)()
    }

    /// The desktop file ID (`org.derisk.files.desktop`).
    pub fn desktop_id(&self) -> String {
        format!("{}.desktop", self.id)
    }

    /// Creates the app opened by one of its desktop actions, or `None` for
    /// an unknown action.
    pub fn create_action(&self, action: &str) -> Option<Box<dyn App>> {
        (self.action)(action)
    }

    /// Creates the app opened on `path`, or `None` for an app that opens
    /// no files.
    pub fn create_open(&self, path: &Path) -> Option<Box<dyn App>> {
        self.open.map(|open| open(path))
    }

    /// Whether the app matches a case-insensitive search.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.name.to_lowercase().contains(&query)
            || self.summary.to_lowercase().contains(&query)
            || self.keywords.iter().any(|k| k.contains(&query))
    }
}

/// Every core app, in launcher order.
pub const APPS: [AppInfo; 8] = [
    AppInfo {
        id: FILES,
        name: "Files",
        summary: "Browse, copy, rename, and trash files",
        icon: "🗀",
        keywords: &["file manager", "folders", "browse", "trash"],
        desktop_file: include_str!("../data/org.derisk.files.desktop"),
        create: || Box::new(derisk_files::FilesApp::default()),
        action: files_action,
        open: Some(|path| files_in(path.to_owned())),
    },
    AppInfo {
        id: SETTINGS,
        name: "Settings",
        summary: "Appearance, wallpaper, top bar, shortcuts, input, and power",
        icon: "⚙",
        keywords: &[
            "preferences",
            "theme",
            "accent",
            "keyboard",
            "power",
            "wallpaper",
            "background",
            "shortcuts",
            "privacy",
            "flatpak",
            "permissions",
            "backup",
            "users",
            "accounts",
            "administrator",
            "network",
            "bluetooth",
            "update",
            "release",
            "channel",
        ],
        desktop_file: include_str!("../data/org.derisk.settings.desktop"),
        create: || Box::new(settings()),
        action: settings_action,
        open: None,
    },
    AppInfo {
        id: EDITOR,
        name: "Text Editor",
        summary: "Edit plain-text files",
        icon: "📝",
        keywords: &["notepad", "text", "code", "write"],
        desktop_file: include_str!("../data/org.derisk.editor.desktop"),
        create: || Box::new(derisk_editor::EditorApp::new()),
        action: |a| {
            new_window(a, "new-document", || {
                Box::new(derisk_editor::EditorApp::new())
            })
        },
        open: Some(|path| Box::new(derisk_editor::EditorApp::open(path))),
    },
    AppInfo {
        id: MONITOR,
        name: "System Monitor",
        summary: "CPU, memory, and running processes",
        icon: "📈",
        keywords: &["task manager", "processes", "cpu", "memory", "kill"],
        desktop_file: include_str!("../data/org.derisk.monitor.desktop"),
        create: || Box::new(derisk_monitor::MonitorApp::default()),
        action: |a| {
            new_window(a, "new-window", || {
                Box::new(derisk_monitor::MonitorApp::default())
            })
        },
        open: None,
    },
    AppInfo {
        id: CALCULATOR,
        name: "Calculator",
        summary: "Arithmetic and scientific functions",
        icon: "🖩",
        keywords: &["math", "calc", "numbers"],
        desktop_file: include_str!("../data/org.derisk.calculator.desktop"),
        create: || Box::new(derisk_calculator::CalculatorApp::default()),
        action: |a| {
            new_window(a, "new-window", || {
                Box::new(derisk_calculator::CalculatorApp::default())
            })
        },
        open: None,
    },
    AppInfo {
        id: MACHINES,
        name: "Machines",
        summary: "Make, start and stop the machines apps run in",
        icon: "💻",
        keywords: &[
            "virtual machine",
            "vm",
            "libvirt",
            "qube",
            "sandbox",
            "hypervisor",
        ],
        desktop_file: include_str!("../data/org.derisk.machines.desktop"),
        create: || Box::new(derisk_machines::MachinesApp::new()),
        action: |a| {
            new_window(a, "new-window", || {
                Box::new(derisk_machines::MachinesApp::new())
            })
        },
        open: None,
    },
    AppInfo {
        id: STORE,
        name: "Store",
        summary: "Find, install and update apps from Flathub",
        icon: "📦",
        keywords: &[
            "app store",
            "software",
            "flathub",
            "flatpak",
            "install",
            "updates",
            "bazaar",
        ],
        desktop_file: include_str!("../data/org.derisk.store.desktop"),
        create: || Box::new(derisk_store::StoreApp::new()),
        action: |a| {
            (a == "installed").then(|| {
                let mut store = derisk_store::StoreApp::new();
                store.show_installed();
                Box::new(store) as Box<dyn App>
            })
        },
        open: None,
    },
    AppInfo {
        id: ANDROID,
        name: "Android",
        summary: "Your own Pixel, and its apps in windows of their own",
        icon: "📱",
        keywords: &[
            "android",
            "apps",
            "pixel",
            "phone",
            "google play",
            "apk",
            "emulator",
        ],
        desktop_file: include_str!("../data/org.derisk.android.desktop"),
        create: || Box::new(derisk_android::AndroidSupport::new()),
        action: |a| {
            new_window(a, "apps", || {
                Box::new(derisk_android::AndroidSupport::apps())
            })
        },
        open: None,
    },
];

/// An app whose only action opens a fresh instance.
fn new_window(action: &str, id: &str, create: fn() -> Box<dyn App>) -> Option<Box<dyn App>> {
    (action == id).then(create)
}

fn files_action(action: &str) -> Option<Box<dyn App>> {
    let home = derisk_sys::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let dir = match action {
        "new-window" => home,
        "documents" => home.join("Documents"),
        "downloads" => home.join("Downloads"),
        _ => return None,
    };
    Some(files_in(dir))
}

/// Files showing `dir`, opening what is picked there with `xdg-open`.
fn files_in(dir: PathBuf) -> Box<dyn App> {
    Box::new(derisk_files::FilesApp::new(
        derisk_files::Browser::new(dir),
        Box::new(derisk_files::xdg_open),
    ))
}

/// Settings, its Privacy page reading Flatpak's apps from the machine they
/// run in when there is one, and this computer's own otherwise.
fn settings() -> derisk_settings::SettingsApp {
    let app = derisk_settings::SettingsApp::default();
    #[cfg(not(target_arch = "wasm32"))]
    let app = app.with_flatpak_finder(derisk_machines::flatpak::find);
    app
}

fn settings_action(action: &str) -> Option<Box<dyn App>> {
    use derisk_settings::Page;

    let page = match action {
        "appearance" => Page::Appearance,
        "wallpaper" => Page::Wallpaper,
        "top-bar" => Page::TopBar,
        "desktop" => Page::Desktop,
        "assistant" => Page::Assistant,
        "input" => Page::Input,
        "shortcuts" => Page::Shortcuts,
        "network" => Page::Network,
        "bluetooth" => Page::Bluetooth,
        "privacy" => Page::Privacy,
        "sign-in" => Page::SignIn,
        "users" => Page::Users,
        "notifications" => Page::Notifications,
        "power" => Page::Power,
        "backups" => Page::Backups,
        "default-apps" => Page::DefaultApps,
        "system" => Page::System,
        "about" => Page::About,
        _ => return None,
    };
    let mut app = settings();
    app.page = page;
    Some(Box::new(app))
}

/// Looks up a core app by ID (`org.derisk.files`) or desktop file ID
/// (`org.derisk.files.desktop`).
pub fn find(id: &str) -> Option<&'static AppInfo> {
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    APPS.iter().find(|app| app.id == id)
}

/// Registers every core app with `runtime`.
pub fn register_all(runtime: &mut Runtime) -> Result<(), Error> {
    APPS.iter()
        .try_for_each(|app| runtime.register(app.manifest()))
}

/// egui visuals for `theme`, so widgets match the shell colors.
pub fn visuals(theme: &Theme) -> egui::Visuals {
    let [r, g, b, _] = theme.background.to_array();
    let dark = u16::from(r) + u16::from(g) + u16::from(b) < 384;
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.override_text_color = Some(theme.foreground);
    visuals.panel_fill = theme.background;
    visuals.window_fill = theme.background;
    visuals.extreme_bg_color = theme.surface;
    visuals.faint_bg_color = theme.surface;
    visuals.selection.bg_fill = theme.accent.gamma_multiply(0.45);
    visuals.selection.stroke.color = theme.accent;
    visuals.hyperlink_color = theme.accent;
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_stroke.color = theme.border;
    widgets.inactive.weak_bg_fill = theme.surface;
    widgets.inactive.bg_fill = theme.surface;
    let hover = theme.surface.lerp_to_gamma(theme.border, 0.35);
    widgets.hovered.weak_bg_fill = hover;
    widgets.hovered.bg_fill = hover;
    widgets.hovered.bg_stroke.color = theme.accent;
    widgets.active.weak_bg_fill = hover;
    widgets.active.bg_fill = hover;
    // The open state also colors window title bars.
    widgets.open.weak_bg_fill = theme.surface;
    widgets.open.bg_fill = theme.surface;
    widgets.active.bg_stroke.color = theme.accent;
    visuals
}

/// Running core apps, kept in step with a [`Runtime`].
pub struct Session {
    runtime: Runtime,
    apps: BTreeMap<InstanceId, Box<dyn App>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("runtime", &self.runtime)
            .field("running", &self.apps.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Session {
    /// A runtime with every core app registered and none running.
    pub fn new() -> Result<Self, Error> {
        let mut runtime = Runtime::new();
        register_all(&mut runtime)?;
        Ok(Self {
            runtime,
            apps: BTreeMap::new(),
        })
    }

    /// The underlying runtime.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Starts a new instance of a core app.
    pub fn launch(&mut self, id: &AppId) -> Result<InstanceId, Error> {
        let info = find(id.as_str()).ok_or_else(|| Error::UnknownApp(id.clone()))?;
        let instance = self.runtime.launch(id)?;
        self.apps.insert(instance, info.create());
        Ok(instance)
    }

    /// Starts a core app through one of its desktop actions, for example
    /// Settings on its Appearance page. An unknown action is reported as
    /// an unknown app.
    pub fn launch_action(&mut self, id: &AppId, action: &str) -> Result<InstanceId, Error> {
        let info = find(id.as_str()).ok_or_else(|| Error::UnknownApp(id.clone()))?;
        let app = info
            .create_action(action)
            .ok_or_else(|| Error::UnknownApp(id.clone()))?;
        let instance = self.runtime.launch(id)?;
        self.apps.insert(instance, app);
        Ok(instance)
    }

    /// Starts a core app on a file or folder, for example the Text Editor
    /// on a document `xdg-open` handed it. An app that opens no files is
    /// reported as an unknown app.
    pub fn launch_path(&mut self, id: &AppId, path: &Path) -> Result<InstanceId, Error> {
        let info = find(id.as_str()).ok_or_else(|| Error::UnknownApp(id.clone()))?;
        let app = info
            .create_open(path)
            .ok_or_else(|| Error::UnknownApp(id.clone()))?;
        let instance = self.runtime.launch(id)?;
        self.apps.insert(instance, app);
        Ok(instance)
    }

    /// Starts a core app the host made itself, for example Files with its
    /// own opener or the editor on a document.
    pub fn launch_app(&mut self, id: &AppId, app: Box<dyn App>) -> Result<InstanceId, Error> {
        find(id.as_str()).ok_or_else(|| Error::UnknownApp(id.clone()))?;
        let instance = self.runtime.launch(id)?;
        self.apps.insert(instance, app);
        Ok(instance)
    }

    /// Stops an instance and drops its app.
    pub fn stop(&mut self, instance: InstanceId) -> Result<AppId, Error> {
        let app = self.runtime.stop(instance)?;
        self.apps.remove(&instance);
        Ok(app)
    }

    /// A running app, to draw with [`mcsapi_ui::run_frame`].
    pub fn app_mut(&mut self, instance: InstanceId) -> Option<&mut (dyn App + 'static)> {
        self.apps.get_mut(&instance).map(Box::as_mut)
    }

    /// Running instances with their apps, ordered by launch.
    pub fn running(&mut self) -> impl Iterator<Item = (InstanceId, &mut (dyn App + 'static))> {
        self.apps.iter_mut().map(|(&id, app)| (id, app.as_mut()))
    }
}
