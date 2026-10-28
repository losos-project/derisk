//! The Backups page: backups of the home to a LosOS box on the local
//! network, the project's home server.
//!
//! Git repositories in the home go to the box's LosOS Git, each as a private
//! repository, and so do the configs (`~/.config` and the home's dotfiles),
//! as the history of one private repository of their own. Everything else
//! goes to the person's LosOS cloud. None of
//! it is derisk's own setting: `losos-backup`, which the OS runs from a user
//! timer, keeps its own, and this page runs it. `losos-backup status` says
//! where things stand as JSON, `choose` pins a box's certificate, `sign-in
//! box` signs in to both apps through LosOS Login, printing a link and a
//! code for this page to show while the person approves them in a browser,
//! `sign-in cloud` trades a password (on its standard input, never the
//! command line) for an app password, and `on` and `off` turn the
//! daily backup on and off. "Back up now" starts the user unit
//! `losos-backup-now.service`, so the run goes on after Settings closes.
//! Like the Sign-in page, all of it applies at once.
//!
//! On a PC the backups reach the box through the LosOS boxes machine, the
//! one machine that talks to boxes; the boxes offered are the ones it sees.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mcsapi_ui::{Theme, egui};
use serde_json::Value;

/// The last run, as `losos-backup status` reports it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Run {
    /// When it finished, in seconds since the epoch.
    pub finished: u64,
    /// `done`, `failed` or `skipped`.
    pub outcome: String,
    /// Why it failed or was skipped.
    pub message: String,
    /// Files uploaded, and their bytes.
    pub uploaded: u64,
    /// See `uploaded`.
    pub bytes: u64,
    /// Files and folders removed from LosOS cloud.
    pub deleted: u64,
    /// Files already up to date.
    pub unchanged: u64,
    /// Files that could not be backed up.
    pub failed: u64,
    /// The first of those, each with why.
    pub failures: Vec<String>,
    /// Repositories pushed.
    pub repos: u64,
    /// Config files in LosOS Git's configs repository, or 0 when they went
    /// to LosOS cloud.
    pub configs: u64,
    /// Whether the run committed a change to them.
    pub configs_changed: bool,
    /// Repositories that could not be, each with why.
    pub repo_failures: Vec<String>,
}

/// Where the backups stand.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Status {
    /// The box chosen, or empty.
    pub box_name: String,
    /// Its certificate's fingerprint.
    pub fingerprint: String,
    /// Whether the daily backup is on.
    pub enabled: bool,
    /// The LosOS cloud account signed in to.
    pub cloud: Option<String>,
    /// The LosOS Git account signed in to.
    pub git: Option<String>,
    /// Whether a backup is going now.
    pub running: bool,
    /// The last run.
    pub last: Option<Run>,
    /// When the last backup that went through finished, or 0.
    pub last_done: u64,
    /// Whether this computer reaches boxes through a LosOS boxes machine.
    pub routed: bool,
    /// The LosOS boxes machines it has.
    pub machines: Vec<String>,
    /// The boxes it can choose now.
    pub boxes: Vec<String>,
}

/// Reads `losos-backup status`.
pub fn parse_status(json: &str) -> Option<Status> {
    let v: Value = serde_json::from_str(json).ok()?;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let number = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
    let list = |v: &Value, key: &str| -> Vec<String> {
        v.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let account = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(Status {
        box_name: text(&v, "box"),
        fingerprint: text(&v, "fingerprint"),
        enabled: v.get("enabled").and_then(Value::as_bool) == Some(true),
        cloud: account("cloud"),
        git: account("git"),
        running: v.get("running").and_then(Value::as_bool) == Some(true),
        last: v.get("last").filter(|l| l.is_object()).map(|l| Run {
            finished: number(l, "finished"),
            outcome: text(l, "outcome"),
            message: text(l, "message"),
            uploaded: number(l, "uploaded"),
            bytes: number(l, "bytes"),
            deleted: number(l, "deleted"),
            unchanged: number(l, "unchanged"),
            failed: number(l, "failed"),
            failures: list(l, "failures"),
            repos: number(l, "repos"),
            configs: number(l, "configs"),
            configs_changed: l.get("configs_changed").and_then(Value::as_bool) == Some(true),
            repo_failures: list(l, "repo_failures"),
        }),
        last_done: number(&v, "last_done"),
        routed: v.get("routed").and_then(Value::as_bool) == Some(true),
        machines: list(&v, "machines"),
        boxes: list(&v, "boxes"),
    })
}

/// How long ago `then` was, from `now`, in words.
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3600 => (secs / 60, "minute"),
        3600..86_400 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// `bytes` in the largest unit that keeps a number above one.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// What the last run did, in one line.
pub fn summary(run: &Run) -> String {
    let mut parts = Vec::new();
    if run.uploaded > 0 {
        parts.push(format!(
            "{} file{} sent ({})",
            run.uploaded,
            if run.uploaded == 1 { "" } else { "s" },
            size(run.bytes)
        ));
    }
    if run.unchanged > 0 {
        parts.push(format!("{} unchanged", run.unchanged));
    }
    if run.deleted > 0 {
        parts.push(format!("{} removed", run.deleted));
    }
    if run.repos > 0 {
        parts.push(format!(
            "{} repositor{}",
            run.repos,
            if run.repos == 1 { "y" } else { "ies" }
        ));
    }
    if run.configs > 0 {
        parts.push(format!(
            "{} config file{} {}",
            run.configs,
            if run.configs == 1 { "" } else { "s" },
            if run.configs_changed {
                "committed"
            } else {
                "unchanged"
            }
        ));
    }
    if parts.is_empty() {
        "Nothing to send.".into()
    } else {
        let mut line = parts.join(", ");
        line.push('.');
        line
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Runs `argv` with `stdin`, returning what it printed, or the last line
/// of what it said went wrong.
fn run(argv: &[String], stdin: Option<String>) -> Result<String, String> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run {}: {e}", argv[0]))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{} failed: {e}", argv[0]))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    // losos-backup reports through miette, whose line starts with a cross.
    let err = String::from_utf8_lossy(&out.stderr);
    Err(err
        .lines()
        .map(|l| l.trim().trim_start_matches('×').trim())
        .rfind(|l| !l.is_empty())
        .map_or_else(|| format!("{} failed", argv[0]), str::to_owned))
}

/// LosOS Login's step of `losos-backup sign-in box`: the page to open and
/// the code to enter there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Prompt {
    /// LosOS Login's page with the code in it.
    pub link: String,
    /// The code, to compare with the page or type into it.
    pub code: String,
}

/// Reads the line `losos-backup sign-in box` prints first.
pub fn parse_prompt(line: &str) -> Option<Prompt> {
    let v: Value = serde_json::from_str(line).ok()?;
    let link = v.get("link")?.as_str()?;
    // Only LosOS Login's page goes to the browser, never an option.
    if !link.starts_with("https://") {
        return None;
    }
    Some(Prompt {
        link: link.to_owned(),
        code: v.get("code")?.as_str()?.to_owned(),
    })
}

/// [`run`] for `sign-in box`: the prompt goes to `prompt` as soon as it is
/// printed, and the result when the person has approved it or it failed.
fn run_sign_in(argv: &[String], prompt: &mpsc::Sender<Prompt>) -> Result<String, String> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run {}: {e}", argv[0]))?;
    let mut last = String::new();
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(p) = parse_prompt(&line) {
                let _ = prompt.send(p);
            } else {
                last = line;
            }
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{} failed: {e}", argv[0]))?;
    if out.status.success() {
        return Ok(last);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(err
        .lines()
        .map(|l| l.trim().trim_start_matches('×').trim())
        .rfind(|l| !l.is_empty())
        .map_or_else(|| format!("{} failed", argv[0]), str::to_owned))
}

fn backup(args: &[&str]) -> Vec<String> {
    std::iter::once("losos-backup")
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// The page's state.
#[derive(Debug, Default)]
pub(crate) struct BackupUi {
    status: Option<Status>,
    /// No `losos-backup` on this system.
    missing: bool,
    reading: Option<mpsc::Receiver<Result<String, String>>>,
    read_at: Option<Instant>,
    job: Option<(&'static str, mpsc::Receiver<Result<String, String>>)>,
    message: Option<(String, bool)>,
    box_name: String,
    cloud_login: String,
    cloud_password: String,
    /// LosOS Login's link and code while `sign-in box` waits.
    prompt: Option<Prompt>,
    prompts: Option<mpsc::Receiver<Prompt>>,
}

impl BackupUi {
    fn refresh(&mut self) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(run(&backup(&["status"]), None));
        });
        self.reading = Some(rx);
        self.read_at = Some(Instant::now());
    }

    fn start(&mut self, label: &'static str, argv: Vec<String>, stdin: Option<String>) {
        let (tx, rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(run(&argv, stdin));
        });
        self.message = None;
        self.job = Some((label, rx));
    }

    fn sign_in_box(&mut self) {
        let (tx, rx) = mpsc::channel();
        let (prompt_tx, prompt_rx) = mpsc::channel();
        derisk_sys::thread::detach(move || {
            let _ = tx.send(run_sign_in(&backup(&["sign-in", "box"]), &prompt_tx));
        });
        self.message = None;
        self.prompt = None;
        self.prompts = Some(prompt_rx);
        self.job = Some(("sign-in box", rx));
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.reading
            && let Ok(result) = rx.try_recv()
        {
            match result.ok().as_deref().and_then(parse_status) {
                Some(status) => {
                    // The field starts at the box chosen, or the first one
                    // found, and is the person's to type in after that.
                    if self.status.is_none() {
                        self.box_name = if status.box_name.is_empty() {
                            status.boxes.first().cloned().unwrap_or_default()
                        } else {
                            status.box_name.clone()
                        };
                    }
                    self.status = Some(status);
                    self.missing = false;
                }
                None => self.missing = self.status.is_none(),
            }
            self.reading = None;
        }
        if let Some(rx) = &self.prompts
            && let Ok(prompt) = rx.try_recv()
        {
            self.prompt = Some(prompt);
        }
        if let Some((label, rx)) = &self.job
            && let Ok(result) = rx.try_recv()
        {
            let label = *label;
            self.message = Some(match result {
                Ok(out) if label == "choose" => (
                    format!(
                        "Chose {}. Its certificate's fingerprint is {}; it should match the one on the box's own pages.",
                        self.box_name,
                        out.trim()
                    ),
                    false,
                ),
                Ok(out) if label == "sign-in box" => (signed_in_message(&out), false),
                Ok(_) => (format!("{}: done", job_label(label)), false),
                Err(e) => (e, true),
            });
            // A password typed is gone once it was tried.
            if label.starts_with("sign-in") {
                self.cloud_password.clear();
            }
            self.prompt = None;
            self.prompts = None;
            self.job = None;
            self.refresh();
        }
        // Every few seconds while the page is open, and every second while
        // a backup runs, so its end shows without a click.
        let every = if self.status.as_ref().is_some_and(|s| s.running) {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        };
        if self.reading.is_none() && self.read_at.is_none_or(|t| t.elapsed() > every) {
            self.refresh();
        }
    }
}

/// What `sign-in box` signed in to, from the line it prints last.
pub fn signed_in_message(out: &str) -> String {
    let v: Value = serde_json::from_str(out.trim()).unwrap_or_default();
    let name = |app: &str| v.get(app).and_then(Value::as_str).map(str::to_owned);
    let mut line = match (name("cloud"), name("git")) {
        (Some(c), Some(g)) if c == g => format!("Signed in to LosOS cloud and LosOS Git as {c}."),
        (Some(c), Some(g)) => {
            format!("Signed in to LosOS cloud as {c} and to LosOS Git as {g}.")
        }
        (Some(c), None) => format!("Signed in to LosOS cloud as {c}."),
        (None, Some(g)) => format!("Signed in to LosOS Git as {g}."),
        (None, None) => "Signed in.".to_owned(),
    };
    if v.get("gitOff").and_then(Value::as_bool) == Some(true) {
        line.push_str(" This box has no LosOS Git, so repositories go to LosOS cloud as files.");
    }
    for problem in v
        .get("problems")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        line.push(' ');
        line.push_str(problem);
    }
    line
}

fn job_label(label: &str) -> &'static str {
    match label {
        "sign-in box" => "Waiting for LosOS Login",
        "sign-in cloud" => "Signing in to LosOS cloud",
        "sign-out" => "Signing out",
        "now" => "Starting a backup",
        "on" => "Turning backups on",
        "off" => "Turning backups off",
        _ => "Choosing the box",
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(crate::layout::note(
        &mcsapi_components::Tokens::current(ui.ctx()),
        text,
    ));
}

const RED: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);

fn section(ui: &mut egui::Ui, theme: &Theme, title: &str) {
    ui.heading(egui::RichText::new(title).color(theme.foreground));
}

pub(crate) fn page(ui: &mut egui::Ui, state: &mut BackupUi, theme: &Theme) {
    state.poll();
    ui.ctx().request_repaint_after(Duration::from_millis(500));
    if state.missing {
        hint(ui, "This system has no backups to a LosOS box.");
        return;
    }
    let Some(status) = state.status.clone() else {
        ui.spinner();
        return;
    };
    if let Some((label, _)) = &state.job {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("{}…", job_label(label)));
        });
        if let Some(prompt) = state.prompt.clone() {
            login_step(ui, &prompt, theme);
        }
        ui.add_space(8.0);
    } else if let Some((message, error)) = &state.message {
        ui.label(egui::RichText::new(message).color(if *error { RED } else { theme.accent }));
        ui.add_space(8.0);
    }
    let busy = state.job.is_some();
    ui.add_enabled_ui(!busy, |ui| {
        last_backup(ui, state, &status, theme);
        ui.add_space(16.0);
        the_box(ui, state, &status, theme);
        if !status.box_name.is_empty() {
            ui.add_space(16.0);
            accounts(ui, state, &status, theme);
        }
    });
}

fn last_backup(ui: &mut egui::Ui, state: &mut BackupUi, status: &Status, theme: &Theme) {
    section(ui, theme, "Backups");
    hint(
        ui,
        "Repositories in your home go to LosOS Git, each as a private repository of yours, and so do your configs, the files in ~/.config and your dotfiles, as one private repository with their history. Everything else goes to LosOS cloud, in Backups, under this computer's name. Caches and the trash stay out.",
    );
    let ready = !status.box_name.is_empty() && (status.cloud.is_some() || status.git.is_some());
    let mut on = status.enabled;
    if ui
        .add_enabled(ready, egui::Checkbox::new(&mut on, "Back up every day"))
        .changed()
    {
        state.start(
            if on { "on" } else { "off" },
            backup(&[if on { "on" } else { "off" }]),
            None,
        );
    }
    if !ready {
        hint(ui, "Choose a box and sign in to it below first.");
        return;
    }
    let now = now();
    if status.running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Backing up…");
        });
    } else if status.last_done > 0 {
        ui.label(format!(
            "Last backup {} to {}.",
            ago(now, status.last_done),
            status.box_name
        ));
    } else {
        ui.label("Not backed up yet.");
    }
    if let Some(run) = &status.last {
        match run.outcome.as_str() {
            "done" => {
                ui.label(egui::RichText::new(summary(run)).color(theme.foreground));
            }
            "skipped" => {
                hint(
                    ui,
                    &format!("Tried {}: {}", ago(now, run.finished), run.message),
                );
            }
            _ => {
                ui.label(
                    egui::RichText::new(format!(
                        "The backup {} stopped: {}",
                        ago(now, run.finished),
                        run.message
                    ))
                    .color(RED),
                );
            }
        }
        let problems: Vec<&String> = run.failures.iter().chain(&run.repo_failures).collect();
        if !problems.is_empty() {
            let count = run.failed + run.repo_failures.len() as u64;
            let title = egui::RichText::new(format!(
                "{count} item{} could not be backed up",
                if count == 1 { "" } else { "s" }
            ))
            .color(RED);
            egui::CollapsingHeader::new(title)
                .id_salt("backup-failures")
                .show(ui, |ui| {
                    for problem in problems {
                        ui.label(problem);
                    }
                });
        }
    }
    if ui
        .add_enabled(!status.running, egui::Button::new("Back up now"))
        .clicked()
    {
        state.start(
            "now",
            [
                "systemctl",
                "--user",
                "start",
                "--no-block",
                "losos-backup-now.service",
            ]
            .map(str::to_owned)
            .to_vec(),
            None,
        );
    }
}

fn the_box(ui: &mut egui::Ui, state: &mut BackupUi, status: &Status, theme: &Theme) {
    section(ui, theme, "Box");
    if status.routed && status.machines.is_empty() {
        hint(
            ui,
            "This computer reaches LosOS boxes through the LosOS boxes machine. Make one in Machines from the losos-boxes template.",
        );
    } else if status.routed {
        hint(
            ui,
            "Backups go through the LosOS boxes machine, which starts for a backup when it is not running.",
        );
    }
    if !status.box_name.is_empty() {
        ui.label(format!("Backing up to {}", status.box_name));
        hint(
            ui,
            &format!("Certificate fingerprint {}", status.fingerprint),
        );
    }
    if !status.boxes.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Found");
            for name in &status.boxes {
                ui.selectable_value(&mut state.box_name, name.clone(), name);
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut state.box_name).hint_text("mattbox.local"));
        let name = state.box_name.trim().to_owned();
        let change = !name.is_empty() && name != status.box_name;
        let label = if status.box_name.is_empty() {
            "Use this box"
        } else {
            "Change box"
        };
        if ui.add_enabled(change, egui::Button::new(label)).clicked() {
            state.start("choose", backup(&["choose", &name]), None);
        }
    });
    if !status.box_name.is_empty() {
        hint(ui, "Changing the box signs you out of both apps.");
    }
}

/// LosOS Login's step, shown while `sign-in box` waits for it.
fn login_step(ui: &mut egui::Ui, prompt: &Prompt, theme: &Theme) {
    hint(
        ui,
        "Open LosOS Login and sign in there. Check that it shows this code, then approve LosOS Desktop backups.",
    );
    ui.label(
        egui::RichText::new(&prompt.code)
            .monospace()
            .size(22.0)
            .color(theme.foreground),
    );
    ui.horizontal_wrapped(|ui| {
        if ui.button("Open LosOS Login").clicked() {
            // An https URL (parse_prompt), so xdg-open cannot read it as
            // an option.
            let _ = Command::new("xdg-open")
                .arg(&prompt.link)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        ui.label(egui::RichText::new(&prompt.link).small());
    });
}

fn accounts(ui: &mut egui::Ui, state: &mut BackupUi, status: &Status, theme: &Theme) {
    section(ui, theme, "Sign in");
    for (which, title, signed_in) in [
        ("cloud", "LosOS cloud", &status.cloud),
        ("git", "LosOS Git", &status.git),
    ] {
        ui.horizontal(|ui| match signed_in {
            Some(login) => {
                ui.label(format!("{title}: signed in as {login}"));
                if ui.button("Sign out").clicked() {
                    state.start("sign-out", backup(&["sign-out", which]), None);
                }
            }
            None => {
                ui.label(format!("{title}: not signed in"));
            }
        });
    }
    ui.add_space(4.0);
    hint(
        ui,
        "Sign in with LosOS Login, the box's own sign-in, in your browser, with your password and your second factor if you have one. This computer gets an app password for LosOS cloud and a token for LosOS Git of its own, which the box lists under your account; your password is never typed here.",
    );
    let label = if status.cloud.is_some() || status.git.is_some() {
        "Sign in again with LosOS Login"
    } else {
        "Sign in with LosOS Login"
    };
    if ui.button(label).clicked() {
        state.sign_in_box();
    }
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Sign in with a password instead")
        .id_salt("backup-password")
        .show(ui, |ui| {
            hint(
                ui,
                "For a box whose LosOS Login cannot sign backups in yet: LosOS cloud only, and not for an account with a second factor.",
            );
            ui.add(egui::TextEdit::singleline(&mut state.cloud_login).hint_text("Name"));
            ui.add(
                egui::TextEdit::singleline(&mut state.cloud_password)
                    .password(true)
                    .hint_text("Password"),
            );
            let ready = !state.cloud_login.trim().is_empty() && !state.cloud_password.is_empty();
            if ui
                .add_enabled(ready, egui::Button::new("Sign in to LosOS cloud"))
                .clicked()
            {
                let argv = backup(&["sign-in", "cloud", state.cloud_login.trim()]);
                let secret = state.cloud_password.clone();
                state.start("sign-in cloud", argv, Some(secret));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_login_prompt_and_the_result() {
        let p = parse_prompt(
            r#"{"link":"https://mattbox.local/auth/consent/openid/device-authorization?user_code=RDQXQRJZ","page":"https://mattbox.local/auth/consent/openid/device-authorization","code":"RDQXQRJZ","expiresIn":300}"#,
        )
        .unwrap();
        assert_eq!(p.code, "RDQXQRJZ");
        assert!(parse_prompt(r#"{"link":"--help","code":"x"}"#).is_none());
        assert!(parse_prompt(r#"{"cloud":"matus"}"#).is_none());
        assert_eq!(
            signed_in_message(
                r#"{"cloud":"notshared","git":"notshared","gitOff":false,"problems":[]}"#
            ),
            "Signed in to LosOS cloud and LosOS Git as notshared."
        );
        assert_eq!(
            signed_in_message(r#"{"cloud":"ada","git":null,"gitOff":true,"problems":[]}"#),
            "Signed in to LosOS cloud as ada. This box has no LosOS Git, so repositories go to LosOS cloud as files."
        );
    }

    #[test]
    fn reads_the_status() {
        let json = r#"{"box":"mattbox.local","fingerprint":"AB:CD","enabled":true,"cloud":"matus","git":null,"running":false,"last":{"started":10,"finished":20,"box":"mattbox.local","machine":"boxes","outcome":"done","message":"","uploaded":3,"bytes":2048,"deleted":1,"unchanged":7,"failed":1,"failures":["~/.htaccess: forbidden"],"repos":2,"configs":14,"configs_changed":true,"repo_failures":[]},"last_done":20,"routed":true,"machines":["boxes"],"boxes":["mattbox.local"]}"#;
        let status = parse_status(json).unwrap();
        assert_eq!(status.box_name, "mattbox.local");
        assert_eq!(status.cloud.as_deref(), Some("matus"));
        assert_eq!(status.git, None);
        let last = status.last.unwrap();
        assert_eq!((last.uploaded, last.repos, last.failures.len()), (3, 2, 1));
        assert_eq!(
            summary(&last),
            "3 files sent (2.0 KB), 7 unchanged, 1 removed, 2 repositories, 14 config files committed."
        );
        assert!(parse_status("not json").is_none());
        assert_eq!(parse_status(r#"{"last":null}"#).unwrap().last, None);
    }

    #[test]
    fn says_how_long_ago() {
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(4000, 100), "1 hour ago");
        assert_eq!(ago(200_000, 0), "2 days ago");
        assert_eq!(ago(130, 0), "2 minutes ago");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1_500_000), "1.5 MB");
    }
}
