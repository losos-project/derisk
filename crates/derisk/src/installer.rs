//! `derisk installer -- <backend...>`: the installer, as pages.
//!
//! derisk draws it and joins networks; the backend, a program started with
//! the command after `--`, lists disks and installs, speaking the JSON-lines
//! protocol in [`derisk::install`]. On LosOS's installer ISO that is
//! `losos-installer serve`.
//!
//! The pages: what is being installed and from where; the network, skipped
//! when a cable already brought the machine online; the disk; whether to ask
//! for a PIN at startup, only when the backend can encrypt on this computer;
//! a last page that names the disk and says it will be erased, with the only
//! red button; the install's steps and output; and Restart.
//!
//! With a PIN, the backend encrypts the installed system's data so that the
//! computer's TPM unlocks it only once the PIN is typed, and hands back a
//! recovery key. The last page shows that key once, with a QR code, and
//! Restart waits until the person says they have kept it: it is the only
//! way in without the PIN, and nothing else has a copy.
//!
//! When an install fails and the backend can send reports (its hello names
//! where), the failed page's button is "Report the problem". That page shows
//! the report exactly as it would be sent, takes a note from the person, and
//! sends it only when they say so: the backend asks GitHub for a sign-in
//! code, the page shows it with a QR code for another device, and once the
//! person approves there the backend files the issue and the page shows its
//! address.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command as Process, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

use derisk::{
    install::{self, Disk, Event, Request},
    report::{self, Report, Reporting},
    wifi::{NetworkPage, Wifi},
    wizard::{self, Nav, Next, Page, Row, ScreenLayout},
};
use egui::{RichText, Ui};
use mcsapi::widgets::Theme;
use mcsapi_components::{Progress, Tokens};
use mcsapi_compositor::{Command, egui};
use tracing::{error, info};

use crate::wizard_host::{self, Flow};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Lines of the backend's output kept for reading after a failure.
const LOG_LINES: usize = 400;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Welcome,
    Network,
    Disk,
    Pin,
    Confirm,
    Installing,
    Report,
}

impl Step {
    const PAGES: [Step; 5] = [
        Step::Welcome,
        Step::Network,
        Step::Disk,
        Step::Pin,
        Step::Confirm,
    ];
}

/// The shortest PIN the backend takes (losos-installer's encrypt.rs).
const MIN_PIN: usize = 6;

/// The PIN page's state.
#[derive(Default)]
struct PinChoice {
    wanted: bool,
    pin: String,
    again: String,
}

impl PinChoice {
    /// Why the page cannot go on yet, if it cannot.
    fn problem(&self) -> Option<&'static str> {
        if !self.wanted {
            None
        } else if self.pin.chars().count() < MIN_PIN {
            Some("The PIN needs at least 6 characters.")
        } else if self.pin.chars().any(char::is_control) {
            Some("The PIN cannot hold a line break or a tab.")
        } else if self.pin != self.again {
            Some("The two PINs are not the same.")
        } else {
            None
        }
    }

    /// The PIN to send, when one was asked for.
    fn chosen(&self) -> Option<String> {
        (self.wanted && self.problem().is_none()).then(|| self.pin.clone())
    }
}

struct Backend {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Option<Event>>,
}

impl Backend {
    fn start(argv: &[String]) -> Result<Self> {
        let mut child = Process::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", argv[0]))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(event) = install::parse_event(&line)
                    && tx.send(Some(event)).is_err()
                {
                    return;
                }
            }
            // The backend is gone.
            let _ = tx.send(None);
        });
        Ok(Self {
            child,
            stdin,
            events,
        })
    }

    fn send(&mut self, request: &Request) {
        if let Err(e) = self
            .stdin
            .write_all(install::request_line(request).as_bytes())
            .and_then(|()| self.stdin.flush())
        {
            error!("cannot reach the backend: {e}");
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Install {
    labels: Vec<String>,
    current: usize,
    fraction: Option<f32>,
    log: Vec<String>,
    done: bool,
    /// Set for good on a failure; `error` is only the dialog saying why,
    /// and "OK" empties it.
    failed: bool,
    error: Option<mcsapi_ui::Error>,
    show_log: bool,
    /// The recovery key, when root was encrypted, and its QR code.
    recovery: Option<(String, Option<qrcode::QrCode>)>,
    /// Whether the person said they have kept it.
    recovery_kept: bool,
}

struct Installer {
    backend: Backend,
    step: Step,
    name: String,
    source: Option<String>,
    release: Option<String>,
    /// Where a failed install can be reported, when the backend can.
    reports_to: Option<String>,
    report: Report,
    backend_lost: bool,
    wifi: Wifi,
    network: NetworkPage,
    disks: Option<Vec<Disk>>,
    disk: Option<usize>,
    /// Whether the backend can encrypt on this computer.
    encryption: bool,
    pin: PinChoice,
    error: Option<mcsapi_ui::Error>,
    install: Install,
}

impl Installer {
    fn poll(&mut self) {
        while let Ok(event) = self.backend.events.try_recv() {
            let Some(event) = event else {
                self.backend_lost = true;
                continue;
            };
            match event {
                Event::Hello {
                    name,
                    source,
                    release,
                    report,
                    encryption,
                } => {
                    self.name = name;
                    self.source = source;
                    self.release = release;
                    self.reports_to = report;
                    self.encryption = encryption;
                    if !encryption {
                        self.pin = PinChoice::default();
                    }
                }
                Event::Report {
                    repository,
                    title,
                    body,
                } => self.report.apply(report::Event::Report {
                    repository,
                    title,
                    body,
                }),
                Event::ReportSignIn { uri, code } => {
                    self.report.apply(report::Event::ReportSignIn { uri, code });
                }
                Event::Reported { url, existing } => {
                    self.report.apply(report::Event::Reported { url, existing });
                }
                Event::ReportFailed { message } => {
                    self.report.apply(report::Event::ReportFailed { message });
                }
                Event::Disks { disks } => {
                    if self.disk.is_some_and(|d| d >= disks.len()) {
                        self.disk = None;
                    }
                    if disks.len() == 1 {
                        self.disk = Some(0);
                    }
                    self.disks = Some(disks);
                }
                Event::Steps { labels } => self.install.labels = labels,
                Event::Step {
                    index,
                    count,
                    label,
                    fraction,
                } => {
                    let install = &mut self.install;
                    install.labels.resize(count.max(index + 1), String::new());
                    install.labels[index] = label;
                    install.current = index;
                    install.fraction = fraction;
                }
                Event::Line { text } => {
                    let log = &mut self.install.log;
                    log.push(text);
                    if log.len() > LOG_LINES {
                        log.drain(..log.len() - LOG_LINES);
                    }
                }
                Event::RecoveryKey { key } => {
                    let qr = qrcode::QrCode::new(key.as_bytes()).ok();
                    self.install.recovery = Some((key, qr));
                }
                Event::Installed => {
                    self.install.done = true;
                    self.install.current = self.install.labels.len();
                }
                Event::Failed { message } => {
                    let error = wizard::failure(message, "the-installer-stopped");
                    if self.step == Step::Installing {
                        self.install.failed = true;
                        self.install.error = Some(error);
                    } else {
                        self.error = Some(error);
                    }
                }
            }
        }
    }

    fn go(&mut self, step: Step) {
        self.error = None;
        if step == Step::Disk {
            self.disks = None;
            self.backend.send(&Request::Disks);
        }
        self.step = step;
    }

    fn chosen(&self) -> Option<&Disk> {
        self.disks.as_ref()?.get(self.disk?)
    }

    fn welcome_page(&mut self, ui: &mut Ui) {
        let tokens = Tokens::current(ui.ctx());
        ui.label(
            RichText::new(format!(
                "This installs {} onto a disk of this computer, replacing everything on it.",
                self.name
            ))
            .font(tokens.body_font()),
        );
        ui.add_space(8.0);
        if let Some(release) = &self.release {
            wizard::notice(
                ui,
                &format!(
                    "The release on the disk at {release} is checked against its signature and installed, so no network is needed."
                ),
                false,
            );
        } else if let Some(source) = &self.source {
            wizard::notice(
                ui,
                &format!(
                    "The newest release is downloaded from {source} while it installs, so this needs a network."
                ),
                false,
            );
        }
        if self.backend_lost {
            ui.add_space(8.0);
            wizard::notice(
                ui,
                "The installer's backend stopped. Restart the computer to try again.",
                true,
            );
        }
    }

    fn disk_page(&mut self, ui: &mut Ui) {
        wizard::error(ui, "installer-error", &mut self.error);
        let Some(disks) = &self.disks else {
            wizard::busy(ui, "Looking for disks…");
            return;
        };
        if disks.is_empty() {
            wizard::notice(
                ui,
                "No disk was found to install onto. The one this installer started from is not offered.",
                true,
            );
            return;
        }
        if let Some(i) = wizard::list(ui, "disks", disks.len(), self.disk, None, |i| {
            let d = &disks[i];
            Row {
                title: d.title().to_owned(),
                detail: format!(
                    "{}{} · {}",
                    d.size_text(),
                    if d.removable { " · removable" } else { "" },
                    d.path
                ),
            }
        }) {
            self.disk = Some(i);
        }
    }

    fn pin_page(&mut self, ui: &mut Ui) {
        let tokens = Tokens::current(ui.ctx());
        ui.add(
            mcsapi_components::Checkbox::new(&mut self.pin.wanted)
                .label("Ask for a PIN at startup"),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "The system's data is encrypted, and this computer's security chip unlocks it only once the PIN is typed. Someone who takes the computer or its disk cannot read it without the PIN. Each person's own files are encrypted with their password either way.",
            )
            .font(tokens.body_font())
            .color(tokens.muted_foreground),
        );
        ui.add_space(12.0);
        if !self.pin.wanted {
            return;
        }
        wizard::field(
            ui,
            "PIN",
            &mut self.pin.pin,
            wizard::Entry::Secret,
            "At least 6 characters",
        );
        wizard::field(
            ui,
            "PIN again",
            &mut self.pin.again,
            wizard::Entry::Secret,
            "",
        );
        if !self.pin.again.is_empty()
            && let Some(problem) = self.pin.problem()
        {
            wizard::notice(ui, problem, true);
        }
        wizard::notice(
            ui,
            "A recovery key is shown once the install is done. Keep it somewhere safe: it is the only way in if the PIN is forgotten.",
            false,
        );
    }

    fn confirm_page(&mut self, ui: &mut Ui) {
        let Some(disk) = self.chosen() else { return };
        let tokens = Tokens::current(ui.ctx());
        ui.label(
            RichText::new(format!("{} ({})", disk.title(), disk.size_text()))
                .font(egui::FontId::proportional(18.0))
                .strong(),
        );
        ui.label(RichText::new(&disk.path).color(tokens.muted_foreground));
        ui.add_space(12.0);
        wizard::notice(
            ui,
            "Everything on this disk will be erased: every partition, every file and any other system on it.",
            true,
        );
        if self.pin.chosen().is_some() {
            ui.add_space(8.0);
            wizard::notice(
                ui,
                "The system's data will be encrypted, and the computer will ask for the PIN at every start.",
                false,
            );
        }
    }

    fn installing_page(&mut self, ui: &mut Ui) {
        let install = &mut self.install;
        // Once the key is there the steps are all ticked, and the key needs
        // the room more: on a phone it would not fit beside them.
        let showing_key = install.done && install.recovery.is_some();
        let labels: Vec<&str> = install.labels.iter().map(String::as_str).collect();
        if labels.is_empty() {
            wizard::busy(ui, "Starting…");
        } else if !showing_key {
            wizard::steps(ui, &labels, install.current, install.failed);
        }
        if let Some(fraction) = install.fraction
            && !install.done
            && !install.failed
        {
            ui.add(Progress::new(fraction));
            ui.add_space(8.0);
        }
        wizard::error(ui, "install-error", &mut install.error);
        if install.done && !install.failed {
            ui.label(
                RichText::new("Installed. Remove the installer's medium, then restart.").strong(),
            );
            ui.add_space(8.0);
        }
        if install.recovery.is_some() {
            // Scrolls on a screen too short for the key, its QR code, the
            // notice and the box under them, so the box is never out of
            // reach and Restart never stuck.
            egui::ScrollArea::vertical()
                .id_salt("recovery-key")
                .max_height((ui.available_height() - 48.0).max(120.0))
                .show(ui, |ui| {
                if let Some((key, qr)) = &install.recovery {
                    let tokens = Tokens::current(ui.ctx());
                    ui.label(
                        RichText::new("Recovery key")
                            .font(tokens.small_font())
                            .strong(),
                    );
                    ui.label(
                        RichText::new(key)
                            .monospace()
                            .size(16.0)
                            .color(tokens.foreground),
                    );
                    if let Some(qr) = qr {
                        derisk_settings::qr(ui, qr);
                    }
                    wizard::notice(
                        ui,
                        "Write this down or scan it, and keep it apart from the computer. It unlocks the system's data without the PIN, and it is not shown again.",
                        false,
                    );
                    ui.add(
                        mcsapi_components::Checkbox::new(&mut install.recovery_kept)
                            .label("I have kept the recovery key"),
                    );
                    ui.add_space(8.0);
                }
                });
        }
        let toggle = if install.show_log {
            "Hide details"
        } else {
            "Show details"
        };
        if ui
            .add(
                mcsapi_components::Button::new(toggle)
                    .variant(mcsapi_components::ButtonVariant::Ghost),
            )
            .clicked()
        {
            install.show_log = !install.show_log;
        }
        // Shown by itself after a failure: it is what says why.
        if install.show_log || install.failed {
            let tokens = Tokens::current(ui.ctx());
            egui::ScrollArea::vertical()
                .id_salt("install-log")
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for line in &install.log {
                        ui.label(
                            RichText::new(line)
                                .monospace()
                                .size(12.0)
                                .color(tokens.muted_foreground),
                        );
                    }
                });
        }
    }
}

impl Flow for Installer {
    fn show(&mut self, ui: &mut Ui, layout: &ScreenLayout, theme: &Theme, _out: &mut Vec<Command>) {
        self.poll();
        let status = self.wifi.status();
        // A cable that comes up skips the Network page on the way forward,
        // as it did in the text installer, and so does a release disk, which
        // is everything an install needs.
        let offline_ok = status.online || self.release.is_some();
        let step = self.step;
        let index = Step::PAGES.iter().position(|s| *s == step);
        let (title, subtitle, next, back) = match step {
            Step::Welcome => (
                "Install",
                "Install the system onto this computer.",
                Next::new("Next", !self.backend_lost),
                false,
            ),
            Step::Network => (
                "Network",
                "The system is downloaded while it installs.",
                Next::new("Next", status.online),
                true,
            ),
            Step::Disk => (
                "Disk",
                "Choose the disk to install onto.",
                Next::new("Next", self.chosen().is_some()),
                true,
            ),
            Step::Pin => (
                "PIN at startup",
                "Choose whether the computer asks for a PIN when it starts.",
                Next::new("Next", self.pin.problem().is_none()),
                true,
            ),
            Step::Confirm => (
                "Erase this disk?",
                "This cannot be undone.",
                Next {
                    label: "Erase and install",
                    enabled: true,
                    destructive: true,
                },
                true,
            ),
            Step::Installing => {
                let done = self.install.done;
                let failed = self.install.failed;
                (
                    if done {
                        "Installed"
                    } else if failed {
                        "The install failed"
                    } else {
                        "Installing"
                    },
                    if failed && self.reports_to.is_some() {
                        "Nothing was installed. Report the problem, or go back to try again."
                    } else if failed {
                        "Nothing was installed. Go back to try again, or pick another disk."
                    } else if done {
                        "The computer is ready to restart into the new system."
                    } else if self.release.is_some() {
                        "This takes a while: the system is copied from the release disk."
                    } else {
                        "This takes a while: the system is downloaded as it installs."
                    },
                    if failed && matches!(self.report.state, Reporting::Sent { .. }) {
                        // Once is enough for one failure.
                        Next::new("Reported", false)
                    } else if failed && self.reports_to.is_some() {
                        Next::new("Report the problem", true)
                    } else {
                        // Not before the recovery key is kept, when there is one.
                        let kept = self.install.recovery.is_none() || self.install.recovery_kept;
                        Next::new("Restart", done && kept)
                    },
                    failed,
                )
            }
            Step::Report => {
                let (subtitle, next) = match &self.report.state {
                    Reporting::Ready => (
                        "Send what went wrong to the developers.",
                        Next::new("Send", true),
                    ),
                    Reporting::Failed(_) => (
                        "The report was not sent.",
                        Next::new("Try again", self.report.written.is_some()),
                    ),
                    Reporting::Sent { .. } => ("The report was sent.", Next::new("Done", true)),
                    Reporting::SignIn { .. } => {
                        ("Sign in to GitHub to send it.", Next::new("Send", false))
                    }
                    Reporting::Writing | Reporting::Starting => (
                        "Send what went wrong to the developers.",
                        Next::new("Send", false),
                    ),
                };
                ("Report the problem", subtitle, next, true)
            }
        };
        let page = Page {
            title,
            subtitle,
            index,
            count: Step::PAGES.len(),
            back,
            next: Some(next),
        };
        let nav = wizard::page(ui, layout, theme, page, |ui| match step {
            Step::Welcome => self.welcome_page(ui),
            Step::Network => self.network.show(ui, &self.wifi, &status),
            Step::Disk => self.disk_page(ui),
            Step::Pin => self.pin_page(ui),
            Step::Confirm => self.confirm_page(ui),
            Step::Installing => self.installing_page(ui),
            Step::Report => self.report.show(ui),
        });
        match (nav, step) {
            (Nav::Next, Step::Welcome) => self.go(if offline_ok {
                Step::Disk
            } else {
                Step::Network
            }),
            (Nav::Next, Step::Network) => self.go(Step::Disk),
            (Nav::Next, Step::Disk) => self.go(if self.encryption {
                Step::Pin
            } else {
                Step::Confirm
            }),
            (Nav::Next, Step::Pin) => self.go(Step::Confirm),
            (Nav::Next, Step::Confirm) => {
                if let Some(disk) = self.chosen().map(|d| d.path.clone()) {
                    let pin = self.pin.chosen();
                    info!(
                        "installing onto {disk}{}",
                        if pin.is_some() { ", encrypted" } else { "" }
                    );
                    self.install = Install::default();
                    self.report = Report::default();
                    self.backend.send(&Request::Install { disk, pin });
                    self.step = Step::Installing;
                }
            }
            (Nav::Next, Step::Installing) if self.install.failed => {
                // Written afresh each time: the journal has more in it now.
                self.report = Report {
                    note: std::mem::take(&mut self.report.note),
                    ..Report::default()
                };
                self.backend.send(&Request::Report);
                self.step = Step::Report;
            }
            (Nav::Next, Step::Installing) => {
                self.backend.send(&Request::Reboot);
            }
            (Nav::Next, Step::Report) => match self.report.state {
                Reporting::Sent { .. } => self.step = Step::Installing,
                _ => {
                    self.report.state = Reporting::Starting;
                    let note = self.report.note.clone();
                    self.backend.send(&Request::ReportSend { note });
                }
            },
            (Nav::Back, Step::Report) => {
                if matches!(
                    self.report.state,
                    Reporting::Starting | Reporting::SignIn { .. }
                ) {
                    self.backend.send(&Request::ReportCancel);
                }
                self.step = Step::Installing;
            }
            (Nav::Back, Step::Network) => self.go(Step::Welcome),
            (Nav::Back, Step::Disk) => self.go(if offline_ok {
                Step::Welcome
            } else {
                Step::Network
            }),
            (Nav::Back, Step::Pin) => self.go(Step::Disk),
            (Nav::Back, Step::Confirm) => self.go(if self.encryption {
                Step::Pin
            } else {
                Step::Disk
            }),
            (Nav::Back, Step::Installing) => self.go(Step::Disk),
            _ => {}
        }
    }
}

/// Runs the installer with the backend command `backend`.
pub fn run(backend: Vec<String>, size: (i32, i32)) -> Result {
    if backend.is_empty() {
        return Err("derisk installer needs the backend's command after `--`".into());
    }
    let installer = Installer {
        backend: Backend::start(&backend)?,
        step: Step::Welcome,
        name: "the system".into(),
        source: None,
        release: None,
        reports_to: None,
        report: Report::default(),
        backend_lost: false,
        // Nothing to save Wi-Fi into: the live system forgets it, and the
        // installed one asks again on its first boot.
        wifi: Wifi::start(false),
        network: NetworkPage::default(),
        disks: None,
        disk: None,
        encryption: false,
        pin: PinChoice::default(),
        error: None,
        install: Install::default(),
    };
    wizard_host::run(installer, "derisk installer", size)
}
