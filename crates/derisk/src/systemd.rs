//! Deep systemd integration, without linking libsystemd.
//!
//! - **Apps as units**: every launch becomes a transient
//!   `app-derisk-<AppID>@<instance>.service` in `app-graphical.slice`, named per
//!   the XDG/systemd app-unit convention, so each app gets its own cgroup,
//!   resource accounting, journal stream and clean shutdown.
//! - **Focus-aware resources**: the focused app's unit gets higher CPU and IO
//!   weight; the previously focused unit returns to the default.
//! - **Service manager protocol**: `sd_notify` readiness, status and watchdog
//!   keep-alives; `LISTEN_FDS` socket activation for the agent socket.
//! - **journald**: structured native-protocol logging with a stderr fallback.
//! - **Session**: environment import into the user manager, the
//!   `derisk-session.target` lifecycle, logind lock/suspend/reboot/power-off,
//!   and failed-unit reporting for the overview.

use std::{
    io,
    os::unix::net::UnixDatagram,
    path::Path,
    process::{Command, Output},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::action::Effect;

/// The launcher component of app unit names.
pub const LAUNCHER: &str = "derisk";
/// The slice graphical apps run in.
pub const APP_SLICE: &str = "app-graphical.slice";
/// The session target started once the compositor is ready.
pub const SESSION_TARGET: &str = "derisk-session.target";
/// CPU/IO weight given to the focused app (systemd's default is 100).
pub const FOCUS_WEIGHT: u32 = 400;
/// Default CPU/IO weight.
pub const DEFAULT_WEIGHT: u32 = 100;

/// Where `derisk display-manager` listens for a session asking it to show
/// the login screen, so someone else can log in while it keeps running.
pub const SWITCH_USER_SOCKET: &str = "/run/derisk-dm/switch.sock";

/// Asks `derisk display-manager` to show the login screen on a VT of its
/// own ([`SWITCH_USER_SOCKET`]). Connecting is the request; it is heard
/// only from the session on the screen, so call it from there.
pub fn request_switch_user() -> io::Result<()> {
    std::os::unix::net::UnixStream::connect(SWITCH_USER_SOCKET).map(drop)
}

/// A logind / service-manager session operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOp {
    /// Lock the session.
    Lock,
    /// Lock the session and show the login screen on another VT, leaving
    /// it running for its owner to come back to.
    SwitchUser,
    /// Suspend to RAM.
    Suspend,
    /// Hibernate.
    Hibernate,
    /// End the session.
    Logout,
    /// Reboot.
    Reboot,
    /// Power off.
    PowerOff,
}

impl SessionOp {
    /// Whether the operation loses unsaved work and needs explicit confirmation.
    pub fn is_destructive(self) -> bool {
        matches!(self, Self::Logout | Self::Reboot | Self::PowerOff)
    }
}

/// Escapes a string for use in a unit name, like `systemd-escape`.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'/' => out.push('-'),
            b'.' if i == 0 => out.push_str("\\x2e"),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b':' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// Whether `app` is a plain command name or desktop-file ID we will launch.
///
/// Paths, arguments and shell syntax are rejected so that agents can only
/// start installed applications, not arbitrary command lines.
pub fn is_launchable(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 128
        && !app.starts_with(['-', '.'])
        && app
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
}

/// The instance part of a launch's unit name: this process's ID above its
/// count of launches. The count alone starts again at 1 with every desktop,
/// while the user's manager, and an app unit something in its cgroup keeps
/// up, can outlive one; systemd refuses a second unit of the same name, so
/// that app would not start again until the old unit went.
pub fn instance(launches: u64) -> u64 {
    (u64::from(std::process::id()) << 20) | launches
}

/// The transient unit name for an app instance.
pub fn app_unit(app: &str, instance: u64) -> String {
    format!(
        "app-{LAUNCHER}-{}@{instance:x}.service",
        escape(app.strip_suffix(".desktop").unwrap_or(app))
    )
}

/// `systemd-run` arguments that launch `app` as a transient user service.
///
/// A desktop-file ID (`foo.desktop`) runs the command `foo`. Returns `None`
/// for names rejected by [`is_launchable`].
pub fn launch_argv(app: &str, instance: u64) -> Option<Vec<String>> {
    if !is_launchable(app) {
        return None;
    }
    let exec = app.strip_suffix(".desktop").unwrap_or(app);
    launch_command_argv(app, instance, &[exec.to_owned()])
}

/// `systemd-run` arguments that run `command` (already split, for example
/// from a `.desktop` file's `Exec`) as a transient user service for `app`.
///
/// Returns `None` if `app` is rejected by [`is_launchable`] or `command` is
/// empty.
pub fn launch_command_argv(app: &str, instance: u64, command: &[String]) -> Option<Vec<String>> {
    if !is_launchable(app) || command.is_empty() {
        return None;
    }
    let exec = app.strip_suffix(".desktop").unwrap_or(app);
    let mut argv: Vec<String> = [
        "systemd-run",
        "--user",
        "--no-block",
        "--collect",
        "--quiet",
        &format!("--unit={}", app_unit(app, instance)),
        &format!("--slice={APP_SLICE}"),
        "--property=Type=exec",
        "--property=ExitType=cgroup",
        &format!("--description={exec} (derisk)"),
        "--",
    ]
    .map(str::to_owned)
    .to_vec();
    argv.extend_from_slice(command);
    Some(argv)
}

/// Command line for a session operation.
pub fn session_argv(op: SessionOp, session_id: Option<&str>) -> Vec<String> {
    let args: Vec<&str> = match op {
        SessionOp::Lock | SessionOp::SwitchUser => match session_id {
            Some(id) => vec!["loginctl", "lock-session", id],
            None => vec!["loginctl", "lock-sessions"],
        },
        SessionOp::Suspend => vec!["systemctl", "suspend"],
        SessionOp::Hibernate => vec!["systemctl", "hibernate"],
        SessionOp::Reboot => vec!["systemctl", "reboot"],
        SessionOp::PowerOff => vec!["systemctl", "poweroff"],
        SessionOp::Logout => match session_id {
            Some(id) => vec!["loginctl", "terminate-session", id],
            None => vec!["systemctl", "--user", "stop", SESSION_TARGET],
        },
    };
    args.into_iter().map(str::to_owned).collect()
}

/// Commands that run an effect through systemd, if it needs any.
///
/// `instance` makes launched unit names unique. Effects handled over Wayland
/// (closing windows, menu activation) return `None`.
pub fn effect_argv(
    effect: &Effect,
    instance: u64,
    session_id: Option<&str>,
) -> Option<Vec<String>> {
    match effect {
        Effect::Launch { app } => launch_argv(app, instance),
        Effect::Session { op } => Some(session_argv(*op, session_id)),
        // The shell only emits absolute paths and URLs with a scheme, so
        // the path cannot be read as an option.
        Effect::Open { path } => launch_argv("xdg-open", instance).map(|mut argv| {
            argv.push(path.clone());
            argv
        }),
        Effect::ResetFailed { unit } => Some(
            ["systemctl", "--user", "reset-failed", "--", unit.as_str()]
                .map(str::to_owned)
                .to_vec(),
        ),
        Effect::RestartUnit { unit } => Some(
            [
                "systemctl",
                "--user",
                "restart",
                "--no-block",
                "--",
                unit.as_str(),
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
        // The host resolves the action's command from the app's .desktop
        // file and uses `launch_command_argv`; a file opens in a core app,
        // which the host draws itself.
        Effect::LaunchAction { .. }
        | Effect::OpenIn { .. }
        | Effect::Close { .. }
        | Effect::MenuActivated { .. }
        | Effect::TrayActivated { .. }
        | Effect::WidgetActivated { .. }
        | Effect::PaletteCommand { .. }
        | Effect::Screenshot
        | Effect::Report { .. }
        | Effect::TutorialSeen => None,
    }
}

/// Commands to run once the compositor's Wayland socket is up.
///
/// Exports the session environment to the user manager and D-Bus activation
/// environment, then starts [`SESSION_TARGET`] (which binds
/// `graphical-session.target`), so portals, tray hosts and autostart units see
/// the right display.
pub fn session_start_argv(wayland_display: &str) -> impl Iterator<Item = Vec<String>> + use<> {
    let environment = [
        format!("WAYLAND_DISPLAY={wayland_display}"),
        "XDG_CURRENT_DESKTOP=derisk".to_owned(),
        "XDG_SESSION_TYPE=wayland".to_owned(),
        "XDG_SESSION_DESKTOP=derisk".to_owned(),
        "DISPLAY=".to_owned(),
    ];
    let mut systemd_environment = ["systemctl", "--user", "set-environment"]
        .map(str::to_owned)
        .to_vec();
    systemd_environment.extend(environment.iter().cloned());
    let mut dbus_environment = ["dbus-update-activation-environment", "--systemd"]
        .map(str::to_owned)
        .to_vec();
    dbus_environment.extend(environment);
    [
        systemd_environment,
        dbus_environment,
        ["systemctl", "--user", "--no-block", "start", SESSION_TARGET]
            .map(str::to_owned)
            .to_vec(),
    ]
    .into_iter()
}

/// The unit owning a process, parsed from `/proc/<pid>/cgroup` (cgroup v2).
pub fn unit_from_cgroup(cgroup: &str) -> Option<String> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    path.rsplit('/')
        .find(|c| c.ends_with(".service") || c.ends_with(".scope"))
        .map(str::to_owned)
}

/// The unit owning `pid`, if it can be read.
pub fn unit_of_pid(pid: u32) -> Option<String> {
    unit_from_cgroup(&std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
}

/// Gives the focused app's unit more CPU/IO weight than background apps.
#[derive(Clone, Debug, Default)]
pub struct FocusBoost {
    boosted: Option<String>,
}

impl FocusBoost {
    /// Commands to move the boost to `unit`; empty if it already has it.
    ///
    /// Only app units (`app-*`) are touched, never the session or system.
    pub fn focus(&mut self, unit: Option<&str>) -> impl Iterator<Item = Vec<String>> + use<> {
        let unit = unit.filter(|u| u.starts_with("app-")).map(str::to_owned);
        let set = |unit: &str, weight: u32| -> Vec<String> {
            [
                "systemctl",
                "--user",
                "set-property",
                "--runtime",
                "--",
                unit,
                &format!("CPUWeight={weight}"),
                &format!("IOWeight={weight}"),
            ]
            .map(str::to_owned)
            .to_vec()
        };
        // Worked out now, not as the commands are read: the boost has moved
        // whether or not the caller runs them.
        let mut commands = [None, None];
        if unit != self.boosted {
            commands = [
                self.boosted.take().map(|old| set(&old, DEFAULT_WEIGHT)),
                unit.as_deref().map(|new| set(new, FOCUS_WEIGHT)),
            ];
            self.boosted = unit;
        }
        commands.into_iter().flatten()
    }
}

/// Parses `systemctl --user list-units --state=failed --plain --no-legend`.
pub fn parse_failed_units(output: &str) -> impl Iterator<Item = &str> {
    output
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|u| u.contains('.'))
}

/// Runs a command without a shell, returning its output.
pub fn run(argv: &[String]) -> io::Result<Output> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    Command::new(program).args(args).output()
}

/// Currently failed user units.
pub fn failed_units() -> Vec<String> {
    let argv = [
        "systemctl",
        "--user",
        "list-units",
        "--state=failed",
        "--plain",
        "--no-legend",
        "--no-pager",
    ]
    .map(str::to_owned);
    run(&argv)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            parse_failed_units(&String::from_utf8_lossy(&o.stdout))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `busctl` arguments that ask logind whether this machine can hibernate.
pub fn can_hibernate_argv() -> Vec<String> {
    [
        "busctl",
        "--system",
        "call",
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
        "CanHibernate",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Whether logind's answer to a `Can*` call, as `busctl` prints it (such as
/// `s "yes"`), means the operation can be asked for. `challenge` can: polkit
/// asks the person first. `no` and `na` cannot.
pub fn parse_can(output: &str) -> bool {
    let answer = output
        .trim()
        .strip_prefix("s ")
        .map(|a| a.trim_matches('"'));
    matches!(answer, Some("yes" | "challenge"))
}

/// Whether the machine can hibernate now. logind says no unless there is a
/// swap device a resume can find and room in it for what is in memory, so
/// the answer changes as memory fills.
pub fn can_hibernate() -> bool {
    run(&can_hibernate_argv())
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| parse_can(&String::from_utf8_lossy(&o.stdout)))
}

fn notify_socket() -> Option<UnixDatagram> {
    let path = std::env::var_os("NOTIFY_SOCKET")?;
    let socket = UnixDatagram::unbound().ok()?;
    let bytes = path.as_encoded_bytes();
    if let Some(name) = bytes.strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name).ok()?;
        socket.connect_addr(&addr).ok()?;
    } else {
        socket.connect(Path::new(&path)).ok()?;
    }
    Some(socket)
}

/// Sends an `sd_notify` message (e.g. `READY=1`). Returns false outside systemd.
pub fn notify(state: &str) -> bool {
    notify_socket().is_some_and(|s| s.send(state.as_bytes()).is_ok())
}

/// `systemctl` arguments that stop the headless agent, so the desktop can
/// serve the agent socket on the same path.
pub fn stop_headless_agent_argv() -> Vec<String> {
    [
        "systemctl",
        "--user",
        "stop",
        "derisk-agent.socket",
        "derisk-agent.service",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The logind session this process runs in, from `XDG_SESSION_ID`.
///
/// Session operations name it, so Lock and Log out act on this session and
/// not on every session of the user.
pub fn session_id() -> Option<String> {
    std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `busctl` arguments that ask logind for the object path of session `id`.
pub fn session_path_argv(id: &str) -> Vec<String> {
    [
        "busctl",
        "--system",
        "call",
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
        "GetSession",
        "s",
        id,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The object path in `busctl call` output such as
/// `o "/org/freedesktop/login1/session/_32"`.
pub fn parse_object_path(output: &str) -> Option<String> {
    let path = output.trim().strip_prefix("o ")?.trim().trim_matches('"');
    path.starts_with('/').then(|| path.to_owned())
}

/// Looks up the object path of session `id`.
pub fn session_path(id: &str) -> Option<String> {
    let output = run(&session_path_argv(id)).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_object_path(&String::from_utf8_lossy(&output.stdout))
}

/// `busctl` arguments that wait for logind's `Lock` signal on the session at
/// `path` and exit when it arrives. logind sends it for `loginctl
/// lock-session`, `loginctl lock-sessions` and anything else that asks for
/// the session to be locked.
pub fn lock_signal_argv(path: &str) -> Vec<String> {
    [
        "busctl",
        "--system",
        "wait",
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
        "Lock",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `busctl` arguments that tell logind whether the session at `path` is
/// locked, which `loginctl show-session -p LockedHint` and logind's idle
/// handling read.
pub fn locked_hint_argv(path: &str, locked: bool) -> Vec<String> {
    [
        "busctl",
        "--system",
        "call",
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
        "SetLockedHint",
        "b",
        if locked { "true" } else { "false" },
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Tells the service manager the shell is ready, with a status line.
pub fn notify_ready(status: &str) -> bool {
    notify(&format!("READY=1\nSTATUS={}", status.replace('\n', " ")))
}

/// Updates the status shown by `systemctl status`.
pub fn notify_status(status: &str) -> bool {
    notify(&format!("STATUS={}", status.replace('\n', " ")))
}

/// Sends a watchdog keep-alive.
pub fn notify_watchdog() -> bool {
    notify("WATCHDOG=1")
}

/// Announces shutdown.
pub fn notify_stopping() -> bool {
    notify("STOPPING=1")
}

/// How often to send watchdog keep-alives (half of `WatchdogSec=`), if enabled.
pub fn watchdog_interval() -> Option<Duration> {
    watchdog_interval_from(
        std::env::var("WATCHDOG_USEC").ok().as_deref(),
        std::env::var("WATCHDOG_PID").ok().as_deref(),
        std::process::id(),
    )
}

/// [`watchdog_interval`] from explicit values.
pub fn watchdog_interval_from(usec: Option<&str>, pid: Option<&str>, me: u32) -> Option<Duration> {
    if pid.is_some_and(|p| p.parse::<u32>().ok() != Some(me)) {
        return None;
    }
    let usec: u64 = usec?.parse().ok().filter(|u| *u > 0)?;
    Some(Duration::from_micros(usec / 2))
}

/// The first socket-activation file descriptor passed to this process.
///
/// Returns `None` unless `LISTEN_PID` names this process and `LISTEN_FDS`
/// is at least one. The descriptor is always 3 (`SD_LISTEN_FDS_START`).
pub fn listen_fd() -> Option<i32> {
    listen_fd_from(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    )
}

/// [`listen_fd`] from explicit values.
pub fn listen_fd_from(pid: Option<&str>, fds: Option<&str>, me: u32) -> Option<i32> {
    (pid?.parse::<u32>().ok()? == me && fds?.parse::<u32>().ok()? >= 1).then_some(3)
}

// A hand-written journald client stood here: `log`, `journal_payload` and a
// `Priority` enum speaking the native protocol to /run/systemd/journal/socket.
// tracing-journald replaced it, so the shell logs through `tracing` like its
// libraries do and the binary picks journald or stderr in one place.
