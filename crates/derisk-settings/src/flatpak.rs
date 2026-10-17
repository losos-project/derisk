//! Flatpak app permissions: what each installed app asks for, the user's
//! overrides of it, and its portal permissions.
//!
//! Static permissions come from the app's `metadata` keyfile and are
//! changed the way `flatpak override --user` changes them: by writing
//! `[Context]` keys to `$XDG_DATA_HOME/flatpak/overrides/<app>`, `!x` to
//! take a permission away and `x` to grant one. Writing the file directly
//! needs no `flatpak` binary and lands in the same place, so the flatpak
//! CLI, GNOME Settings and Flatseal all see the same overrides.
//!
//! Portal permissions (camera, location, background, notifications,
//! screenshots) live in xdg-desktop-portal's permission store, which only
//! its D-Bus service writes; they go through `flatpak permission-set`.
//!
//! On LosOS's PCs Flatpak does not run on this computer at all: its apps
//! live in a virtual machine of their own. [`Installation`] is what the
//! Privacy page reads and changes, so the same page works on [`Dirs`] here
//! and, through derisk-machines, on that machine's.

use std::{
    collections::BTreeMap,
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
};

use derisk_sys::fs;

/// A `[Context]` key of a flatpak keyfile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// `shared=`: network, ipc.
    Shared,
    /// `sockets=`: wayland, x11, pulseaudio, cups, ...
    Sockets,
    /// `devices=`: dri, all, kvm, ...
    Devices,
    /// `features=`: bluetooth, devel, ...
    Features,
    /// `filesystems=`: home, host, xdg-download, paths.
    Filesystems,
}

impl Key {
    /// Every key, in file order.
    pub const ALL: [Self; 5] = [
        Self::Shared,
        Self::Sockets,
        Self::Devices,
        Self::Features,
        Self::Filesystems,
    ];

    /// The key's name in the keyfile.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Sockets => "sockets",
            Self::Devices => "devices",
            Self::Features => "features",
            Self::Filesystems => "filesystems",
        }
    }
}

/// The `[Context]` group: per key, each value and whether it is granted
/// (`x`, or `x:ro`/`x:rw`/`x:create` for filesystems) or taken away (`!x`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Context {
    entries: BTreeMap<(Key, String), Grant>,
}

/// How a permission is set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Grant {
    /// Granted (read-write for filesystems).
    Yes,
    /// Read-only (filesystems only).
    ReadOnly,
    /// Taken away.
    No,
}

impl Context {
    /// Parses the `[Context]` group of a flatpak keyfile.
    pub fn parse(keyfile: &str) -> Self {
        let mut context = Self::default();
        let mut in_context = false;
        for line in keyfile.lines().map(str::trim) {
            if line.starts_with('[') {
                in_context = line == "[Context]";
                continue;
            }
            if !in_context || line.starts_with('#') {
                continue;
            }
            let Some((name, values)) = line.split_once('=') else {
                continue;
            };
            let Some(key) = Key::ALL.into_iter().find(|k| k.name() == name.trim()) else {
                continue;
            };
            for value in values.split(';').map(str::trim).filter(|v| !v.is_empty()) {
                let (value, grant) = match value.strip_prefix('!') {
                    Some(v) => (v, Grant::No),
                    None => match value.strip_suffix(":ro") {
                        Some(v) => (v, Grant::ReadOnly),
                        None => (
                            value
                                .strip_suffix(":rw")
                                .or_else(|| value.strip_suffix(":create"))
                                .unwrap_or(value),
                            Grant::Yes,
                        ),
                    },
                };
                context.entries.insert((key, value.to_owned()), grant);
            }
        }
        context
    }

    /// How `value` under `key` is set here, if at all.
    pub fn get(&self, key: Key, value: &str) -> Option<Grant> {
        self.entries.get(&(key, value.to_owned())).copied()
    }

    /// Sets or clears `value` under `key`.
    pub fn set(&mut self, key: Key, value: &str, grant: Option<Grant>) {
        match grant {
            Some(g) => {
                self.entries.insert((key, value.to_owned()), g);
            }
            None => {
                self.entries.remove(&(key, value.to_owned()));
            }
        }
    }

    /// Every value set under `key`.
    pub fn values(&self, key: Key) -> impl Iterator<Item = (&str, Grant)> {
        self.entries
            .iter()
            .filter(move |((k, _), _)| *k == key)
            .map(|((_, v), g)| (v.as_str(), *g))
    }

    /// `self` with `over` applied on top, as flatpak merges overrides.
    pub fn merged(&self, over: &Self) -> Self {
        let mut merged = self.clone();
        merged.entries.extend(over.entries.clone());
        merged
    }

    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Replaces the `[Context]` group in `keyfile`, keeping every other
    /// group (session bus policy, environment) as it was.
    pub fn write_into(&self, keyfile: &str) -> String {
        let mut out = String::new();
        let mut skipping = false;
        for line in keyfile.lines() {
            if line.trim().starts_with('[') {
                skipping = line.trim() == "[Context]";
            }
            if !skipping {
                out.push_str(line);
                out.push('\n');
            }
        }
        if !self.is_empty() {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str("[Context]\n");
            for key in Key::ALL {
                let values: Vec<String> = self
                    .values(key)
                    .map(|(v, g)| match g {
                        Grant::Yes => v.to_owned(),
                        Grant::ReadOnly => format!("{v}:ro"),
                        Grant::No => format!("!{v}"),
                    })
                    .collect();
                if !values.is_empty() {
                    out.push_str(&format!("{}={};\n", key.name(), values.join(";")));
                }
            }
        }
        out.trim_start_matches('\n').to_owned()
    }
}

/// A switchable static permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Permission {
    /// Which `[Context]` key.
    pub key: Key,
    /// The value under it.
    pub value: &'static str,
    /// What it means, for the Settings app.
    pub label: &'static str,
    /// The group it is listed under.
    pub group: &'static str,
}

const fn p(key: Key, value: &'static str, label: &'static str, group: &'static str) -> Permission {
    Permission {
        key,
        value,
        label,
        group,
    }
}

/// The static permissions the Privacy page lists for every app.
pub const PERMISSIONS: [Permission; 20] = [
    p(Key::Shared, "network", "Network", "Connections"),
    p(Key::Features, "bluetooth", "Bluetooth", "Connections"),
    p(Key::Sockets, "cups", "Printing", "Connections"),
    p(Key::Sockets, "ssh-auth", "SSH agent", "Connections"),
    p(Key::Sockets, "pcsc", "Smart cards", "Connections"),
    p(
        Key::Sockets,
        "wayland",
        "Wayland display",
        "Display & sound",
    ),
    p(Key::Sockets, "x11", "X11 display", "Display & sound"),
    p(
        Key::Sockets,
        "fallback-x11",
        "X11 when Wayland is missing",
        "Display & sound",
    ),
    p(
        Key::Shared,
        "ipc",
        "Shared memory with X11",
        "Display & sound",
    ),
    p(
        Key::Sockets,
        "pulseaudio",
        "Sound and microphone (PulseAudio)",
        "Display & sound",
    ),
    p(Key::Devices, "dri", "GPU acceleration", "Devices"),
    p(
        Key::Devices,
        "all",
        "All devices (cameras, controllers, USB)",
        "Devices",
    ),
    p(
        Key::Filesystems,
        "host",
        "All files on the computer",
        "Files",
    ),
    p(Key::Filesystems, "home", "Home folder", "Files"),
    p(Key::Filesystems, "xdg-download", "Downloads", "Files"),
    p(Key::Filesystems, "xdg-documents", "Documents", "Files"),
    p(Key::Filesystems, "xdg-pictures", "Pictures", "Files"),
    p(Key::Filesystems, "xdg-videos", "Videos", "Files"),
    p(Key::Filesystems, "xdg-music", "Music", "Files"),
    p(
        Key::Sockets,
        "session-bus",
        "Full session bus (escapes portals)",
        "Advanced",
    ),
];

/// A portal permission in the permission store.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum Portal {
    /// Run in the background.
    Background,
    /// Show notifications.
    Notifications,
    /// The camera, through the camera portal.
    Camera,
    /// The microphone, through the device portal.
    Microphone,
    /// Location, through the location portal.
    Location,
    /// Take screenshots without asking each time.
    Screenshot,
}

impl Portal {
    /// Every portal permission, in display order.
    pub const ALL: [Self; 6] = [
        Self::Camera,
        Self::Microphone,
        Self::Location,
        Self::Background,
        Self::Notifications,
        Self::Screenshot,
    ];

    /// The permission store table and object ID.
    pub const fn store(self) -> (&'static str, &'static str) {
        match self {
            Self::Background => ("background", "background"),
            Self::Notifications => ("notifications", "notification"),
            Self::Camera => ("devices", "camera"),
            Self::Microphone => ("devices", "microphone"),
            Self::Location => ("location", "location"),
            Self::Screenshot => ("screenshot", "screenshot"),
        }
    }

    /// What it means, for the Settings app.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Background => "Run in the background",
            Self::Notifications => "Notifications",
            Self::Camera => "Camera",
            Self::Microphone => "Microphone",
            Self::Location => "Location",
            Self::Screenshot => "Screenshots without asking",
        }
    }

    /// The values that grant and deny it. The location portal stores an
    /// accuracy level and a timestamp; the others store yes or no.
    pub const fn values(self, allow: bool) -> &'static [&'static str] {
        match (self, allow) {
            (Self::Location, true) => &["EXACT", "0"],
            (Self::Location, false) => &["NONE", "0"],
            (_, true) => &["yes"],
            (_, false) => &["no"],
        }
    }

    /// Reads a stored first value: `Some(true)` granted, `Some(false)`
    /// denied.
    pub fn parse(self, value: &str) -> Option<bool> {
        match (self, value) {
            (Self::Location, "NONE") | (_, "no") => Some(false),
            (Self::Location, "EXACT" | "CITY" | "NEIGHBORHOOD" | "STREET" | "COUNTRY")
            | (_, "yes") => Some(true),
            _ => None,
        }
    }
}

/// An installed Flatpak app.
#[derive(Clone, Debug, PartialEq)]
pub struct App {
    /// The app ID, e.g. `org.gnome.Maps`.
    pub id: String,
    /// Its name from its desktop file, else the ID.
    pub name: String,
    /// Installed for this user rather than system-wide.
    pub user: bool,
    /// What the app asks for, from its metadata.
    pub requested: Context,
    /// The user's overrides for this app.
    pub overrides: Context,
    /// D-Bus names it may talk to, from its metadata (shown, not edited).
    pub bus_names: Vec<String>,
}

/// Where Flatpak keeps apps and overrides.
#[derive(Clone, Debug, PartialEq)]
pub struct Dirs {
    /// The per-user installation, `$XDG_DATA_HOME/flatpak`.
    pub user: PathBuf,
    /// System installations, `/var/lib/flatpak` by default.
    pub system: Vec<PathBuf>,
}

impl Dirs {
    /// The standard places, honoring `$FLATPAK_USER_DIR` and
    /// `$FLATPAK_SYSTEM_DIR`.
    pub fn standard() -> Option<Self> {
        let user = derisk_sys::env::var_os("FLATPAK_USER_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                derisk_sys::env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .filter(|d| d.is_absolute())
                    .or_else(|| {
                        derisk_sys::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share"))
                    })
                    .map(|d| d.join("flatpak"))
            })?;
        let system = derisk_sys::env::var_os("FLATPAK_SYSTEM_DIR")
            .map_or_else(|| PathBuf::from("/var/lib/flatpak"), PathBuf::from);
        Some(Self {
            user,
            system: vec![system],
        })
    }

    /// The user override file for `app` (`global` for every app).
    pub fn override_path(&self, app: &str) -> PathBuf {
        self.user.join("overrides").join(app)
    }

    /// Installed apps, user installations first, by name. An app installed
    /// both ways is listed once, as flatpak runs the user's copy.
    pub fn apps(&self) -> Vec<App> {
        let mut apps: Vec<App> = Vec::new();
        let roots =
            std::iter::once((&self.user, true)).chain(self.system.iter().map(|s| (s, false)));
        for (root, user) in roots {
            let Ok(entries) = fs::read_dir(root.join("app")) else {
                continue;
            };
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                if apps.iter().any(|a| a.id == id) {
                    continue;
                }
                let active = entry.path().join("current/active");
                let Ok(metadata) = fs::read_to_string(active.join("metadata")) else {
                    continue;
                };
                let name = fs::read_to_string(
                    active.join(format!("export/share/applications/{id}.desktop")),
                )
                .ok()
                .and_then(|desktop| {
                    desktop
                        .lines()
                        .find_map(|l| l.strip_prefix("Name=").map(str::to_owned))
                })
                .unwrap_or_else(|| id.clone());
                let overrides = fs::read_to_string(self.override_path(&id)).unwrap_or_default();
                apps.push(App::from_files(id, name, user, &metadata, &overrides));
            }
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        apps
    }

    /// Writes `app`'s overrides, removing the file when none are left.
    pub fn save_overrides(&self, app: &str, overrides: &Context) -> io::Result<()> {
        let path = self.override_path(app);
        let old = fs::read_to_string(&path).unwrap_or_default();
        let text = overrides.write_into(&old);
        if text.trim().is_empty() {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        fs::create_dir_all(path.parent().expect("overrides dir"))?;
        let tmp = path.with_extension("derisk-tmp");
        fs::write(&tmp, text)?;
        fs::rename(tmp, path)
    }

    /// The overrides every app gets (`flatpak override --user` without an
    /// app).
    pub fn global_overrides(&self) -> Context {
        fs::read_to_string(self.override_path("global"))
            .map(|t| Context::parse(&t))
            .unwrap_or_default()
    }
}

fn bus_names(metadata: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_bus = false;
    for line in metadata.lines().map(str::trim) {
        if line.starts_with('[') {
            in_bus = line == "[Session Bus Policy]" || line == "[System Bus Policy]";
            continue;
        }
        if in_bus && let Some((name, policy)) = line.split_once('=') {
            names.push(format!("{} ({})", name.trim(), policy.trim()));
        }
    }
    names
}

impl App {
    /// The app `id`, called `name` and installed for the user or not, from
    /// its `metadata` keyfile and the user's `overrides` of it, as they are
    /// in an installation's files.
    pub fn from_files(
        id: String,
        name: String,
        user: bool,
        metadata: &str,
        overrides: &str,
    ) -> App {
        App {
            bus_names: bus_names(metadata),
            requested: Context::parse(metadata),
            overrides: Context::parse(overrides),
            id,
            name,
            user,
        }
    }

    /// Whether `perm` is in effect, after the global and the app's own
    /// overrides.
    pub fn effective(&self, global: &Context, key: Key, value: &str) -> Option<Grant> {
        self.overrides
            .get(key, value)
            .or_else(|| global.get(key, value))
            .or_else(|| self.requested.get(key, value))
            .filter(|g| *g != Grant::No)
    }

    /// Turns a permission on or off: clears the override when that matches
    /// what the app gets anyway, else records it.
    pub fn switch(&mut self, global: &Context, key: Key, value: &str, on: bool) {
        let without = global
            .get(key, value)
            .or_else(|| self.requested.get(key, value))
            .is_some_and(|g| g != Grant::No);
        let grant = (on != without).then_some(if on { Grant::Yes } else { Grant::No });
        self.overrides.set(key, value, grant);
    }
}

/// An error from the `flatpak` command.
#[derive(Debug)]
pub struct CommandError(pub String);

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn flatpak(args: &[&str]) -> Result<String, CommandError> {
    let program = derisk_sys::env::var_os("DERISK_FLATPAK").unwrap_or_else(|| "flatpak".into());
    let output = Command::new(&program)
        .args(args)
        .output()
        .map_err(|e| CommandError(format!("{}: {e}", program.to_string_lossy())))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(CommandError(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

/// The portal permissions stored for `app`: granted, denied, or absent
/// (the portal asks the first time).
pub fn portal_permissions(app: &str) -> Result<BTreeMap<Portal, bool>, CommandError> {
    Ok(parse_permission_show(&flatpak(&["permission-show", app])?))
}

/// Parses `flatpak permission-show` output: tab-separated table, object,
/// app, permissions (comma-separated) and data.
///
/// flatpak prints its `Table Object App ...` header only to a terminal, so
/// output read through a pipe starts with a permission. A header, when
/// there is one, matches no portal and is skipped like any unknown row.
pub fn parse_permission_show(text: &str) -> BTreeMap<Portal, bool> {
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
        let [table, object, _app, permissions, ..] = fields[..] else {
            continue;
        };
        let first = permissions.split(',').next().unwrap_or_default().trim();
        if let Some(portal) = Portal::ALL
            .into_iter()
            .find(|p| p.store() == (table, object))
            && let Some(allowed) = portal.parse(first)
        {
            found.insert(portal, allowed);
        }
    }
    found
}

/// Grants or denies a portal permission, or with `None` forgets it so the
/// portal asks again.
pub fn set_portal(app: &str, portal: Portal, allow: Option<bool>) -> Result<(), CommandError> {
    let args = portal_args(app, portal, allow);
    flatpak(&args.iter().map(String::as_str).collect::<Vec<_>>()).map(drop)
}

/// What an [`Installation`] has found of its apps so far.
#[derive(Clone, Debug, PartialEq)]
pub enum Scan {
    /// Its apps, by name, and the overrides every app gets.
    Apps {
        /// The apps.
        apps: Vec<App>,
        /// The user's overrides of every app (`overrides/global`).
        global: Context,
    },
    /// Not known yet; the line says what it waits for.
    Waiting(String),
    /// Cannot be read now; the line says why. `startable` when starting
    /// something (its machine) would let it be read.
    Unavailable {
        /// Why, for the page.
        reason: String,
        /// Whether [`Installation::start`] would help.
        startable: bool,
    },
}

/// Finds the installation Flatpak apps are in, when it is not this
/// computer's own: the machine Flatpak runs in, say. `None` when there is
/// none, and this computer's own is read.
pub type FindInstallation = fn() -> Option<Box<dyn Installation>>;

/// Where the Privacy page finds Flatpak apps and changes what they may
/// reach: this computer's own [`Dirs`], or a machine Flatpak runs in.
///
/// Nothing here may block for long: the page is drawn by the compositor,
/// and a machine is a guest agent's round trip away. A slow installation
/// answers [`Scan::Waiting`] and `None` until it knows, does its changes
/// in the background, and says through [`Installation::status`] when one
/// failed; the page asks again every frame meanwhile.
pub trait Installation: fmt::Debug + Send {
    /// Where the apps run, for the page to say: `None` for this computer,
    /// or a machine's name.
    fn machine(&self) -> Option<&str> {
        None
    }

    /// The apps, as far as known; `again` asks the installation to look
    /// afresh rather than answer what it found before.
    fn scan(&mut self, again: bool) -> Scan;

    /// Writes `overrides` as the user's overrides of `app`; empty removes
    /// them.
    fn save_overrides(&mut self, app: &str, overrides: &Context) -> Result<(), String>;

    /// `app`'s portal permissions, `None` while they are being read.
    fn portal_permissions(&mut self, app: &str) -> Option<Result<BTreeMap<Portal, bool>, String>>;

    /// Allows, denies or forgets (`None`) `portal` for `app`.
    fn set_portal(&mut self, app: &str, portal: Portal, allow: Option<bool>) -> Result<(), String>;

    /// A change done in the background that failed since last asked.
    fn status(&mut self) -> Option<String> {
        None
    }

    /// Starts what the apps are in, when [`Scan::Unavailable`] said that
    /// would help.
    fn start(&mut self) {}
}

impl Installation for Dirs {
    fn scan(&mut self, _again: bool) -> Scan {
        Scan::Apps {
            apps: self.apps(),
            global: self.global_overrides(),
        }
    }

    fn save_overrides(&mut self, app: &str, overrides: &Context) -> Result<(), String> {
        Dirs::save_overrides(self, app, overrides).map_err(|e| e.to_string())
    }

    fn portal_permissions(&mut self, app: &str) -> Option<Result<BTreeMap<Portal, bool>, String>> {
        Some(portal_permissions(app).map_err(|e| e.0))
    }

    fn set_portal(&mut self, app: &str, portal: Portal, allow: Option<bool>) -> Result<(), String> {
        set_portal(app, portal, allow).map_err(|e| e.0)
    }
}

/// The arguments of the `flatpak` command line that sets (`Some`) or
/// forgets (`None`) `portal` for `app` in the permission store. A machine
/// runs the same command in its guest.
pub fn portal_args(app: &str, portal: Portal, allow: Option<bool>) -> Vec<String> {
    let (table, object) = portal.store();
    let mut args: Vec<String> = match allow {
        Some(_) => vec!["permission-set".into()],
        None => vec!["permission-remove".into()],
    };
    args.extend([table, object, app].map(str::to_owned));
    if let Some(allow) = allow {
        args.extend(portal.values(allow).iter().map(|v| (*v).to_owned()));
    }
    args
}
