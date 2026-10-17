//! `derisk setup`: the first-boot setup, before the login screen.
//!
//! Run as root on the seat by the system (LosOS orders it before the display
//! manager). When the machine already has a regular user it exits at once,
//! so it can be started on every boot; otherwise it asks for a language, a
//! keyboard layout, a time zone, a network and the first account (see
//! [`derisk::setup`]), saves them through localectl, timedatectl and homectl,
//! and exits. The display manager then logs that account straight in with
//! the password it was just given ([`crate::first_login`]); every later
//! login goes through the login screen.

use std::{
    io::Read,
    os::unix::net::UnixDatagram,
    process::{Command as Process, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use derisk::{
    locale::{self, Language, Layout},
    setup::{self, Account, Choices, Step, Task},
    wifi::{NetworkPage, Wifi},
    wizard::{self, Nav, Next, Page, Row, ScreenLayout},
};
use egui::{RichText, Ui};
use mcsapi::widgets::Theme;
use mcsapi_compositor::{Command, egui};
use tracing::{info, warn};

use crate::{
    first_login,
    wizard_host::{self, Flow},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What `derisk setup` was asked to do.
#[derive(Debug)]
pub struct Options {
    /// Show the pages even when a regular user exists (for trying them).
    pub force: bool,
    /// Draw the pages but run nothing: no localectl, no account.
    pub dry_run: bool,
    /// The window's size when nested.
    pub size: (i32, i32),
}

impl Default for Options {
    fn default() -> Self {
        Self {
            force: false,
            dry_run: false,
            size: (1280, 800),
        }
    }
}

enum Progress {
    Started(usize),
    Failed(usize, String),
    Done,
}

struct Applying {
    tasks: Vec<Task>,
    current: usize,
    /// Set for good on a failure; `error` is only the dialog saying why,
    /// and "OK" empties it.
    failed: bool,
    error: Option<mcsapi_ui::Error>,
    done: bool,
    events: Receiver<Progress>,
}

struct Setup {
    step: Step,
    dry_run: bool,
    languages: Vec<Language>,
    language: Option<usize>,
    layouts: Vec<Layout>,
    layout: Option<usize>,
    layout_search: String,
    layout_try: String,
    zones: Vec<String>,
    zone: Option<usize>,
    zone_search: String,
    /// Bring the current choice into view the first frame a list shows.
    scroll: bool,
    wifi: Wifi,
    network: NetworkPage,
    account: Account,
    account_error: Option<mcsapi_ui::Error>,
    applying: Option<Applying>,
}

impl Setup {
    fn new(dry_run: bool) -> Self {
        let languages = locale::installed_languages();
        let current = std::env::var("LANG").unwrap_or_default();
        let language = languages
            .iter()
            .position(|l| l.locale == current)
            .or_else(|| languages.iter().position(|l| l.locale.starts_with("en_US")));
        let layouts = locale::installed_layouts();
        let saved = locale::saved_keyboard(std::path::Path::new(locale::X11_KEYBOARD_CONF));
        let wanted = if saved.layout.is_empty() {
            "us".to_owned()
        } else {
            saved.layout
        };
        let layout = layouts.iter().position(|l| l.name == wanted);
        let zones = locale::installed_time_zones();
        let zone_now = locale::current_time_zone();
        let zone = zones.iter().position(|z| *z == zone_now);
        Self {
            step: Step::Language,
            dry_run,
            languages,
            language,
            layouts,
            layout,
            layout_search: String::new(),
            layout_try: String::new(),
            zones,
            zone,
            zone_search: String::new(),
            scroll: true,
            wifi: Wifi::start(!dry_run),
            network: NetworkPage::default(),
            account: Account::default(),
            account_error: None,
            applying: None,
        }
    }

    fn choices(&self) -> Choices {
        Choices {
            locale: self
                .language
                .and_then(|i| self.languages.get(i))
                .map(|l| l.locale.clone())
                .unwrap_or_default(),
            layout: self
                .layout
                .and_then(|i| self.layouts.get(i))
                .map(|l| l.name.clone())
                .unwrap_or_default(),
            time_zone: self
                .zone
                .and_then(|i| self.zones.get(i))
                .cloned()
                .unwrap_or_default(),
            account: self.account.clone(),
            password_age: derisk::password_age::PasswordAge::system(),
            groups: setup::device_groups(std::path::Path::new("/etc/group")),
        }
    }

    fn go(&mut self, step: Step) {
        self.step = step;
        self.scroll = true;
    }

    fn apply(&mut self) {
        let tasks = setup::plan(&self.choices());
        let (tx, events) = mpsc::channel();
        let run = tasks.clone();
        let dry_run = self.dry_run;
        thread::spawn(move || {
            for (i, task) in run.iter().enumerate() {
                let _ = tx.send(Progress::Started(i));
                if dry_run {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    continue;
                }
                if let Err(e) = execute(task) {
                    let _ = tx.send(Progress::Failed(i, e));
                    return;
                }
            }
            let _ = tx.send(Progress::Done);
        });
        self.applying = Some(Applying {
            tasks,
            current: 0,
            failed: false,
            error: None,
            done: false,
            events,
        });
        self.go(Step::Applying);
    }

    /// Leaves the display manager the account just made, so the first
    /// login skips the login screen. Written now, as setup quits, rather than
    /// when the account was made, so it is seconds old when the display
    /// manager takes it however long "All set" stayed on screen. Without it
    /// the login screen simply asks.
    fn leave_login(&self) {
        let path = std::path::Path::new(first_login::PATH);
        // One left behind by a setup that crashed is not this account's.
        let _ = std::fs::remove_file(path);
        if let Err(error) =
            first_login::write(path, &self.account.user_name, &self.account.password)
        {
            warn!(%error, "cannot leave the first login to the display manager");
        }
    }

    fn language_page(&mut self, ui: &mut Ui) {
        if self.languages.is_empty() {
            wizard::notice(ui, "No languages are installed; English it is.", false);
            return;
        }
        let languages = &self.languages;
        let scroll = std::mem::take(&mut self.scroll)
            .then_some(self.language)
            .flatten();
        if let Some(i) = wizard::list(
            ui,
            "languages",
            languages.len(),
            self.language,
            scroll,
            |i| Row {
                title: languages[i].name.clone(),
                detail: languages[i].locale.clone(),
            },
        ) {
            self.language = Some(i);
        }
    }

    fn keyboard_page(&mut self, ui: &mut Ui, out: &mut Vec<Command>) {
        if self.layouts.is_empty() {
            wizard::notice(
                ui,
                "No keyboard layouts were found; the current one stays.",
                false,
            );
            return;
        }
        wizard::field(
            ui,
            "Try it",
            &mut self.layout_try,
            wizard::Entry::Text,
            "Type here to test the layout",
        );
        let search = wizard::field(
            ui,
            "Search",
            &mut self.layout_search,
            wizard::Entry::Text,
            "Layout or language",
        );
        let shown: Vec<usize> = (0..self.layouts.len())
            .filter(|&i| {
                let l = &self.layouts[i];
                locale::matches(
                    &format!("{} {}", l.description, l.name),
                    &self.layout_search,
                )
            })
            .collect();
        let selected = self.layout.and_then(|l| shown.iter().position(|&i| i == l));
        let scroll = (std::mem::take(&mut self.scroll) || search.changed())
            .then_some(selected)
            .flatten();
        let layouts = &self.layouts;
        if let Some(row) = wizard::list(ui, "layouts", shown.len(), selected, scroll, |row| {
            let l = &layouts[shown[row]];
            Row {
                title: l.description.clone(),
                detail: l.name.clone(),
            }
        }) {
            let i = shown[row];
            self.layout = Some(i);
            // Live, so "Try it" types in the layout just picked.
            out.push(Command::Keymap {
                layout: self.layouts[i].name.clone(),
                variant: String::new(),
                options: String::new(),
            });
        }
    }

    fn zone_page(&mut self, ui: &mut Ui) {
        let search = wizard::field(
            ui,
            "Search",
            &mut self.zone_search,
            wizard::Entry::Text,
            "City or region",
        );
        let shown: Vec<usize> = (0..self.zones.len())
            .filter(|&i| {
                let z = &self.zones[i];
                locale::matches(
                    &format!("{} {z}", locale::time_zone_label(z)),
                    &self.zone_search,
                )
            })
            .collect();
        let selected = self.zone.and_then(|z| shown.iter().position(|&i| i == z));
        let scroll = (std::mem::take(&mut self.scroll) || search.changed())
            .then_some(selected)
            .flatten();
        let zones = &self.zones;
        if let Some(row) = wizard::list(ui, "zones", shown.len(), selected, scroll, |row| {
            let z = &zones[shown[row]];
            Row {
                title: locale::time_zone_label(z),
                detail: z.clone(),
            }
        }) {
            self.zone = Some(shown[row]);
        }
    }

    fn account_page(&mut self, ui: &mut Ui) {
        let mut real_name = self.account.real_name.clone();
        if wizard::field(
            ui,
            "Full name",
            &mut real_name,
            wizard::Entry::Text,
            "Ada Lovelace",
        )
        .changed()
        {
            self.account.set_real_name(real_name);
        }
        if wizard::field(
            ui,
            "User name",
            &mut self.account.user_name,
            wizard::Entry::Text,
            "ada",
        )
        .changed()
        {
            self.account.user_name_edited = true;
        }
        wizard::field(
            ui,
            "Password",
            &mut self.account.password,
            wizard::Entry::Secret,
            "Password",
        );
        wizard::field(
            ui,
            "Confirm password",
            &mut self.account.confirm,
            wizard::Entry::Secret,
            "Password again",
        );
        wizard::error(ui, "account-error", &mut self.account_error);
        if let Some(problem) = self.account.problem() {
            // Only once there is something to judge: an empty form is not
            // an error.
            if !self.account.real_name.is_empty() && !self.account.password.is_empty() {
                wizard::notice(ui, problem, false);
            }
        }
    }

    fn applying_page(&mut self, ui: &mut Ui) {
        let Some(applying) = &mut self.applying else {
            return;
        };
        while let Ok(event) = applying.events.try_recv() {
            match event {
                Progress::Started(i) => applying.current = i,
                Progress::Failed(i, e) => {
                    applying.current = i;
                    applying.failed = true;
                    applying.error = Some(wizard::failure(e, "setup-could-not-finish"));
                }
                Progress::Done => {
                    applying.current = applying.tasks.len();
                    applying.done = true;
                }
            }
        }
        let labels: Vec<&str> = applying.tasks.iter().map(|t| t.label).collect();
        wizard::steps(ui, &labels, applying.current, applying.failed);
        wizard::error(ui, "setup-error", &mut applying.error);
        if applying.done {
            ui.add_space(8.0);
            ui.label(RichText::new("All set. Continue to your desktop.").strong());
        }
    }
}

impl Flow for Setup {
    fn show(&mut self, ui: &mut Ui, layout: &ScreenLayout, theme: &Theme, out: &mut Vec<Command>) {
        let status = self.wifi.status();
        let step = self.step;
        let (next, back) = match step {
            Step::Network => (
                Next::new(if status.online { "Next" } else { "Skip" }, true),
                true,
            ),
            Step::Account => (Next::new("Set up", self.account.problem().is_none()), true),
            Step::Applying => {
                let applying = self.applying.as_ref();
                let done = applying.is_some_and(|a| a.done);
                let failed = applying.is_some_and(|a| a.failed);
                (Next::new("Continue", done), failed)
            }
            Step::Language => (
                Next::new("Next", self.language.is_some() || self.languages.is_empty()),
                false,
            ),
            _ => (Next::new("Next", true), step.back().is_some()),
        };
        let page = Page {
            title: step.title(),
            subtitle: step.subtitle(),
            index: (step != Step::Applying).then(|| step.index()),
            count: Step::PAGES.len(),
            back,
            next: Some(next),
        };
        let nav = wizard::page(ui, layout, theme, page, |ui| match step {
            Step::Language => self.language_page(ui),
            Step::Keyboard => self.keyboard_page(ui, out),
            Step::TimeZone => self.zone_page(ui),
            Step::Network => self.network.show(ui, &self.wifi, &status),
            Step::Account => self.account_page(ui),
            Step::Applying => self.applying_page(ui),
        });
        match (nav, step) {
            (Nav::Back, Step::Applying) => {
                // Only after a failure: back to the account, the step most
                // likely refused (the password policy), with the dialog
                // again if "OK" has not closed it yet.
                self.account_error = self.applying.take().and_then(|a| a.error);
                self.go(Step::Account);
            }
            (Nav::Back, _) => {
                if let Some(back) = step.back() {
                    self.go(back);
                }
            }
            (Nav::Next, Step::Account) => {
                self.account_error = None;
                self.apply();
            }
            (Nav::Next, Step::Applying) => {
                info!("setup finished");
                if !self.dry_run {
                    self.leave_login();
                }
                out.push(Command::Quit);
            }
            (Nav::Next, _) => self.go(step.next()),
            (Nav::None, _) => {}
        }
    }
}

/// How often a running task is checked for a password question.
const ASK_POLL: Duration = Duration::from_millis(200);

/// Runs one task, returning what it said on failure. A task that stops to
/// ask for a password is answered "cancel" ([`cancel_questions`]), so it
/// fails with its reason instead of waiting for an answer nobody can give.
fn execute(task: &Task) -> std::result::Result<(), String> {
    let mut child = Process::new(&task.argv[0])
        .args(&task.argv[1..])
        .envs(task.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", task.argv[0]))?;
    let pid = child.id();
    let mut stderr = child.stderr.take();
    let (status, said) = thread::scope(|scope| {
        // Read as it comes, so a chatty task never blocks on a full pipe
        // while this thread waits for it.
        let reader = scope.spawn(move || {
            let mut said = String::new();
            if let Some(stderr) = &mut stderr {
                let _ = stderr.read_to_string(&mut said);
            }
            said
        });
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {}
                Err(e) => break Err(e),
            }
            cancel_questions(pid);
            thread::sleep(ASK_POLL);
        };
        (status, reader.join().unwrap_or_default())
    });
    let status = status.map_err(|e| format!("{}: {e}", task.argv[0]))?;
    if status.success() {
        return Ok(());
    }
    Err(setup::task_failure(&said)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{} failed ({status})", task.argv[0])))
}

/// Answers "cancel" to every password question process `pid` is waiting
/// on (see [`setup::ask_socket`] for why homectl asks one).
fn cancel_questions(pid: u32) {
    let Ok(dir) = std::fs::read_dir(setup::ASK_PASSWORD_DIR) else {
        return;
    };
    for entry in dir.flatten() {
        if !entry.file_name().as_encoded_bytes().starts_with(b"ask.") {
            continue;
        }
        let Ok(ask) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Some(socket) = setup::ask_socket(&ask, pid) {
            warn!(pid, "a setup task asked for a password; cancelling it");
            if let Err(error) =
                UnixDatagram::unbound().and_then(|s| s.send_to(setup::ASK_CANCEL, &socket))
            {
                warn!(%error, "cannot cancel the password question");
            }
        }
    }
}

/// Whether a regular user exists, by userdb (which includes homed's).
fn configured() -> bool {
    Process::new("userdbctl")
        .args([
            "user",
            "--disposition=regular",
            "--json=short",
            "--no-pager",
        ])
        .stderr(Stdio::null())
        .output()
        .map(|o| setup::has_regular_users(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(false)
}

/// Runs the setup, or returns at once when there is nothing to set up.
pub fn run(options: Options) -> Result {
    if !options.force && configured() {
        info!("a regular user exists; nothing to set up");
        return Ok(());
    }
    wizard_host::run(Setup::new(options.dry_run), "derisk setup", options.size)
}
