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
    conversation::Source,
    decorations::Button,
    desktop::{self, DesktopEntry},
    effects::{Effects, SettingsWatch},
    geom::{inset, rect},
    greetd::{Login, Request},
    idle::{self, Event as IdleEvent},
    ipc::{self, LiveRequest},
    keyboard::{Output as Typed, Predictor},
    keys::{self, Key, Mods, SuperTap},
    lock::LockScreen,
    overview::Battery,
    palette, privacy,
    shell::{Mode, PointerOutcome, Shell, WindowMachine},
    snap::{Direction, SnapZone},
    systemd::{self, SessionOp},
    time::Clock,
    ui::{
        ChoiceInput, PolkitInput, ShellUi, WindowPictures, set_touch_style, show_choice, show_lock,
        show_polkit,
    },
    wallpaper::{self, Visibility, WallpaperPainter},
    widgets,
    window_motion::{Shown, WindowMotion},
};
use derisk_settings::{Power, Privacy, Shortcuts};
use mcsapi::WindowId;
use tracing::{debug, info, warn};

use crate::{
    machines::{self, MachineDisplays},
    notifications, pam, polkit_agent, power, unlock,
};
use mcsapi_compositor::{
    self as compositor, AppId, Apps, Blur, Capture, ClientRequest, Command, Compositor, Edges,
    Input, InstanceId, KeyInput, KeyRoute, Keysym, Modifiers, Motion, OutputTiming, Placement,
    Press, Remote, Reserved, Role, RuntimeClient, TextField, Theme, WindowHint, WorkspaceInfo,
    a11y::{Origin, Snapshot, Subtree},
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

/// The derisk shell as seen by the compositor.
pub struct Session {
    shell: Shell,
    ui: ShellUi,
    execute: bool,
    wayland_display: String,
    launches: u64,
    commands: Vec<Command>,
    super_tap: SuperTap,
    start: Instant,
    /// A power off, reboot or log out waiting for the screen to shut, and
    /// when it was carried out.
    ending: Vec<Effect>,
    ended: Option<Instant>,
    /// Windows opening, gliding and snapping, and where each is drawn this
    /// frame while it does.
    window_motion: WindowMotion,
    shown: HashMap<WindowId, Shown>,
    last_tick: Option<Instant>,
    last_sleep_check: Option<Instant>,
    palette_open: bool,
    index: Option<mpsc::Receiver<Index>>,
    core_apps: Vec<DesktopEntry>,
    installed: Vec<DesktopEntry>,
    pending_actions: PendingActions,
    settings: SettingsWatch,
    /// Whether the output is phone-sized, shared with [`CoreApps`] so the
    /// apps get touch-sized widgets too.
    phone: Arc<AtomicBool>,
    /// Where the on-screen keyboard keeps the words it learned, and when it
    /// last wrote them.
    words: Option<PathBuf>,
    words_saved: Instant,
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
    /// Apps' notifications, which `crate::notifications` fills.
    notifications: notifications::Center,
}

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

/// Node IDs of the title-bar buttons the shell paints, in the space above
/// [`computer::REGISTERED_ID_LIMIT`] so they never meet registered nodes.
const TITLE_BAR_NODES: u64 = u64::MAX - 8;

/// Core-app actions waiting for the compositor to launch their app, shared
/// between [`Session`] (which queues them) and [`CoreApps`] (which takes
/// them when the launch arrives).
type PendingActions = Arc<Mutex<Vec<(String, String)>>>;

/// What the palette indexes in the background: files under home and the
/// installed applications.
type Index = (Vec<palette::File>, Vec<DesktopEntry>);

/// The core apps' own `.desktop` files, parsed.
fn core_desktop_entries() -> Vec<DesktopEntry> {
    derisk_apps::APPS
        .iter()
        .filter_map(|app| DesktopEntry::parse(&app.desktop_id(), app.desktop_file))
        .collect()
}

/// Installed applications except hidden ones and copies of the core apps.
fn installed_apps() -> Vec<DesktopEntry> {
    desktop::scan(&desktop::application_dirs())
        .into_iter()
        .filter(|e| !e.no_display && derisk_apps::find(&e.id).is_none())
        .collect()
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
        self.start.elapsed().as_secs_f64() * 1000.0
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

    fn new(options: &Options, pending_actions: PendingActions, phone: Arc<AtomicBool>) -> Self {
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
            start: Instant::now(),
            ending: Vec::new(),
            ended: None,
            window_motion: WindowMotion::default(),
            shown: HashMap::new(),
            last_tick: None,
            last_sleep_check: None,
            palette_open: false,
            index: None,
            core_apps,
            installed,
            pending_actions,
            settings: SettingsWatch::new(derisk_settings::default_path()),
            phone,
            words,
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
            notifications: notifications::Center::default(),
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
    fn agent_line(
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

    /// The title-bar buttons of each visible window, which the shell
    /// paints itself, so screen readers and agents can press them.
    fn title_bar_nodes(&self) -> Vec<Subtree> {
        let bar = self.shell.profile().title_bar;
        self.shell
            .placements()
            .into_iter()
            .map(|p| {
                let nodes: Vec<(NodeId, accesskit::Node)> = bar
                    .buttons(p.frame)
                    .map(|(button, area)| {
                        let mut node = accesskit::Node::new(accesskit::Role::Button);
                        node.set_label(match button {
                            Button::Close => "Close",
                            Button::Minimize => "Minimize",
                            Button::Maximize => "Maximize",
                        });
                        node.set_bounds(accesskit::Rect {
                            x0: f64::from(area.loc.x),
                            y0: f64::from(area.loc.y),
                            x1: f64::from(area.loc.x + area.size.w),
                            y1: f64::from(area.loc.y + area.size.h),
                        });
                        node.add_action(accesskit::Action::Click);
                        (NodeId(TITLE_BAR_NODES + button_index(button)), node)
                    })
                    .collect();
                Subtree {
                    window: p.window,
                    roots: nodes.iter().map(|(id, _)| *id).collect(),
                    nodes,
                    origin: Origin::Shell,
                }
            })
            .collect()
    }

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
            if let Some(mut unit) = systemd::launch_command_argv(app, self.launches, argv) {
                let split = unit.iter().position(|a| a == "--").unwrap_or(unit.len());
                unit.insert(
                    split,
                    format!("--setenv=WAYLAND_DISPLAY={}", self.wayland_display),
                );
                for (key, value) in &self.theme_env {
                    unit.insert(split, format!("--setenv={key}={value}"));
                }
                let _ = systemd::run(&unit);
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

    /// Runs an app's desktop action: core apps open in-process through
    /// [`CoreApps`], installed apps run the action's `Exec`.
    fn launch_action(&mut self, app: &str, id: &str) {
        if let Some(core) = derisk_apps::find(app) {
            if core.create_action(id).is_none() {
                info!("{} has no action {id:?}", core.id);
                return;
            }
            if let Ok(mut pending) = self.pending_actions.lock() {
                pending.push((core.id.to_owned(), id.to_owned()));
            }
            self.commands.push(Command::Launch(core.id.to_owned()));
            return;
        }
        let Some(argv) = self
            .installed
            .iter()
            .find(|e| e.id == app)
            .and_then(|e| e.action_argv(id))
        else {
            info!("{app} has no action {id:?}");
            return;
        };
        self.spawn_command(app, &argv);
    }

    fn perform(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match &effect {
                Effect::Launch { app } => self.launch(app),
                Effect::LaunchAction { app, id } => self.launch_action(app, id),
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
                            op: SessionOp::Lock
                        }
                    ) {
                        self.lock_screen();
                    }
                    if self.execute {
                        self.launches += 1;
                        let session = self.session_id.as_deref();
                        if let Some(argv) = systemd::effect_argv(&effect, self.launches, session) {
                            let _ = systemd::run(&argv);
                        }
                    } else {
                        info!(
                            "not executing {} (run with --execute)",
                            serde_json::to_string(&effect).unwrap_or_default()
                        );
                    }
                }
                Effect::Open { path } => {
                    if self.execute {
                        self.launches += 1;
                        let session = self.session_id.as_deref();
                        if let Some(mut argv) =
                            systemd::effect_argv(&effect, self.launches, session)
                        {
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
        let elapsed = self.start.elapsed().as_millis() as u32;
        self.ui.startup_frame(elapsed).done
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
        // wait, the name in the title bar cannot.
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
        if self.lock.is_locked() || !self.polkit.is_empty() {
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
        self.poll_idle();
        // Every frame, so a banner goes when its time is up rather than
        // up to a second later.
        self.notifications.expire(Instant::now());
        // Every frame, before the host asks where the windows are.
        self.move_windows();
        if !self.ending.is_empty() && self.ui.has_shut() {
            let ending = std::mem::take(&mut self.ending);
            self.perform(ending);
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
        }
        if self
            .last_tick
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_tick = Some(Instant::now());
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
            self.shell.effects = Effects::from_settings(&settings);
            self.shortcuts = settings.shortcuts;
            self.power = settings.power;
            self.wallpaper.configure(&settings.wallpaper);
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
            || !self.polkit.is_empty()
            || !self.choices.is_empty()
            || self.shell.overview_visible()
            || self.shell.palette_visible()
            || self.shell.pending_confirmation().is_some()
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
        if self.lock.is_locked() || !self.polkit.is_empty() || !self.choices.is_empty() {
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
        // or triggers a shortcut, and a choice, so Enter answers it.
        if !self.polkit.is_empty() || !self.choices.is_empty() {
            self.super_tap.cancel();
            return KeyRoute::Chrome;
        }
        let is_super = matches!(key.sym, Keysym::Super_L | Keysym::Super_R);
        if self.super_tap.key(is_super, key.pressed) {
            self.dispatch(vec![Action::Overview { visible: None }]);
            return KeyRoute::Consume;
        }
        if is_super {
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
        let mods = Mods {
            logo: key.mods.logo,
            shift: key.mods.shift,
            ctrl: key.mods.ctrl,
            alt: key.mods.alt,
        };
        if let Some(action) =
            layout_key(key.sym).and_then(|k| keys::binding_with(&self.shortcuts, mods, k))
        {
            if key.pressed {
                self.dispatch(vec![action]);
            }
            return KeyRoute::Consume;
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
        // With no window to type into, the search field has the focus (the
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
        if self.lock.is_locked() {
            self.poll_unlock();
        }
        if self.lock.is_locked() {
            let theme = self.ui.theme;
            if show_lock(
                ui,
                &mut self.lock,
                &self.shell,
                &theme,
                &self.user,
                &mut self.lock_wallpaper,
            ) && let Some(request) = self.lock.login.submit()
            {
                self.send_auth(request);
            }
            return;
        }
        let actions = self.ui.show(ui, &self.shell, elapsed_ms);
        self.dispatch(actions);
        self.show_notifications(ui);
        self.show_polkit(ui);
        if self.polkit.is_empty() {
            self.show_choice(ui);
        }
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
            let outcome = self.shell.ask(&ask.text, Source::User, ask.confirmed);
            self.perform(outcome.effects);
            if let Err(e) = outcome.result {
                info!("request failed: {e}");
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
        match look.max_fps {
            Some(fps) => timing.interval_for(fps),
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
        let command = match self.installed.iter().find(|e| e.id == app) {
            Some(entry) => entry.argv(),
            None => vec![app.strip_suffix(".desktop").unwrap_or(app).to_owned()],
        };
        if command.is_empty() {
            warn!("invalid Exec for {app:?}");
            return Vec::new();
        }
        self.launches += 1;
        if self.execute
            && let Some(mut argv) = systemd::launch_command_argv(app, self.launches, &command)
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
        let mut subtrees = self.title_bar_nodes();
        subtrees.extend(self.registered.values().map(|(_, s)| s.clone()));
        subtrees
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
        if node.0 >= TITLE_BAR_NODES {
            if action != accesskit::Action::Click {
                return;
            }
            let w = Some(window.get());
            let action = match node.0 - TITLE_BAR_NODES {
                0 => Action::Close { window: w },
                1 => Action::Minimize { window: w },
                2 => Action::ToggleMaximize { window: w },
                _ => return,
            };
            self.dispatch(vec![action]);
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

fn button_index(button: Button) -> u64 {
    match button {
        Button::Close => 0,
        Button::Minimize => 1,
        Button::Maximize => 2,
    }
}

/// The derisk core apps, as in-process windows.
struct CoreApps {
    session: derisk_apps::Session,
    pending_actions: PendingActions,
    phone: Arc<AtomicBool>,
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
            Some(action) => self.session.launch_action(app, &action),
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
    }
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
    let session = Session::new(&options, pending_actions.clone(), phone.clone());
    let lock_path = session.session_path.clone();
    if options.execute {
        let _ = systemd::run(&systemd::stop_headless_agent_argv());
    }
    let apps = CoreApps {
        session: derisk_apps::Session::new()?,
        pending_actions: pending_actions.clone(),
        phone,
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
            pending.push((derisk_apps::SETTINGS.to_owned(), "default-apps".to_owned()));
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
    Some(match sym {
        Keysym::Left => Key::Arrow(Direction::Left),
        Keysym::Right => Key::Arrow(Direction::Right),
        Keysym::Up => Key::Arrow(Direction::Up),
        Keysym::Down => Key::Arrow(Direction::Down),
        Keysym::Return | Keysym::KP_Enter | Keysym::Linefeed => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::Escape => Key::Escape,
        Keysym::space => Key::Space,
        _ => {
            let c = sym.key_char()?.to_ascii_lowercase();
            match c {
                '0'..='9' => Key::Digit(c as u8 - b'0'),
                'a'..='z' => Key::Letter(c),
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
