//! `derisk greeter`: the lock screen as a login screen, for greetd.
//!
//! greetd, or `derisk display-manager`, runs this as its greeter, straight on
//! the seat's VT (mcsapi-compositor drives the display itself when there is no
//! session to nest in). It shows the lock screen's look over an empty output, asks who
//! is logging in, relays PAM's questions from greetd, and once greetd has
//! authenticated the user asks it to start the session command and exits;
//! greetd starts the session when the greeter is gone.
//!
//! Nothing here checks a password. greetd owns PAM and the logind session,
//! which is what lets the session it opens be a real login (pam_systemd,
//! pam_systemd_home unlocking the home area) rather than a process the
//! greeter forked. The conversation itself is [`derisk::greetd::Login`]; this
//! module only moves its messages over `$GREETD_SOCK` on a worker thread, so
//! a slow PAM module never stalls a frame.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use derisk::{
    animation::LoginAnimation,
    backlight::{self, PasswordDim},
    greetd::{self, Login, Phase, Request, Response},
    time::Clock,
    ui::{GreeterInput, LoginUser, paint_login, paint_startup, paint_wallpaper, show_greeter},
};
use mcsapi::WindowId;
use mcsapi_compositor::{
    self as compositor, Command, Compositor, KeyInput, KeyRoute, Placement, Press, Theme, egui,
};
use tracing::{error, info};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What `derisk greeter` was asked to do.
#[derive(Debug, Default)]
pub struct Options {
    /// Who to fill in. Empty: the only regular user, if there is one.
    pub user: Option<String>,
    /// The session greetd starts, as a command line.
    pub command: Vec<String>,
    /// Extra `KEY=VALUE` pairs for the session's environment.
    pub env: Vec<String>,
}

impl Options {
    /// Reads `derisk greeter`'s arguments (what follows `greeter`).
    pub fn parse(args: &[String]) -> std::result::Result<Self, String> {
        let mut options = Self::default();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--user" => options.user = Some(it.next().ok_or("--user needs a name")?.clone()),
                "--env" => {
                    let pair = it.next().ok_or("--env needs KEY=VALUE")?;
                    if !pair.contains('=') {
                        return Err("--env needs KEY=VALUE".into());
                    }
                    options.env.push(pair.clone());
                }
                "--" => {
                    options.command = it.cloned().collect();
                    break;
                }
                other => return Err(format!("unknown greeter option: {other}")),
            }
        }
        Ok(options)
    }
}

/// The session a greeter run as `greeter` (its whole command line, `derisk
/// greeter ... -- <session>`) would start, with its environment: what the
/// display manager starts itself when it logs someone in without one.
pub fn session_of(greeter: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let at = greeter.iter().position(|arg| arg == "greeter")?;
    let options = Options::parse(&greeter[at + 1..]).ok()?;
    (!options.command.is_empty()).then(|| (options.command, session_env(options.env)))
}

/// The session's environment unless `--env` overrides a key: a Wayland
/// session of the derisk desktop, for pam_systemd's session type and for
/// portals and toolkits choosing a backend.
const SESSION_ENV: [&str; 3] = [
    "XDG_SESSION_TYPE=wayland",
    "XDG_SESSION_DESKTOP=derisk",
    "XDG_CURRENT_DESKTOP=derisk",
];

struct Greeter {
    login: Login,
    users: Vec<LoginUser>,
    /// `loginctl activate` going back to someone's running session: its
    /// result arrives here, and the greeter exits once it worked.
    switching: Option<mpsc::Receiver<std::result::Result<(), String>>>,
    /// Submit on the first frame: one user, so go straight to the password
    /// the way the lock screen does.
    auto_submit: bool,
    output: (i32, i32),
    clock: Clock,
    /// The clock the last frame showed.
    drawn_clock: Option<Clock>,
    last_tick: Option<Instant>,
    theme: Theme,
    to_greetd: mpsc::Sender<Request>,
    from_greetd: mpsc::Receiver<std::io::Result<Response>>,
    next_window: u64,
    /// Whether the greeter has decided to exit.
    quit: bool,
    /// When (ms since the first frame) the session was started: the exit
    /// animation runs from then, and the greeter quits at its end.
    leaving: Option<u32>,
    commands: Vec<Command>,
    /// The backlight, turned down while a password is typed.
    password_dim: PasswordDim,
}

impl Greeter {
    fn send(&mut self, request: Option<Request>) {
        if let Some(request) = request
            && self.to_greetd.send(request).is_err()
        {
            self.lost("the connection closed");
        }
    }

    /// greetd is gone, so no login can finish here. Exit, and greetd (if it
    /// is still running) starts a fresh greeter.
    fn lost(&mut self, error: &str) {
        error!("lost greetd: {error}");
        self.login.disconnected(error);
        self.quit();
    }

    /// Ends the compositor, once: the host asks for commands until none are
    /// left, so the request must be handed over exactly one time.
    fn quit(&mut self) {
        if !self.quit {
            self.quit = true;
            self.commands.push(Command::Quit);
        }
    }

    /// Goes to `user`'s running session instead of logging them in again:
    /// logind makes it the seat's active session (polkit lets the active
    /// session on a seat do that), it shows its own lock screen, and this
    /// greeter is no longer needed.
    fn switch_to(&mut self, user: usize) {
        let Some(session) = self.users.get(user).and_then(|u| u.session.clone()) else {
            return;
        };
        if self.switching.is_some() {
            return;
        }
        info!("switching to a running session");
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            let out = std::process::Command::new("loginctl")
                .args(["activate", &session])
                .stdin(std::process::Stdio::null())
                .output();
            let _ = done.send(match out {
                Ok(out) if out.status.success() => Ok(()),
                Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
                Err(e) => Err(e.to_string()),
            });
        });
        self.switching = Some(result);
    }

    /// Applies greetd's answers that arrived since the last frame. `now` is
    /// ms since the first frame.
    fn poll(&mut self, now: u32) {
        if let Some(result) = self.switching.as_ref().and_then(|r| r.try_recv().ok()) {
            self.switching = None;
            match result {
                Ok(()) => self.quit(),
                Err(e) => {
                    error!("cannot switch to the session: {e}");
                    self.login.tell("Could not switch to that session", true);
                }
            }
        }
        loop {
            match self.from_greetd.try_recv() {
                Ok(Ok(response)) => {
                    let next = self.login.respond(response);
                    self.send(next);
                }
                Ok(Err(e)) => return self.lost(&e.to_string()),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return self.lost("worker stopped"),
            }
        }
        let phase = self.login.phase();
        let field = backlight::Field {
            shown: !matches!(phase, Phase::Starting | Phase::Started),
            typed: matches!(phase, Phase::Prompt { secret: true, .. })
                && !self.login.answer.is_empty(),
            checking: *phase == Phase::Waiting,
        };
        if let Some((name, level)) = self.password_dim.update(field, Instant::now())
            && let Err(e) = backlight::set(&name, level)
        {
            error!("cannot set the backlight: {e}");
        }
        if *self.login.phase() == Phase::Started && self.leaving.is_none() {
            // The display manager logs the session; logind records whose it is.
            info!("starting the session");
            self.leaving = Some(now);
        }
        // The session starts once the screen has closed round the dot,
        // which it opens from.
        if self
            .leaving
            .is_some_and(|at| now.saturating_sub(at) as f32 >= LoginAnimation::LEAVE_MS)
        {
            self.quit();
        }
    }
}

impl compositor::Shell for Greeter {
    /// No client belongs on a login screen. One that connects anyway gets an
    /// ID and is never placed, so it is never drawn and never focused.
    fn map_window(&mut self, _app_id: &str, _title: &str) -> WindowId {
        self.next_window += 1;
        WindowId::new(self.next_window).expect("window IDs start at 1")
    }

    fn unmap_window(&mut self, _window: WindowId) {}

    fn set_output(&mut self, size: (i32, i32)) {
        self.output = size;
    }

    fn focused(&self) -> Option<WindowId> {
        None
    }

    fn placements(&self) -> Vec<Placement> {
        Vec::new()
    }

    fn tick(&mut self) {
        if self
            .last_tick
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.last_tick = Some(Instant::now());
        self.clock = Clock::now_local();
    }

    fn chrome_wants_pointer(&self, _at: (i32, i32)) -> bool {
        true
    }

    fn pointer_down(&mut self, _at: (i32, i32), _time_ms: u64) -> Press {
        Press::Handled
    }

    /// Every key goes to the field; there is no client to give one to.
    fn key(&mut self, _key: &KeyInput) -> KeyRoute {
        KeyRoute::Chrome
    }

    fn theme(&self) -> Theme {
        self.theme
    }

    fn paint_background(&mut self, painter: &egui::Painter, screen: egui::Rect) {
        paint_wallpaper(painter, screen, &self.theme);
    }

    fn chrome(&mut self, ui: &mut egui::Ui, elapsed_ms: u32) {
        // In the default theme's letters, as the session after it.
        mcsapi_components::install_fonts(ui.ctx(), &mcsapi_ui::theme::Fonts::default());
        self.poll(elapsed_ms);
        if std::mem::take(&mut self.auto_submit) {
            let request = self.login.submit();
            self.send(request);
        }
        let screen = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(self.output.0 as f32, self.output.1 as f32),
        );
        let (input, field) = show_greeter(
            ui,
            &mut self.login,
            &self.users,
            screen,
            &self.clock,
            &self.theme,
        );
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("derisk-greeter-motion"),
        ));
        let login = LoginAnimation;
        match self.leaving {
            Some(at) => {
                let frame = login.leaving(elapsed_ms.saturating_sub(at) as f32);
                paint_startup(&painter, screen, frame, &self.theme, field);
            }
            None if !login.arrived(elapsed_ms) => {
                paint_login(
                    &painter,
                    screen,
                    login.frame(elapsed_ms),
                    &self.theme,
                    field,
                );
            }
            None => {}
        }
        // A name typed for someone signed in already goes to their session
        // too, the same as picking them.
        let typed = (input == GreeterInput::Submit && *self.login.phase() == Phase::User)
            .then(|| {
                let name = self.login.username.trim();
                self.users
                    .iter()
                    .position(|u| u.name == name && u.session.is_some())
            })
            .flatten();
        let request = match (input, typed) {
            (_, Some(user)) | (GreeterInput::Switch(user), _) => {
                self.switch_to(user);
                None
            }
            (GreeterInput::Submit, None) => self.login.submit(),
            (GreeterInput::Back, None) => self.login.back(),
            (GreeterInput::None, None) => None,
        };
        self.send(request);
        // greetd's answers, and loginctl's, arrive between frames: while one
        // is awaited, look for it every frame, as the arrival and leaving
        // animations need every frame too. Otherwise the screen waits for a
        // key; drawing it sixty times a second for nothing kept a software
        // renderer busy while the login screen sat there.
        let awaiting = self.switching.is_some()
            || matches!(
                self.login.phase(),
                Phase::Waiting | Phase::Starting | Phase::Cancelling { .. } | Phase::Started
            );
        if awaiting || self.leaving.is_some() || !login.arrived(elapsed_ms) {
            ui.ctx().request_repaint();
        }
    }

    /// The clock's minute is the one thing here that changes on its own.
    fn needs_frame(&mut self) -> bool {
        let clock = Some(self.clock);
        clock != std::mem::replace(&mut self.drawn_clock, clock)
    }

    fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }
}

/// Carries requests to greetd and its answers back, one at a time, until
/// either side hangs up. The lock screen uses it too, with a `derisk auth`
/// process's pipes in place of greetd's socket.
pub(crate) fn spawn_worker(
    mut stream: impl Read + Write + Send + 'static,
) -> (
    mpsc::Sender<Request>,
    mpsc::Receiver<std::io::Result<Response>>,
) {
    let (to_greetd, requests) = mpsc::channel::<Request>();
    let (answers, from_greetd) = mpsc::channel();
    thread::spawn(move || {
        let mut started = false;
        for request in requests {
            let starting = matches!(request, Request::StartSession { .. });
            let result = greetd::write_request(&mut stream, &request)
                .and_then(|()| greetd::read_response(&mut stream));
            started |= starting && matches!(result, Ok(Response::Success));
            let failed = result.is_err();
            if answers.send(result).is_err() || failed {
                return;
            }
        }
        // The greeter is exiting without starting a session: leave greetd
        // with no half-authenticated session behind. After a start there is
        // nothing to cancel, and greetd is waiting for this exit to run it.
        if !started {
            let _ = greetd::write_request(&mut stream, &Request::CancelSession);
            let _ = greetd::read_response(&mut stream);
        }
    });
    (to_greetd, from_greetd)
}

/// Regular users, from userdb (which includes systemd-homed's) and then
/// /etc/passwd, sorted and without repeats, each with their running session
/// on this seat if they have one.
fn regular_users() -> Vec<LoginUser> {
    let output = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .stderr(std::process::Stdio::null())
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut users: Vec<LoginUser> = userdb_users(&output(
        "userdbctl",
        &[
            "user",
            "--disposition=regular",
            "--json=short",
            "--no-pager",
        ],
    ))
    .collect();
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        users.extend(passwd_users(&passwd));
    }
    // userdb comes first, so its record (with the full name) is the one
    // kept when /etc/passwd lists the same person.
    users.sort_by(|a, b| a.name.cmp(&b.name));
    users.dedup_by(|later, first| later.name == first.name);
    let sessions = output("loginctl", &["list-sessions", "--json=short", "--no-pager"]);
    for (name, id) in seat_sessions(&sessions) {
        if let Some(user) = users.iter_mut().find(|u| u.name == name) {
            user.session.get_or_insert(id);
        }
    }
    users
}

/// People in `userdbctl user --json=short` output: one JSON object per
/// record, possibly with RS separators.
fn userdb_users(json: &str) -> impl Iterator<Item = LoginUser> {
    let text = json.replace('\u{1e}', "\n");
    let records: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&text)
        .into_iter::<serde_json::Value>()
        .map_while(std::result::Result::ok)
        .collect();
    records.into_iter().filter_map(|record| {
        Some(LoginUser {
            name: record.get("userName")?.as_str()?.to_owned(),
            real_name: record
                .get("realName")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_owned(),
            session: None,
        })
    })
}

/// Users in /etc/passwd in the regular UID range (1000–60000, systemd's and
/// most distributions') that have a login shell, with the full name from
/// the comment field.
fn passwd_users(passwd: &str) -> impl Iterator<Item = LoginUser> + '_ {
    passwd.lines().filter_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let uid: u32 = fields.get(2)?.parse().ok()?;
        let shell = fields.get(6)?;
        let login = !shell.ends_with("/nologin") && !shell.ends_with("/false");
        ((1000..=60000).contains(&uid) && login).then(|| LoginUser {
            name: fields[0].to_owned(),
            real_name: fields
                .get(4)
                .and_then(|gecos| gecos.split(',').next())
                .unwrap_or_default()
                .to_owned(),
            session: None,
        })
    })
}

/// The user sessions on seat0 in `loginctl list-sessions --json=short`
/// output, as (user, session ID): the desktops someone left running, which
/// the greeter goes back to rather than starting a second one.
fn seat_sessions(json: &str) -> impl Iterator<Item = (String, String)> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    list.into_iter().filter_map(|session| {
        let text = |key: &str| session.get(key)?.as_str().map(str::to_owned);
        let ours = text("class").as_deref() == Some("user")
            && text("seat").as_deref() == Some("seat0")
            // A session logind is closing (its user logged out) is not
            // one to go back to.
            && text("state").as_deref() != Some("closing");
        ours.then(|| Some((text("user")?, text("session")?)))
            .flatten()
    })
}

/// `env` with `defaults` added for every key it does not set.
fn session_env(env: Vec<String>) -> Vec<String> {
    let key = |pair: &str| pair.split('=').next().unwrap_or_default().to_owned();
    let mut out = env;
    for default in SESSION_ENV {
        if !out.iter().any(|pair| key(pair) == key(default)) {
            out.push(default.to_owned());
        }
    }
    out
}

/// Runs the greeter until greetd has a session to start, or is gone.
pub fn run(options: Options) -> Result {
    if options.command.is_empty() {
        return Err("derisk greeter needs the session's command after `--`".into());
    }
    let socket = std::env::var_os("GREETD_SOCK")
        .ok_or("GREETD_SOCK is not set; derisk greeter runs under greetd")?;
    let stream = UnixStream::connect(&socket)
        .map_err(|e| format!("cannot reach greetd on {}: {e}", socket.to_string_lossy()))?;
    let (to_greetd, from_greetd) = spawn_worker(stream);
    let users = regular_users();
    // The only person goes straight to their password, unless they are
    // signed in already: then the list offers to go back to their session.
    let preset = options.user.clone().or_else(|| match users.as_slice() {
        [only] if only.session.is_none() => Some(only.name.clone()),
        _ => None,
    });
    let greeter = Greeter {
        auto_submit: preset.is_some(),
        login: Login::new(
            preset.unwrap_or_default(),
            options.command,
            session_env(options.env),
        ),
        users,
        switching: None,
        output: (1280, 800),
        clock: Clock::now_local(),
        drawn_clock: None,
        last_tick: None,
        theme: Theme::default(),
        to_greetd,
        from_greetd,
        next_window: 0,
        quit: false,
        leaving: None,
        commands: Vec::new(),
        password_dim: PasswordDim::system(),
    };
    Compositor::new(greeter).title("derisk greeter").run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwd_offers_people_not_services() {
        let passwd = "root:x:0:0::/root:/bin/sh\n\
                      greeter:x:996:996::/var/empty:/bin/sh\n\
                      alice:x:1000:100::/home/alice:/bin/bash\n\
                      backup:x:1001:1001::/:/usr/sbin/nologin\n\
                      nobody:x:65534:65534::/:/bin/false\n";
        let users: Vec<_> = passwd_users(passwd).collect();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "alice");
    }

    #[test]
    fn full_names_come_from_userdb_and_passwd() {
        let userdb = "\u{1e}{\"userName\":\"ada\",\"realName\":\"Ada Lovelace\"}\n\
                      \u{1e}{\"userName\":\"bob\"}\n";
        let users: Vec<_> = userdb_users(userdb).collect();
        assert_eq!(users[0].display_name(), "Ada Lovelace");
        assert_eq!(users[1].display_name(), "bob");
        let passwd = "carol:x:1000:100:Carol Danvers,,,:/home/carol:/bin/bash\n";
        assert_eq!(
            passwd_users(passwd).next().unwrap().real_name,
            "Carol Danvers"
        );
    }

    #[test]
    fn running_desktops_are_the_seats_user_sessions() {
        // systemd 261's `loginctl list-sessions --json=short`.
        let json = r#"[
            {"session":"2","uid":1000,"user":"ada","seat":"seat0","leader":901,"class":"user","tty":"tty1","idle":true,"since":null,"state":"active"},
            {"session":"c1","uid":994,"user":"derisk-greeter","seat":"seat0","leader":950,"class":"greeter","tty":"tty2","idle":false,"since":null,"state":"online"},
            {"session":"4","uid":1001,"user":"bob","seat":null,"leader":990,"class":"user","tty":"pts/0","idle":false,"since":null,"state":"active"},
            {"session":"5","uid":1002,"user":"carol","seat":"seat0","leader":991,"class":"user","tty":"tty3","idle":false,"since":null,"state":"closing"}
        ]"#;
        assert_eq!(
            seat_sessions(json).collect::<Vec<_>>(),
            [("ada".to_owned(), "2".to_owned())]
        );
        assert_eq!(seat_sessions("").count(), 0);
    }

    #[test]
    fn session_env_keeps_overrides() {
        let env = session_env(vec!["XDG_CURRENT_DESKTOP=other".into()]);
        assert!(env.contains(&"XDG_CURRENT_DESKTOP=other".to_owned()));
        assert!(!env.contains(&"XDG_CURRENT_DESKTOP=derisk".to_owned()));
        assert!(env.contains(&"XDG_SESSION_TYPE=wayland".to_owned()));
    }

    #[test]
    fn the_display_manager_finds_the_session_in_the_greeters_command() {
        let argv = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let (cmd, env) = session_of(&argv(
            "/bin/derisk greeter --env XDG_CURRENT_DESKTOP=x -- /bin/derisk session --execute",
        ))
        .unwrap();
        assert_eq!(cmd, ["/bin/derisk", "session", "--execute"]);
        assert!(env.contains(&"XDG_CURRENT_DESKTOP=x".to_owned()));
        assert!(env.contains(&"XDG_SESSION_TYPE=wayland".to_owned()));
        assert!(session_of(&argv("/bin/derisk greeter")).is_none());
        assert!(session_of(&argv("/bin/other -- session")).is_none());
    }
}
