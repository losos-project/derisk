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
    greetd::{self, Login, Phase, Request, Response},
    time::Clock,
    ui::{GreeterInput, paint_login, paint_startup, paint_wallpaper, show_greeter},
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
    users: Vec<String>,
    /// Submit on the first frame: one user, so go straight to the password
    /// the way the lock screen does.
    auto_submit: bool,
    output: (i32, i32),
    clock: Clock,
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

    /// Applies greetd's answers that arrived since the last frame. `now` is
    /// ms since the first frame.
    fn poll(&mut self, now: u32) {
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
        let request = match input {
            GreeterInput::Submit => self.login.submit(),
            GreeterInput::Back => self.login.back(),
            GreeterInput::None => None,
        };
        self.send(request);
        // greetd's answers arrive between frames; keep drawing so they show.
        ui.ctx().request_repaint();
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
/// /etc/passwd, sorted and without repeats.
fn regular_users() -> Vec<String> {
    let mut users = Vec::new();
    if let Ok(out) = std::process::Command::new("userdbctl")
        .args([
            "user",
            "--disposition=regular",
            "--json=short",
            "--no-pager",
        ])
        .stderr(std::process::Stdio::null())
        .output()
    {
        // One JSON object per record, possibly with RS separators.
        let text = String::from_utf8_lossy(&out.stdout).replace('\u{1e}', "\n");
        for record in serde_json::Deserializer::from_str(&text).into_iter::<serde_json::Value>() {
            let Ok(record) = record else { break };
            if let Some(name) = record.get("userName").and_then(|n| n.as_str()) {
                users.push(name.to_owned());
            }
        }
    }
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        users.extend(passwd_users(&passwd));
    }
    users.sort();
    users.dedup();
    users
}

/// Users in /etc/passwd in the regular UID range (1000–60000, systemd's and
/// most distributions') that have a login shell.
fn passwd_users(passwd: &str) -> impl Iterator<Item = String> + '_ {
    passwd.lines().filter_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        let uid: u32 = fields.get(2)?.parse().ok()?;
        let shell = fields.get(6)?;
        let login = !shell.ends_with("/nologin") && !shell.ends_with("/false");
        ((1000..=60000).contains(&uid) && login).then(|| fields[0].to_owned())
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
    let preset = options
        .user
        .clone()
        .or_else(|| (users.len() == 1).then(|| users[0].clone()));
    let greeter = Greeter {
        auto_submit: preset.is_some(),
        login: Login::new(
            preset.unwrap_or_default(),
            options.command,
            session_env(options.env),
        ),
        users,
        output: (1280, 800),
        clock: Clock::now_local(),
        last_tick: None,
        theme: Theme::default(),
        to_greetd,
        from_greetd,
        next_window: 0,
        quit: false,
        leaving: None,
        commands: Vec::new(),
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
        assert_eq!(passwd_users(passwd).collect::<Vec<_>>(), ["alice"]);
    }

    #[test]
    fn session_env_keeps_overrides() {
        let env = session_env(vec!["XDG_CURRENT_DESKTOP=other".into()]);
        assert!(env.contains(&"XDG_CURRENT_DESKTOP=other".to_owned()));
        assert!(!env.contains(&"XDG_CURRENT_DESKTOP=derisk".to_owned()));
        assert!(env.contains(&"XDG_SESSION_TYPE=wayland".to_owned()));
    }
}
