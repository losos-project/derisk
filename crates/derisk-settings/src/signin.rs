//! The Sign-in page: the password and when it expires, fingerprints,
//! verification codes, and security keys.
//!
//! Everything here applies at once, like the Privacy page's Flatpak
//! permissions, because none of it is derisk's own setting: the password
//! and security keys live in the person's systemd-homed record (changed with
//! `homectl`, which homed lets an owner do to their own account), the
//! fingerprints in fprintd (`fprintd-enroll`), and the codes' secret in
//! `~/.google_authenticator` ([`crate::totp`]). The commands run on a
//! worker thread, each line they print shown as it comes, so a reader
//! asking for the next touch or a key asking to be tapped is seen while it
//! waits.
//!
//! What the system asks for at login is the system's to decide (its PAM
//! stack); this page only sets up what the person brings.

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc,
    time::UNIX_EPOCH,
};

use mcsapi_ui::{Theme, egui};

use crate::totp::{self, Secret};

/// A command the page runs, with secrets passed in its environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    /// What the status line calls it.
    pub label: &'static str,
    /// The command line.
    pub argv: Vec<String>,
    /// `KEY=VALUE` pairs homectl reads instead of asking: `PASSWORD`,
    /// `NEWPASSWORD`, `PIN`. Never on the command line, where `ps` shows it.
    pub env: Vec<(String, String)>,
}

/// Changes `user`'s password from `current` to `new`.
pub fn change_password(user: &str, current: &str, new: &str) -> Job {
    Job {
        label: "Changing the password",
        argv: vec!["homectl".into(), "passwd".into(), user.into()],
        env: vec![
            ("PASSWORD".into(), current.into()),
            ("NEWPASSWORD".into(), new.into()),
        ],
    }
}

/// Makes the security key plugged in now the one that unlocks `user`'s
/// account, with `pin` when it has one. homectl replaces any key set up
/// before: it enrolls the list it is given.
pub fn set_up_key(user: &str, password: &str, pin: &str) -> Job {
    let mut env = vec![("PASSWORD".into(), password.into())];
    if !pin.is_empty() {
        env.push(("PIN".into(), pin.into()));
    }
    Job {
        label: "Setting up the security key",
        argv: vec![
            "homectl".into(),
            "update".into(),
            user.into(),
            "--fido2-device=auto".into(),
            // No terminal to ask on: a missing PIN fails with a message
            // instead of waiting for one.
            "--no-ask-password".into(),
        ],
        env,
    }
}

/// Removes every security key from `user`'s account.
pub fn remove_keys(user: &str, password: &str) -> Job {
    Job {
        label: "Removing security keys",
        argv: vec![
            "homectl".into(),
            "update".into(),
            user.into(),
            "--fido2-device=".into(),
            "--no-ask-password".into(),
        ],
        env: vec![("PASSWORD".into(), password.into())],
    }
}

/// Enrolls `finger` (fprintd's name, such as `right-index-finger`).
pub fn enroll(finger: &str) -> Job {
    Job {
        label: "Adding a fingerprint",
        argv: vec!["fprintd-enroll".into(), "-f".into(), finger.into()],
        env: Vec::new(),
    }
}

/// Deletes all of `user`'s fingerprints.
pub fn delete_fingerprints(user: &str) -> Job {
    Job {
        label: "Removing fingerprints",
        argv: vec!["fprintd-delete".into(), user.into()],
        env: Vec::new(),
    }
}

/// Fingers fprintd can enroll, with their labels, in the order offered.
pub const FINGERS: [(&str, &str); 10] = [
    ("right-index-finger", "Right index finger"),
    ("left-index-finger", "Left index finger"),
    ("right-thumb", "Right thumb"),
    ("left-thumb", "Left thumb"),
    ("right-middle-finger", "Right middle finger"),
    ("left-middle-finger", "Left middle finger"),
    ("right-ring-finger", "Right ring finger"),
    ("left-ring-finger", "Left ring finger"),
    ("right-little-finger", "Right little finger"),
    ("left-little-finger", "Left little finger"),
];

fn finger_label(name: &str) -> &str {
    FINGERS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(name, |(_, label)| label)
}

/// What `fprintd-list` said about the reader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reader {
    /// There is no reader (or no fprintd).
    None,
    /// The fingers enrolled, by fprintd's names.
    Enrolled(Vec<String>),
}

/// Reads `fprintd-list <user>`'s output.
pub fn parse_fprintd_list(output: &str) -> Reader {
    if output.trim().is_empty() || output.contains("No devices available") {
        return Reader::None;
    }
    Reader::Enrolled(
        output
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- #"))
            .filter_map(|rest| rest.split_once(": "))
            .map(|(_, finger)| finger.trim().to_owned())
            .collect(),
    )
}

/// Says what one of `fprintd-enroll`'s lines means for the person at the
/// reader, or `None` for lines that are not news.
pub fn enroll_message(line: &str) -> Option<&'static str> {
    let result = line.trim().strip_prefix("Enroll result: ")?;
    Some(match result {
        "enroll-stage-passed" => "Got it. Lift your finger and place it again.",
        "enroll-completed" => "Fingerprint added.",
        "enroll-retry-scan" => "That scan didn't take. Place your finger again.",
        "enroll-swipe-too-short" => "Swipe too short. Try again.",
        "enroll-finger-not-centered" => "Center your finger on the reader and try again.",
        "enroll-remove-and-retry" => "Lift your finger and try again.",
        "enroll-duplicate" => "That finger is already enrolled.",
        "enroll-data-full" => "The reader has no room for another fingerprint.",
        "enroll-disconnected" => "The reader was disconnected.",
        _ => "The fingerprint could not be added.",
    })
}

/// What the person's homed record says, for this page.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Account {
    /// Whether homed knows the account; the password and security key
    /// sections need it.
    pub homed: bool,
    /// Security keys that unlock it.
    pub keys: usize,
    /// When the password expires.
    pub expiry: Option<Expiry>,
}

/// Where a password stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expiry {
    /// No maximum age is set.
    Never,
    /// It expires in this many whole days (0: within a day).
    InDays(u64),
    /// It has expired; the next login asks for a new one.
    Expired,
}

const DAY_USEC: u64 = 24 * 60 * 60 * 1_000_000;

impl Account {
    /// Reads `homectl inspect --json=short` output at `now_usec`
    /// (microseconds since the epoch, homed's unit).
    pub fn parse(json: &str, now_usec: u64) -> Self {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };
        if record.get("userName").is_none() {
            return Self::default();
        }
        let field = |name: &str| record.get(name).and_then(serde_json::Value::as_u64);
        let expiry = match (
            field("passwordChangeMaxUSec"),
            field("lastPasswordChangeUSec"),
        ) {
            (Some(max), Some(changed)) if max != u64::MAX => {
                let ends = changed.saturating_add(max);
                if ends <= now_usec {
                    Expiry::Expired
                } else {
                    Expiry::InDays((ends - now_usec) / DAY_USEC)
                }
            }
            _ => Expiry::Never,
        };
        Self {
            homed: true,
            keys: record
                .get("fido2HmacCredential")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len),
            expiry: Some(expiry),
        }
    }
}

fn now() -> std::time::Duration {
    derisk_sys::time::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// What a running job sends back.
enum Event {
    Line(String),
    Done(Result<(), String>),
}

/// A job running on its worker thread.
pub(crate) struct Running {
    label: &'static str,
    events: mpsc::Receiver<Event>,
    /// The latest line worth showing.
    last: Option<String>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("label", &self.label)
            .finish()
    }
}

impl Running {
    /// What the status line says while it runs: its latest line, or what
    /// it is.
    pub(crate) fn shown(&self) -> &str {
        self.last.as_deref().unwrap_or(self.label)
    }

    /// Takes the lines printed since the last frame. Once the job is done,
    /// returns the status line and whether it reports a failure: the job's
    /// last line, which is homectl's or fprintd's reason, or its exit.
    pub(crate) fn poll(&mut self) -> Option<(String, bool)> {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Line(line) => {
                    let shown = enroll_message(&line).map(str::to_owned).or_else(|| {
                        // homectl's own lines, minus its log prefixes.
                        let line = line.trim();
                        (!line.is_empty() && !line.starts_with("Enrolling "))
                            .then(|| line.to_owned())
                    });
                    if shown.is_some() {
                        self.last = shown;
                    }
                }
                Event::Done(result) => {
                    let last = self.last.take();
                    return Some(match result {
                        Ok(()) => (format!("{}: done", self.label), false),
                        Err(e) => (last.unwrap_or(e), true),
                    });
                }
            }
        }
        None
    }
}

/// Runs `job` on a worker thread, reading what it prints as it comes.
///
/// homectl reads secrets from its environment once. When homed refuses the
/// one it was given (a new password its quality check rejects), homectl asks
/// for another, and with no terminal it asks a password agent with no time
/// limit; nothing on the desktop answers those, so the job would never end.
/// While it runs, every question it asks is answered "cancel", and homectl
/// exits with homed's reason as its last line.
pub(crate) fn spawn(job: Job) -> Running {
    let (tx, events) = mpsc::channel();
    let label = job.label;
    derisk_sys::thread::detach(move || {
        let child = Command::new(&job.argv[0])
            .args(&job.argv[1..])
            .envs(job.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                let _ = tx.send(Event::Done(Err(format!("cannot run {}: {e}", job.argv[0]))));
                return;
            }
        };
        let pid = child.id();
        let done = std::sync::atomic::AtomicBool::new(false);
        let finished = std::thread::scope(|scope| {
            // stderr carries homectl's messages ("Please confirm presence on
            // security token"), stdout fprintd's; both are read as they come.
            if let Some(err) = child.stderr.take() {
                let tx = tx.clone();
                scope.spawn(move || {
                    for line in BufReader::new(err).lines().map_while(Result::ok) {
                        let _ = tx.send(Event::Line(line));
                    }
                });
            }
            scope.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    cancel_questions(pid);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            });
            if let Some(out) = child.stdout.take() {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = tx.send(Event::Line(line));
                }
            }
            let finished = child.wait();
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            finished
        });
        let done = match finished {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("{} failed ({status})", job.argv[0])),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(Event::Done(done));
    });
    Running {
        label,
        events,
        last: None,
    }
}

/// Answers "cancel" to each password question process `pid` is waiting on,
/// in the system's agent directory and the user's own
/// ([`derisk_install::account::ask_socket`]).
#[cfg(unix)]
fn cancel_questions(pid: u32) {
    use derisk_install::account;
    let user = derisk_sys::env::var_os("XDG_RUNTIME_DIR")
        .map(|dir| std::path::PathBuf::from(dir).join("systemd/ask-password"));
    let dirs = std::iter::once(std::path::PathBuf::from(account::ASK_PASSWORD_DIR)).chain(user);
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_name().as_encoded_bytes().starts_with(b"ask.") {
                continue;
            }
            let Ok(ask) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            if let Some(socket) = account::ask_socket(&ask, pid) {
                let _ = std::os::unix::net::UnixDatagram::unbound()
                    .and_then(|s| s.send_to(account::ASK_CANCEL, &socket));
            }
        }
    }
}

/// A browser page runs no commands, so nothing ever asks.
#[cfg(not(unix))]
fn cancel_questions(_pid: u32) {}

/// Setting up verification codes: the new secret, until a code from the
/// app proves it was scanned.
struct CodeSetup {
    secret: Secret,
    qr: Option<qrcode::QrCode>,
    typed: String,
}

/// The page's state.
#[derive(Default)]
pub(crate) struct SigninUi {
    loaded: bool,
    user: String,
    account: Account,
    reader: Option<Reader>,
    codes_on: bool,
    code_setup: Option<CodeSetup>,
    /// Recovery codes, shown once after codes are turned on.
    recovery: Option<Vec<u32>>,
    finger: usize,
    current: String,
    new: String,
    confirm: String,
    key_password: String,
    key_pin: String,
    running: Option<Running>,
    status: Option<(String, bool)>,
}

impl std::fmt::Debug for SigninUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No secrets in debug output.
        f.debug_struct("SigninUi")
            .field("user", &self.user)
            .field("account", &self.account)
            .field("reader", &self.reader)
            .field("codes_on", &self.codes_on)
            .finish_non_exhaustive()
    }
}

/// Empties a field that held a secret, overwriting its bytes first: the
/// buffer keeps its capacity, so the spaces land where the secret was.
pub(crate) fn wipe(text: &mut String) {
    let len = text.len();
    text.clear();
    text.extend(std::iter::repeat_n(' ', len));
    text.clear();
}

impl SigninUi {
    /// Reads where everything stands; again after each job.
    fn load(&mut self) {
        self.loaded = true;
        self.user = derisk_sys::env::var("USER").unwrap_or_default();
        let output = |argv: &[&str]| {
            Command::new(argv[0])
                .args(&argv[1..])
                .stderr(Stdio::null())
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                .unwrap_or_default()
        };
        let now_usec = u64::try_from(now().as_micros()).unwrap_or(u64::MAX);
        self.account = Account::parse(
            &output(&[
                "homectl",
                "inspect",
                &self.user,
                "--json=short",
                "--no-pager",
            ]),
            now_usec,
        );
        self.reader = Some(parse_fprintd_list(&output(&["fprintd-list", &self.user])));
        self.codes_on = totp::file_path().is_some_and(derisk_sys::fs::exists);
    }

    fn run(&mut self, job: Job) {
        self.status = None;
        self.running = Some(spawn(job));
    }

    fn poll(&mut self) {
        let Some(running) = &mut self.running else {
            return;
        };
        if let Some(result) = running.poll() {
            self.status = Some(result);
            self.running = None;
            self.load();
        }
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(crate::layout::note(
        &mcsapi_components::Tokens::current(ui.ctx()),
        text,
    ));
}

fn secret_field(ui: &mut egui::Ui, text: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(text)
            .password(true)
            .hint_text(hint),
    );
}

/// Draws `code` as squares, dark on light whatever the theme, since that
/// is what cameras read.
fn qr(ui: &mut egui::Ui, code: &qrcode::QrCode) {
    let width = code.width();
    let quiet = 4;
    let side = (width + 2 * quiet) as f32;
    let size = 200.0_f32.min(ui.available_width());
    let module = size / side;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 0, egui::Color32::WHITE);
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let (x, y) = ((i % width + quiet) as f32, (i / width + quiet) as f32);
            let min = rect.min + egui::vec2(x * module, y * module);
            painter.rect_filled(
                egui::Rect::from_min_size(min, egui::vec2(module, module)),
                0,
                egui::Color32::BLACK,
            );
        }
    }
}

pub(crate) fn page(ui: &mut egui::Ui, state: &mut SigninUi, theme: &Theme) {
    if !state.loaded {
        state.load();
    }
    state.poll();
    let busy = state.running.is_some();
    if let Some(running) = &state.running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(running.shown());
        });
        ui.add_space(8.0);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    } else if let Some((status, error)) = &state.status {
        let color = if *error {
            egui::Color32::from_rgb(248, 113, 113)
        } else {
            theme.accent
        };
        ui.label(egui::RichText::new(status).color(color));
        ui.add_space(8.0);
    }
    ui.add_enabled_ui(!busy, |ui| {
        password(ui, state, theme);
        ui.add_space(16.0);
        fingerprints(ui, state, theme);
        ui.add_space(16.0);
        codes(ui, state, theme);
        ui.add_space(16.0);
        keys(ui, state, theme);
    });
}

fn section(ui: &mut egui::Ui, theme: &Theme, title: &str) {
    ui.heading(egui::RichText::new(title).color(theme.foreground));
}

fn password(ui: &mut egui::Ui, state: &mut SigninUi, theme: &Theme) {
    section(ui, theme, "Password");
    if !state.account.homed {
        hint(ui, "Your account's password is managed outside derisk.");
        return;
    }
    hint(
        ui,
        match state.account.expiry {
            Some(Expiry::Expired) => {
                "Your password has expired. Change it now, or you'll be asked at your next login."
            }
            Some(Expiry::InDays(0)) => "Your password expires today.",
            Some(Expiry::InDays(1)) => "Your password expires tomorrow.",
            Some(Expiry::InDays(_)) => "",
            _ => "Your password does not expire.",
        },
    );
    if let Some(Expiry::InDays(days)) = state.account.expiry
        && days > 1
    {
        hint(ui, &format!("Your password expires in {days} days."));
    }
    egui::Grid::new("signin-password")
        .num_columns(2)
        .spacing([24.0, 8.0])
        .show(ui, |ui| {
            ui.label("Current password");
            secret_field(ui, &mut state.current, "");
            ui.end_row();
            ui.label("New password");
            secret_field(ui, &mut state.new, "");
            ui.end_row();
            ui.label("Again");
            secret_field(ui, &mut state.confirm, "");
            ui.end_row();
        });
    let mismatch = !state.confirm.is_empty() && state.new != state.confirm;
    if mismatch {
        hint(ui, "The new passwords do not match.");
    }
    let ready = !state.current.is_empty() && !state.new.is_empty() && state.new == state.confirm;
    if ui
        .add_enabled(ready, egui::Button::new("Change password"))
        .clicked()
    {
        let job = change_password(&state.user, &state.current, &state.new);
        wipe(&mut state.current);
        wipe(&mut state.new);
        wipe(&mut state.confirm);
        state.run(job);
    }
}

fn fingerprints(ui: &mut egui::Ui, state: &mut SigninUi, theme: &Theme) {
    section(ui, theme, "Fingerprints");
    let enrolled = match &state.reader {
        Some(Reader::Enrolled(fingers)) => fingers.clone(),
        _ => {
            hint(ui, "No fingerprint reader was found.");
            return;
        }
    };
    hint(
        ui,
        "A fingerprint unlocks the lock screen and answers administrator prompts. Logging in still takes your password, which is what unlocks your files.",
    );
    if enrolled.is_empty() {
        ui.label("No fingerprints yet.");
    } else {
        let names: Vec<&str> = enrolled.iter().map(|f| finger_label(f)).collect();
        ui.label(names.join(", "));
    }
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("signin-finger")
            .selected_text(FINGERS[state.finger].1)
            .show_ui(ui, |ui| {
                for (i, (_, label)) in FINGERS.iter().enumerate() {
                    ui.selectable_value(&mut state.finger, i, *label);
                }
            });
        if ui.button("Add fingerprint").clicked() {
            state.run(enroll(FINGERS[state.finger].0));
        }
        if !enrolled.is_empty() && ui.button("Remove all").clicked() {
            state.run(delete_fingerprints(&state.user));
        }
    });
}

fn codes(ui: &mut egui::Ui, state: &mut SigninUi, theme: &Theme) {
    section(ui, theme, "Verification codes");
    hint(
        ui,
        "Logging in asks for a code from an authenticator app after your password.",
    );
    if let Some(recovery) = &state.recovery {
        ui.label("Recovery codes. Each one works once instead of a code. Keep them somewhere safe; they are not shown again.");
        let text: Vec<String> = recovery.iter().map(u32::to_string).collect();
        ui.monospace(text.join("  "));
        if ui.button("I saved them").clicked() {
            state.recovery = None;
        }
        return;
    }
    if let Some(setup) = &mut state.code_setup {
        ui.label("Scan this with your authenticator app, or type the key in.");
        if let Some(code) = &setup.qr {
            qr(ui, code);
        }
        let key = setup.secret.base32();
        let grouped: Vec<String> = key
            .as_bytes()
            .chunks(4)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        ui.monospace(grouped.join(" "));
        let mut turn_on = false;
        let mut cancel = false;
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut setup.typed)
                    .hint_text("Code from the app")
                    .desired_width(140.0),
            );
            // Only a code the app made proves it holds the secret, so a
            // mistyped key cannot lock anyone out at their next login.
            let verified = setup.secret.verify(&setup.typed, now().as_secs());
            turn_on = ui
                .add_enabled(verified, egui::Button::new("Turn on"))
                .clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if turn_on {
            let secret = setup.secret.clone();
            turn_codes_on(state, &secret);
        } else if cancel {
            state.code_setup = None;
        }
        return;
    }
    if state.codes_on {
        ui.label("On.");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Set up again").clicked() {
                start_codes(state);
            }
            if ui.button("Turn off").clicked() {
                state.status = Some(match totp::file_path().map(|p| totp::remove_file(&p)) {
                    Some(Ok(())) => ("Verification codes are off".into(), false),
                    Some(Err(e)) => (format!("Could not turn codes off: {e}"), true),
                    None => ("HOME is not set".into(), true),
                });
                state.codes_on = totp::file_path().is_some_and(derisk_sys::fs::exists);
            }
        });
    } else if ui.button("Set up codes").clicked() {
        start_codes(state);
    }
}

fn turn_codes_on(state: &mut SigninUi, secret: &Secret) {
    let result = totp::recovery_codes().and_then(|recovery| {
        let path = totp::file_path().ok_or_else(|| std::io::Error::other("HOME is not set"))?;
        totp::write_file(&path, &totp::file_text(secret, &recovery))?;
        Ok(recovery)
    });
    match result {
        Ok(recovery) => {
            state.recovery = Some(recovery);
            state.status = Some(("Verification codes are on".into(), false));
        }
        Err(e) => state.status = Some((format!("Could not turn codes on: {e}"), true)),
    }
    state.code_setup = None;
    state.codes_on = totp::file_path().is_some_and(derisk_sys::fs::exists);
}

fn start_codes(state: &mut SigninUi) {
    match Secret::generate() {
        Ok(secret) => {
            let host = derisk_sys::fs::read_to_string("/etc/hostname")
                .map(|h| h.trim().to_owned())
                .unwrap_or_default();
            let issuer = if host.is_empty() {
                "LosOS".to_owned()
            } else {
                format!("LosOS on {host}")
            };
            let uri = secret.uri(&state.user, &issuer);
            let qr = qrcode::QrCode::new(uri.as_bytes()).ok();
            state.code_setup = Some(CodeSetup {
                secret,
                qr,
                typed: String::new(),
            });
        }
        Err(e) => state.status = Some((format!("No random source: {e}"), true)),
    }
}

fn keys(ui: &mut egui::Ui, state: &mut SigninUi, theme: &Theme) {
    section(ui, theme, "Security keys");
    if !state.account.homed {
        hint(ui, "Security keys need an account made by LosOS setup.");
        return;
    }
    hint(
        ui,
        "A FIDO2 security key (a passkey on a key) unlocks your account at login and on the lock screen in place of your password: plug it in, enter its PIN and touch it.",
    );
    ui.label(match state.account.keys {
        0 => "No security key is set up.".to_owned(),
        1 => "One security key is set up.".to_owned(),
        n => format!("{n} security keys are set up."),
    });
    egui::Grid::new("signin-keys")
        .num_columns(2)
        .spacing([24.0, 8.0])
        .show(ui, |ui| {
            ui.label("Your password");
            secret_field(ui, &mut state.key_password, "");
            ui.end_row();
            ui.label("Key PIN");
            secret_field(ui, &mut state.key_pin, "If it has one");
            ui.end_row();
        });
    let ready = !state.key_password.is_empty();
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(ready, egui::Button::new("Set up the key plugged in"))
            .clicked()
        {
            let job = set_up_key(&state.user, &state.key_password, &state.key_pin);
            wipe(&mut state.key_password);
            wipe(&mut state.key_pin);
            state.run(job);
        }
        if state.account.keys > 0
            && ui
                .add_enabled(ready, egui::Button::new("Remove keys"))
                .clicked()
        {
            let job = remove_keys(&state.user, &state.key_password);
            wipe(&mut state.key_password);
            wipe(&mut state.key_pin);
            state.run(job);
        }
    });
    hint(
        ui,
        "Setting up a key replaces the one set up before. Your password keeps working.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_go_in_the_environment() {
        let job = change_password("ada", &"o".repeat(6), &"n".repeat(6));
        assert_eq!(job.argv, ["homectl", "passwd", "ada"]);
        assert!(job.argv.iter().all(|a| !a.contains("oooooo")));
        assert_eq!(job.env[1], ("NEWPASSWORD".to_owned(), "n".repeat(6)));

        let key = set_up_key("ada", &"p".repeat(6), "");
        assert_eq!(
            key.argv,
            [
                "homectl",
                "update",
                "ada",
                "--fido2-device=auto",
                "--no-ask-password"
            ]
        );
        assert_eq!(key.env.len(), 1, "no PIN, no PIN variable");
        // Built rather than written out, like the password above: a
        // literal passed as a password reads to CodeQL as a real one.
        let (password, pin) = ("x".repeat(8), "1".repeat(4));
        assert_eq!(set_up_key("ada", &password, &pin).env[1].0, "PIN");
        assert_eq!(remove_keys("ada", &password).argv[3], "--fido2-device=");
    }

    #[test]
    fn fprintd_list_is_read() {
        let enrolled = "found 1 devices\nDevice at /net/reactivated/Fprint/Device/0\n\
                        Using device /net/reactivated/Fprint/Device/0\n\
                        Fingerprints for user ada on Synaptics (press):\n \
                        - #0: right-index-finger\n - #1: left-thumb\n";
        assert_eq!(
            parse_fprintd_list(enrolled),
            Reader::Enrolled(vec!["right-index-finger".into(), "left-thumb".into()])
        );
        let none = "found 1 devices\nDevice at /net/reactivated/Fprint/Device/0\n\
                    User ada has no fingers enrolled for Synaptics.\n";
        assert_eq!(parse_fprintd_list(none), Reader::Enrolled(Vec::new()));
        assert_eq!(parse_fprintd_list("No devices available\n"), Reader::None);
        assert_eq!(parse_fprintd_list(""), Reader::None);
        assert_eq!(finger_label("left-thumb"), "Left thumb");
    }

    #[test]
    fn enroll_lines_become_instructions() {
        assert_eq!(enroll_message("Enrolling right-index-finger finger."), None);
        assert_eq!(
            enroll_message("Enroll result: enroll-stage-passed"),
            Some("Got it. Lift your finger and place it again.")
        );
        assert_eq!(
            enroll_message("Enroll result: enroll-completed"),
            Some("Fingerprint added.")
        );
        assert_eq!(
            enroll_message("Enroll result: something-new"),
            Some("The fingerprint could not be added.")
        );
    }

    #[test]
    fn records_say_when_the_password_expires() {
        let record = format!(
            r#"{{"userName":"ada","passwordChangeMaxUSec":{},"lastPasswordChangeUSec":{},"fido2HmacCredential":["a","b"]}}"#,
            10 * DAY_USEC,
            100 * DAY_USEC
        );
        let at = |day: u64| Account::parse(&record, day * DAY_USEC);
        assert_eq!(at(100).expiry, Some(Expiry::InDays(10)));
        assert_eq!(at(109).expiry, Some(Expiry::InDays(1)));
        assert_eq!(at(110).expiry, Some(Expiry::Expired));
        assert_eq!(at(100).keys, 2);
        assert!(at(100).homed);
        let plain = Account::parse(r#"{"userName":"ada"}"#, 0);
        assert_eq!(plain.expiry, Some(Expiry::Never));
        assert_eq!(Account::parse("", 0), Account::default());
    }
}
