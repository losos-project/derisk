//! Shell state on top of mcsapi: tiling plus floating, snapped and minimized
//! windows, Windows-style dragging, and the overview, menu and tray models.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use derisk_settings::BarPosition;
use mcsapi::{Desktop, Geometry, Layout, WindowId, Workspace, WorkspaceId};
use serde::Serialize;

use crate::{
    action::{Action, Effect, LayoutKind},
    adaptive::{FormFactor, Habits, Profile},
    apps::Apps,
    assistant,
    conversation::{Conversation, Source, StepRef, StepStatus},
    effects::{Effects, Look},
    geom::{Point, Rect, centered, contains, gap_insets, inset_sides, rect},
    keyboard,
    menu::{self, GlobalMenu},
    mobile::NavBar,
    overview::Battery,
    snap::{Nudge, SnapZone, zone_at},
    systemd::{self, SessionOp},
    time::Clock,
    tray::Tray,
    widgets::CustomWidgets,
};

/// A window action waiting for a launched app's window, with the
/// conversation step it reports to.
type Deferred = (Action, Option<StepRef>);

/// A request waiting for a launched app's window before it goes on.
#[derive(Debug)]
struct Pending {
    /// The app's ID or name, lowercase, as the launch named it.
    app: String,
    /// The launch's own step, done when the window maps.
    launch: Option<StepRef>,
    /// The steps after the launch, still to run.
    rest: Vec<Deferred>,
}

/// Maximum number of workspaces.
pub const MAX_WORKSPACES: u64 = 9;

/// Pointer travel, in logical pixels, before a press that moves a window
/// (Super held, or on its title in the top bar) becomes a drag.
pub const DRAG_THRESHOLD: i32 = 6;

/// Widest the on-screen keyboard gets on a desktop, in logical pixels: keys
/// stretched across a 1920 px screen are too far apart to type on.
pub const KEYBOARD_MAX_WIDTH: i32 = 960;

/// Shortest a window gets when it shrinks to clear the on-screen keyboard.
/// One that would end up shorter moves up instead, keeping its size.
const KEYBOARD_MIN_CLIENT: i32 = 160;

/// A shell operation failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    /// An mcsapi policy error.
    #[error(transparent)]
    #[diagnostic(code(derisk::shell::policy))]
    Policy(#[from] mcsapi::Error),
    /// The window is not managed.
    #[error("unknown window: {0}")]
    #[diagnostic(code(derisk::shell::unknown_window))]
    UnknownWindow(u64),
    /// The workspace does not exist.
    #[error("unknown workspace: {0}")]
    #[diagnostic(code(derisk::shell::unknown_workspace))]
    UnknownWorkspace(u64),
    /// The action needs a focused window and there is none.
    #[error("no focused window")]
    #[diagnostic(code(derisk::shell::no_focused_window))]
    NoFocusedWindow,
    /// A destructive session operation was not confirmed by the user.
    #[error("{0:?} needs explicit confirmation (set \"confirmed\": true)")]
    #[diagnostic(code(derisk::shell::needs_confirmation))]
    NeedsConfirmation(SessionOp),
    /// An agent or injected input asked for a destructive session
    /// operation; it waits for the person to confirm it on screen.
    #[error("{0:?} is waiting for the person at the computer to confirm it on screen")]
    #[diagnostic(code(derisk::shell::confirm_on_screen))]
    ConfirmOnScreen(SessionOp),
    /// The app name is not a plain command or desktop-file ID.
    #[error("not a launchable app name: {0:?}")]
    #[diagnostic(code(derisk::shell::not_launchable))]
    NotLaunchable(String),
    /// No tray item has this ID.
    #[error("unknown tray item: {0:?}")]
    #[diagnostic(code(derisk::shell::unknown_tray_item))]
    UnknownTrayItem(String),
    /// No custom widget has that button.
    #[error("widget {0:?} has no button {1:?}")]
    #[diagnostic(code(derisk::shell::unknown_widget_button))]
    UnknownWidgetButton(String, String),
    /// Unit actions only apply to currently failed user units.
    #[error("not a failed user unit: {0:?}")]
    #[diagnostic(code(derisk::shell::unknown_unit))]
    UnknownUnit(String),
    /// The path is not an absolute path to an existing, non-executable file
    /// or folder.
    #[error("cannot open {0:?}")]
    #[diagnostic(code(derisk::shell::not_openable))]
    NotOpenable(String),
    /// The window is from an untrusted machine, whose windows may not be
    /// kept on top of others: one could sit over another window and pass
    /// itself off as part of it.
    #[error("windows from {0}, an untrusted machine, cannot be kept on top")]
    #[diagnostic(code(derisk::shell::untrusted_on_top))]
    UntrustedOnTop(String),
    /// The assistant did not understand this request.
    #[error("I don't know how to \"{0}\"")]
    #[diagnostic(code(derisk::shell::not_understood))]
    NotUnderstood(String),
    /// Not a valid desktop action ID.
    #[error("not a desktop action ID: {0:?}")]
    #[diagnostic(code(derisk::shell::unknown_action))]
    UnknownAction(String),
    /// No web search engine has been chosen in Settings.
    #[error("no search engine chosen in Settings")]
    #[diagnostic(code(derisk::shell::no_search_engine))]
    NoSearchEngine,
    /// No program registered this palette source, or it has gone.
    #[error("no program registered palette source {0:?}")]
    #[diagnostic(code(derisk::shell::unknown_palette_source))]
    UnknownPaletteSource(String),
}

/// What running a sequence of actions did.
///
/// A failing action stops the rest, but the actions before it stay applied,
/// so their effects (launching an app, say) must still be carried out:
/// `effects` holds them whether or not `result` is an error.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use]
pub struct Outcome {
    /// Effects of the actions that were applied.
    pub effects: Vec<Effect>,
    /// The error that stopped the run, if any.
    pub result: Result<(), Error>,
}

impl Outcome {
    /// The effects if every action applied, else the error (dropping the
    /// effects of the actions before it).
    pub fn into_result(self) -> Result<Vec<Effect>, Error> {
        self.result.map(|()| self.effects)
    }
}

/// How a window is placed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Mode {
    /// Arranged automatically by the workspace layout.
    Tiled,
    /// Free-floating at a fixed frame.
    Floating {
        /// Frame.
        #[serde(flatten)]
        frame: Rect,
    },
    /// Snapped to a zone of the work area.
    Snapped {
        /// Zone.
        zone: SnapZone,
    },
}

#[derive(Clone, Debug)]
struct WindowInfo {
    app_id: String,
    title: String,
    mode: Mode,
    /// Mode to return to when un-maximizing.
    before_maximize: Option<Mode>,
    /// Last free-floating frame, used when dragging a window out of a snap.
    restore: Option<Geometry>,
    minimized: bool,
    /// The window this one belongs to, as a dialog belongs to its app's
    /// main window (see [`Shell::set_parent`]).
    parent: Option<WindowId>,
    /// Whether it is a modal dialog: its parent takes no input while it is
    /// open.
    modal: bool,
    /// Whether it asked to be seen while it was not focused: it rang the
    /// bell, or asked to come forward when it could not.
    attention: bool,
    /// The virtual machine it came from, when it came from one.
    machine: Option<WindowMachine>,
    /// Whether it is kept above the windows that are not, whichever has the
    /// keyboard ([`Action::KeepOnTop`]).
    on_top: bool,
}

/// The virtual machine a window came from, as the compositor knows it: from
/// the socket its client connected through, which only that machine's
/// windows reach (`derisk_machines::display`). The guest has no say in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowMachine {
    /// The machine's name.
    pub name: String,
    /// The colour its windows are drawn in.
    pub label: derisk_machines::Label,
}

/// Where a window is drawn this frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowPlacement {
    /// The window.
    pub window: WindowId,
    /// Full frame. Windows have no title bar, so it is also the client's.
    pub frame: Geometry,
    /// Area for the client surface; configure the toplevel to this size.
    pub client: Geometry,
    /// Placement mode.
    pub mode: Mode,
    /// Keyboard focus.
    pub focused: bool,
}

/// Where the dragged window would land if released now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropTarget {
    /// Stay floating where it is.
    Float,
    /// Snap into a zone; `preview` is the outline to draw.
    Snap {
        /// Zone.
        zone: SnapZone,
        /// Preview frame.
        preview: Geometry,
    },
    /// Rejoin tiling at the position of another tiled window.
    Tile {
        /// Window under the pointer.
        onto: WindowId,
        /// Preview frame.
        preview: Geometry,
    },
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    window: WindowId,
    origin: Point,
    grab: Point,
    started: bool,
    target: DropTarget,
}

/// After snapping, the other windows offered for the complementary zone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapAssist {
    /// Zone to fill.
    pub zone: SnapZone,
    /// Its frame.
    pub frame: Geometry,
    /// Candidate windows.
    pub candidates: Vec<WindowId>,
}

/// What a pointer press did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PointerOutcome {
    /// Nothing was under the pointer.
    Desktop,
    /// The press hit a client surface; forward it at these surface-local coordinates.
    Client {
        /// Window.
        window: WindowId,
        /// Surface-local position.
        local: Point,
    },
    /// The shell handled the press (decorations, drag start); `effects` are for the host.
    Handled {
        /// Host work to do.
        effects: Vec<Effect>,
    },
}

/// The desktop shell state.
#[derive(Debug)]
pub struct Shell {
    desktop: Desktop,
    open: Vec<WorkspaceId>,
    windows: BTreeMap<WindowId, WindowInfo>,
    stack: Vec<WindowId>,
    next_id: u64,
    output: Geometry,
    profile: Profile,
    drag: Option<Drag>,
    /// Super is held: a press anywhere on a window moves it.
    move_modifier: bool,
    overview: bool,
    palette: bool,
    snap_assist: Option<SnapAssist>,
    pending: Vec<Pending>,
    /// Global menus registered by apps.
    pub menus: GlobalMenu,
    /// System tray items.
    pub tray: Tray,
    /// Custom overview widgets registered by programs.
    pub widgets: CustomWidgets,
    /// Data programs registered for the palette's plugins, such as a
    /// browser's tabs.
    pub palette_sources: crate::palette::Sources,
    /// Learned launch habits.
    pub habits: Habits,
    /// Current time, updated by the host.
    pub clock: Clock,
    /// Battery state, updated by the host.
    pub battery: Option<Battery>,
    /// Natural-language requests and their progress (the palette's agent
    /// conversation).
    pub conversation: Conversation,
    /// Failed user units, updated by the host (see [`systemd::failed_units`]).
    pub failed_units: Vec<String>,
    /// Virtual machines that run, updated by the host from the machines' folders; the
    /// top bar shows one dot in its label's colour for each.
    pub machines: Vec<WindowMachine>,
    /// Whether logind says the machine can hibernate, updated by the host
    /// (see [`systemd::can_hibernate`]). The palette offers Hibernate only
    /// when it can.
    pub can_hibernate: bool,
    /// Effect preferences, updated by the host from the settings file.
    pub effects: Effects,
    /// Whether the on-screen keyboard is showing (see
    /// [`Shell::keyboard_area`]).
    keyboard: bool,
    /// Whether the latest input was injected by an agent or came from an
    /// AT-SPI action, rather than from the keyboard or pointer.
    synthetic_input: bool,
    /// Nesting depth of agent requests being handled ([`Shell::as_agent`]).
    agent_depth: u32,
    /// A destructive session operation waiting for the person to confirm.
    confirmation: Option<SessionOp>,
    /// Whether an agent or injected input asked for [`Shell::confirmation`],
    /// rather than the person's own keys.
    confirmation_from_agent: bool,
    /// Whether the list of keyboard shortcuts is showing.
    shortcut_help: bool,
    /// The tour of the desktop is showing.
    tutorial: bool,
    /// The windows [`Action::ShowDesktop`] minimized, to bring back when it
    /// is pressed again; emptied once any of them is restored another way.
    desktop_shown: Vec<WindowId>,
    /// The gap between tiled windows the person chose in Settings
    /// (`desktop.gaps`), in place of the profile's own; `None` until the
    /// host passes it in.
    gaps: Option<i32>,
    /// Names and icons for app IDs, set by the host from `.desktop` files.
    pub apps: Apps,
    /// Whether the screen is dimmed because nobody used it for a while,
    /// set by the host (see [`crate::idle`]).
    pub dimmed: bool,
    /// What the privacy display covers because someone else is looking,
    /// set by the host (see [`crate::privacy_display`]).
    pub privacy_cover: Option<crate::privacy_display::Area>,
    /// Whether a visible app keeps the session from going idle (a playing
    /// video), set by the host.
    pub kept_awake: bool,
}

/// File types whose default handler runs the file as a program rather than
/// showing it: desktop launchers, Java archives, Windows programs (through
/// Wine), Flatpak references, AppImages and Android packages (through the
/// Android Translation Layer). Compared without case, as shared-mime-info
/// matches globs.
const LAUNCHER_EXTENSIONS: &[&str] = &[
    "desktop",
    "jar",
    "exe",
    "msi",
    "bat",
    "cmd",
    "com",
    "lnk",
    "flatpakref",
    "flatpakrepo",
    "appimage",
    "apk",
];

/// The link that opens Sonne's agent panel with `text` as the prompt to send.
pub fn agent_url(text: &str) -> String {
    let mut url = String::from("zed://agent?prompt=");
    for byte in text.trim().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                url.push(byte as char);
            }
            b' ' => url.push('+'),
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// Whether `path` is safe to hand to `xdg-open`: absolute, existing, and
/// neither executable nor a launcher, so opening it cannot run a program.
///
/// A launcher is recognised by its extension in any case, and by content
/// too, because shared-mime-info sniffs a file whose name matches no glob:
/// `[Desktop Entry]` anywhere in the first 4 KiB, an ELF (AppImages are
/// ELF) or Windows `MZ` executable whatever its name, and a zip (a jar or
/// apk) only without an extension, since documents like .docx are zips too
/// and their glob decides their type first.
/// Whether `url` is a web address `xdg-open` hands to a browser: `https://`
/// or `http://`, with a host, and nothing in it a shell or an argument
/// parser could read another way.
fn is_web_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"));
    rest.is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && url.len() <= 8192
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn openable(path: &Path) -> bool {
    use std::{io::Read, os::unix::fs::PermissionsExt};

    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !path.is_absolute() {
        return false;
    }
    if meta.is_dir() {
        return true;
    }
    if !meta.is_file() || meta.permissions().mode() & 0o111 != 0 {
        return false;
    }
    let launcher = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        LAUNCHER_EXTENSIONS
            .iter()
            .any(|l| e.eq_ignore_ascii_case(l))
    });
    if launcher {
        return false;
    }
    let mut head = Vec::with_capacity(4096);
    let read = std::fs::File::open(path).and_then(|f| f.take(4096).read_to_end(&mut head));
    let program = head.starts_with(b"\x7fELF")
        || head.starts_with(b"MZ")
        || (path.extension().is_none() && head.starts_with(b"PK\x03\x04"));
    read.is_ok() && !program && !head.windows(15).any(|w| w == b"[Desktop Entry]")
}

impl Shell {
    /// Creates a shell for an output with one open workspace.
    pub fn new(output: Geometry, touch: bool) -> Self {
        let desktop = Desktop::new((1..=MAX_WORKSPACES).filter_map(WorkspaceId::new))
            .expect("workspace IDs are unique and nonzero");
        let first = desktop.active().id();
        let mut shell = Self {
            desktop,
            open: vec![first],
            windows: BTreeMap::new(),
            stack: Vec::new(),
            next_id: 1,
            output,
            profile: Profile::detect(output.size.w, output.size.h, touch),
            drag: None,
            move_modifier: false,
            overview: false,
            palette: false,
            confirmation_from_agent: false,
            shortcut_help: false,
            tutorial: false,
            desktop_shown: Vec::new(),
            gaps: None,
            snap_assist: None,
            pending: Vec::new(),
            conversation: Conversation::default(),
            menus: GlobalMenu::default(),
            tray: Tray::default(),
            widgets: CustomWidgets::default(),
            palette_sources: crate::palette::Sources::default(),
            habits: Habits::default(),
            clock: Clock::default(),
            battery: None,
            failed_units: Vec::new(),
            machines: Vec::new(),
            can_hibernate: false,
            effects: Effects::default(),
            keyboard: false,
            synthetic_input: false,
            agent_depth: 0,
            confirmation: None,
            apps: Apps::default(),
            dimmed: false,
            privacy_cover: None,
            kept_awake: false,
        };
        shell.apply_profile_layout();
        shell
    }

    /// The effects in force now, from [`Shell::effects`] and the battery.
    pub fn look(&self) -> Look {
        self.effects.resolve(self.battery)
    }

    /// Records whether the latest input was synthetic (injected by an agent,
    /// or an AT-SPI action) or real; the host calls this when it changes.
    pub fn set_synthetic_input(&mut self, synthetic: bool) {
        self.synthetic_input = synthetic;
    }

    /// Whether the latest input was synthetic.
    pub fn synthetic_input(&self) -> bool {
        self.synthetic_input
    }

    /// Runs `f` as an agent's request: destructive session operations it
    /// asks for wait for an on-screen confirmation even when it claims the
    /// person confirmed.
    pub fn as_agent<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.agent_depth += 1;
        let result = f(self);
        self.agent_depth -= 1;
        result
    }

    /// Whether what is happening now may not come from the person: an
    /// agent's request, or input an agent injected.
    pub fn untrusted(&self) -> bool {
        self.agent_depth > 0 || self.synthetic_input
    }

    /// A destructive session operation an agent or injected input asked
    /// for, waiting for [`Action::Confirm`] from real input.
    pub fn pending_confirmation(&self) -> Option<SessionOp> {
        self.confirmation
    }

    /// Whether an agent asked for the pending confirmation, not the person.
    pub fn confirmation_from_agent(&self) -> bool {
        self.confirmation_from_agent
    }

    /// Updates the output (resolution change, rotation, dock/undock).
    pub fn set_output(&mut self, output: Geometry, touch: bool) {
        let before = self.profile.form_factor;
        self.output = output;
        self.profile = Profile::detect(output.size.w, output.size.h, touch);
        self.apply_gaps();
        if before != self.profile.form_factor {
            self.apply_profile_layout();
        }
    }

    /// Sets the gap between tiled windows, in logical pixels: Settings'
    /// `desktop.gaps`. Windows move to it on the next frame.
    pub fn set_gaps(&mut self, gaps: u8) {
        self.gaps = Some(i32::from(gaps));
        self.apply_gaps();
    }

    fn apply_gaps(&mut self) {
        // A phone shows one window at a time, so it has none to separate.
        if let Some(gaps) = self.gaps
            && !self.is_phone()
        {
            self.profile.gap = gaps;
        }
    }

    fn apply_profile_layout(&mut self) {
        let active = self.desktop.active().id();
        let ids: Vec<WorkspaceId> = self.desktop.workspaces().map(|w| w.id()).collect();
        for id in ids {
            self.desktop.switch_to(id).expect("existing workspace");
            self.desktop.set_layout(self.profile.layout);
        }
        self.desktop.switch_to(active).expect("existing workspace");
    }

    /// The adaptive metrics in use.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// The whole output.
    pub fn output(&self) -> Geometry {
        self.output
    }

    /// Where the top bar is drawn: along the top or bottom edge, as set in
    /// [`Effects::top_bar`]. An auto-hidden bar is drawn here while revealed.
    /// A phone's status bar is always along the top, since the navigation
    /// bar holds the bottom edge.
    pub fn bar_area(&self) -> Geometry {
        let o = self.output;
        let bar = self.profile.top_bar.min(o.size.h - 1);
        match self.effects.top_bar.position {
            _ if self.is_phone() => rect(o.loc.x, o.loc.y, o.size.w, bar),
            BarPosition::Top => rect(o.loc.x, o.loc.y, o.size.w, bar),
            BarPosition::Bottom => rect(o.loc.x, o.loc.y + o.size.h - bar, o.size.w, bar),
        }
    }

    /// The output minus the top bar. An auto-hidden bar slides over windows
    /// instead of taking room from them. Phones also lose the navigation bar
    /// and keep their status bar shown. The on-screen keyboard takes no room
    /// here: it floats over windows (see [`Shell::keyboard_area`]).
    pub fn work_area(&self) -> Geometry {
        let o = self.output;
        if self.is_phone() {
            let bar = self.profile.top_bar.min(o.size.h - 1);
            let nav = self.profile.nav_bar.min(o.size.h - bar - 1).max(0);
            return rect(o.loc.x, o.loc.y + bar, o.size.w, o.size.h - bar - nav);
        }
        if self.effects.top_bar.autohide {
            return o;
        }
        let bar = self.bar_area().size.h;
        let y = match self.effects.top_bar.position {
            BarPosition::Top => o.loc.y + bar,
            BarPosition::Bottom => o.loc.y,
        };
        rect(o.loc.x, y, o.size.w, o.size.h - bar)
    }

    /// The touch navigation bar (zero height except on phones).
    pub fn nav_bar(&self) -> NavBar {
        NavBar {
            height: self.profile.nav_bar,
        }
    }

    /// The underlying mcsapi policy (workspaces, focus, layout).
    pub fn desktop(&self) -> &Desktop {
        &self.desktop
    }

    /// Open workspaces in display order. Workspace number `n` (as used by
    /// actions, keys and IPC) is the `n`th entry.
    pub fn workspaces(&self) -> &[WorkspaceId] {
        &self.open
    }

    /// The 1-based number of an open workspace.
    pub fn workspace_number(&self, workspace: WorkspaceId) -> Option<u64> {
        self.open
            .iter()
            .position(|w| *w == workspace)
            .map(|i| i as u64 + 1)
    }

    /// The active workspace's number.
    pub fn active_workspace(&self) -> u64 {
        self.workspace_number(self.desktop.active().id())
            .expect("the active workspace is open")
    }

    /// Whether another workspace can be opened.
    pub fn can_add_workspace(&self) -> bool {
        (self.open.len() as u64) < MAX_WORKSPACES
    }

    /// The open workspace numbered `n`, opening a new empty one at the end
    /// when `n` is one past the last.
    fn workspace(&mut self, n: u64) -> Result<WorkspaceId, Error> {
        let count = self.open.len() as u64;
        if (1..=count).contains(&n) {
            return Ok(self.open[n as usize - 1]);
        }
        if n != count + 1 || !self.can_add_workspace() {
            return Err(Error::UnknownWorkspace(n));
        }
        let id = self
            .desktop
            .workspaces()
            .map(|w| w.id())
            .find(|id| !self.open.contains(id))
            .expect("fewer than MAX_WORKSPACES are open");
        // A reused workspace starts fresh, with the profile's layout.
        let active = self.desktop.active().id();
        self.desktop.switch_to(id).expect("existing workspace");
        self.desktop.set_layout(self.profile.layout);
        self.desktop.switch_to(active).expect("existing workspace");
        self.open.push(id);
        Ok(id)
    }

    /// Closes empty workspaces other than the active one.
    fn prune_workspaces(&mut self) {
        let active = self.desktop.active().id();
        let desktop = &self.desktop;
        self.open.retain(|id| {
            *id == active
                || desktop
                    .workspaces()
                    .any(|ws| ws.id() == *id && ws.windows().len() > 0)
        });
    }

    /// Minimizes every visible window on the active workspace, or brings
    /// back the ones it minimized last time if they are all still hidden,
    /// as Windows' Super+D does.
    fn show_desktop(&mut self) {
        let members: Vec<WindowId> = self.desktop.active().windows().collect();
        let hidden = !self.desktop_shown.is_empty()
            && self
                .desktop_shown
                .iter()
                .all(|w| members.contains(w) && self.is_minimized(*w))
            && members.iter().all(|w| self.is_minimized(*w));
        if hidden {
            for w in std::mem::take(&mut self.desktop_shown) {
                if let Some(info) = self.windows.get_mut(&w) {
                    info.minimized = false;
                }
            }
            // The window on top before gets the keyboard back.
            if let Some(&top) = self
                .stack
                .iter()
                .rev()
                .find(|w| members.contains(w) && !self.is_minimized(**w))
            {
                let _ = self.focus(top);
            }
            return;
        }
        self.desktop_shown = members
            .into_iter()
            .filter(|w| !self.is_minimized(*w))
            .collect();
        for w in &self.desktop_shown {
            if let Some(info) = self.windows.get_mut(w) {
                info.minimized = true;
            }
        }
        self.focus_visible(true);
    }

    /// Whether the overview is showing.
    pub fn overview_visible(&self) -> bool {
        self.overview
    }

    /// Whether the command palette is showing.
    pub fn palette_visible(&self) -> bool {
        self.palette
    }

    /// Whether the list of keyboard shortcuts is showing.
    pub fn shortcut_help_visible(&self) -> bool {
        self.shortcut_help
    }

    /// Whether the tour of the desktop is showing.
    pub fn tutorial_visible(&self) -> bool {
        self.tutorial
    }

    /// Whether the on-screen keyboard is showing.
    pub fn keyboard_visible(&self) -> bool {
        self.keyboard
    }

    /// Where the on-screen keyboard is drawn while it shows: along the bottom
    /// edge, above a phone's navigation bar or a top bar moved to the bottom.
    ///
    /// It is an overlay, not part of the layout: it covers whatever windows
    /// are under it, and only the focused window moves out from under it
    /// (see [`Shell::placements`]), the way a phone resizes the app being
    /// typed in. Other windows keep their places, so showing the keyboard
    /// never reshuffles the desktop. On a wide screen it stays phone-sized
    /// rather than stretching keys across the whole output.
    pub fn keyboard_area(&self) -> Option<Geometry> {
        if !self.keyboard {
            return None;
        }
        let o = self.output;
        let below = if self.is_phone() {
            self.profile.nav_bar
        } else if self.effects.top_bar.position == BarPosition::Bottom
            && !self.effects.top_bar.autohide
        {
            self.bar_area().size.h
        } else {
            0
        };
        let height = keyboard::HEIGHT.min(o.size.h - below).max(0);
        let width = if self.profile.form_factor == FormFactor::Desktop {
            o.size.w.min(KEYBOARD_MAX_WIDTH)
        } else {
            o.size.w
        };
        Some(rect(
            o.loc.x + (o.size.w - width) / 2,
            o.loc.y + o.size.h - below - height,
            width,
            height,
        ))
    }

    /// The pending Snap Assist offer, if any.
    pub fn snap_assist(&self) -> Option<&SnapAssist> {
        self.snap_assist.as_ref()
    }

    /// The active drag's drop target, for drawing a preview.
    pub fn drag_target(&self) -> Option<DropTarget> {
        self.drag.filter(|d| d.started).map(|d| d.target)
    }

    /// The window the pointer is dragging, once the drag has started.
    pub fn dragged(&self) -> Option<WindowId> {
        self.drag.filter(|d| d.started).map(|d| d.window)
    }

    /// Every window, on any workspace.
    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.windows.keys().copied()
    }

    /// A window's app ID and title.
    pub fn window_label(&self, window: WindowId) -> Option<(&str, &str)> {
        self.windows
            .get(&window)
            .map(|i| (i.app_id.as_str(), i.title.as_str()))
    }

    /// Whether a window is minimized.
    pub fn is_minimized(&self, window: WindowId) -> bool {
        self.windows.get(&window).is_some_and(|i| i.minimized)
    }

    /// A window's placement mode.
    pub fn mode(&self, window: WindowId) -> Option<Mode> {
        self.windows.get(&window).map(|i| i.mode)
    }

    /// The workspace containing a window.
    pub fn workspace_of(&self, window: WindowId) -> Option<WorkspaceId> {
        self.desktop
            .workspaces()
            .find(|ws| ws.windows().any(|w| w == window))
            .map(|ws| ws.id())
    }

    /// The focused, visible window on the active workspace.
    pub fn focused(&self) -> Option<WindowId> {
        self.desktop
            .active()
            .focused()
            .filter(|w| !self.is_minimized(*w))
    }

    /// Starts managing a new toplevel on the active workspace and focuses it.
    ///
    /// Actions an agent queued after launching `app_id` are applied now.
    pub fn map_window(&mut self, app_id: &str, title: &str) -> (WindowId, Vec<Effect>) {
        let id = WindowId::new(self.next_id).expect("IDs start at 1");
        self.next_id += 1;
        // Tiled windows are drawn under snapped ones, so a new window tiled
        // beside two halves filled the work area behind them and showed only
        // as the gaps between them: Settings opened from the palette that
        // way looked like a sliver. Over snapped windows it floats instead,
        // drawn on top and centred. A phone snaps nothing.
        let over_snapped = !self.is_phone()
            && self.desktop.active().windows().any(|w| {
                !self.is_minimized(w) && matches!(self.windows[&w].mode, Mode::Snapped { .. })
            });
        let mode = if over_snapped {
            Mode::Floating {
                frame: self.default_float().into(),
            }
        } else {
            Mode::Tiled
        };
        self.desktop.insert(id).expect("fresh IDs are unique");
        self.windows.insert(
            id,
            WindowInfo {
                app_id: app_id.to_owned(),
                title: title.to_owned(),
                mode,
                before_maximize: None,
                restore: None,
                minimized: false,
                parent: None,
                modal: false,
                attention: false,
                machine: None,
                on_top: false,
            },
        );
        self.stack.push(id);
        let effects = self.apply_pending(app_id);
        (id, effects)
    }

    /// Updates a window's app ID, for toolkits that set it after mapping
    /// (GPUI), and applies actions an agent queued for that app.
    pub fn set_app_id(&mut self, window: WindowId, app_id: &str) -> Result<Vec<Effect>, Error> {
        let info = self.info_mut(window)?;
        if info.app_id == app_id {
            return Ok(Vec::new());
        }
        info.app_id = app_id.to_owned();
        Ok(self.apply_pending(app_id))
    }

    /// Carries on with a request that was waiting for `app_id`'s window:
    /// the launch's own step is done now that its window is here, and the
    /// steps after it run (and may wait again, for the next launch).
    fn apply_pending(&mut self, app_id: &str) -> Vec<Effect> {
        let key = app_id.to_lowercase();
        let Some(i) = self
            .pending
            .iter()
            .position(|p| p.app == key || key.contains(p.app.as_str()))
        else {
            return Vec::new();
        };
        let pending = self.pending.remove(i);
        if let Some(step) = pending.launch {
            self.conversation.set(step, StepStatus::Done);
        }
        self.run_deferred(pending.rest).effects
    }

    /// Stops managing a destroyed toplevel.
    pub fn unmap_window(&mut self, window: WindowId) -> Result<(), Error> {
        self.desktop.remove(window)?;
        self.windows.remove(&window);
        // Its dialogs outlive it as ordinary windows.
        for info in self.windows.values_mut() {
            if info.parent == Some(window) {
                info.parent = None;
                info.modal = false;
            }
        }
        self.stack.retain(|w| *w != window);
        self.menus.unregister(window.get());
        self.prune_workspaces();
        if self.drag.is_some_and(|d| d.window == window) {
            self.drag = None;
        }
        if let Some(assist) = &mut self.snap_assist {
            assist.candidates.retain(|w| *w != window);
            if assist.candidates.is_empty() {
                self.snap_assist = None;
            }
        }
        Ok(())
    }

    /// Updates a window's title.
    pub fn set_title(&mut self, window: WindowId, title: &str) -> Result<(), Error> {
        self.info_mut(window)?.title = title.to_owned();
        Ok(())
    }

    /// Marks a window as one from a virtual machine. A window from an
    /// untrusted machine stops being kept on top, should it have been
    /// before the compositor knew where it came from.
    pub fn set_window_machine(
        &mut self,
        window: WindowId,
        machine: WindowMachine,
    ) -> Result<(), Error> {
        let info = self.info_mut(window)?;
        if machine.label.is_untrusted() {
            info.on_top = false;
        }
        info.machine = Some(machine);
        Ok(())
    }

    /// Whether a window comes from an untrusted machine
    /// ([`derisk_machines::Label::is_untrusted`]), which limits what it may
    /// do to the rest of the desktop.
    pub fn is_untrusted(&self, window: WindowId) -> bool {
        self.window_machine(window)
            .is_some_and(|machine| machine.label.is_untrusted())
    }

    /// Whether a window is kept on top ([`Action::KeepOnTop`]).
    pub fn is_on_top(&self, window: WindowId) -> bool {
        self.windows.get(&window).is_some_and(|info| info.on_top)
    }

    /// The virtual machine a window came from, if it came from one.
    pub fn window_machine(&self, window: WindowId) -> Option<&WindowMachine> {
        self.windows.get(&window)?.machine.as_ref()
    }

    fn info_mut(&mut self, window: WindowId) -> Result<&mut WindowInfo, Error> {
        self.windows
            .get_mut(&window)
            .ok_or(Error::UnknownWindow(window.get()))
    }

    fn target(&self, window: Option<u64>) -> Result<WindowId, Error> {
        match window {
            Some(raw) => WindowId::new(raw)
                .filter(|id| self.windows.contains_key(id))
                .ok_or(Error::UnknownWindow(raw)),
            None => self.focused().ok_or(Error::NoFocusedWindow),
        }
    }

    fn raise(&mut self, window: WindowId) {
        self.stack.retain(|w| *w != window);
        self.stack.push(window);
    }

    /// Focuses `window`, or the modal dialog open over it, which keeps the
    /// keyboard until it closes. Returns the window focused.
    fn focus(&mut self, window: WindowId) -> Result<WindowId, Error> {
        let ws = self
            .workspace_of(window)
            .ok_or(Error::UnknownWindow(window.get()))?;
        self.desktop.switch_to(ws)?;
        let info = self.info_mut(window)?;
        info.minimized = false;
        info.attention = false;
        self.raise(window);
        let target = match self.modal_over(window) {
            Some(dialog) => {
                let info = self.info_mut(dialog)?;
                info.minimized = false;
                info.attention = false;
                self.raise(dialog);
                dialog
            }
            None => window,
        };
        self.desktop.focus(target)?;
        Ok(target)
    }

    /// The modal dialog open over `window`, the innermost when a dialog
    /// opened another.
    fn modal_over(&self, window: WindowId) -> Option<WindowId> {
        let mut over = None;
        let mut at = window;
        // Bounded by the number of windows, should a client make a cycle.
        for _ in 0..self.windows.len() {
            let Some(next) = self
                .windows
                .iter()
                .find(|(_, i)| i.modal && i.parent == Some(at))
                .map(|(w, _)| *w)
            else {
                break;
            };
            over = Some(next);
            at = next;
        }
        over
    }

    /// Records the window `window` belongs to (`None`: none any more). A
    /// dialog floats centred on its parent, on the parent's workspace,
    /// rather than taking a tile of its own.
    pub fn set_parent(&mut self, window: WindowId, parent: Option<WindowId>) -> Result<(), Error> {
        let parent = parent.filter(|p| *p != window && self.windows.contains_key(p));
        let info = self.info_mut(window)?;
        if info.parent == parent {
            return Ok(());
        }
        info.parent = parent;
        let Some(parent) = parent else {
            return Ok(());
        };
        if let Some(ws) = self.workspace_of(parent)
            && self.workspace_of(window) != Some(ws)
        {
            self.desktop.move_window(window, ws)?;
        }
        // Out of the tiling first, so the parent's frame is the one it
        // keeps once the dialog no longer takes a tile beside it.
        let float = self.default_float();
        self.set_mode(
            window,
            Mode::Floating {
                frame: float.into(),
            },
        )?;
        let frame = match self.frame_of(parent) {
            Some(p) => centered(p, p.size.w * 3 / 4, p.size.h * 3 / 4),
            None => float,
        };
        // Kept on screen when the parent hangs off its edge.
        let area = self.work_area();
        let (w, h) = (frame.size.w.min(area.size.w), frame.size.h.min(area.size.h));
        let frame = rect(
            frame.loc.x.clamp(area.loc.x, area.loc.x + area.size.w - w),
            frame.loc.y.clamp(area.loc.y, area.loc.y + area.size.h - h),
            w,
            h,
        );
        self.set_mode(
            window,
            Mode::Floating {
                frame: frame.into(),
            },
        )?;
        self.raise(window);
        Ok(())
    }

    /// Marks `window` as a modal dialog, or not. Its parent's input goes
    /// to it while it is open.
    pub fn set_modal(&mut self, window: WindowId, modal: bool) -> Result<(), Error> {
        self.info_mut(window)?.modal = modal;
        let parent = self.windows[&window].parent;
        if modal && parent.is_some() && self.focused() == parent {
            self.focus(window)?;
        }
        Ok(())
    }

    /// Asks for the person's attention on `window`, or stops asking. A
    /// focused window needs none; focusing it later clears it.
    pub fn set_attention(&mut self, window: WindowId, on: bool) -> Result<(), Error> {
        let focused = self.focused() == Some(window);
        self.info_mut(window)?.attention = on && !focused;
        Ok(())
    }

    /// Windows asking for attention, oldest first. One focused since, by
    /// whatever means, no longer is.
    pub fn wanting_attention(&self) -> impl Iterator<Item = WindowId> + '_ {
        let focused = self.focused();
        self.windows
            .iter()
            .filter(move |(w, i)| i.attention && Some(**w) != focused)
            .map(|(w, _)| *w)
    }

    /// Whether a window on `workspace` asks for attention.
    pub fn workspace_wants_attention(&self, workspace: WorkspaceId) -> bool {
        self.desktop
            .workspaces()
            .find(|ws| ws.id() == workspace)
            .is_some_and(|ws| {
                ws.windows()
                    .any(|w| self.windows.get(&w).is_some_and(|i| i.attention))
            })
    }

    fn focus_visible(&mut self, forward: bool) {
        let count = self.desktop.active().windows().len();
        for _ in 0..count {
            let next = if forward {
                self.desktop.focus_next()
            } else {
                self.desktop.focus_previous()
            };
            if let Some(w) = next.filter(|w| !self.is_minimized(*w)) {
                self.raise(w);
                return;
            }
        }
    }

    fn default_float(&self) -> Geometry {
        let area = self.work_area();
        centered(area, area.size.w * 3 / 5, area.size.h * 7 / 10)
    }

    fn frame_of(&self, window: WindowId) -> Option<Geometry> {
        self.placements()
            .into_iter()
            .find(|p| p.window == window)
            .map(|p| p.frame)
    }

    fn set_mode(&mut self, window: WindowId, mode: Mode) -> Result<(), Error> {
        let info = self.info_mut(window)?;
        if let Mode::Floating { frame } = info.mode {
            info.restore = Some(frame.into());
        }
        if mode
            != (Mode::Snapped {
                zone: SnapZone::Maximize,
            })
        {
            info.before_maximize = None;
        }
        info.mode = mode;
        Ok(())
    }

    fn snap(&mut self, window: WindowId, zone: SnapZone) -> Result<(), Error> {
        if zone == SnapZone::Maximize {
            let info = self.info_mut(window)?;
            if info.mode != (Mode::Snapped { zone }) {
                info.before_maximize = Some(info.mode);
            }
        }
        self.set_mode(window, Mode::Snapped { zone })?;
        self.focus(window)?;
        self.offer_snap_assist(window, zone);
        Ok(())
    }

    fn offer_snap_assist(&mut self, snapped: WindowId, zone: SnapZone) {
        let assist = self
            .snap_assist
            .take()
            .filter(|a| a.zone == zone && a.candidates.contains(&snapped));
        if assist.is_some() {
            // The offer was just accepted.
            return;
        }
        self.snap_assist = zone.complement().and_then(|zone| {
            let candidates: Vec<WindowId> = self
                .desktop
                .active()
                .windows()
                .filter(|w| *w != snapped && !self.is_minimized(*w))
                .filter(|w| self.mode(*w) != Some(Mode::Snapped { zone }))
                .collect();
            (!candidates.is_empty()).then(|| SnapAssist {
                zone,
                frame: zone.geometry(self.work_area(), self.profile.gap),
                candidates,
            })
        });
    }

    /// Dismisses the Snap Assist offer.
    pub fn dismiss_snap_assist(&mut self) {
        self.snap_assist = None;
    }

    fn float(&mut self, window: WindowId) -> Result<(), Error> {
        let frame = self.windows[&window]
            .restore
            .unwrap_or_else(|| self.default_float());
        self.set_mode(
            window,
            Mode::Floating {
                frame: frame.into(),
            },
        )
    }

    fn check_failed_unit(&self, unit: &str) -> Result<(), Error> {
        if self.failed_units.iter().any(|u| u == unit) {
            Ok(())
        } else {
            Err(Error::UnknownUnit(unit.to_owned()))
        }
    }

    /// Applies one action, returning work for the host.
    ///
    /// Workspaces the action leaves empty are closed, unless active.
    pub fn apply(&mut self, action: Action) -> Result<Vec<Effect>, Error> {
        let result = self.apply_action(action);
        self.prune_workspaces();
        result
    }

    fn apply_action(&mut self, action: Action) -> Result<Vec<Effect>, Error> {
        if !matches!(action, Action::Snap { .. }) {
            self.snap_assist = None;
        }
        match action {
            Action::Launch { app } => {
                if !systemd::is_launchable(&app) {
                    return Err(Error::NotLaunchable(app));
                }
                self.habits.record(&app, self.clock.hour);
                return Ok(vec![Effect::Launch { app }]);
            }
            Action::LaunchAction { app, id } => {
                if !systemd::is_launchable(&app) {
                    return Err(Error::NotLaunchable(app));
                }
                if !crate::desktop::is_action_id(&id) {
                    return Err(Error::UnknownAction(id));
                }
                self.habits.record(&app, self.clock.hour);
                return Ok(vec![Effect::LaunchAction { app, id }]);
            }
            Action::Close { window } => {
                let window = self.target(window)?.get();
                return Ok(vec![Effect::Close { window }]);
            }
            Action::Focus { window } => {
                let w = self.target(Some(window))?;
                self.focus(w)?;
            }
            Action::FocusNext => self.focus_visible(true),
            Action::FocusPrevious => self.focus_visible(false),
            Action::Promote => {
                let w = self.target(None)?;
                self.set_mode(w, Mode::Tiled)?;
                self.desktop.promote_focused();
            }
            Action::Snap { window, zone } => {
                let w = self.target(window)?;
                self.snap(w, zone)?;
            }
            Action::Nudge { window, direction } => {
                let w = self.target(window)?;
                let current = match self.windows[&w].mode {
                    Mode::Snapped { zone } => Some(zone),
                    _ => None,
                };
                match SnapZone::nudge(current, direction) {
                    Nudge::Snap(zone) => self.snap(w, zone)?,
                    Nudge::Restore => {
                        let back = self.windows[&w].before_maximize.unwrap_or(Mode::Tiled);
                        match back {
                            Mode::Floating { .. } => self.float(w)?,
                            other => self.set_mode(w, other)?,
                        }
                    }
                    Nudge::Minimize => {
                        return self.apply(Action::Minimize {
                            window: Some(w.get()),
                        });
                    }
                }
            }
            Action::Tile { window } => {
                let w = self.target(window)?;
                self.set_mode(w, Mode::Tiled)?;
            }
            Action::Float { window } => {
                let w = self.target(window)?;
                self.float(w)?;
            }
            Action::ToggleMaximize { window } => {
                let w = self.target(window)?;
                let info = &self.windows[&w];
                if info.mode
                    == (Mode::Snapped {
                        zone: SnapZone::Maximize,
                    })
                {
                    match info.before_maximize.unwrap_or(Mode::Tiled) {
                        Mode::Floating { .. } => self.float(w)?,
                        other => self.set_mode(w, other)?,
                    }
                } else {
                    self.snap(w, SnapZone::Maximize)?;
                    self.snap_assist = None;
                }
            }
            Action::KeepOnTop { window, on } => {
                let w = self.target(window)?;
                let info = self.info_mut(w)?;
                let on = on.unwrap_or(!info.on_top);
                if on
                    && let Some(machine) = info.machine.as_ref().filter(|m| m.label.is_untrusted())
                {
                    return Err(Error::UntrustedOnTop(machine.name.clone()));
                }
                info.on_top = on;
            }
            Action::Minimize { window } => {
                let w = self.target(window)?;
                self.info_mut(w)?.minimized = true;
                if self.desktop.active().focused() == Some(w) {
                    self.focus_visible(true);
                }
            }
            Action::Restore { window } => {
                let w = self.target(Some(window))?;
                self.focus(w)?;
            }
            Action::SwitchWorkspace { workspace: n } => {
                let ws = self.workspace(n)?;
                self.desktop.switch_to(ws)?;
            }
            Action::MoveToWorkspace {
                window,
                workspace: n,
            } => {
                let w = self.target(window)?;
                let ws = self.workspace(n)?;
                self.desktop.move_window(w, ws)?;
            }
            Action::SetLayout { layout } => self.desktop.set_layout(match layout {
                LayoutKind::Tall => Layout::Tall,
                LayoutKind::Monocle => Layout::Monocle,
            }),
            Action::Overview { visible } => {
                self.overview = visible.unwrap_or(!self.overview);
                self.drag = None;
            }
            Action::Palette { visible } => {
                self.palette = visible.unwrap_or(!self.palette);
                self.drag = None;
                // The palette's search field is the one to type in on a
                // phone: bring the keyboard up with it, and down again after.
                if self.is_phone() {
                    self.keyboard = self.palette;
                }
            }
            Action::Keyboard { visible } => {
                self.keyboard = visible.unwrap_or(!self.keyboard);
            }
            Action::ShortcutHelp { visible } => {
                self.shortcut_help = visible.unwrap_or(!self.shortcut_help);
            }
            Action::Tutorial { visible } => {
                let show = visible.unwrap_or(!self.tutorial);
                if show == self.tutorial {
                    return Ok(Vec::new());
                }
                self.tutorial = show;
                if show {
                    // The tour points at the desktop itself, so nothing
                    // else covers it.
                    self.palette = false;
                    self.overview = false;
                    self.shortcut_help = false;
                    if self.is_phone() {
                        self.keyboard = false;
                    }
                } else {
                    return Ok(vec![Effect::TutorialSeen]);
                }
            }
            Action::ShowDesktop => self.show_desktop(),
            Action::StepWorkspace { by, carry_window } => {
                let n = self
                    .active_workspace()
                    .saturating_add_signed(by)
                    .clamp(1, self.open.len() as u64 + 1);
                if carry_window {
                    let w = self.target(None)?;
                    let ws = self.workspace(n)?;
                    self.desktop.move_window(w, ws)?;
                    self.desktop.switch_to(ws)?;
                    self.focus(w)?;
                } else {
                    // Past the last workspace only a window may open a new
                    // one: an empty one would close again at once.
                    let n = n.min(self.open.len() as u64);
                    let ws = self.workspace(n)?;
                    self.desktop.switch_to(ws)?;
                }
            }
            Action::Screenshot => return Ok(vec![Effect::Screenshot]),
            Action::ConfirmSession { op } => {
                if op.is_destructive() {
                    self.confirmation = Some(op);
                    self.confirmation_from_agent = self.untrusted();
                } else {
                    return Ok(vec![Effect::Session { op }]);
                }
            }
            Action::Open { path } => {
                if !openable(Path::new(&path)) {
                    return Err(Error::NotOpenable(path));
                }
                return Ok(vec![Effect::Open { path }]);
            }
            Action::SearchWeb { query } => {
                let engine = self.effects.search.ok_or(Error::NoSearchEngine)?;
                // An https URL, so xdg-open cannot read it as an option.
                return Ok(vec![Effect::Open {
                    path: engine.url(&query),
                }]);
            }
            Action::OpenUrl { url } => {
                if !is_web_url(&url) {
                    return Err(Error::NotOpenable(url));
                }
                return Ok(vec![Effect::Open { path: url }]);
            }
            Action::PaletteCommand { source, command } => {
                if !self.palette_sources.has(&source) {
                    return Err(Error::UnknownPaletteSource(source));
                }
                return Ok(vec![Effect::PaletteCommand { source, command }]);
            }
            Action::AskAgent { text } => {
                // Sonne registers the zed: scheme; xdg-open cannot read a
                // URL that starts with it as an option.
                return Ok(vec![Effect::Open {
                    path: agent_url(&text),
                }]);
            }
            Action::ActivateMenu { window, item } => {
                let w = self.target(window)?;
                if let Some(action) = menu::shell_action(&item, w.get()) {
                    return self.apply(action);
                }
                return Ok(vec![Effect::MenuActivated {
                    window: w.get(),
                    item,
                }]);
            }
            Action::Session { op, confirmed } => {
                if op.is_destructive() && !confirmed {
                    return Err(Error::NeedsConfirmation(op));
                }
                // An agent saying "confirmed" is not the person agreeing,
                // and neither is a click or Enter an agent injected: a web
                // page title can talk an agent into anything. Ask on screen.
                if op.is_destructive() && self.untrusted() {
                    self.confirmation = Some(op);
                    self.confirmation_from_agent = true;
                    return Err(Error::ConfirmOnScreen(op));
                }
                return Ok(vec![Effect::Session { op }]);
            }
            Action::Confirm { accept } => {
                let Some(op) = self.confirmation else {
                    return Ok(Vec::new());
                };
                if !accept {
                    self.confirmation = None;
                    return Ok(Vec::new());
                }
                if self.untrusted() {
                    return Err(Error::ConfirmOnScreen(op));
                }
                self.confirmation = None;
                return Ok(vec![Effect::Session { op }]);
            }
            Action::ActivateTray { id, item } => {
                if !self.tray.items().any(|i| i.id == id) {
                    return Err(Error::UnknownTrayItem(id));
                }
                return Ok(vec![Effect::TrayActivated { id, item }]);
            }
            Action::ActivateWidget { id, item } => {
                if !self.widgets.has_button(&id, &item) {
                    return Err(Error::UnknownWidgetButton(id, item));
                }
                return Ok(vec![Effect::WidgetActivated { id, item }]);
            }
            Action::RestartUnit { unit } => {
                self.check_failed_unit(&unit)?;
                return Ok(vec![Effect::RestartUnit { unit }]);
            }
            Action::ResetFailed { unit } => {
                self.check_failed_unit(&unit)?;
                self.failed_units.retain(|u| *u != unit);
                return Ok(vec![Effect::ResetFailed { unit }]);
            }
        }
        Ok(Vec::new())
    }

    /// Applies a sequence of actions.
    ///
    /// Window actions without an explicit window that follow a `Launch` are
    /// deferred until that app maps a window, so "open firefox and snap it
    /// left" acts on Firefox. Stops at the first error; earlier actions stay
    /// applied, and [`Outcome::effects`] holds their effects for the host to
    /// carry out either way.
    pub fn run(&mut self, actions: impl IntoIterator<Item = Action>) -> Outcome {
        self.run_steps(actions.into_iter().collect(), None)
    }

    /// Interprets a natural-language request with the built-in assistant
    /// and runs it, recording it in [`Shell::conversation`] with each
    /// step's progress.
    ///
    /// `confirmed` confirms destructive session operations; only pass it
    /// when the person explicitly confirmed this request (the palette asks
    /// again first). Agents never do.
    pub fn ask(&mut self, text: &str, source: Source, confirmed: bool) -> Outcome {
        match assistant::interpret(text) {
            Ok(actions) => {
                let actions = actions
                    .into_iter()
                    .map(|a| match a {
                        Action::Session { op, .. } => Action::Session {
                            op,
                            confirmed: confirmed || !op.is_destructive(),
                        },
                        other => other,
                    })
                    .collect();
                self.run_recorded(text, source, actions)
            }
            Err(e) => {
                self.conversation
                    .not_understood(source, text, &e.to_string());
                Outcome {
                    effects: Vec::new(),
                    result: Err(Error::NotUnderstood(e.0)),
                }
            }
        }
    }

    /// Runs `actions` like [`Shell::run`], recording them as one turn of
    /// the conversation under `request`.
    pub fn run_recorded(&mut self, request: &str, source: Source, actions: Vec<Action>) -> Outcome {
        let turn = self.conversation.start(source, request, &actions);
        self.run_steps(actions, Some(turn))
    }

    fn run_steps(&mut self, actions: Vec<Action>, turn: Option<u64>) -> Outcome {
        let steps = actions
            .into_iter()
            .enumerate()
            .map(|(i, action)| (action, turn.map(|t| (t, i))))
            .collect();
        self.run_deferred(steps)
    }

    /// Runs steps in order. A launch followed by steps aimed at the new
    /// window ("open files and snap it left") stops the run there: the
    /// launch's step stays waiting, and it and everything after it go on
    /// when the app's window maps ([`Shell::map_window`]), so the steps
    /// finish one at a time, in the order they were asked for.
    fn run_deferred(&mut self, steps: Vec<Deferred>) -> Outcome {
        let mut effects = Vec::new();
        let mut steps = steps.into_iter().peekable();
        while let Some((action, step)) = steps.next() {
            let launched = match &action {
                Action::Launch { app } | Action::LaunchAction { app, .. } => {
                    Some(app.strip_suffix(".desktop").unwrap_or(app).to_lowercase())
                }
                _ => None,
            };
            match self.apply(action) {
                Ok(more) => effects.extend(more),
                Err(e) => {
                    if let Some(s) = step {
                        self.conversation.set(s, StepStatus::Failed(e.to_string()));
                    }
                    for (_, rest) in steps {
                        if let Some(r) = rest {
                            self.conversation.set(r, StepStatus::Skipped);
                        }
                    }
                    return Outcome {
                        effects,
                        result: Err(e),
                    };
                }
            }
            let waits =
                launched.filter(|_| steps.peek().is_some_and(|(a, _)| a.targets_new_window()));
            if let Some(app) = waits {
                self.pending.push(Pending {
                    app,
                    launch: step,
                    rest: steps.collect(),
                });
                break;
            }
            if let Some(s) = step {
                self.conversation.set(s, StepStatus::Done);
            }
        }
        Outcome {
            effects,
            result: Ok(()),
        }
    }

    /// Frames for the active workspace, bottom to top.
    ///
    /// Tiled windows come first (only the topmost in monocle), then snapped and
    /// floating windows in stacking order. Minimized windows are omitted.
    pub fn placements(&self) -> Vec<WindowPlacement> {
        let mut out = self.arranged();
        if let Some(keyboard) = self.keyboard_area() {
            for p in out.iter_mut().filter(|p| p.focused) {
                p.frame = clear_of_keyboard(p.frame, keyboard, self.work_area());
                p.client = p.frame;
            }
        }
        out
    }

    /// Where a workspace's windows sit, bottom to top, as they would if it
    /// were the active one (the overview draws each workspace in
    /// miniature from these). Only the active workspace has a focused
    /// window.
    pub fn placements_on(&self, workspace: WorkspaceId) -> Vec<WindowPlacement> {
        if workspace == self.desktop.active().id() {
            return self.arranged();
        }
        self.desktop
            .workspaces()
            .find(|ws| ws.id() == workspace)
            .map(|ws| self.arranged_on(ws, None))
            .unwrap_or_default()
    }

    /// [`Shell::placements`] as the layout has them, before the focused
    /// window makes room for the on-screen keyboard.
    fn arranged(&self) -> Vec<WindowPlacement> {
        self.arranged_on(self.desktop.active(), self.focused())
    }

    fn arranged_on(&self, ws: &Workspace, focused: Option<WindowId>) -> Vec<WindowPlacement> {
        let members: BTreeSet<WindowId> = ws.windows().collect();
        let area = self.work_area();
        let gap = self.profile.gap;
        let visible = |w: &WindowId| !self.is_minimized(*w);
        // No title bar: the client has the whole frame.
        let place = |window: WindowId, frame: Geometry| WindowPlacement {
            window,
            frame,
            client: frame,
            mode: self.windows[&window].mode,
            focused: focused == Some(window),
        };

        let tiled: Vec<WindowId> = ws
            .windows()
            .filter(visible)
            .filter(|w| self.windows[w].mode == Mode::Tiled)
            .collect();
        let ((area_before, area_after), (tile_before, tile_after)) = gap_insets(gap);
        let tile_area = inset_sides(area, area_before, area_after);
        // Phones are monocle whatever the workspace's layout and the
        // window's mode: the topmost window fills the work area, floating
        // and snapped ones included, since there is no room to show two.
        if self.is_phone() {
            return self
                .stack
                .iter()
                .rev()
                .find(|w| members.contains(w) && visible(w))
                .map(|&w| place(w, tile_area))
                .into_iter()
                .collect();
        }
        let topmost_tiled = self.stack.iter().rev().find(|w| tiled.contains(w)).copied();
        let layout = ws.layout();
        let mut out: Vec<WindowPlacement> = match layout.arrange(tile_area, tiled.iter().copied()) {
            Ok(placements) if layout != Layout::Monocle && tiled.len() > 1 => placements
                .map(|p| place(p.window, inset_sides(p.geometry, tile_before, tile_after)))
                .collect(),
            // Monocle, a lone tiled window, or too little space to tile: the
            // topmost tiled window fills the work area. Gaps separate tiles
            // from each other, and one tile has nothing to be separated from.
            _ => topmost_tiled.map(|w| place(w, area)).into_iter().collect(),
        };

        for &w in &self.stack {
            if !members.contains(&w) || !visible(&w) {
                continue;
            }
            match self.windows[&w].mode {
                Mode::Tiled => {}
                Mode::Floating { frame } => out.push(place(w, frame.into())),
                Mode::Snapped { zone } => out.push(place(w, zone.geometry(area, gap))),
            }
        }
        // Windows kept on top go above the rest, in the order they are
        // stacked among themselves, so focusing another window raises it
        // only as far as the first of them.
        let (mut kept, rest): (Vec<_>, Vec<_>) = out
            .into_iter()
            .partition(|p| self.windows[&p.window].on_top);
        let mut out = rest;
        out.append(&mut kept);
        out
    }

    /// The topmost window frame under `point`.
    pub fn window_at(&self, point: Point) -> Option<WindowPlacement> {
        self.placements()
            .into_iter()
            .rev()
            .find(|p| contains(p.frame, point))
    }

    /// Handles a primary-button press at `point` (logical, output-relative).
    ///
    /// Focuses the window under it. With Super held (see
    /// [`Shell::set_move_modifier`]) the press arms a drag wherever it
    /// lands on the window; otherwise it belongs to the client.
    pub fn pointer_down(&mut self, point: Point, _time_ms: u64) -> Result<PointerOutcome, Error> {
        let Some(hit) = self.window_at(point) else {
            self.snap_assist = None;
            return Ok(PointerOutcome::Desktop);
        };
        let window = hit.window;
        if self.focus(window)? != window {
            // A modal dialog is open over it: the press brings the dialog
            // forward and goes no further.
            return Ok(PointerOutcome::Handled {
                effects: Vec::new(),
            });
        }
        // A phone's one window fills the screen: there is nowhere to move
        // it to.
        if self.move_modifier && !self.is_phone() {
            self.arm_drag(window, hit.frame, point);
            return Ok(PointerOutcome::Handled {
                effects: Vec::new(),
            });
        }
        Ok(PointerOutcome::Client {
            window,
            local: (point.0 - hit.client.loc.x, point.1 - hit.client.loc.y),
        })
    }

    /// Whether Super is held, which turns a press on a window into a move.
    pub fn set_move_modifier(&mut self, held: bool) {
        self.move_modifier = held;
    }

    /// Arms a drag of the focused window from its title in the top bar at
    /// `point`, which then moves with [`Shell::pointer_motion`] as dragging
    /// its title bar did.
    pub fn grab_title(&mut self, point: Point) {
        if self.is_phone() {
            return;
        }
        if let Some(window) = self.focused()
            && let Some(frame) = self.frame_of(window)
        {
            self.arm_drag(window, frame, point);
        }
    }

    /// Arms a drag of `window` from `point`, which keeps its offset from
    /// the frame's corner as the window moves; a point above the frame (on
    /// the top bar) keeps the window hanging below the pointer.
    fn arm_drag(&mut self, window: WindowId, frame: Geometry, point: Point) {
        self.drag = Some(Drag {
            window,
            origin: point,
            grab: (point.0 - frame.loc.x, point.1 - frame.loc.y),
            started: false,
            target: DropTarget::Float,
        });
    }

    /// Moves an armed or active drag. Returns the drop target for previewing.
    ///
    /// Like Windows, a snapped, maximized or tiled window detaches into its
    /// restore size on the first real movement, keeping the pointer at the same
    /// relative position across the window.
    pub fn pointer_motion(&mut self, point: Point) -> Option<DropTarget> {
        let mut drag = self.drag?;
        if !drag.started {
            let (dx, dy) = (point.0 - drag.origin.0, point.1 - drag.origin.1);
            if dx.abs().max(dy.abs()) < DRAG_THRESHOLD {
                return None;
            }
            drag.started = true;
            self.snap_assist = None;
            let frame = self.frame_of(drag.window)?;
            let info = &self.windows[&drag.window];
            if !matches!(info.mode, Mode::Floating { .. }) {
                let restore = info.restore.unwrap_or_else(|| self.default_float());
                let scale = |grab: i32, from: i32, to: i32| {
                    let ratio = f64::from(grab) / f64::from(from.max(1));
                    (ratio * f64::from(to)) as i32
                };
                // A title grabbed in the top bar can be off to one side of
                // its window: the window then comes out centred on the
                // pointer.
                let gx = if (0..frame.size.w).contains(&drag.grab.0) {
                    scale(drag.grab.0, frame.size.w, restore.size.w)
                } else {
                    restore.size.w / 2
                };
                // From the top bar the pointer is above the window, and
                // stays the same distance above it.
                let gy = if drag.grab.1 < 0 {
                    drag.grab.1
                } else {
                    scale(drag.grab.1, frame.size.h, restore.size.h)
                };
                drag.grab = (gx, gy);
            }
        }
        let size = match self.windows[&drag.window].mode {
            Mode::Floating { frame } => (frame.w, frame.h),
            _ => {
                let restore = self.windows[&drag.window]
                    .restore
                    .unwrap_or_else(|| self.default_float());
                (restore.size.w, restore.size.h)
            }
        };
        // Its top stays below the top bar: with no title bar to hold, a
        // window pushed up past it by Super+drag would hide its top edge.
        let area = self.work_area();
        let top = (point.1 - drag.grab.1).max(area.loc.y);
        let frame = rect(point.0 - drag.grab.0, top, size.0, size.1);
        let info = self.windows.get_mut(&drag.window)?;
        info.mode = Mode::Floating {
            frame: frame.into(),
        };
        info.before_maximize = None;
        self.raise(drag.window);

        drag.target =
            if let Some(zone) = zone_at(point, area, self.profile.snap) {
                DropTarget::Snap {
                    zone,
                    preview: zone.geometry(area, self.profile.gap),
                }
            } else if let Some(under) = self.placements().into_iter().rev().find(|p| {
                p.window != drag.window && p.mode == Mode::Tiled && contains(p.frame, point)
            }) {
                DropTarget::Tile {
                    onto: under.window,
                    preview: under.frame,
                }
            } else {
                DropTarget::Float
            };
        self.drag = Some(drag);
        Some(drag.target)
    }

    /// Ends a drag, applying its drop target. Returns it if a drag happened.
    pub fn pointer_up(&mut self) -> Option<DropTarget> {
        let drag = self.drag.take().filter(|d| d.started)?;
        let window = drag.window;
        if let Mode::Floating { frame } = self.windows.get(&window)?.mode {
            self.windows.get_mut(&window)?.restore = Some(frame.into());
        }
        match drag.target {
            DropTarget::Float => {}
            DropTarget::Snap { zone, .. } => {
                self.snap(window, zone).ok()?;
            }
            DropTarget::Tile { onto, .. } => {
                let main = self.desktop.active().windows().next();
                self.windows.get_mut(&window)?.mode = Mode::Tiled;
                if main == Some(onto) {
                    self.desktop.focus(window).ok()?;
                    self.desktop.promote_focused();
                }
            }
        }
        Some(drag.target)
    }

    /// Windows on a workspace, in tiling order.
    pub fn windows_on(&self, workspace: WorkspaceId) -> Vec<WindowId> {
        self.desktop
            .workspaces()
            .find(|ws| ws.id() == workspace)
            .map(|ws| ws.windows().collect())
            .unwrap_or_default()
    }

    /// Whether the device is a phone-sized screen.
    pub fn is_phone(&self) -> bool {
        self.profile.form_factor == FormFactor::Phone
    }
}

/// `frame` moved out from under the on-screen keyboard at `keyboard`, staying
/// inside `area`: its bottom edge rises to the keyboard's top when enough of
/// it is left, and otherwise the whole window slides up. A frame the
/// keyboard does not cover is returned as it is.
pub fn clear_of_keyboard(frame: Geometry, keyboard: Geometry, area: Geometry) -> Geometry {
    let top = keyboard.loc.y;
    let overlaps_x = frame.loc.x < keyboard.loc.x + keyboard.size.w
        && keyboard.loc.x < frame.loc.x + frame.size.w;
    if !overlaps_x || frame.loc.y + frame.size.h <= top {
        return frame;
    }
    if top - frame.loc.y >= KEYBOARD_MIN_CLIENT {
        return rect(frame.loc.x, frame.loc.y, frame.size.w, top - frame.loc.y);
    }
    let y = (top - frame.size.h).max(area.loc.y);
    rect(
        frame.loc.x,
        y,
        frame.size.w,
        (top - y).clamp(1, frame.size.h),
    )
}
