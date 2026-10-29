//! `derisk session`: the derisk shell running on
//! [`mcsapi_compositor`], with the derisk core apps from `derisk-apps`.
//!
//! The compositor (in the mcsapi workspace) owns the display, Wayland socket,
//! input and rendering. This module adapts [`Shell`] and [`ShellUi`] to its
//! [`compositor::Shell`] trait, provides the core apps as in-process
//! windows, serves the agent protocol against the live desktop, and locks
//! the screen.
//!
//! While locked the compositor is given no window placements and no focus,
//! so no client is drawn or receives input; every key goes to the lock
//! screen's field, which answers PAM through `derisk auth`
//! (`crate::unlock`), with the fingerprint reader listening beside it when
//! the system offers one. The screen locks on derisk's own Lock action and,
//! with `--execute`, whenever logind asks this session to lock
//! (`loginctl lock-session`, `lock-sessions`).
//!
//! With `--execute` the session is also polkit's authentication agent
//! (`crate::polkit_agent`): a request dims the desktop under a dialog that
//! takes every key, so no window sees what is typed into it. It is also the
//! session's notification server (`crate::notifications`): apps'
//! notifications show as cards in the top-right corner, which take the
//! pointer over them but never the keyboard or the focus.

use std::{
    collections::HashMap,
    io::{BufRead as _, BufReader, Write as _},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command as ProcessCommand, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use derisk::{
    action::{Action, Effect},
    animation,
    backlight::{self, PasswordDim},
    conversation::{Source, StepRef},
    desktop::{self, DesktopEntry},
    effects::{Effects, SettingsWatch},
    familiarity::Familiarity,
    geom::{inset, rect},
    greetd::{Login, Phase, Request},
    idle::{self, Event as IdleEvent},
    ipc::{self, LiveRequest},
    keyboard::{Output as Typed, Predictor},
    keys::{Key, Mods, Sequence, Step, SuperTap},
    lock::LockScreen,
    model,
    overview::Battery,
    palette, privacy,
    privacy_display::{self, Area as CoverArea, PrivacyDisplay, Report as FacesReport},
    report::{self, Problem, ReportDialog, Reporter},
    shell::{Mode, PointerOutcome, Shell, WindowMachine},
    snap::{Direction, SnapZone},
    systemd::{self, SessionOp},
    time::Clock,
    tutorial,
    ui::{
        ChoiceInput, HungInput, KeyHint, LockInput, PolkitInput, ShellUi, TitleDragPhase,
        WindowPictures, set_touch_style, show_choice, show_hung, show_lock, show_polkit,
    },
    wallpaper::{self, Visibility, WallpaperPainter},
    widgets,
    window_motion::{Shown, WindowMotion},
};
use derisk_settings::{KeyName, Power, Privacy, Shortcuts, familiarity};
use mcsapi::WindowId;
use tracing::{debug, info, warn};

use crate::{
    ask,
    box_search::{self, BoxSearch},
    file_search::FileSearch,
    machines::{self, MachineDisplays},
    notifications, pam, polkit_agent, power, unlock,
};
use mcsapi_compositor::{
    self as compositor, AppId, Apps, Blur, Capture, ClientRequest, Command, Compositor, Edges,
    Input, InstanceId, KeyInput, KeyRoute, Keysym, Modifiers, Motion, OutputTiming, Placement,
    Press, Remote, Reserved, Role, RuntimeClient, TextField, Theme, WindowHint, WorkspaceInfo,
    a11y::{Snapshot, Subtree},
    accesskit::{self, NodeId},
    egui,
};
use serde_json::{Value, json};

use crate::computer;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// `derisk session` options.
#[derive(Debug, Default)]
pub struct Options {
    /// Run systemd effects (apps as transient units, logind session operations).
    pub execute: bool,
    /// Agent socket path (default `$XDG_RUNTIME_DIR/derisk/agent.sock`).
    pub socket: Option<PathBuf>,
    /// Apps to launch once the session is up.
    pub launch: Vec<String>,
    /// Initial window size.
    pub size: (i32, i32),
    /// Shorten the startup animation.
    pub reduced_motion: bool,
    /// Programs the compositor starts as panels, overlays or apps
    /// ([`RuntimeClient`]), such as GPUI ones.
    pub runtime: Vec<RuntimeClient>,
}

/// Parses a `--runtime` role: `app`, `overlay`, or
/// `panel:<top|bottom|left|right>:<size>[:keyboard]`.
pub fn parse_role(text: &str) -> std::result::Result<Role, String> {
    let mut parts = text.split(':');
    let role = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("app"), None, ..) => Role::App,
        (Some("overlay"), None, ..) => Role::Overlay,
        (Some("panel"), Some(edge), Some(size), keyboard) => Role::Panel {
            edge: match edge {
                "top" => compositor::Edge::Top,
                "bottom" => compositor::Edge::Bottom,
                "left" => compositor::Edge::Left,
                "right" => compositor::Edge::Right,
                _ => return Err(format!("unknown panel edge {edge:?}")),
            },
            size: size
                .parse()
                .ok()
                .filter(|size| *size > 0)
                .ok_or_else(|| format!("bad panel size {size:?}"))?,
            keyboard: match keyboard {
                None => false,
                Some("keyboard") => true,
                Some(other) => return Err(format!("unknown panel flag {other:?}")),
            },
        },
        _ => {
            return Err(format!(
                "unknown role {text:?} (app, overlay, or panel:<edge>:<size>[:keyboard])"
            ));
        }
    };
    if parts.next().is_some() {
        return Err(format!("unknown role {text:?}"));
    }
    Ok(role)
}

/// The GPUI build of the core apps (`derisk-gpui`), installed beside this
/// executable. The apps it has ported run there as their own Wayland
/// clients; without it they run in-process with egui.
fn gpui_apps() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.with_file_name("derisk-gpui");
    path.is_file().then_some(path)
}

/// What the chrome shows of the state [`Session`]'s tick reads, so a
/// frame is drawn when it changes and not otherwise.
#[derive(PartialEq)]
struct Polled {
    clock: Clock,
    battery: Option<Battery>,
    failed_units: Vec<String>,
    machines: Vec<WindowMachine>,
    can_hibernate: bool,
    dimmed: bool,
    locked: bool,
    cover: Option<CoverArea>,
    /// Where each window is drawn: it changes every frame a window moves.
    shown: HashMap<WindowId, Shown>,
    /// The cards showing, and how many notifications there are at all.
    notifications: (Vec<u32>, usize),
}

/// The derisk shell as seen by the compositor.
pub struct Session {
    pub(crate) shell: Shell,
    ui: ShellUi,
    execute: bool,
    wayland_display: String,
    launches: u64,
    commands: Vec<Command>,
    super_tap: SuperTap,
    /// A multi-key shortcut typed so far (`Super+W`, waiting for `Left`).
    sequence: Sequence,
    /// Keys whose press the shell took for a shortcut: their releases are
    /// the shell's too, so a window never sees a release without a press.
    swallowed: Vec<Keysym>,
    start: Instant,
    /// A power off, reboot or log out waiting for the screen to shut, and
    /// when it was carried out.
    ending: Vec<Effect>,
    ended: Option<Instant>,
    /// Windows opening, gliding and snapping, and where each is drawn this
    /// frame while it does.
    window_motion: WindowMotion,
    /// The windows' motion's time: since the session started, sped up as
    /// the person gets used to the desktop, and its reading this frame.
    motion_clock: animation::MotionClock,
    motion_ms: f64,
    /// Time spent at the desktop, which sets how fast everything moves.
    familiarity: Familiarity,
    shown: HashMap<WindowId, Shown>,
    /// What [`Shell::needs_frame`](compositor::Shell::needs_frame) last
    /// saw of the state [`tick`](compositor::Shell::tick) changes.
    polled: Option<Polled>,
    /// [`tick`](compositor::Shell::tick) changed something the chrome
    /// shows that [`Polled`] does not hold.
    redraw: bool,
    last_tick: Option<Instant>,
    last_sleep_check: Option<Instant>,
    palette_open: bool,
    index: Option<mpsc::Receiver<Index>>,
    core_apps: Vec<DesktopEntry>,
    installed: Vec<DesktopEntry>,
    pending_actions: PendingActions,
    settings: SettingsWatch,
    /// Where Ask derisk sends requests its rules don't follow, when it
    /// does; from Settings › Assistant.
    model: Option<model::Config>,
    /// The language model's thread ([`ask`]), once the compositor runs.
    asking: Option<ask::Asking>,
    /// Whether the output is phone-sized, shared with [`CoreApps`] so the
    /// apps get touch-sized widgets too.
    phone: Arc<AtomicBool>,
    /// The theme's font families, shared with [`CoreApps`]: egui draws
    /// only its own fonts until they are installed in each context.
    fonts: Arc<Mutex<mcsapi_ui::theme::Fonts>>,
    /// Where the on-screen keyboard keeps the words it learned, and when it
    /// last wrote them.
    words: Option<PathBuf>,
    words_saved: Instant,
    /// Where finishing the tour is remembered, and whether this login has
    /// looked for that yet: once the startup animation ends, a login that
    /// has not seen the tour starts it.
    tutorial_mark: Option<PathBuf>,
    tutorial_checked: bool,
    /// The backlight, turned down while the lock screen or a polkit dialog
    /// has a password typed into it, and while the privacy display covers
    /// the screen at maximum protection.
    password_dim: PasswordDim,
    /// The privacy display, and the face counter it runs while one of its
    /// conditions holds.
    privacy_display: PrivacyDisplay,
    faces: Option<FacesWatch>,
    /// Whether a focused text field brought the on-screen keyboard up, so
    /// leaving the field takes it down again; one the person opened stays.
    keyboard_for_field: bool,
    shortcuts: Shortcuts,
    wallpaper: WallpaperPainter,
    /// The lock screen's picture, when one is set.
    lock_wallpaper: WallpaperPainter,
    privacy: Privacy,
    last_sweep: Option<Instant>,
    /// Request numbers for [`Command::Describe`] and [`Command::Capture`].
    requests: u64,
    /// Agents waiting for the accessibility tree.
    trees: HashMap<u64, (mpsc::Sender<String>, TreeQuery)>,
    /// Agents waiting for a screenshot, and whether they want it inline.
    shots: HashMap<u64, (mpsc::Sender<String>, bool)>,
    /// Captures the person asked for with Print, and the pictures being
    /// saved from them: where each landed, or why not.
    pictures: (
        Vec<u64>,
        Vec<mpsc::Receiver<std::result::Result<PathBuf, String>>>,
    ),
    /// Accessibility nodes programs registered for their windows, with
    /// the agent connection that owns them.
    registered: HashMap<WindowId, (u64, Subtree)>,
    /// Event lines to each agent connection that registered a tree or menus.
    listeners: HashMap<u64, mpsc::Sender<String>>,
    /// Environment launched apps get from the published theme.
    theme_env: Vec<(String, String)>,
    /// The icon theme last handed to GSettings and the activation
    /// environment ([`derisk::theme::icon_theme_argv`]).
    synced_icons: Option<String>,
    /// `derisk-gpui`, when installed: see [`gpui_apps`].
    gpui: Option<PathBuf>,
    /// The output's size, and the strips runtime panels cover in it.
    output: (i32, i32),
    reserved: Reserved,
    lock: LockScreen,
    /// Who the lock screen authenticates.
    user: String,
    /// The lock screen field's PAM conversation, while locked.
    auth: Option<unlock::Conversation>,
    /// The fingerprint reader's, beside it, when the system offers one.
    reader: Option<unlock::Conversation>,
    /// polkit's requests to authenticate, the first one shown.
    polkit: Vec<polkit_agent::Prompt>,
    /// Choices apps asked the person to make (`derisk choose`), the first
    /// one shown.
    choices: Vec<Choice>,
    /// This logind session, from `XDG_SESSION_ID`.
    session_id: Option<String>,
    /// Its logind object path, for the lock signal and the locked hint.
    session_path: Option<String>,
    /// swayidle, telling the session when nobody uses it (see
    /// [`derisk::idle`]), with the Power settings it was started for.
    idle: Option<IdleWatch>,
    power: Power,
    /// When swayidle was last started, so one that keeps exiting (or is
    /// not installed) is retried once a minute rather than every second.
    idle_started: Option<Instant>,
    /// Whether a screen locker other than derisk's own (swaylock, through
    /// `ext_session_lock_v1`) has the session locked.
    locked_elsewhere: bool,
    /// Virtual machines and the sockets their windows come in through.
    machines: Option<MachineDisplays>,
    /// power-profiles-daemon's profiles, offered in the palette.
    power_modes: power::PowerModes,
    /// Searches of the file index for the palette's typed text.
    file_search: Option<FileSearch>,
    /// Searches of the LosOS boxes' files and Git for it.
    box_search: Option<BoxSearch>,
    /// Apps' notifications, which `crate::notifications` fills.
    notifications: notifications::Center,
    /// The process behind each Wayland window, from its connection.
    pids: HashMap<WindowId, u32>,
    /// Windows that stopped answering the compositor's pings, oldest
    /// first; the first one not waited for has its card on screen.
    hung: Vec<Hung>,
    /// The report on screen, while there is one.
    report: Option<ReportDialog>,
    /// Where reports go, once the reporter said at startup (see
    /// [`report::probe`]).
    reports_to: mpsc::Receiver<Option<String>>,
}

/// A window that stopped responding.
struct Hung {
    window: WindowId,
    /// When it went silent: the compositor tells only once it has been for
    /// [`UNRESPONSIVE_AFTER`].
    since: Instant,
    /// Whether the person chose to wait for it, or reported it: its card
    /// stays away until it answers and stops again.
    waiting: bool,
}

/// How long mcsapi's compositor waits for a pong before it says a window
/// stopped responding.
const UNRESPONSIVE_AFTER: Duration = Duration::from_secs(5);

/// A choice waiting for the person: the screen to share, say.
struct Choice {
    title: String,
    options: Vec<String>,
    /// The highlighted option.
    picked: usize,
    /// Where the answer goes, as an agent protocol response line.
    reply: mpsc::Sender<String>,
}

/// What an agent asked of the accessibility tree.
enum TreeQuery {
    Tree {
        window: Option<u64>,
    },
    Find {
        role: Option<String>,
        name: Option<String>,
        window: Option<u64>,
    },
}

/// Core-app actions waiting for the compositor to launch their app, shared
/// between [`Session`] (which queues them) and [`CoreApps`] (which takes
/// them when the launch arrives).
type PendingActions = Arc<Mutex<Vec<(String, Pending)>>>;

/// How a queued core app starts.
#[derive(Debug)]
enum Pending {
    /// Through one of its desktop actions.
    Action(String),
    /// On a file or folder.
    Open(PathBuf),
}

/// What the palette indexes in the background: files under home and the
/// installed applications.
type Index = (Vec<palette::File>, Vec<DesktopEntry>);

/// The core apps' own `.desktop` files, parsed.
fn core_desktop_entries() -> Vec<DesktopEntry> {
    derisk_apps::APPS
        .iter()
        .filter_map(|app| DesktopEntry::parse(&app.desktop_id(), app.desktop_file))
        .filter(DesktopEntry::installed)
        .collect()
}

/// Installed applications except hidden ones and copies of the core apps.
fn installed_apps() -> Vec<DesktopEntry> {
    desktop::scan(&desktop::application_dirs())
        .into_iter()
        .filter(listed)
        .collect()
}

/// Whether an installed app belongs in the palette and the app index: not
/// hidden from menus, and not a copy of a core app.
fn listed(entry: &DesktopEntry) -> bool {
    !entry.no_display && derisk_apps::find(&entry.id).is_none()
}

/// The core apps and installed apps, with their desktop actions, for the
/// palette's apps plugin.
fn palette_apps(core: &[DesktopEntry], installed: &[DesktopEntry]) -> Vec<palette::App> {
    let core = core.iter().map(|e| {
        let icon = derisk_apps::find(&e.id).map_or("🖥", |app| app.icon);
        palette::desktop_app(e, icon)
    });
    let installed = installed.iter().map(|e| palette::desktop_app(e, "🖥"));
    core.chain(installed).collect()
}

/// Names and icons for app IDs: the core apps (with their emoji for a
/// theme without their icon), then installed apps.
fn app_index(core: &[DesktopEntry], installed: &[DesktopEntry]) -> derisk::apps::Apps {
    let core = core.iter().map(|e| {
        let glyph = derisk_apps::find(&e.id).map_or("", |app| app.icon);
        (e, glyph)
    });
    derisk::apps::Apps::new(core.chain(installed.iter().map(|e| (e, ""))))
}

impl Session {
    /// The clock window motion runs on, in milliseconds.
    fn motion_now(&self) -> f64 {
        self.motion_ms
    }

    /// How many times as fast as the film everything moves: quicker the
    /// longer the person has used the desktop, unless Settings ›
    /// Appearance turned that off.
    fn motion_speed(&self) -> f32 {
        if self.shell.effects.settle_motion {
            self.familiarity.speed()
        } else {
            1.0
        }
    }

    /// Works out where each window is drawn this frame.
    fn move_windows(&mut self) {
        self.window_motion.keyboard(self.shell.keyboard_visible());
        self.shown = self.window_motion.update(
            self.motion_now(),
            &self.shell.placements(),
            self.shell.window_ids(),
            self.shell.dragged(),
            self.shell.work_area(),
            self.ui.animates(&self.shell),
        );
    }

    fn new(
        options: &Options,
        pending_actions: PendingActions,
        phone: Arc<AtomicBool>,
        fonts: Arc<Mutex<mcsapi_ui::theme::Fonts>>,
    ) -> Self {
        let (w, h) = options.size;
        let mut shell = Shell::new(rect(0, 0, w, h), false);
        phone.store(shell.is_phone(), Ordering::Relaxed);
        let mut ui = ShellUi::new(&shell, options.reduced_motion);
        let core_apps = core_desktop_entries();
        let installed = installed_apps();
        // The keyboard predicts app names too, below words the person used.
        let words = Predictor::default_path();
        if let Some(path) = &words {
            ui.keyboard.predictor.load(path);
        }
        ui.keyboard.predictor.add_vocabulary(
            core_apps
                .iter()
                .chain(&installed)
                .flat_map(|e| e.name.split_whitespace()),
            200,
        );
        ui.palette.apps = palette_apps(&core_apps, &installed);
        let session_id = systemd::session_id();
        let session_path = session_id
            .as_deref()
            .filter(|_| options.execute)
            .and_then(systemd::session_path);
        let user = pam::current_user()
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_default();
        shell.apps = app_index(&core_apps, &installed);
        Self {
            shell,
            ui,
            execute: options.execute,
            wayland_display: String::new(),
            launches: 0,
            commands: Vec::new(),
            super_tap: SuperTap::default(),
            sequence: Sequence::default(),
            swallowed: Vec::new(),
            start: Instant::now(),
            ending: Vec::new(),
            ended: None,
            window_motion: WindowMotion::default(),
            motion_clock: animation::MotionClock::default(),
            motion_ms: 0.0,
            familiarity: Familiarity::open(familiarity::default_path()),
            shown: HashMap::new(),
            polled: None,
            redraw: true,
            last_tick: None,
            last_sleep_check: None,
            palette_open: false,
            index: None,
            core_apps,
            installed,
            pending_actions,
            settings: SettingsWatch::new(derisk_settings::default_path()),
            model: None,
            asking: None,
            phone,
            fonts,
            words,
            tutorial_mark: tutorial::mark_path(),
            tutorial_checked: false,
            password_dim: PasswordDim::system(),
            privacy_display: PrivacyDisplay::default(),
            faces: None,
            words_saved: Instant::now(),
            keyboard_for_field: false,
            shortcuts: Shortcuts::default(),
            wallpaper: WallpaperPainter::default(),
            lock_wallpaper: WallpaperPainter::default(),
            privacy: derisk_settings::Settings::default().privacy,
            last_sweep: None,
            requests: 0,
            trees: HashMap::new(),
            shots: HashMap::new(),
            pictures: (Vec::new(), Vec::new()),
            registered: HashMap::new(),
            listeners: HashMap::new(),
            theme_env: Vec::new(),
            synced_icons: None,
            gpui: gpui_apps(),
            output: (w, h),
            reserved: Reserved::default(),
            lock: LockScreen::default(),
            user,
            auth: None,
            reader: None,
            polkit: Vec::new(),
            choices: Vec::new(),
            session_id,
            session_path,
            idle: None,
            power: derisk_settings::Settings::default().power,
            idle_started: None,
            locked_elsewhere: false,
            machines: MachineDisplays::start(),
            power_modes: power::PowerModes::default(),
            file_search: FileSearch::start(),
            box_search: BoxSearch::start(),
            notifications: notifications::Center::default(),
            pids: HashMap::new(),
            hung: Vec::new(),
            report: None,
            reports_to: {
                // The reporter answers at once, but it is a process to
                // start: not on the way to the first frame.
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(report::probe(&report::command()));
                });
                rx
            },
        }
    }

    /// Locks the screen now, and tells logind the session is locked.
    fn lock_screen(&mut self) {
        // Another locker already hides the session, and unlocking it
        // should not uncover a second lock screen.
        if self.lock.is_locked() || self.locked_elsewhere {
            return;
        }
        // PAM would be asked about user "" and refuse every password, so the
        // lock could only be left from another VT.
        if self.user.is_empty() {
            warn!(
                "not locking: the session's user name is unknown, so no password could unlock it"
            );
            return;
        }
        let Some((first, reader)) = self.lock.lock(&self.user, unlock::fingerprint_offered())
        else {
            return;
        };
        // PAM starts asking at once, so the field shows what this stack
        // wants (a password, or a security key's PIN) and the reader is
        // already listening before anything is typed.
        self.auth = None;
        self.send_auth(first);
        self.reader = reader.and_then(|request| {
            let reader = unlock::start(pam::FINGERPRINT_SERVICE)?;
            reader.send(request);
            Some(reader)
        });
        self.shell.pointer_up();
        self.set_locked_hint(true);
        info!("screen locked");
    }

    /// Sends the field's next request, starting its conversation first if
    /// there is none (it stopped, or never started).
    fn send_auth(&mut self, request: Request) {
        if self.auth.is_none() {
            self.auth = unlock::start(pam::SERVICE);
        }
        let sent = self.auth.as_ref().is_some_and(|auth| auth.send(request));
        if !sent {
            self.auth = None;
            self.lock
                .login
                .disconnected("derisk auth could not be started");
        }
    }

    fn set_locked_hint(&self, locked: bool) {
        if self.execute
            && let Some(path) = &self.session_path
        {
            let _ = systemd::run(&systemd::locked_hint_argv(path, locked));
        }
    }

    /// (Re)starts swayidle when the session runs for real and the Power
    /// settings it was started for changed, or it exited.
    fn sync_idle(&mut self) {
        if !self.execute || self.wayland_display.is_empty() {
            return;
        }
        let changed = self.idle.as_ref().is_some_and(|w| w.power != self.power);
        let running = self.idle.as_mut().is_some_and(|w| w.running());
        let retry = self
            .idle_started
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
        if !changed && (running || !retry) {
            return;
        }
        self.idle_started = Some(Instant::now());
        // The old one goes first, so two never dim the screen at once.
        self.idle = None;
        self.idle = IdleWatch::start(self.power, &self.wayland_display);
        if !self.idle.as_mut().is_some_and(|w| w.running()) {
            warn!("swayidle did not start: the screen will not dim, lock or suspend when idle");
        }
    }

    /// Acts on what swayidle reported since the last frame.
    fn poll_idle(&mut self) {
        // The frame since the lock showed it: the machine may sleep.
        if let Some(watch) = self.idle.as_mut()
            && std::mem::take(&mut watch.sleep_waiting)
        {
            let _ = watch.stdin.write_all(idle::SLEEP_DONE);
        }
        let events: Vec<IdleEvent> = self
            .idle
            .as_ref()
            .map(|w| w.events.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                IdleEvent::Dim => self.shell.dimmed = true,
                IdleEvent::Undim => self.shell.dimmed = false,
                IdleEvent::Lock => {
                    self.lock_screen();
                    // Locked, the screen is the lock screen; dimming it
                    // too would only hide the field.
                    self.shell.dimmed = false;
                }
                IdleEvent::Sleep => {
                    self.lock_screen();
                    self.shell.dimmed = false;
                    if let Some(watch) = self.idle.as_mut() {
                        watch.sleep_waiting = true;
                    }
                }
                IdleEvent::Suspend => self.perform(vec![Effect::Session {
                    op: SessionOp::Suspend,
                }]),
            }
        }
    }

    /// Applies what both conversations answered since the last frame, and
    /// unlocks once PAM accepted either.
    fn poll_unlock(&mut self) {
        drive(&mut self.auth, &mut self.lock.login);
        if let Some(reader) = self.lock.fingerprint.as_mut() {
            drive(&mut self.reader, reader);
        }
        if self.lock.poll() {
            // Ends whichever conversation is still waiting, the reader's
            // usually: it is killed, as PAM cannot be interrupted.
            self.auth = None;
            self.reader = None;
            self.set_locked_hint(false);
            info!("screen unlocked");
        }
    }

    /// Shows a request from polkit, after any already showing.
    pub(crate) fn polkit_begin(&mut self, prompt: polkit_agent::Prompt) {
        self.shell.pointer_up();
        self.polkit.push(prompt);
        // The keyboard learns no words typed here, and comes up on a
        // touchscreen as it does for any password field.
        compositor::Shell::text_input(self, Some(TextField { password: true }));
    }

    /// Turns the backlight down while a password is typed into the lock
    /// screen or a polkit dialog, and back up once it is not.
    fn dim_for_password(&mut self) {
        let covered_dim = self.watch_privacy();
        let login = &self.lock.login;
        let locked = self.lock.is_locked();
        let secret = matches!(login.phase(), Phase::Prompt { secret: true, .. });
        let dialogs = self.polkit.iter().map(|p| &p.dialog);
        let field = backlight::Field {
            shown: locked || !self.polkit.is_empty() || covered_dim,
            typed: covered_dim
                || (locked && secret && !login.answer.is_empty())
                || dialogs
                    .clone()
                    .any(|d| d.secret() && d.editable() && !d.answer.is_empty()),
            checking: (locked && *login.phase() == Phase::Waiting)
                || dialogs.clone().any(|d| d.ask().is_some() && !d.editable()),
        };
        if let Some((name, level)) = self.password_dim.update(field, Instant::now())
            && let Err(error) = backlight::set(&name, level)
        {
            warn!(%error, "cannot set the backlight");
        }
    }

    /// Runs the privacy display for this frame: starts or stops the face
    /// counter with its conditions, takes its counts, and sets what the
    /// shell covers. Returns whether maximum protection wants the
    /// backlight down.
    fn watch_privacy(&mut self) -> bool {
        let now = Instant::now();
        if let Some(watch) = self.faces.as_mut() {
            for report in watch.reports.try_iter() {
                self.privacy_display.report(report, now);
            }
            if !watch.running() {
                self.privacy_display.camera_stopped();
                self.faces = None;
            }
        }
        if self.ui.take_show_anyway() {
            self.privacy_display.show_anyway();
        }
        let locked = self.lock.is_locked();
        let focused = self
            .shell
            .focused()
            .and_then(|w| self.shell.window_label(w))
            .map(|(app, _)| app);
        let situation = privacy_display::Situation {
            password: locked || !self.polkit.is_empty(),
            // Locked, no app is on screen whatever had the focus.
            focused_app: focused.filter(|_| !locked),
            // From the stack rather than what was drawn, which is a frame
            // behind: a pop-up's first frame is already covered.
            notifications: self
                .notifications
                .stack()
                .shown(self.settings.current().notifications.do_not_disturb)
                .next()
                .is_some(),
        };
        let frame =
            self.privacy_display
                .update(&self.settings.current().privacy_display, &situation, now);
        match (frame.camera, self.faces.is_some()) {
            (true, false) => {
                self.faces = FacesWatch::start();
                if self.faces.is_none() {
                    self.privacy_display.camera_stopped();
                }
            }
            (false, true) => self.faces = None,
            _ => {}
        }
        self.shell.privacy_cover = frame.cover;
        frame.dim
    }

    fn polkit_dialog(&mut self, cookie: &str) -> Option<&mut derisk::polkit::AuthDialog> {
        self.polkit
            .iter_mut()
            .find(|p| p.cookie == cookie)
            .map(|p| &mut p.dialog)
    }

    /// What polkit's helper said in the request for `cookie`.
    pub(crate) fn polkit_line(&mut self, cookie: &str, line: &derisk::polkit::HelperLine) {
        if let Some(dialog) = self.polkit_dialog(cookie) {
            dialog.apply(line);
        }
    }

    /// PAM refused an attempt; the request starts another.
    pub(crate) fn polkit_failed(&mut self, cookie: &str) {
        if let Some(dialog) = self.polkit_dialog(cookie) {
            dialog.failed();
        }
    }

    /// The request for `cookie` is over, whichever way it went.
    pub(crate) fn polkit_end(&mut self, cookie: &str) {
        self.polkit.retain(|p| p.cookie != cookie);
        if self.polkit.is_empty() {
            compositor::Shell::text_input(self, None);
        }
    }

    /// The notifications, for the server's thread to run its calls on.
    pub(crate) fn notifications_mut(&mut self) -> &mut notifications::Center {
        &mut self.notifications
    }

    /// Draws apps' notifications and acts on what the person did to them.
    /// Do not disturb is read every frame, so turning it on in Settings
    /// hides the banners at once.
    fn show_notifications(&mut self, ui: &mut egui::Ui) {
        let quiet = self.settings.current().notifications.do_not_disturb;
        let input = self
            .ui
            .show_notifications(ui, &self.shell, self.notifications.stack(), quiet);
        let now = Instant::now();
        for input in input {
            self.notifications.input(input, now);
        }
    }

    /// Draws the first of polkit's requests over the desktop and sends on
    /// what the person did. Only real input counts: an agent driving the
    /// desktop must not authenticate for the person (the shell marks
    /// synthetic input), though it may cancel.
    fn show_polkit(&mut self, ui: &mut egui::Ui) {
        let Some(prompt) = self.polkit.first_mut() else {
            return;
        };
        let theme = self.ui.theme;
        let input = show_polkit(ui, &mut prompt.dialog, &self.shell, &theme);
        let synthetic = self.shell.synthetic_input();
        let reply = match input {
            PolkitInput::None => None,
            PolkitInput::Cancel => Some(polkit_agent::Reply::Cancel),
            _ if synthetic => None,
            PolkitInput::Submit => prompt.dialog.submit().map(polkit_agent::Reply::Answer),
            PolkitInput::Choose(index) => prompt
                .dialog
                .choose(index)
                .then_some(polkit_agent::Reply::Identity(index)),
        };
        if let Some(reply) = reply
            && prompt.replies.send(reply).is_err()
        {
            // Its request already ended; the dialog goes with it.
            self.polkit.remove(0);
        }
    }

    /// Draws the first choice waiting and answers it once the person picks
    /// or cancels. Only real input picks: an agent must not share the
    /// screen for the person, though it may cancel.
    fn show_choice(&mut self, ui: &mut egui::Ui) {
        let Some(choice) = self.choices.first_mut() else {
            return;
        };
        let theme = self.ui.theme;
        let input = show_choice(
            ui,
            &choice.title,
            &choice.options,
            &mut choice.picked,
            &self.shell,
            &theme,
        );
        let answer = match input {
            ChoiceInput::None => return,
            ChoiceInput::Pick(_) if self.shell.synthetic_input() => return,
            ChoiceInput::Pick(index) => json!({"ok": true, "result": choice.options[index]}),
            ChoiceInput::Cancel => json!({"ok": false, "error": "cancelled"}),
        };
        let _ = choice.reply.send(answer.to_string());
        self.choices.remove(0);
    }

    /// Whether a dialog over the desktop takes the pointer and the keys:
    /// polkit's, a choice, a report, or a hung window's card.
    fn dialog_open(&self) -> bool {
        !self.polkit.is_empty()
            || !self.choices.is_empty()
            || self.report.is_some()
            || self.hung.iter().any(|h| !h.waiting)
    }

    /// Draws the report on screen, else the card of the first window that
    /// stopped responding and nobody chose to wait for.
    fn show_report(&mut self, ui: &mut egui::Ui) {
        let width = 560.0_f32.min(self.output.0 as f32 - 80.0);
        if let Some(dialog) = &mut self.report {
            if !dialog.show(ui, width) {
                self.report = None;
            }
            return;
        }
        let Some(i) = self.hung.iter().position(|h| !h.waiting) else {
            return;
        };
        let window = self.hung[i].window;
        let Some((app, _)) = self.shell.window_label(window) else {
            self.hung.remove(i);
            return;
        };
        let app = app.to_owned();
        let name = self.shell.apps.look(&app).name.into_owned();
        let pid = self.pids.get(&window).copied();
        match show_hung(ui, &name, pid.is_some(), self.shell.can_report, &self.shell) {
            HungInput::None => {}
            HungInput::Wait => self.hung[i].waiting = true,
            // Only real input ends a process: an agent may wait or report,
            // not throw away what the person has not saved.
            HungInput::ForceQuit if self.shell.synthetic_input() => {}
            HungInput::ForceQuit => {
                self.hung[i].waiting = true;
                if let Some(pid) = pid {
                    force_quit(pid);
                }
            }
            HungInput::Report => {
                self.hung[i].waiting = true;
                self.open_report(
                    Problem::NotResponding {
                        app,
                        pid: None,
                        seconds: 0,
                    },
                    Some(window),
                );
            }
        }
    }

    /// Shows the report of `problem`, written by the reporter. For a window
    /// that stopped responding, `window` is where its process and how long
    /// it has been silent come from.
    fn open_report(&mut self, mut problem: Problem, window: Option<WindowId>) {
        if !self.shell.can_report {
            info!(?problem, "this system sends no reports");
            return;
        }
        if let (Problem::NotResponding { pid, seconds, .. }, Some(window)) = (&mut problem, window)
        {
            *pid = self.pids.get(&window).copied();
            *seconds = self
                .hung
                .iter()
                .find(|h| h.window == window)
                .map_or(0, |h| h.since.elapsed().as_secs());
        }
        let name = match &problem {
            Problem::NotResponding { app, .. } | Problem::Stuck { app, .. } => {
                self.shell.apps.look(app).name.into_owned()
            }
            Problem::Failed { .. } => String::new(),
        };
        match Reporter::start(&report::command(), &problem) {
            Ok(reporter) => {
                self.report = Some(ReportDialog {
                    summary: problem.summary(&name),
                    reporter,
                });
            }
            Err(e) => warn!(error = %e, "cannot start {}", report::PROGRAM),
        }
    }

    /// A key on the lock screen: wakes a reader that stopped listening.
    fn wake_reader(&mut self) {
        if let Some(request) = self.lock.wake_fingerprint()
            && !self.reader.as_ref().is_some_and(|r| r.send(request))
        {
            // Its process is gone; the field still works, so the reader is
            // simply no longer offered until the next lock.
            self.reader = None;
            self.lock.fingerprint = None;
        }
    }

    /// Takes the theme from the current settings for the chrome and core
    /// apps, and publishes it for everything else ([`derisk::theme`]).
    fn apply_theme(&mut self) {
        let settings = self.settings.current();
        let library = mcsapi_theme::Library::xdg("derisk");
        let (theme, error) = settings.theme_spec(&library);
        if let Some(error) = error {
            warn!(
                "theme {}: {error}; using the automatic theme",
                settings.appearance.theme
            );
        }
        self.ui.theme = Theme::from(&theme);
        self.ui.tokens = mcsapi_components::Tokens::from_spec(&theme);
        *self.fonts.lock().unwrap_or_else(|e| e.into_inner()) = theme.fonts.clone();
        // The chrome and the core apps share this process, so they all
        // draw from the icon theme the theme names.
        derisk::icons::set_theme(&theme.icons.theme);
        let id = match settings.appearance.theme {
            id if id.is_automatic() => match settings.appearance.scheme {
                derisk_settings::ColorScheme::Dark => "derisk-dark".to_owned(),
                derisk_settings::ColorScheme::Light => "derisk-light".to_owned(),
            },
            id => id.as_str().to_owned(),
        };
        let Some(dir) = derisk::theme::runtime_dir() else {
            return;
        };
        if let Err(error) = derisk::theme::publish(&dir, &id, &theme) {
            warn!("publishing the theme to {}: {error}", dir.display());
        }
        self.theme_env =
            derisk::theme::environment(&dir, &theme, std::env::var_os("XDG_CONFIG_DIRS"));
        // Only a real session (--execute) changes the user's GSettings and
        // activation environment, and only when the icon theme moved, since
        // this runs on every settings change.
        if self.execute && self.synced_icons.as_deref() != Some(theme.icons.theme.as_str()) {
            for argv in derisk::theme::icon_theme_argv(&theme.icons.theme) {
                let failed = match systemd::run(&argv) {
                    Ok(output) if output.status.success() => None,
                    Ok(output) => Some(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = failed {
                    warn!("{}: {error}", argv[0]);
                }
            }
            self.synced_icons = Some(theme.icons.theme.clone());
        }
    }

    /// Handles one agent request line from connection `conn`; the response
    /// goes to `reply`, now or once the compositor has answered.
    /// Starts the model on `text`, in a turn of its own.
    fn ask_model(&mut self, text: &str, config: model::Config) {
        let Some(asking) = &mut self.asking else {
            self.shell.conversation.not_understood(
                Source::User,
                text,
                "The language model's thread did not start",
            );
            return;
        };
        let turn = self.shell.conversation.start_model(text, "Thinking…");
        self.shell.model_running = Some(turn);
        if let Err(e) = asking.ask(turn, text, config) {
            self.shell.conversation.finish(turn, Err(e.to_owned()));
            self.shell.model_running = asking.running();
        }
    }

    /// The model's thread finished turn `turn`.
    pub(crate) fn model_finished(
        &mut self,
        turn: u64,
        outcome: std::result::Result<String, String>,
    ) {
        self.shell.conversation.finish(turn, outcome);
        if let Some(asking) = &mut self.asking {
            asking.finished(turn);
            self.shell.model_running = asking.running();
        }
    }

    /// Whether the lock screen is up, when agents get nothing.
    pub(crate) fn is_locked(&self) -> bool {
        self.lock.is_locked()
    }

    /// Runs one of the model's actions as an agent's, as step `step` of
    /// its turn when it has one.
    pub(crate) fn model_act(
        &mut self,
        action: Action,
        step: Option<StepRef>,
    ) -> std::result::Result<String, String> {
        let did = model::did(&action);
        let outcome = self.shell.as_agent(|shell| match step {
            Some(step) => shell.run_step(action, step),
            None => shell.run([action]),
        });
        self.perform(outcome.effects);
        outcome.result.map(|()| did).map_err(|e| e.to_string())
    }

    /// The installed apps as `list_apps` tells them: name and the ID to
    /// open each by, derisk's own first.
    pub(crate) fn app_list(&self) -> String {
        self.core_apps
            .iter()
            .chain(&self.installed)
            .map(|e| format!("{} ({})", e.name, e.id))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn agent_line(
        &mut self,
        conn: u64,
        line: &str,
        reply: mpsc::Sender<String>,
        events: &mpsc::Sender<String>,
    ) {
        let respond = |result: std::result::Result<Value, String>| {
            let line = match result {
                Ok(result) => json!({"ok": true, "result": result}),
                Err(error) => json!({"ok": false, "error": error}),
            };
            let _ = reply.send(line.to_string());
        };
        // Here, unlike in a headless agent, the connection that registers a
        // window's menus owns them and hears of picks.
        if let Ok(ipc::Request::RegisterMenu { window, menus }) =
            serde_json::from_str::<ipc::Request>(line)
        {
            let result = ipc::register_menu(&mut self.shell, conn, window, menus);
            if result.is_ok() {
                self.listeners.insert(conn, events.clone());
            }
            return respond(result);
        }
        // Likewise the connection that registers a custom widget owns it.
        match serde_json::from_str::<ipc::Request>(line) {
            Ok(ipc::Request::RegisterWidget { widget }) => {
                let result = self.shell.widgets.register(widget, Some(conn));
                if result.is_ok() {
                    self.listeners.insert(conn, events.clone());
                }
                return respond(result.map(|()| Value::Null));
            }
            Ok(ipc::Request::RemoveWidget { id }) => {
                return respond(
                    self.shell
                        .widgets
                        .remove(&id, Some(conn))
                        .map(|()| Value::Null),
                );
            }
            // And the connection that registers palette data owns it.
            Ok(ipc::Request::RegisterPalette { source, data }) => {
                let result = self
                    .shell
                    .palette_sources
                    .register(source, data, Some(conn));
                if result.is_ok() {
                    self.listeners.insert(conn, events.clone());
                }
                return respond(result.map(|()| Value::Null));
            }
            Ok(ipc::Request::RemovePalette { source }) => {
                return respond(
                    self.shell
                        .palette_sources
                        .remove(&source, Some(conn))
                        .map(|()| Value::Null),
                );
            }
            _ => {}
        }
        let request = match ipc::live_request(line) {
            None => {
                let (response, effects) = ipc::handle_line(&mut self.shell, line);
                self.perform(effects);
                let _ = reply.send(response);
                return;
            }
            Some(Err(e)) => return respond(Err(e)),
            Some(Ok(request)) => request,
        };
        match request {
            LiveRequest::Tree { window } => {
                self.requests += 1;
                self.trees
                    .insert(self.requests, (reply, TreeQuery::Tree { window }));
                self.commands.push(Command::Describe(self.requests));
            }
            LiveRequest::Find { role, name, window } => {
                self.requests += 1;
                self.trees.insert(
                    self.requests,
                    (reply, TreeQuery::Find { role, name, window }),
                );
                self.commands.push(Command::Describe(self.requests));
            }
            LiveRequest::Act {
                element,
                action,
                value,
            } => {
                self.commands.push(Command::Act {
                    element,
                    action: computer::element_action(action),
                    value,
                });
                respond(Ok(json!({"queued": true})));
            }
            LiveRequest::Screenshot { inline } => {
                self.requests += 1;
                self.shots.insert(self.requests, (reply, inline));
                self.commands.push(Command::Capture(self.requests));
            }
            LiveRequest::Input { events } => match computer::inputs(&events) {
                Ok(inputs) => {
                    let n = inputs.len();
                    self.commands.extend(inputs.into_iter().map(Command::Input));
                    respond(Ok(json!({"queued": n})));
                }
                Err(e) => respond(Err(e)),
            },
            LiveRequest::Choose { title, options } => {
                if options.is_empty() {
                    return respond(Err("nothing to choose from".to_owned()));
                }
                self.shell.pointer_up();
                self.choices.push(Choice {
                    title: title.unwrap_or_else(|| "Share your screen".to_owned()),
                    options,
                    picked: 0,
                    reply,
                });
            }
            LiveRequest::RegisterTree { window, nodes } => {
                let Some(id) =
                    WindowId::new(window).filter(|w| self.shell.window_label(*w).is_some())
                else {
                    return respond(Err(format!("unknown window: {window}")));
                };
                if let Some((owner, _)) = self.registered.get(&id)
                    && *owner != conn
                {
                    return respond(Err(format!(
                        "another connection registered window {window}'s tree"
                    )));
                }
                match computer::subtree(id, &nodes) {
                    Ok(subtree) => {
                        self.registered.insert(id, (conn, subtree));
                        self.listeners.insert(conn, events.clone());
                        respond(Ok(json!({"nodes": nodes.len()})));
                    }
                    Err(e) => respond(Err(e)),
                }
            }
        }
    }

    /// An agent connection closed: its registered trees and menus go with
    /// it, so a closed program's tab list does not linger in the palette.
    fn agent_disconnected(&mut self, conn: u64) {
        self.registered.retain(|_, (owner, _)| *owner != conn);
        self.shell.menus.disown(conn);
        self.shell.widgets.disown(conn);
        self.shell.palette_sources.disown(conn);
        self.listeners.remove(&conn);
    }

    // Each window's title-bar buttons were accessibility nodes here, since
    // the shell painted them itself. The buttons are in the top bar now,
    // which is egui and names its own.

    /// Applies actions from any source and carries out their effects.
    fn dispatch(&mut self, actions: Vec<Action>) {
        if actions.is_empty() {
            return;
        }
        let outcome = self.shell.run(actions);
        // Actions before a failure stay applied, so their effects still run.
        self.perform(outcome.effects);
        if let Err(e) = outcome.result {
            info!("action failed: {e}");
        }
        self.palette_opened();
    }

    /// Tells the person where each screenshot they took landed.
    fn poll_pictures(&mut self) {
        let mut saved = Vec::new();
        self.pictures.1.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                saved.push(result);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        for result in saved {
            let (summary, body) = match result {
                Ok(path) => (
                    "Screenshot saved",
                    path.file_name()
                        .map(|n| format!("In Pictures/Screenshots as {}", n.to_string_lossy()))
                        .unwrap_or_default(),
                ),
                Err(e) => ("Couldn't save the screenshot", e),
            };
            self.notifications
                .notify_local(summary, &body, Instant::now());
        }
    }

    /// Forgets a multi-key shortcut typed so far, and its hint.
    fn cancel_sequence(&mut self) {
        self.sequence.cancel();
        self.show_sequence();
    }

    /// Shows what may follow the chords typed so far, or nothing.
    fn show_sequence(&mut self) {
        let typed = self.sequence.typed();
        self.ui.key_hint = (!typed.is_empty()).then(|| KeyHint {
            typed: typed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            next: self
                .shortcuts
                .continuations(typed)
                .map(|(shortcut, rest)| {
                    let keys = rest.iter().map(ToString::to_string).collect::<Vec<_>>();
                    (keys.join(" "), shortcut.label())
                })
                .collect(),
        });
    }

    /// Re-indexes home on a background thread each time the palette opens,
    /// so file results stay fresh without stalling a frame, and reads the
    /// power modes the same way.
    fn palette_opened(&mut self) {
        let open = self.shell.palette_visible();
        if open && !self.palette_open {
            self.power_modes.start(None);
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let home = std::env::var_os("HOME").map(PathBuf::from);
                let files = home
                    .as_deref()
                    .map(|home| palette::index_files(home, 4, 20_000))
                    .unwrap_or_default()
                    .iter()
                    .map(|path| palette::file(path, home.as_deref()))
                    .collect::<Vec<_>>();
                let _ = tx.send((files, installed_apps()));
            });
            self.index = Some(rx);
        }
        self.palette_open = open;
        if let Some((files, installed)) = self.index.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.ui.palette.set_files(files);
            self.ui.palette.apps = palette_apps(&self.core_apps, &installed);
            self.shell.apps = app_index(&self.core_apps, &installed);
            self.installed = installed;
            self.index = None;
        }
        if let Some(read) = self.power_modes.poll() {
            // Without power-profiles-daemon there is nothing to switch, so
            // there are no rows rather than dead ones.
            let shown = match read {
                Some(profiles) => self.shell.palette_sources.register(
                    power::SOURCE.into(),
                    power::palette(&profiles),
                    Some(SHELL),
                ),
                None => self
                    .shell
                    .palette_sources
                    .remove(power::SOURCE, Some(SHELL)),
            };
            if let Err(e) = shown {
                debug!(error = e, "power modes are not in the palette");
            }
        }
    }

    /// Runs `argv` for `app`: as a transient unit with `--execute`,
    /// otherwise as a child with this session's `WAYLAND_DISPLAY`.
    fn spawn_command(&mut self, app: &str, argv: &[String]) {
        self.launches += 1;
        if self.execute {
            let instance = systemd::instance(self.launches);
            if let Some(mut unit) = systemd::launch_command_argv(app, instance, argv) {
                let split = unit.iter().position(|a| a == "--").unwrap_or(unit.len());
                unit.insert(
                    split,
                    format!("--setenv=WAYLAND_DISPLAY={}", self.wayland_display),
                );
                for (key, value) in &self.theme_env {
                    unit.insert(split, format!("--setenv={key}={value}"));
                }
                // Said in the journal, where the unit's own lines are, so a
                // launch that came to nothing is not a silent one.
                match systemd::run(&unit) {
                    Ok(out) if out.status.success() => {}
                    Ok(out) => warn!(
                        app,
                        "cannot launch: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                    Err(e) => warn!(app, "cannot run systemd-run: {e}"),
                }
            }
        } else if let Some((program, args)) = argv.split_first()
            && let Err(e) = std::process::Command::new(program)
                .args(args)
                .env("WAYLAND_DISPLAY", &self.wayland_display)
                .envs(self.theme_env.iter().map(|(k, v)| (k, v)))
                .spawn()
        {
            warn!("{program}: {e}");
        }
    }

    /// Opens `app`: a core app ported to GPUI as a `derisk-gpui` process when
    /// that is installed, any other core app in-process.
    fn launch(&mut self, app: &str) {
        let id = derisk_apps::find(app).map(|core| core.id);
        if let (Some(gpui), Some(id)) = (&self.gpui, id)
            && derisk_gpui::APPS.contains(&id)
        {
            let argv = vec![gpui.to_string_lossy().into_owned(), id.to_owned()];
            self.spawn_command(id, &argv);
            return;
        }
        // Settings may send apps to a machine. Not derisk's own, which only
        // this compositor can draw; and here after all when that machine
        // is gone, rather than nowhere.
        let machine = &self.settings.current().machines.open_apps_in;
        if id.is_none()
            && !machine.is_empty()
            && let Some(machines) = &self.machines
            && machines.open(machine, app)
        {
            return;
        }
        self.commands.push(Command::Launch(app.to_owned()));
    }

    /// A press on the overview's machines card: open the Machines app, or
    /// start or shut down the machine the button names (`start:work`).
    fn machines_pressed(&mut self, item: &str) {
        if item == machines::MANAGE {
            self.launch(derisk_machines::APP_ID);
            return;
        }
        let acted = item.split_once(':').is_some_and(|(op, machine)| {
            self.machines
                .as_mut()
                .is_some_and(|m| m.act(op, machine, None))
        });
        if !acted {
            warn!(item, "no such machine or command");
        }
    }

    /// Shows the shell the output without the strips runtime panels cover.
    fn apply_output(&mut self) {
        let (w, h) = self.output;
        let area = self
            .reserved
            .shrink(mcsapi::Geometry::new((0, 0).into(), (w, h).into()));
        self.shell.set_output(
            rect(area.loc.x, area.loc.y, area.size.w, area.size.h),
            false,
        );
        self.phone.store(self.shell.is_phone(), Ordering::Relaxed);
    }

    /// The installed app a desktop file ID names. One installed since the
    /// apps were last read (the palette reads them again only as it opens)
    /// is not among them, and its ID would run as a command that does not
    /// exist: so on a miss they are read again, which takes a few
    /// milliseconds, and the palette and the app index learn of it too.
    /// Apps hidden from menus are found as well, since asking for one by
    /// its ID is how they are meant to start.
    fn installed_entry(&mut self, app: &str) -> Option<DesktopEntry> {
        if let Some(entry) = self.installed.iter().find(|e| e.id == app) {
            return Some(entry.clone());
        }
        // A plain command is no desktop file ID; reading every application
        // directory for it would find nothing.
        if !app.ends_with(".desktop") {
            return None;
        }
        let all = desktop::scan(&desktop::application_dirs());
        let found = all.iter().find(|e| e.id == app).cloned();
        let installed: Vec<DesktopEntry> = all.into_iter().filter(listed).collect();
        if installed != self.installed {
            self.ui.palette.apps = palette_apps(&self.core_apps, &installed);
            self.shell.apps = app_index(&self.core_apps, &installed);
            self.installed = installed;
        }
        found
    }

    /// Runs an app's desktop action: core apps open in-process through
    /// [`CoreApps`], installed apps run the action's `Exec`.
    fn launch_action(&mut self, app: &str, id: &str) {
        if let Some(core) = derisk_apps::find(app) {
            if core.create_action(id).is_none() {
                info!("{} has no action {id:?}", core.id);
                return;
            }
            if let Ok(mut pending) = self.pending_actions.lock() {
                pending.push((core.id.to_owned(), Pending::Action(id.to_owned())));
            }
            self.commands.push(Command::Launch(core.id.to_owned()));
            return;
        }
        let Some(argv) = self.installed_entry(app).and_then(|e| e.action_argv(id)) else {
            info!("{app} has no action {id:?}");
            return;
        };
        self.spawn_command(app, &argv);
    }

    /// Opens `path` in a core app, as `xdg-open` asks through the app's
    /// desktop file. Only derisk's own apps are drawn here, so another
    /// app's ID opens nothing.
    fn open_in(&mut self, app: &str, path: &Path) {
        let Some(core) = derisk_apps::find(app) else {
            info!("{app} is not a core app, so it opens no file here");
            return;
        };
        if let Ok(mut pending) = self.pending_actions.lock() {
            pending.push((core.id.to_owned(), Pending::Open(path.to_owned())));
        }
        self.commands.push(Command::Launch(core.id.to_owned()));
    }

    fn perform(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match &effect {
                Effect::Report { problem, window } => {
                    self.open_report(problem.clone(), window.and_then(WindowId::new));
                }
                Effect::Launch { app } => self.launch(app),
                Effect::LaunchAction { app, id } => self.launch_action(app, id),
                Effect::OpenIn { app, path } => self.open_in(app, path),
                Effect::Close { window } => {
                    if let Some(id) = WindowId::new(*window) {
                        self.commands.push(Command::Close(id));
                    }
                }
                Effect::Session {
                    op: SessionOp::PowerOff | SessionOp::Reboot | SessionOp::Logout,
                } if !self.ui.has_shut() && !self.lock.is_locked() => {
                    // The screen shuts into the boot screen's line first
                    // (see `tick`); the lock screen has no shell to shut.
                    self.ui.shut_down();
                    self.ending.push(effect);
                }
                Effect::Session { .. }
                | Effect::RestartUnit { .. }
                | Effect::ResetFailed { .. } => {
                    // Lock at once rather than waiting for logind's signal
                    // to come back, and without --execute too.
                    if matches!(
                        effect,
                        Effect::Session {
                            op: SessionOp::Lock | SessionOp::SwitchUser
                        }
                    ) {
                        self.lock_screen();
                    }
                    if self.execute {
                        self.launches += 1;
                        let session = self.session_id.as_deref();
                        let instance = systemd::instance(self.launches);
                        if let Some(argv) = systemd::effect_argv(&effect, instance, session) {
                            let _ = systemd::run(&argv);
                        }
                        if matches!(
                            effect,
                            Effect::Session {
                                op: SessionOp::SwitchUser
                            }
                        ) {
                            switch_user();
                        }
                    } else {
                        info!(
                            "not executing {} (run with --execute)",
                            serde_json::to_string(&effect).unwrap_or_default()
                        );
                    }
                }
                Effect::TutorialSeen => {
                    if let Some(mark) = &self.tutorial_mark {
                        let written = mark
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(mark, ""));
                        if let Err(e) = written {
                            warn!(path = %mark.display(), error = %e, "cannot remember the tour was seen");
                        }
                    }
                }
                Effect::Screenshot => {
                    self.requests += 1;
                    self.pictures.0.push(self.requests);
                    self.commands.push(Command::Capture(self.requests));
                }
                Effect::Open { path } => {
                    if self.execute {
                        self.launches += 1;
                        let session = self.session_id.as_deref();
                        let instance = systemd::instance(self.launches);
                        if let Some(mut argv) = systemd::effect_argv(&effect, instance, session) {
                            let split = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
                            argv.insert(
                                split,
                                format!("--setenv=WAYLAND_DISPLAY={}", self.wayland_display),
                            );
                            let _ = systemd::run(&argv);
                        }
                    } else if let Err(e) = std::process::Command::new("xdg-open")
                        .arg(path)
                        .env("WAYLAND_DISPLAY", &self.wayland_display)
                        .envs(self.theme_env.iter().map(|(k, v)| (k, v)))
                        .spawn()
                    {
                        warn!("xdg-open {path}: {e}");
                    }
                }
                Effect::MenuActivated { window, item } => {
                    // A pick from menus a connection registered goes back to
                    // it; unowned menus have nobody to tell yet.
                    match self
                        .shell
                        .menus
                        .recipient(*window, item)
                        .and_then(|conn| self.listeners.get(&conn))
                    {
                        Some(events) => {
                            let _ = events.send(ipc::menu_event(*window, item).to_string());
                        }
                        None => info!("{}", serde_json::to_string(&effect).unwrap_or_default()),
                    }
                }
                Effect::WidgetActivated { id, item }
                    if self.shell.widgets.recipient(id, item) == Some(SHELL) =>
                {
                    self.machines_pressed(item);
                }
                Effect::WidgetActivated { id, item } => {
                    match self
                        .shell
                        .widgets
                        .recipient(id, item)
                        .and_then(|conn| self.listeners.get(&conn))
                    {
                        Some(events) => {
                            let _ = events.send(widgets::widget_event(id, item).to_string());
                        }
                        None => info!("{}", serde_json::to_string(&effect).unwrap_or_default()),
                    }
                }
                Effect::PaletteCommand { source, command }
                    if self.shell.palette_sources.recipient(source) == Some(SHELL)
                        && source == power::SOURCE =>
                {
                    match power::picked(command) {
                        Some(profile) => self.power_modes.start(Some(profile.to_owned())),
                        None => warn!(%command, "no such power mode"),
                    }
                }
                Effect::PaletteCommand { source, command }
                    if self.shell.palette_sources.recipient(source) == Some(SHELL) =>
                {
                    let text = |key: &str| command[key].as_str().unwrap_or_default();
                    let app = command["app"].as_str();
                    if !self
                        .machines
                        .as_mut()
                        .is_some_and(|m| m.act(text("op"), text("machine"), app))
                    {
                        warn!(%command, "no such machine or command");
                    }
                }
                Effect::PaletteCommand { source, command } => {
                    match self
                        .shell
                        .palette_sources
                        .recipient(source)
                        .and_then(|conn| self.listeners.get(&conn))
                    {
                        Some(events) => {
                            let _ = events.send(palette::event(source, command).to_string());
                        }
                        None => info!("{}", serde_json::to_string(&effect).unwrap_or_default()),
                    }
                }
                Effect::TrayActivated { .. } => {
                    info!("{}", serde_json::to_string(&effect).unwrap_or_default())
                }
            }
        }
    }

    /// Applies Settings → Privacy to files on disk: right after it changes
    /// and then every few minutes, so a recent-files list an app writes
    /// again goes away soon after.
    fn sweep(&mut self) {
        if self
            .last_sweep
            .is_some_and(|t| t.elapsed() < Duration::from_secs(300))
        {
            return;
        }
        self.last_sweep = Some(Instant::now());
        let Some(data) = privacy::data_home() else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        match privacy::sweep(&self.privacy, &data, now) {
            Ok(swept) if swept.trashed > 0 => {
                info!("emptied {} old item(s) from the trash", swept.trashed)
            }
            Ok(_) => {}
            Err(e) => warn!("privacy sweep: {e}"),
        }
    }

    fn startup_done(&self) -> bool {
        self.ui.startup_done()
    }
}

impl compositor::Shell for Session {
    fn map_window(&mut self, app_id: &str, title: &str) -> WindowId {
        let (id, effects) = self.shell.map_window(app_id, title);
        self.perform(effects);
        id
    }

    fn set_app_id(&mut self, window: WindowId, app_id: &str) {
        if let Ok(effects) = self.shell.set_app_id(window, app_id) {
            self.perform(effects);
        }
    }

    fn unmap_window(&mut self, window: WindowId) {
        let _ = self.shell.unmap_window(window);
        self.pids.remove(&window);
        self.hung.retain(|h| h.window != window);
    }

    fn window_process(&mut self, window: WindowId, pid: u32) {
        self.pids.insert(window, pid);
    }

    fn window_responding(&mut self, window: WindowId, responding: bool) {
        if responding {
            self.hung.retain(|h| h.window != window);
        } else if !self.hung.iter().any(|h| h.window == window) {
            let now = Instant::now();
            self.hung.push(Hung {
                window,
                since: now.checked_sub(UNRESPONSIVE_AFTER).unwrap_or(now),
                waiting: false,
            });
        }
    }

    /// A window that came in through a machine's socket wears that
    /// machine's label. The socket is the machine's alone (see
    /// [`derisk_machines::display`]), so this is where it came from,
    /// whatever its client says about itself.
    fn window_sandbox(&mut self, window: WindowId, sandbox: &compositor::Sandbox) {
        if sandbox.engine.as_deref() != Some(derisk_machines::SANDBOX_ENGINE) {
            return;
        }
        let Some(name) = sandbox.app_id.clone() else {
            return;
        };
        // Gray if the machine is not in the last list yet: the colour can
        // wait, the name in the top bar cannot.
        let label = self
            .machines
            .as_ref()
            .and_then(|m| m.machine(&name))
            .map(|m| m.label)
            .unwrap_or_default();
        let _ = self
            .shell
            .set_window_machine(window, WindowMachine { name, label });
    }

    fn set_output(&mut self, size: (i32, i32)) {
        self.output = size;
        self.apply_output();
    }

    fn set_reserved(&mut self, reserved: Reserved) {
        self.reserved = reserved;
        self.apply_output();
    }

    fn focused(&self) -> Option<WindowId> {
        if self.lock.is_locked() || !self.polkit.is_empty() || self.report.is_some() {
            return None;
        }
        self.shell.focused()
    }

    /// None while locked, which unmaps every client window.
    fn placements(&self) -> Vec<Placement> {
        if self.lock.is_locked() {
            return Vec::new();
        }
        self.shell
            .placements()
            .into_iter()
            .map(|p| Placement {
                window: p.window,
                frame: p.frame,
                client: p.client,
                focused: p.focused,
                tiled: tiled_edges(p.mode),
                maximized: p.mode
                    == (Mode::Snapped {
                        zone: SnapZone::Maximize,
                    }),
                motion: self.shown.get(&p.window).map(|s| Motion {
                    shown: s.frame,
                    opacity: s.opacity,
                }),
            })
            .collect()
    }

    fn set_title(&mut self, window: WindowId, title: &str) {
        let _ = self.shell.set_title(window, title);
    }

    fn focus(&mut self, window: WindowId) {
        if self.lock.is_locked() {
            return;
        }
        self.dispatch(vec![Action::Focus {
            window: window.get(),
        }]);
    }

    fn session_started(&mut self, wayland_display: &str) {
        self.wayland_display = wayland_display.to_owned();
        if self.execute {
            for argv in systemd::session_start_argv(wayland_display) {
                let _ = systemd::run(&argv);
            }
        }
        info!("derisk session on WAYLAND_DISPLAY={wayland_display}");
        systemd::notify_ready("derisk session running");
    }

    /// Refreshes the clock, battery, effect settings and failed units about
    /// once a second, and whether the machine can hibernate every half
    /// minute.
    fn tick(&mut self) {
        self.motion_clock.set_speed(self.motion_speed());
        self.ui.motion_speed = self.motion_speed();
        self.motion_ms = self
            .motion_clock
            .at(self.start.elapsed().as_secs_f64() * 1000.0);
        self.poll_idle();
        self.dim_for_password();
        if !self.tutorial_checked && self.startup_done() && !self.lock.is_locked() {
            self.tutorial_checked = true;
            if self.tutorial_mark.as_ref().is_some_and(|m| !m.exists()) {
                self.dispatch(vec![Action::Tutorial {
                    visible: Some(true),
                }]);
                self.redraw = true;
            }
        }
        // Every frame, so a banner goes when its time is up rather than
        // up to a second later.
        self.notifications.expire(Instant::now());
        self.poll_pictures();
        // Every frame, so what the index finds shows the frame after it
        // answers.
        if let Some(search) = &mut self.file_search
            && let Some(found) = search.poll(self.ui.palette.file_search())
        {
            self.ui.palette.set_file_matches(found);
            self.redraw = true;
        }
        // The same for the boxes, whose rows come from the `boxes` source.
        // The shell holds it from the start, with no rows until a search
        // finds some, so no agent connection can register rows under it.
        if !self.shell.palette_sources.has(box_search::SOURCE) {
            let _ = self.shell.palette_sources.register(
                box_search::SOURCE.into(),
                box_search::palette("", Vec::new()),
                Some(SHELL),
            );
        }
        if let Some(search) = &mut self.box_search
            && let Some((text, hits)) = search.poll(self.ui.palette.box_search())
        {
            if let Err(e) = self.shell.palette_sources.register(
                box_search::SOURCE.into(),
                box_search::palette(&text, hits),
                Some(SHELL),
            ) {
                debug!(error = e, "the boxes' rows are not in the palette");
            }
            self.redraw = true;
        }
        // Every frame, before the host asks where the windows are.
        self.move_windows();
        if !self.ending.is_empty() && self.ui.has_shut() {
            let ending = std::mem::take(&mut self.ending);
            self.perform(ending);
            self.redraw = true;
            self.ended = Some(Instant::now());
        }
        // Still here a while later (refused, or not --execute): the screen
        // opens again.
        if self
            .ended
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.ended = None;
            self.ui.open_up();
            self.redraw = true;
        }
        if self
            .last_tick
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_tick = Some(Instant::now());
        // Only a real session counts, so trying derisk out nested in
        // another desktop doesn't quicken the person's own.
        let in_use = self.execute && !self.lock.is_locked() && !self.shell.dimmed;
        self.familiarity.tick(Instant::now(), in_use);
        if let Some(machines) = &mut self.machines {
            machines.poll(&mut self.commands);
            self.shell.machines = machines
                .running()
                .map(|m| WindowMachine {
                    name: m.name.clone(),
                    label: m.label,
                })
                .collect();
            // Registered again each tick; the palette and the overview ask
            // their plugins again only when what they read has changed.
            let shown = match machines.card() {
                Some(card) => self
                    .shell
                    .widgets
                    .register(card, Some(SHELL))
                    .and_then(|()| {
                        self.shell.palette_sources.register(
                            machines::SOURCE.into(),
                            machines.palette(),
                            Some(SHELL),
                        )
                    }),
                None => {
                    let _ = self.shell.widgets.remove(machines::SOURCE, Some(SHELL));
                    let _ = self
                        .shell
                        .palette_sources
                        .remove(machines::SOURCE, Some(SHELL));
                    Ok(())
                }
            };
            if let Err(e) = shown {
                debug!(error = e, "machines are not in the palette and overview");
            }
        }
        self.shell.clock = Clock::now_local();
        self.shell.battery = Battery::read(Path::new("/sys/class/power_supply"));
        if let Some(settings) = self.settings.poll() {
            self.redraw = true;
            self.shell.effects = Effects::from_settings(&settings);
            self.shell.set_gaps(settings.desktop.gaps);
            self.shortcuts = settings.shortcuts;
            self.ui.shortcuts = settings.shortcuts;
            self.power = settings.power;
            self.wallpaper.configure(&settings.wallpaper);
            let assistant = &settings.assistant;
            self.model = assistant.use_model.then(|| model::Config {
                url: assistant.url.clone(),
                model: assistant.model.clone(),
            });
            self.shell.model = self.model.as_ref().map(|c| c.model.clone());
            self.lock_wallpaper
                .configure(&wallpaper::lock_screen(&settings.wallpaper));
            if settings.privacy != self.privacy {
                self.privacy = settings.privacy;
                self.last_sweep = None;
            }
            self.apply_theme();
        }
        if !self.privacy.remember_recent {
            self.ui.palette.history = Default::default();
        }
        self.sweep();
        self.sync_idle();
        if let Ok(to) = self.reports_to.try_recv() {
            if let Some(to) = &to {
                info!(to, "problems can be reported");
            }
            self.shell.can_report = to.is_some();
        }
        if self.execute {
            self.shell.failed_units = systemd::failed_units();
            // logind works the answer out from swap and memory, which is
            // more than a frame should wait on every second.
            if self
                .last_sleep_check
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
            {
                self.last_sleep_check = Some(Instant::now());
                self.shell.can_hibernate = systemd::can_hibernate();
            }
        }
        // Learned words reach the disk at most twice a minute.
        if self.ui.keyboard.predictor.is_dirty()
            && self.words_saved.elapsed() >= Duration::from_secs(30)
            && let Some(path) = &self.words
        {
            self.words_saved = Instant::now();
            if let Err(e) = self.ui.keyboard.predictor.save(path) {
                warn!("{}: {e}", path.display());
            }
        }
    }

    fn chrome_wants_pointer(&self, (x, y): (i32, i32)) -> bool {
        let inside = |g: mcsapi::Geometry| {
            x >= g.loc.x && y >= g.loc.y && x < g.loc.x + g.size.w && y < g.loc.y + g.size.h
        };
        self.lock.is_locked()
            || self.dialog_open()
            || self.shell.overview_visible()
            || self.shell.palette_visible()
            || self.shell.pending_confirmation().is_some()
            || self.shell.tutorial_visible()
            || self.shell.privacy_cover == Some(CoverArea::Screen)
            || self
                .ui
                .bar_rect()
                .is_some_and(|r| r.contains(egui::pos2(x as f32, y as f32)))
            // Below the work area on a phone: the navigation bar.
            || (self.shell.is_phone() && {
                let area = self.shell.work_area();
                y >= area.loc.y + area.size.h
            })
            // The on-screen keyboard floats over windows, so taps on it must
            // not fall through to the one underneath.
            || self.shell.keyboard_area().is_some_and(inside)
            || self.shell.snap_assist().is_some_and(|a| inside(a.frame))
            // A click on a notification card is the card's, not that of
            // the window under it, which would come forward and take focus.
            || self
                .ui
                .notification_rects()
                .iter()
                .any(|r| r.contains(egui::pos2(x as f32, y as f32)))
            || !self.startup_done()
    }

    fn pointer_down(&mut self, at: (i32, i32), time_ms: u64) -> Press {
        self.super_tap.cancel();
        if self.lock.is_locked() || self.dialog_open() {
            return Press::Handled;
        }
        match self.shell.pointer_down(at, time_ms) {
            Ok(PointerOutcome::Handled { effects }) => {
                self.perform(effects);
                Press::Handled
            }
            Ok(PointerOutcome::Client { .. } | PointerOutcome::Desktop) | Err(_) => Press::Client,
        }
    }

    fn pointer_motion(&mut self, at: (i32, i32)) {
        if !self.lock.is_locked() {
            self.shell.pointer_motion(at);
        }
    }

    fn pointer_up(&mut self) {
        if !self.lock.is_locked() {
            self.shell.pointer_up();
        }
    }

    fn key(&mut self, key: &KeyInput) -> KeyRoute {
        // Tracked before anything can return early, so a release behind
        // the lock screen still lets go of Super+drag.
        if matches!(key.sym, Keysym::Super_L | Keysym::Super_R) {
            self.shell.set_move_modifier(key.pressed);
        }
        // Every key goes to the lock screen's field: no shortcut, and no
        // client.
        if self.lock.is_locked() {
            self.super_tap.cancel();
            if key.pressed {
                self.wake_reader();
            }
            return KeyRoute::Chrome;
        }
        // So do polkit's dialog, so nothing typed into it reaches a window
        // or triggers a shortcut, and a choice, a report or a hung window's
        // card, so Enter answers it.
        if self.dialog_open() {
            self.super_tap.cancel();
            return KeyRoute::Chrome;
        }
        let is_super = matches!(key.sym, Keysym::Super_L | Keysym::Super_R);
        if self.super_tap.key(is_super, key.pressed) {
            // A tap while a sequence waits takes it back, rather than
            // opening the overview over a half-typed shortcut.
            if self.sequence.is_pending() {
                self.cancel_sequence();
            } else {
                self.dispatch(vec![Action::Overview { visible: None }]);
            }
            return KeyRoute::Consume;
        }
        if is_super {
            return KeyRoute::Consume;
        }
        if !key.pressed
            && let Some(i) = self.swallowed.iter().position(|s| *s == key.sym)
        {
            self.swallowed.swap_remove(i);
            return KeyRoute::Consume;
        }
        // The confirmation dialog takes every key until it is answered.
        if self.shell.pending_confirmation().is_some() {
            if key.pressed && key.sym == Keysym::Escape {
                self.dispatch(vec![Action::Confirm { accept: false }]);
                return KeyRoute::Consume;
            }
            return KeyRoute::Chrome;
        }
        // Super+B moves keyboard focus into the top bar; Tab and Shift+Tab
        // walk it, Escape hands the keyboard back to the window.
        if key.mods.logo && !key.mods.ctrl && !key.mods.alt && key.sym == Keysym::b {
            if key.pressed {
                self.ui.focus_top_bar();
            }
            return KeyRoute::Consume;
        }
        if key.pressed {
            let mods = Mods {
                logo: key.mods.logo,
                shift: key.mods.shift,
                ctrl: key.mods.ctrl,
                alt: key.mods.alt,
            };
            let step = match layout_key(key.sym) {
                Some(k) => self.sequence.press(&self.shortcuts, mods, k),
                // Shift, Ctrl and the like between two chords of a sequence
                // are how the next chord is typed, not the end of it.
                None if self.sequence.is_pending() => return KeyRoute::Consume,
                None => Step::Unbound,
            };
            if step != Step::Unbound {
                // A release lost to a focus change must not pile up.
                if self.swallowed.len() >= 8 {
                    self.swallowed.remove(0);
                }
                self.swallowed.push(key.sym);
                self.show_sequence();
            }
            match step {
                Step::Action(action) => {
                    self.dispatch(vec![action]);
                    return KeyRoute::Consume;
                }
                Step::Pending | Step::Cancelled => return KeyRoute::Consume,
                Step::Unbound => {}
            }
        }
        // The tour takes the keys: arrows and Enter step through it.
        if self.shell.tutorial_visible() {
            if key.pressed && key.sym == Keysym::Escape {
                self.dispatch(vec![Action::Tutorial {
                    visible: Some(false),
                }]);
                return KeyRoute::Consume;
            }
            return KeyRoute::Chrome;
        }
        if self.shell.shortcut_help_visible() {
            if key.pressed && key.sym == Keysym::Escape {
                self.dispatch(vec![Action::ShortcutHelp {
                    visible: Some(false),
                }]);
                return KeyRoute::Consume;
            }
            return KeyRoute::Chrome;
        }
        if self.shell.palette_visible() {
            if key.pressed && key.sym == Keysym::Escape {
                self.dispatch(vec![Action::Palette {
                    visible: Some(false),
                }]);
                return KeyRoute::Consume;
            }
            return KeyRoute::Chrome;
        }
        // With no window to type into, the search button has the focus (the
        // startup's dot lands there): typing opens the palette with it.
        if self.shell.focused().is_none()
            && !self.shell.overview_visible()
            && key.pressed
            && !key.mods.ctrl
            && !key.mods.alt
            && key.text.is_some_and(|c| !c.is_control())
        {
            self.dispatch(vec![Action::Palette {
                visible: Some(true),
            }]);
            return KeyRoute::Chrome;
        }
        if self.shell.overview_visible() {
            if key.pressed && key.sym == Keysym::Escape {
                self.dispatch(vec![Action::Overview {
                    visible: Some(false),
                }]);
                return KeyRoute::Consume;
            }
            return KeyRoute::Chrome;
        }
        KeyRoute::Client
    }

    fn client_request(&mut self, window: WindowId, request: ClientRequest) {
        if self.lock.is_locked() {
            return;
        }
        let window = Some(window.get());
        let action = match request {
            ClientRequest::Maximize => Action::ToggleMaximize { window },
            ClientRequest::Minimize => Action::Minimize { window },
            // Requests added to the compositor later are ignored until handled.
            _ => return,
        };
        self.dispatch(vec![action]);
    }

    /// The chrome asks egui for the frames its own motion needs; this adds
    /// the ones for what the tick polled, so a still desktop draws once a
    /// minute for the clock rather than every refresh.
    fn needs_frame(&mut self) -> bool {
        let stack = self.notifications.stack();
        let do_not_disturb = self.settings.current().notifications.do_not_disturb;
        let polled = Polled {
            clock: self.shell.clock,
            battery: self.shell.battery,
            failed_units: self.shell.failed_units.clone(),
            machines: self.shell.machines.clone(),
            can_hibernate: self.shell.can_hibernate,
            dimmed: self.shell.dimmed,
            locked: self.lock.is_locked(),
            cover: self.shell.privacy_cover,
            shown: self.shown.clone(),
            notifications: (
                stack.shown(do_not_disturb).map(|n| n.id).collect(),
                stack.len(),
            ),
        };
        let changed = self.polled.as_ref() != Some(&polled);
        self.polled = Some(polled);
        std::mem::take(&mut self.redraw) || changed
    }

    fn theme(&self) -> Theme {
        self.ui.theme
    }

    fn paint_background(&mut self, painter: &egui::Painter, screen: egui::Rect) {
        let look = self.shell.look();
        let seen = Visibility {
            // Tiled windows leave gaps between them; strips that thin aren't
            // worth decoding video for.
            covered: !self.shell.overview_visible()
                && wallpaper::covered(
                    inset(self.shell.work_area(), self.shell.profile().gap),
                    self.shell.placements().iter().map(|p| p.frame),
                ),
            low_power: look.low_power,
            reduce_motion: !look.animate,
        };
        self.wallpaper.paint(painter, screen, &self.ui.theme, seen);
    }

    fn paint_decoration(&mut self, painter: &egui::Painter, placement: &Placement) {
        if let Some(p) = self
            .shell
            .placements()
            .into_iter()
            .find(|p| p.window == placement.window)
        {
            self.ui.paint_decoration(painter, &self.shell, &p);
        }
    }

    fn wants_window_pictures(&self) -> bool {
        // The overview's cards show what each window holds.
        self.shell.overview_visible() && !self.lock.is_locked()
    }

    fn window_pictures(&mut self, pictures: Option<compositor::WindowPictures>) {
        self.ui.window_pictures = pictures.map(|p| WindowPictures {
            texture: p.texture,
            uv: Box::new(move |area| p.uv(area)),
        });
    }

    fn chrome(&mut self, ui: &mut egui::Ui, elapsed_ms: u32) {
        install_fonts(ui.ctx(), &self.fonts);
        if self.lock.is_locked() {
            self.poll_unlock();
        }
        if self.lock.is_locked() {
            let theme = self.ui.theme;
            match show_lock(
                ui,
                &mut self.lock,
                &self.shell,
                &theme,
                &self.user,
                &mut self.lock_wallpaper,
                self.execute && Path::new(systemd::SWITCH_USER_SOCKET).exists(),
            ) {
                LockInput::Submit => {
                    if let Some(request) = self.lock.login.submit() {
                        self.send_auth(request);
                    }
                }
                LockInput::SwitchUser => switch_user(),
                LockInput::None => {}
            }
            self.ui.show_privacy_cover(ui, &self.shell);
            return;
        }
        let actions = self.ui.show(ui, &self.shell, elapsed_ms);
        self.dispatch(actions);
        // Dragging the focused window's title in the top bar moves the
        // window, as dragging its title bar did. The press is the bar's, so
        // the drag arrives from egui rather than the compositor's pointer.
        for drag in self.ui.take_title_drags() {
            let at = (drag.at.x.round() as i32, drag.at.y.round() as i32);
            match drag.phase {
                TitleDragPhase::Start => self.shell.grab_title(at),
                TitleDragPhase::Move => {
                    self.shell.pointer_motion(at);
                }
                TitleDragPhase::End => {
                    self.shell.pointer_up();
                }
            }
        }
        self.show_notifications(ui);
        self.show_polkit(ui);
        if self.polkit.is_empty() {
            self.show_choice(ui);
        }
        if self.polkit.is_empty() && self.choices.is_empty() {
            self.show_report(ui);
        }
        self.ui.show_privacy_cover(ui, &self.shell);
        // What the on-screen keyboard typed for windows goes to the focused
        // one. It goes through the same synthetic-input path agents use, so
        // the keyboard can't confirm what only a direct tap may (the
        // power-off dialog).
        for typed in self.ui.take_window_input() {
            let key = |sym| Input::Key {
                sym,
                mods: Modifiers::default(),
            };
            self.commands.push(Command::Input(match typed {
                Typed::Text(text) => Input::Text(text),
                Typed::Backspace => key(Keysym::BackSpace),
                Typed::Enter => key(Keysym::Return),
            }));
        }
        // Requests typed in the palette; their progress shows in its
        // conversation.
        for ask in self.ui.take_asks() {
            // What the rules don't follow goes to the model, when there is
            // one; the rules' "I don't know how" otherwise.
            if let Some(config) = self.model.clone()
                && derisk::assistant::interpret(&ask.text).is_err()
            {
                self.ask_model(&ask.text, config);
                continue;
            }
            let outcome = self.shell.ask(&ask.text, Source::User, ask.confirmed);
            self.perform(outcome.effects);
            if let Err(e) = outcome.result {
                info!("request failed: {e}");
            }
        }
        // Allowing a step the model asked about, or stopping it, only
        // counts from the person: an agent's injected click on Allow would
        // let the model approve itself.
        let (answers, stop) = self.ui.take_model_answers();
        if let Some(asking) = &self.asking
            && !self.shell.untrusted()
        {
            if stop {
                asking.stop();
            }
            for allow in answers {
                asking.answer(allow);
            }
        }
        // A window picked in the overview grows out of its card. Worked out
        // again now, so it is never drawn at its frame first.
        if let Some((window, card)) = self.ui.take_picked() {
            let card = rect(
                card.left().round() as i32,
                card.top().round() as i32,
                card.width().round() as i32,
                card.height().round() as i32,
            );
            self.window_motion.grow(window, card, self.motion_now());
            self.move_windows();
        }
        self.palette_opened();
    }

    fn blur_regions(&self) -> Vec<Blur> {
        if self.lock.is_locked() {
            return Vec::new();
        }
        self.ui
            .blur_regions()
            .iter()
            .map(|b| Blur {
                area: b.area,
                corner_radius: b.corner_radius,
                strength: b.strength,
            })
            .collect()
    }

    fn frame_interval(&self, timing: &OutputTiming) -> Duration {
        let look = self.shell.look();
        let timing = OutputTiming {
            vrr: look.vrr.unwrap_or(timing.vrr),
            ..*timing
        };
        match look.min_fps {
            Some(fps) => timing.interval_at_least(fps),
            None => timing.refresh_interval(),
        }
    }

    fn spawn_argv(&mut self, app: &str) -> Vec<String> {
        if !systemd::is_launchable(app) {
            warn!("refusing to launch {app:?}");
            return Vec::new();
        }
        // An installed app's desktop file ID runs its `Exec`; any other name
        // runs as a plain command.
        let command = match self.installed_entry(app) {
            Some(entry) => entry.argv(),
            None => vec![app.strip_suffix(".desktop").unwrap_or(app).to_owned()],
        };
        if command.is_empty() {
            warn!("invalid Exec for {app:?}");
            return Vec::new();
        }
        self.launches += 1;
        if self.execute
            && let Some(mut argv) =
                systemd::launch_command_argv(app, systemd::instance(self.launches), &command)
        {
            // systemd-run takes its options before the `--` and the command.
            let split = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
            argv.insert(
                split,
                format!("--setenv=WAYLAND_DISPLAY={}", self.wayland_display),
            );
            return argv;
        }
        command
    }

    fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    /// Nothing while locked: window titles would show through the lock.
    fn access_subtrees(&mut self) -> Vec<Subtree> {
        if self.lock.is_locked() {
            return Vec::new();
        }
        self.registered.values().map(|(_, s)| s.clone()).collect()
    }

    fn access_action(
        &mut self,
        window: WindowId,
        node: NodeId,
        action: accesskit::Action,
        value: Option<&str>,
    ) {
        if self.lock.is_locked() {
            return;
        }
        // A node a program registered: tell that program.
        if let Some((conn, _)) = self.registered.get(&window)
            && let Some(events) = self.listeners.get(conn)
        {
            let event = computer::action_event(window, node, action, value);
            let _ = events.send(event.to_string());
        }
    }

    fn described(&mut self, request: u64, tree: Snapshot) {
        let Some((reply, query)) = self.trees.remove(&request) else {
            return;
        };
        let result = match query {
            TreeQuery::Tree { window } => computer::tree_json(&tree, window),
            TreeQuery::Find { role, name, window } => {
                computer::find_json(&tree, role.as_deref(), name.as_deref(), window)
            }
        };
        let _ = reply.send(json!({"ok": true, "result": result}).to_string());
    }

    fn captured(&mut self, request: u64, frame: std::result::Result<Capture, String>) {
        if let Some(i) = self.pictures.0.iter().position(|r| *r == request) {
            self.pictures.0.swap_remove(i);
            let clock = Clock::now_local();
            let stamp = format!(
                "{}-{:02}-{:02} {:02}-{:02}",
                clock.year, clock.month, clock.day, clock.hour, clock.minute
            );
            let (tx, rx) = mpsc::channel();
            self.pictures.1.push(rx);
            // Encoding takes a while; keep it off the compositor thread.
            std::thread::spawn(move || {
                let saved = frame.and_then(|capture| {
                    let png = computer::png(&capture)?;
                    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
                    let dir = PathBuf::from(home).join("Pictures").join("Screenshots");
                    computer::save_picture(&dir, &stamp, &png).map_err(|e| e.to_string())
                });
                let _ = tx.send(saved);
            });
            return;
        }
        let Some((reply, inline)) = self.shots.remove(&request) else {
            return;
        };
        // Encoding takes a while; keep it off the compositor thread.
        std::thread::spawn(move || {
            let result = frame.and_then(|capture| {
                let png = computer::png(&capture)?;
                let dir = computer::screenshot_dir()
                    .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_owned())?;
                let path = computer::save_screenshot(&dir, request, &png)
                    .map_err(|e| format!("cannot save the screenshot: {e}"))?;
                let mut result = json!({
                    "path": path,
                    "width": capture.width,
                    "height": capture.height,
                });
                if inline {
                    result["png_base64"] = json!(computer::base64(&png));
                }
                Ok(result)
            });
            let line = match result {
                Ok(result) => json!({"ok": true, "result": result}),
                Err(error) => json!({"ok": false, "error": error}),
            };
            let _ = reply.send(line.to_string());
        });
    }

    fn input_source(&mut self, synthetic: bool) {
        self.shell.set_synthetic_input(synthetic);
    }

    /// Dialogs float centred on the window they belong to, and a modal one
    /// keeps its parent's input until it closes.
    fn window_hint(&mut self, window: WindowId, hint: WindowHint) {
        let result = match hint {
            WindowHint::Parent(parent) => self.shell.set_parent(window, parent),
            WindowHint::Modal(modal) => self.shell.set_modal(window, modal),
            WindowHint::Size(size) => self.shell.fit_floating(window, size),
            _ => Ok(()),
        };
        if let Err(e) = result {
            warn!("window {window}: {e}");
        }
    }

    /// The bell asks for attention on a window the person is not looking
    /// at; in the one they are, they already heard it, or see its cause.
    fn bell(&mut self, window: Option<WindowId>) {
        if let Some(window) = window {
            let _ = self.shell.set_attention(window, true);
        }
    }

    /// A window asked to come forward with a token from something the
    /// person did. It does, unless the screen is locked or a dialog of the
    /// shell's own has the keyboard (the palette, polkit's prompt), where
    /// taking it away would lose what is being typed: it asks for
    /// attention instead.
    ///
    /// A window from an untrusted machine always only asks. The token
    /// proves that something in that machine was clicked, and the machine
    /// can click itself: a window that could bring itself forward could
    /// cover the one being typed into and take what is typed next. The
    /// person brings it forward themselves, from the top bar or the palette.
    fn activate(&mut self, window: WindowId) {
        let busy = self.lock.is_locked()
            || self.locked_elsewhere
            || self.shell.palette_visible()
            || !self.polkit.is_empty()
            || self.shell.is_untrusted(window);
        if busy {
            let _ = self.shell.set_attention(window, true);
        } else {
            self.dispatch(vec![Action::Focus {
                window: window.get(),
            }]);
        }
    }

    /// Shown in the top bar, so a screen that does not dim has a reason.
    fn idle_inhibited(&mut self, inhibited: bool) {
        self.shell.kept_awake = inhibited;
    }

    /// Another screen locker took the session, or gave it back. logind is
    /// told, as for derisk's own lock, and derisk does not lock beneath it.
    fn session_locked(&mut self, locked: bool) {
        self.locked_elsewhere = locked;
        self.shell.dimmed = false;
        if !self.lock.is_locked() {
            self.set_locked_hint(locked);
        }
        info!(
            "session {} by another screen locker",
            if locked { "locked" } else { "unlocked" }
        );
    }

    /// The open workspaces, numbered as the shell numbers them, for pagers
    /// such as waybar's.
    fn workspaces(&self) -> Vec<WorkspaceInfo> {
        let active = self.shell.active_workspace();
        self.shell
            .workspaces()
            .iter()
            .filter_map(|ws| {
                let n = self.shell.workspace_number(*ws)?;
                let mut info = WorkspaceInfo::new(n.to_string(), n.to_string(), n == active);
                info.urgent = self.shell.workspace_wants_attention(*ws);
                Some(info)
            })
            .collect()
    }

    fn activate_workspace(&mut self, id: &str) {
        if let Ok(workspace) = id.parse() {
            self.dispatch(vec![Action::SwitchWorkspace { workspace }]);
        }
    }

    /// A GTK or Qt app's text field took or lost focus. On a touchscreen the
    /// keyboard comes up for it, the way a phone's does; with a pointer the
    /// hardware keyboard is the one in use, so it only stops learning words
    /// typed into password fields.
    fn text_input(&mut self, field: Option<TextField>) {
        self.ui
            .keyboard
            .set_learning(!field.is_some_and(|f| f.password));
        if !self.shell.is_phone() && !self.shell.profile().touch {
            return;
        }
        let show = match field {
            Some(_) if !self.shell.keyboard_visible() => {
                self.keyboard_for_field = true;
                true
            }
            None if self.keyboard_for_field => {
                self.keyboard_for_field = false;
                false
            }
            _ => return,
        };
        self.dispatch(vec![Action::Keyboard {
            visible: Some(show),
        }]);
    }
}

/// The derisk core apps, as in-process windows.
struct CoreApps {
    session: derisk_apps::Session,
    pending_actions: PendingActions,
    phone: Arc<AtomicBool>,
    fonts: Arc<Mutex<mcsapi_ui::theme::Fonts>>,
}

impl Apps for CoreApps {
    /// Accepts app IDs (`org.derisk.files`), desktop file IDs
    /// (`org.derisk.files.desktop`), their last segment (`files`), and
    /// display names (`text editor`, `texteditor`).
    fn resolve(&self, name: &str) -> Option<AppId> {
        let name = name.trim().to_lowercase();
        let name = name.strip_suffix(".desktop").unwrap_or(&name).to_owned();
        derisk_apps::APPS
            .iter()
            .find(|app| {
                let display = app.name.to_lowercase();
                app.id == name
                    || app.id.rsplit('.').next() == Some(name.as_str())
                    || display == name
                    || display.replace(' ', "") == name
            })
            .map(|app| app.app_id())
    }

    /// Launches `app`, through its desktop action if one was queued for it.
    fn launch(&mut self, app: &AppId) -> std::result::Result<InstanceId, mcsapi_runtime::Error> {
        let action = self.pending_actions.lock().ok().and_then(|mut pending| {
            let i = pending.iter().position(|(id, _)| id == app.as_str())?;
            Some(pending.remove(i).1)
        });
        match action {
            Some(Pending::Action(action)) => self.session.launch_action(app, &action),
            Some(Pending::Open(path)) => self.session.launch_path(app, &path),
            None => self.session.launch(app),
        }
    }

    fn stop(&mut self, instance: InstanceId) {
        let _ = self.session.stop(instance);
    }

    fn app_mut(&mut self, instance: InstanceId) -> Option<&mut dyn mcsapi_compositor::App> {
        self.session
            .app_mut(instance)
            .map(|app| app as &mut dyn mcsapi_compositor::App)
    }

    fn prepare(&mut self, ctx: &egui::Context, theme: &Theme) {
        ctx.set_visuals(derisk_apps::visuals(theme));
        set_touch_style(ctx, self.phone.load(Ordering::Relaxed));
        install_fonts(ctx, &self.fonts);
    }
}

/// Draws `ctx` in the theme's font families, which the chrome and the core
/// apps share.
fn install_fonts(ctx: &egui::Context, fonts: &Mutex<mcsapi_ui::theme::Fonts>) {
    let fonts = fonts.lock().unwrap_or_else(|e| e.into_inner()).clone();
    mcsapi_components::install_fonts(ctx, &fonts);
}

/// Whether to open Settings on its Default apps page at login: the OS's
/// policy has turned a choice screen on and nothing is chosen on it yet.
/// It comes back every login until there is a choice, as a choice screen
/// should; it is a window like any other, and closing it skips it for now.
fn choice_needed() -> bool {
    let settings = derisk_settings::default_path()
        .and_then(|path| derisk_settings::Settings::load(&path).ok())
        .map(|(settings, _)| settings)
        .unwrap_or_default();
    derisk_settings::choice::Policy::load().unanswered(&settings.defaults)
}

/// Runs the session until the window is closed.
pub fn run(options: Options) -> Result {
    let pending_actions = PendingActions::default();
    let phone = Arc::new(AtomicBool::new(false));
    let fonts = Arc::default();
    let session = Session::new(
        &options,
        pending_actions.clone(),
        phone.clone(),
        Arc::clone(&fonts),
    );
    let lock_path = session.session_path.clone();
    if options.execute {
        let _ = systemd::run(&systemd::stop_headless_agent_argv());
    }
    let apps = CoreApps {
        session: derisk_apps::Session::new()?,
        pending_actions: pending_actions.clone(),
        phone,
        fonts,
    };
    let (w, h) = options.size;
    let mut compositor = Compositor::new(session)
        .title("derisk")
        .size(w, h)
        .apps(apps);
    // One remote for everything: each call to `remote()` replaces the
    // compositor's job channel, which would disconnect the earlier handles.
    let remote = compositor.remote();
    if let Some(path) = agent_socket(options.socket, remote.clone())? {
        info!("agent protocol on {}", path.display());
    }
    // Ask derisk's language model runs its tools as an agent connection of
    // its own.
    {
        let conn = CONNECTIONS.fetch_add(1, Ordering::Relaxed);
        let for_model = remote.clone();
        remote.run(move |session: &mut Session| {
            session.asking = ask::Asking::start(for_model, conn);
        });
    }
    // Only the real session: a nested one would take its parent's place
    // as the logind session's agent.
    if options.execute
        && let Some(session_id) = systemd::session_id()
    {
        let user = pam::current_user().unwrap_or_default();
        polkit_agent::start(session_id, user, remote.clone());
    }
    // Only the real session as well: a nested one shares its parent's
    // session bus, where the parent (or the person's own server) already
    // owns the name.
    if options.execute {
        notifications::start(remote.clone());
    }
    let lock_watch = lock_path.map(|path| LockWatch::start(path, remote));
    for app in options.launch {
        compositor = compositor.launch(app);
    }
    if choice_needed() {
        if let Ok(mut pending) = pending_actions.lock() {
            pending.push((
                derisk_apps::SETTINGS.to_owned(),
                Pending::Action("default-apps".to_owned()),
            ));
        }
        compositor = compositor.launch(derisk_apps::SETTINGS);
    }
    for client in options.runtime {
        compositor = compositor.runtime(client);
    }
    let result = std::thread::scope(|scope| {
        // The palette's and the overview's plugins compile while the
        // session comes up, so neither's first opening waits for them.
        scope.spawn(palette::preload);
        scope.spawn(widgets::preload);
        compositor.run()
    });
    if let Some(watch) = lock_watch {
        watch.stop();
    }
    result?;
    systemd::notify_stopping();
    Ok(())
}

/// Feeds `login` what its conversation answered and sends what it asks
/// next. A conversation whose process is gone is dropped, and `login` told,
/// so its next request starts a fresh one.
fn drive(conversation: &mut Option<unlock::Conversation>, login: &mut Login) {
    let Some(conv) = conversation.as_ref() else {
        return;
    };
    while let Some(answer) = conv.poll() {
        match answer {
            Ok(response) => {
                if let Some(next) = login.respond(response)
                    && !conv.send(next)
                {
                    login.disconnected("derisk auth stopped");
                    *conversation = None;
                    return;
                }
            }
            Err(e) => {
                login.disconnected(&e.to_string());
                *conversation = None;
                return;
            }
        }
    }
}

/// Locks the screen whenever logind sends this session `Lock`, until the
/// session ends or `busctl wait` stops working.
const LOCK_WATCH_RETRY_MIN: Duration = Duration::from_secs(1);
const LOCK_WATCH_RETRY_MAX: Duration = Duration::from_secs(30);

/// Sleeps for `delay`, waking early when the watch is stopped. Returns
/// whether the watch should go on.
fn sleep_unless_stopped(delay: Duration, stopping: &AtomicBool) -> bool {
    let until = Instant::now() + delay;
    while Instant::now() < until {
        if stopping.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !stopping.load(Ordering::SeqCst)
}

/// `derisk-faces watch` running for the privacy display, its counts read on
/// a thread of their own into `reports`.
struct FacesWatch {
    child: Child,
    reports: mpsc::Receiver<FacesReport>,
}

impl FacesWatch {
    fn start() -> Option<Self> {
        let [program, command] = privacy_display::HELPER;
        // Beside derisk's own executable first, as derisk-gpui is found, so
        // a session started without the system's PATH still finds it.
        let beside = std::env::current_exe()
            .ok()
            .map(|exe| exe.with_file_name(program))
            .filter(|path| path.is_file());
        let mut child = ProcessCommand::new(beside.as_deref().unwrap_or(program.as_ref()))
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .inspect_err(|e| warn!("{program}: {e}: the privacy display cannot see"))
            .ok()?;
        let stdout = child.stdout.take()?;
        // A few counts a second; when the session has not taken the last
        // ones, a newer count replaces nothing and is dropped rather than
        // holding the camera's reader up.
        let (send, reports) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if let Some(report) = FacesReport::parse(&line)
                    && let Err(mpsc::TrySendError::Disconnected(_)) = send.try_send(report)
                {
                    return;
                }
            }
        });
        Some(Self { child, reports })
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for FacesWatch {
    /// Stopping the helper closes the camera; the reader thread ends at the
    /// end of its output.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// swayidle running for the session, its lines read on a thread of their
/// own into `events`.
struct IdleWatch {
    child: Child,
    /// Where [`idle::SLEEP_DONE`] goes.
    stdin: ChildStdin,
    events: mpsc::Receiver<IdleEvent>,
    /// Sleep waits for the lock screen, which the next frame draws.
    sleep_waiting: bool,
    /// The settings its timeouts come from.
    power: Power,
}

impl IdleWatch {
    fn start(power: Power, wayland_display: &str) -> Option<Self> {
        let argv = idle::argv(&power);
        let mut child = ProcessCommand::new(&argv[0])
            .args(&argv[1..])
            .env("WAYLAND_DISPLAY", wayland_display)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .inspect_err(|e| warn!("swayidle: {e}"))
            .ok()?;
        let stdout = child.stdout.take()?;
        let stdin = child.stdin.take()?;
        // A handful of lines a minute at most; a full queue means the
        // session stopped reading, and the thread stops with it.
        let (send, events) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if let Some(event) = IdleEvent::parse(&line)
                    && send.send(event).is_err()
                {
                    return;
                }
            }
        });
        Some(Self {
            child,
            stdin,
            events,
            sleep_waiting: false,
            power,
        })
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for IdleWatch {
    /// The reader thread ends at the end of swayidle's output.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct LockWatch {
    child: Arc<Mutex<Option<Child>>>,
    stopping: Arc<AtomicBool>,
}

impl LockWatch {
    fn start(path: String, remote: Remote<Session>) -> Self {
        let child = Arc::new(Mutex::new(None::<Child>));
        let stopping = Arc::new(AtomicBool::new(false));
        let watch = Self {
            child: child.clone(),
            stopping: stopping.clone(),
        };
        std::thread::spawn(move || {
            let argv = systemd::lock_signal_argv(&path);
            let mut backoff = LOCK_WATCH_RETRY_MIN;
            loop {
                match wait_for_lock(&argv, &child, &stopping) {
                    Ok(true) => {
                        backoff = LOCK_WATCH_RETRY_MIN;
                        if !remote.run(Session::lock_screen) {
                            return;
                        }
                    }
                    Ok(false) => return,
                    // A bus restart or a missing busctl must not turn lock
                    // requests off for the rest of the session: keep trying,
                    // more slowly, until the session ends.
                    Err(e) => {
                        warn!(
                            "lost logind lock requests ({e}); retrying in {}s",
                            backoff.as_secs()
                        );
                        if !sleep_unless_stopped(backoff, &stopping) {
                            return;
                        }
                        backoff = (backoff * 2).min(LOCK_WATCH_RETRY_MAX);
                    }
                }
            }
        });
        watch
    }

    /// Ends the watch and the `busctl` it is waiting in, so a session that
    /// closes leaves no process behind.
    fn stop(self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Runs one `busctl wait`. Returns whether the signal arrived, or `false`
/// when the watch was stopped.
fn wait_for_lock(
    argv: &[String],
    slot: &Mutex<Option<Child>>,
    stopping: &AtomicBool,
) -> std::io::Result<bool> {
    {
        let mut slot = slot.lock().map_err(|_| std::io::Error::other("poisoned"))?;
        if stopping.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let (program, args) = argv.split_first().ok_or(std::io::ErrorKind::InvalidInput)?;
        *slot = Some(
            ProcessCommand::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?,
        );
    }
    // The child stays in the slot, where stop() can kill it; poll it rather
    // than block in wait() while holding the lock.
    loop {
        let mut slot = slot.lock().map_err(|_| std::io::Error::other("poisoned"))?;
        let Some(child) = slot.as_mut() else {
            return Ok(false);
        };
        if let Some(status) = child.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
            }
            *slot = None;
            if stopping.load(Ordering::SeqCst) {
                return Ok(false);
            }
            if status.success() {
                return Ok(true);
            }
            return Err(std::io::Error::other(stderr.trim().to_owned()));
        }
        drop(slot);
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Binds the agent socket and serves each connection on its own thread,
/// running requests on the live shell through `remote`.
fn agent_socket(path: Option<PathBuf>, remote: Remote<Session>) -> Result<Option<PathBuf>> {
    let Some(path) = path.or_else(|| {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(|dir| PathBuf::from(dir).join("derisk").join("agent.sock"))
    }) else {
        return Ok(None);
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        ipc::check_socket_dir(dir)?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            return Err(format!("{} exists and is not a socket", path.display()).into());
        }
        // Refuse to take over a live session's socket; remove a stale one.
        match UnixStream::connect(&path) {
            Ok(_) => return Err(format!("{} is already in use", path.display()).into()),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let remote = remote.clone();
            std::thread::spawn(move || serve_agent(stream, &remote));
        }
    });
    Ok(Some(path))
}

/// Numbers agent connections, so registered trees and menus know their
/// owner.
static CONNECTIONS: AtomicU64 = AtomicU64::new(0);

/// The owner of what the session registers itself, such as its machines'
/// palette rows and overview card. Counting from 0, no connection reaches
/// it, so no program can replace them or hear what is picked in them.
const SHELL: u64 = u64::MAX;

fn serve_agent(stream: UnixStream, remote: &Remote<Session>) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let conn = CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    let writer = Arc::new(Mutex::new(writer));
    // Events for trees and menus this connection registered arrive between
    // responses.
    let (events, event_lines) = mpsc::channel::<String>();
    {
        let writer = writer.clone();
        std::thread::spawn(move || {
            for line in event_lines {
                let Ok(mut w) = writer.lock() else { return };
                if writeln!(w, "{line}").is_err() {
                    return;
                }
            }
        });
    }
    let mut reader = BufReader::new(stream);
    while let Ok(Some(line)) = ipc::read_request(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let (reply, response) = mpsc::channel();
        let events = events.clone();
        let queued = remote.run(move |session: &mut Session| {
            // A locked desktop answers nothing: no window titles, no
            // launches, no actions.
            if session.lock.is_locked() {
                let _ = reply.send(
                    serde_json::json!({"ok": false, "error": "the session is locked"}).to_string(),
                );
                return;
            }
            session.agent_line(conn, &line, reply, &events);
        });
        if !queued {
            return;
        }
        let Ok(response) = response.recv() else {
            break;
        };
        let Ok(mut w) = writer.lock() else { break };
        if writeln!(w, "{response}").is_err() {
            break;
        }
    }
    remote.run(move |session: &mut Session| session.agent_disconnected(conn));
}

/// Layout-independent key for shortcuts, from the unmodified keysym.
fn layout_key(sym: Keysym) -> Option<Key> {
    let other = Key::Other;
    Some(match sym {
        Keysym::Left => Key::Arrow(Direction::Left),
        Keysym::Right => Key::Arrow(Direction::Right),
        Keysym::Up => Key::Arrow(Direction::Up),
        Keysym::Down => Key::Arrow(Direction::Down),
        Keysym::Return | Keysym::KP_Enter | Keysym::Linefeed => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::Escape => Key::Escape,
        Keysym::space => Key::Space,
        Keysym::BackSpace => other(KeyName::Backspace),
        Keysym::Delete | Keysym::KP_Delete => other(KeyName::Delete),
        Keysym::Home => other(KeyName::Home),
        Keysym::End => other(KeyName::End),
        Keysym::Prior => other(KeyName::PageUp),
        Keysym::Next => other(KeyName::PageDown),
        Keysym::Print | Keysym::Sys_Req => other(KeyName::Print),
        _ if (Keysym::F1.raw()..=Keysym::F12.raw()).contains(&sym.raw()) => {
            other(KeyName::Function((sym.raw() - Keysym::F1.raw() + 1) as u8))
        }
        _ => {
            let c = sym.key_char()?.to_ascii_lowercase();
            match c {
                '0'..='9' => Key::Digit(c as u8 - b'0'),
                'a'..='z' => Key::Letter(c),
                // Shifted, these are `?`, `<`, `>`, `_` and `+` on US
                // layouts; the chord names the key, so map them back.
                '/' | '?' => other(KeyName::Slash),
                ',' | '<' => other(KeyName::Comma),
                '.' | '>' => other(KeyName::Period),
                '-' | '_' => other(KeyName::Minus),
                '=' | '+' => other(KeyName::Equal),
                _ => return None,
            }
        }
    })
}

/// Which edges of a window touch a neighbour or the screen edge.
fn tiled_edges(mode: Mode) -> Edges {
    let edges = |left, right, top, bottom| Edges {
        left,
        right,
        top,
        bottom,
    };
    match mode {
        Mode::Tiled => Edges::ALL,
        Mode::Floating { .. } => Edges::NONE,
        Mode::Snapped { zone } => match zone {
            SnapZone::Left => edges(true, false, true, true),
            SnapZone::Right => edges(false, true, true, true),
            SnapZone::TopLeft => edges(true, false, true, false),
            SnapZone::TopRight => edges(false, true, true, false),
            SnapZone::BottomLeft => edges(true, false, false, true),
            SnapZone::BottomRight => edges(false, true, false, true),
            SnapZone::Maximize => Edges::NONE,
        },
    }
}

/// Ends a window's process that stopped responding, with SIGKILL: it is
/// not running its event loop, so it would not act on a gentler signal
/// either. Through kill(1), which keeps unsafe code to `pam`.
fn force_quit(pid: u32) {
    let pid = pid.to_string();
    match ProcessCommand::new("kill")
        .args(["-s", "KILL", "--", &pid])
        .status()
    {
        Ok(status) if status.success() => info!(pid, "force quit"),
        Ok(status) => warn!(pid, %status, "kill refused"),
        Err(e) => warn!(pid, error = %e, "cannot run kill"),
    }
}

/// Asks the display manager for the login screen, so someone else can log
/// in while this session keeps running behind its lock screen.
fn switch_user() {
    match systemd::request_switch_user() {
        Ok(()) => info!("asked for the login screen"),
        Err(e) => warn!("cannot ask the display manager to switch user: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_sleep_ends_early_when_stopped() {
        let stopping = AtomicBool::new(true);
        let start = Instant::now();
        assert!(!sleep_unless_stopped(Duration::from_secs(30), &stopping));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn retry_sleep_goes_on_when_not_stopped() {
        let stopping = AtomicBool::new(false);
        assert!(sleep_unless_stopped(Duration::from_millis(150), &stopping));
    }

    #[test]
    fn runtime_roles_parse() {
        assert_eq!(parse_role("app"), Ok(Role::App));
        assert_eq!(parse_role("overlay"), Ok(Role::Overlay));
        assert_eq!(
            parse_role("panel:bottom:48"),
            Ok(Role::Panel {
                edge: compositor::Edge::Bottom,
                size: 48,
                keyboard: false,
            })
        );
        assert_eq!(
            parse_role("panel:left:64:keyboard"),
            Ok(Role::Panel {
                edge: compositor::Edge::Left,
                size: 64,
                keyboard: true,
            })
        );
        for bad in [
            "",
            "panel",
            "panel:top",
            "panel:up:32",
            "panel:top:0",
            "overlay:x",
            "app:top:3",
        ] {
            assert!(parse_role(bad).is_err(), "{bad}");
        }
    }
}
