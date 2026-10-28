//! Reporting a problem on the desktop as an issue: a window that stopped
//! responding, a failed service, or an action that never finished.
//!
//! derisk does not write or send the report itself. It runs a reporter,
//! [`PROGRAM`] (`losos-report serve` on LosOS), and speaks JSON lines with
//! it, the protocol the installer's backend speaks for a failed install
//! ([`crate::install`]): the reporter's `hello` names where reports go, or
//! `null` when this system sends none; `report` asks it to write one for a
//! [`Problem`]; `report_send` signs in and files it.
//!
//! [`Report`] is the page both draw: the whole text, exactly as it would be
//! sent, a field for the person's own note, and then the sign-in code with
//! a QR code for a phone, and the issue's address once it is filed. Nothing
//! is sent until the person says so.

use std::{
    io::{self, BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

use egui::{RichText, Ui};
use mcsapi::toolkit::egui;
use mcsapi_components::Tokens;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::wizard;

/// The reporter derisk runs, with `serve`.
pub const PROGRAM: &str = "losos-report";

/// What went wrong, as the reporter's `report` request names it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Problem {
    /// A window gave no answer to the compositor's pings.
    NotResponding {
        /// The app's ID. Never its title, which is often a document's name.
        app: String,
        /// The process behind the window.
        #[serde(default)]
        pid: Option<u32>,
        /// How long it has been silent.
        seconds: u64,
    },
    /// A unit is in the failed state.
    Failed {
        /// The unit.
        unit: String,
        /// Whether it is the person's (`systemctl --user`) or the system's.
        user: bool,
    },
    /// An app's action is still running long after it should be done.
    Stuck {
        /// The app's ID.
        app: String,
        /// The action, as the app labels it.
        action: String,
        /// How long it has been running.
        seconds: u64,
        /// The process doing it.
        #[serde(default)]
        pid: Option<u32>,
    },
}

impl Problem {
    /// The reporter's `report` request for it.
    pub fn request(&self) -> Value {
        let mut request = serde_json::to_value(self).unwrap_or_default();
        request["request"] = json!("report");
        request
    }

    /// What happened, in a line, with the app called `name`.
    pub fn summary(&self, name: &str) -> String {
        match self {
            Self::NotResponding { .. } => format!("{name} stopped responding"),
            Self::Failed { unit, .. } => format!("{unit} failed"),
            Self::Stuck { action, .. } => {
                format!("{name}: \u{201c}{action}\u{201d} never finished")
            }
        }
    }
}

/// What a reporter says, the part of the installer's protocol about reports.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// It is ready, and reports go to `report` (`owner/name`), or nowhere.
    Hello {
        /// Where reports go.
        #[serde(default)]
        report: Option<String>,
    },
    /// The report, exactly as it would be sent.
    Report {
        /// Where it goes.
        repository: String,
        /// The issue's title.
        title: String,
        /// The issue's text, in Markdown.
        body: String,
    },
    /// To send it, the person enters `code` at `uri`.
    ReportSignIn {
        /// The page to sign in at.
        uri: String,
        /// What to enter there.
        code: String,
    },
    /// It was filed at `url`; on an issue already open for the same
    /// problem when `existing`.
    Reported {
        /// Where it can be read.
        url: String,
        /// Whether it was added to someone else's.
        #[serde(default)]
        existing: bool,
    },
    /// It could not be written or sent.
    ReportFailed {
        /// Why, in a sentence.
        message: String,
    },
}

/// Where a report is.
#[derive(Default)]
pub enum Reporting {
    /// The reporter is writing it.
    #[default]
    Writing,
    /// Written, shown, not sent.
    Ready,
    /// Asked to send; waiting for the sign-in code.
    Starting,
    /// Waiting for the person to sign in with `code` at `uri`.
    SignIn {
        /// The page.
        uri: String,
        /// The code.
        code: String,
        /// `uri` as a QR code for a phone.
        qr: Option<qrcode::QrCode>,
    },
    /// Filed at `url`.
    Sent {
        /// The issue.
        url: String,
        /// Whether it went onto an issue someone else opened.
        existing: bool,
        /// `url` as a QR code.
        qr: Option<qrcode::QrCode>,
    },
    /// Could not be written or sent, and why.
    Failed(String),
}

/// One report and the page that shows it.
#[derive(Default)]
pub struct Report {
    /// Where it is.
    pub state: Reporting,
    /// The repository, title and body, as the reporter wrote them.
    pub written: Option<(String, String, String)>,
    /// The person's own words, added before the rest.
    pub note: String,
}

impl Report {
    /// Takes in one of the reporter's events. `hello` is not about this
    /// report and changes nothing.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Hello { .. } => {}
            Event::Report {
                repository,
                title,
                body,
            } => {
                self.written = Some((repository, title, body));
                self.state = Reporting::Ready;
            }
            Event::ReportSignIn { uri, code } => {
                let qr = qrcode::QrCode::new(uri.as_bytes()).ok();
                self.state = Reporting::SignIn { uri, code, qr };
            }
            Event::Reported { url, existing } => {
                info!("reported at {url}");
                let qr = qrcode::QrCode::new(url.as_bytes()).ok();
                self.state = Reporting::Sent { url, existing, qr };
            }
            Event::ReportFailed { message } => self.state = Reporting::Failed(message),
        }
    }

    /// Whether it is waiting on GitHub: a cancel stops that.
    pub fn sending(&self) -> bool {
        matches!(self.state, Reporting::Starting | Reporting::SignIn { .. })
    }

    /// Whether "Send" can be pressed: it is written and not on its way.
    pub fn can_send(&self) -> bool {
        self.written.is_some() && matches!(self.state, Reporting::Ready | Reporting::Failed(_))
    }

    /// Whether it was filed.
    pub fn sent(&self) -> bool {
        matches!(self.state, Reporting::Sent { .. })
    }

    /// Draws the page for where it is.
    pub fn show(&mut self, ui: &mut Ui) {
        let tokens = Tokens::current(ui.ctx());
        match &self.state {
            Reporting::Writing => {
                wizard::busy(ui, "Writing the report…");
                return;
            }
            Reporting::Starting => {
                wizard::busy(ui, "Asking GitHub for a sign-in code…");
                return;
            }
            Reporting::SignIn { uri, code, qr } => {
                ui.label(
                    RichText::new(format!(
                        "On a phone or another computer, open {uri} and enter this code:"
                    ))
                    .font(tokens.body_font()),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(code)
                        .font(egui::FontId::monospace(32.0))
                        .strong(),
                );
                ui.add_space(8.0);
                if let Some(qr) = qr {
                    derisk_settings::qr(ui, qr);
                    ui.add_space(8.0);
                }
                wizard::busy(ui, "The report is sent as soon as you approve it there.");
                return;
            }
            Reporting::Sent { url, existing, qr } => {
                let said = if *existing {
                    "Someone already reported this problem, so the report was added to theirs."
                } else {
                    "Reported. Thank you: the developers can see what went wrong."
                };
                ui.label(RichText::new(said).font(tokens.body_font()).strong());
                ui.add_space(8.0);
                ui.label(RichText::new(url).monospace());
                ui.add_space(8.0);
                if let Some(qr) = qr {
                    derisk_settings::qr(ui, qr);
                }
                return;
            }
            Reporting::Failed(message) => {
                wizard::notice(ui, message, true);
                ui.add_space(8.0);
            }
            Reporting::Ready => {}
        }
        let Some((repository, title, body)) = &self.written else {
            return;
        };
        wizard::notice(
            ui,
            &format!(
                "This opens an issue in {repository} on GitHub with the text below. It is public, so MAC and IP addresses are left out. Nothing is sent until you sign in to GitHub and approve it."
            ),
            false,
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new("What were you doing? (optional)")
                .font(tokens.small_font())
                .strong()
                .color(tokens.foreground),
        );
        ui.add_space(2.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.note)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .id_salt("report-text")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(RichText::new(title).strong());
                ui.add_space(4.0);
                ui.label(
                    RichText::new(body)
                        .monospace()
                        .size(12.0)
                        .color(tokens.muted_foreground),
                );
            });
    }
}

/// The request line that sends a report with `note` before it.
pub fn send_line(note: &str) -> String {
    format!("{}\n", json!({"request": "report_send", "note": note}))
}

/// The request line that stops waiting for the sign-in.
pub const CANCEL_LINE: &str = "{\"request\":\"report_cancel\"}\n";

/// A running reporter, writing and sending one report.
pub struct Reporter {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Event>,
    /// The report it is writing.
    pub report: Report,
}

/// Starts `argv`, reading its events on a thread of their own.
fn spawn(argv: &[String]) -> io::Result<(Child, ChildStdin, Receiver<Event>)> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no reporter to run"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        return Err(io::Error::other("the reporter has no stdin or stdout"));
    };
    let (tx, events) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(event) = serde_json::from_str::<Event>(line.trim())
                && tx.send(event).is_err()
            {
                return;
            }
        }
    });
    Ok((child, stdin, events))
}

/// `PROGRAM serve`.
pub fn command() -> Vec<String> {
    vec![PROGRAM.to_owned(), "serve".to_owned()]
}

impl Reporter {
    /// Starts `argv` (normally [`command`]) and asks it to write `problem`'s
    /// report.
    pub fn start(argv: &[String], problem: &Problem) -> io::Result<Self> {
        let (child, stdin, events) = spawn(argv)?;
        let mut reporter = Self {
            child,
            stdin,
            events,
            report: Report::default(),
        };
        reporter.write(&format!("{}\n", problem.request()));
        Ok(reporter)
    }

    fn write(&mut self, line: &str) {
        if let Err(e) = self
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
        {
            warn!("cannot reach the reporter: {e}");
            self.report.state = Reporting::Failed(format!("The reporter stopped: {e}"));
        }
    }

    /// Takes in what the reporter said since the last call.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.report.apply(event);
        }
    }

    /// Sends the report, the person's note first.
    pub fn send(&mut self) {
        self.report.state = Reporting::Starting;
        let line = send_line(&self.report.note);
        self.write(&line);
    }

    /// Stops waiting for the sign-in.
    pub fn cancel(&mut self) {
        if self.report.sending() {
            self.write(CANCEL_LINE);
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where `argv`'s reports go, from its `hello`: `None` when this system
/// sends none, or the reporter is not installed (derisk runs on more than
/// LosOS). Blocks until it answers, so it belongs on a thread.
pub fn probe(argv: &[String]) -> Option<String> {
    let (mut child, stdin, events) = spawn(argv).ok()?;
    let answer = match events.recv() {
        Ok(Event::Hello { report }) => report,
        _ => None,
    };
    // Its stdin closing is its cue to exit.
    drop(stdin);
    let _ = child.wait();
    answer
}

/// The dialog a report is shown in over the desktop: what happened, the
/// report, and Cancel and Send (Done once it is sent).
pub struct ReportDialog {
    /// What happened, in a line ([`Problem::summary`]).
    pub summary: String,
    /// The reporter writing and sending it.
    pub reporter: Reporter,
}

impl ReportDialog {
    /// Draws it, `width` wide at most. Returns whether it stays open.
    pub fn show(&mut self, ui: &mut Ui, width: f32) -> bool {
        use mcsapi_components::NativeDialog;
        use mcsapi_ui::dialog::{ActionRole, DialogAction};

        self.reporter.poll();
        let report = &self.reporter.report;
        // The reporter answers between frames: from a thread, which egui
        // does not hear.
        if !report.sent() && !matches!(report.state, Reporting::Ready) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
        let actions: Vec<DialogAction> = if report.sent() {
            vec![DialogAction::new("Done", ActionRole::Default)]
        } else if report.can_send() {
            vec![
                DialogAction::new("Cancel", ActionRole::Cancel),
                DialogAction::new("Send", ActionRole::Default),
            ]
        } else {
            vec![DialogAction::new("Cancel", ActionRole::Cancel)]
        };
        let mut open = true;
        let summary = self.summary.clone();
        let reporter = &mut self.reporter;
        let answer = NativeDialog::new("derisk-report", &mut open, "Report a problem")
            .width(width)
            .show(ui.ctx(), |ui| {
                ui.label(RichText::new(&summary).size(18.0).strong());
                ui.add_space(10.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), 380.0), |ui| {
                    reporter.report.show(ui);
                });
                ui.add_space(12.0);
                NativeDialog::actions(ui, &actions)
            })
            .flatten();
        let chosen = answer.map(|i| actions[i].role);
        match chosen {
            Some(ActionRole::Default) if self.reporter.report.sent() => false,
            Some(ActionRole::Default) => {
                self.reporter.send();
                true
            }
            Some(_) => {
                self.reporter.cancel();
                false
            }
            None if !open => {
                self.reporter.cancel();
                false
            }
            None => true,
        }
    }
}
