//! Ask derisk's language model ([`derisk::model`]), on a thread of its own:
//! a request the assistant's rules don't follow goes to the model server,
//! and each tool the model calls comes back to the session to run, as an
//! agent's request would, between frames.
//!
//! One request runs at a time. Its turn in the palette's conversation
//! gains a step per tool; one that needs approval waits, marked
//! [`StepStatus::Asking`], until the person allows it or not in the
//! palette ([`Asking::answer`]), or stops the request ([`Asking::stop`]).

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use derisk::{
    action::Action,
    conversation::{StepRef, StepStatus},
    ipc,
    model::{self, Config, Desktop, Http, Ran, Tool},
};
use derisk_settings::Settings;
use mcsapi_compositor::Remote;
use serde_json::Value;
use tracing::{info, warn};

use crate::host::Session;

/// How long `open_app` waits for the app's window before telling the
/// model it is still starting.
const WINDOW_WAIT: Duration = Duration::from_secs(20);

/// A request for the thread.
struct Job {
    turn: u64,
    text: String,
    config: Config,
    answers: Receiver<bool>,
    stop: Arc<AtomicBool>,
}

/// The request running now.
#[derive(Debug)]
struct Running {
    turn: u64,
    answers: Sender<bool>,
    stop: Arc<AtomicBool>,
}

/// The model's thread, and the request it is on.
#[derive(Debug)]
pub struct Asking {
    jobs: Sender<Job>,
    running: Option<Running>,
}

impl Asking {
    /// Starts the thread, which runs tools on the session through
    /// `remote` as agent connection `conn`. It ends when this is dropped.
    pub fn start(remote: Remote<Session>, conn: u64) -> Option<Self> {
        let (jobs, jobs_rx) = bounded::<Job>(1);
        let spawned = std::thread::Builder::new()
            .name("derisk-model".into())
            .spawn(move || {
                for job in jobs_rx {
                    let turn = job.turn;
                    let mut server = Http::new(&job.config.url);
                    let mut desktop = SessionDesktop {
                        remote: remote.clone(),
                        conn,
                        turn,
                        answers: job.answers,
                        stop: job.stop,
                    };
                    let outcome =
                        model::converse(&job.config, &job.text, &mut server, &mut desktop);
                    if let Err(e) = &outcome {
                        info!(error = %e, "the model did not finish a request");
                    }
                    remote.run(move |session: &mut Session| session.model_finished(turn, outcome));
                }
            });
        if let Err(e) = spawned {
            warn!(error = %e, "Ask derisk cannot use a language model");
            return None;
        }
        Some(Self {
            jobs,
            running: None,
        })
    }

    /// Sends `text`, the request turn `turn` shows, to the model. Refused
    /// while another request runs.
    pub fn ask(&mut self, turn: u64, text: &str, config: Config) -> Result<(), &'static str> {
        if self.running.is_some() {
            return Err("Still working on the last request. Stop it first, or wait for it.");
        }
        let (answers, answers_rx) = bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let job = Job {
            turn,
            text: text.to_owned(),
            config,
            answers: answers_rx,
            stop: stop.clone(),
        };
        match self.jobs.try_send(job) {
            Ok(()) => {
                self.running = Some(Running {
                    turn,
                    answers,
                    stop,
                });
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err("Still finishing the last request."),
            Err(TrySendError::Disconnected(_)) => Err("The language model's thread stopped."),
        }
    }

    /// The person's answer to the step waiting for approval.
    pub fn answer(&self, allow: bool) {
        if let Some(running) = &self.running {
            let _ = running.answers.try_send(allow);
        }
    }

    /// Stops the request running now, at its next step.
    pub fn stop(&self) {
        if let Some(running) = &self.running {
            running.stop.store(true, Ordering::Relaxed);
        }
    }

    /// The turn of the request running now.
    pub fn running(&self) -> Option<u64> {
        self.running.as_ref().map(|r| r.turn)
    }

    /// The thread finished turn `turn`.
    pub fn finished(&mut self, turn: u64) {
        if self.running.as_ref().is_some_and(|r| r.turn == turn) {
            self.running = None;
        }
    }
}

/// The session, as the model's thread sees it.
struct SessionDesktop {
    remote: Remote<Session>,
    conn: u64,
    turn: u64,
    answers: Receiver<bool>,
    stop: Arc<AtomicBool>,
}

impl SessionDesktop {
    /// Runs `job` on the session and waits for what it returns; `None`
    /// once the session has stopped or is locked, when nothing should run
    /// or be read, as for any agent.
    fn call<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Session) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = bounded(1);
        let queued = self.remote.run(move |session: &mut Session| {
            if session.is_locked() {
                return;
            }
            let _ = tx.send(job(session));
        });
        if !queued {
            return None;
        }
        rx.recv().ok()
    }

    /// Sends `request` over the agent protocol and returns its result.
    fn agent(&self, request: &Value) -> Result<String, String> {
        let line = request.to_string();
        let conn = self.conn;
        let (reply, response) = mpsc::channel();
        self.call(move |session| {
            // Nothing this connection registers needs to hear back.
            let (events, _) = mpsc::channel();
            session.agent_line(conn, &line, reply, &events);
        })
        .ok_or_else(locked)?;
        let response: Value = response
            .recv_timeout(Duration::from_secs(30))
            .ok()
            .and_then(|line| serde_json::from_str(&line).ok())
            .ok_or("the desktop did not answer")?;
        if response["ok"] == true {
            Ok(response["result"].to_string())
        } else {
            Err(response["error"]
                .as_str()
                .unwrap_or("the desktop refused")
                .to_owned())
        }
    }

    fn set(&self, step: StepRef, status: StepStatus) {
        self.call(move |session| session.shell.conversation.set(step, status));
    }

    /// Waits for the person to allow the step; `None` if they stopped the
    /// request instead.
    fn approval(&self) -> Option<bool> {
        loop {
            if self.stopped() {
                return None;
            }
            match self.answers.recv_timeout(Duration::from_millis(200)) {
                Ok(allow) => return Some(allow),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Opens `app` and waits for a window that wasn't there before.
    fn open(&self, app: String, step: StepRef) -> Result<String, String> {
        let before: Vec<u64> = self
            .call(|session| {
                ipc::state(&session.shell)
                    .windows
                    .iter()
                    .map(|w| w.id)
                    .collect()
            })
            .ok_or_else(locked)?;
        let launch = Action::Launch { app: app.clone() };
        self.call(move |session| session.model_act(launch, None))
            .ok_or_else(locked)??;
        let started = Instant::now();
        while started.elapsed() < WINDOW_WAIT && !self.stopped() {
            std::thread::sleep(Duration::from_millis(250));
            let before = before.clone();
            let new = self
                .call(move |session| {
                    ipc::state(&session.shell)
                        .windows
                        .into_iter()
                        .find(|w| !before.contains(&w.id))
                        .map(|w| (w.id, w.title))
                })
                .ok_or_else(locked)?;
            if let Some((id, title)) = new {
                self.set(step, StepStatus::Done);
                return Ok(format!("Opened {app} as window {id}, \"{title}\"."));
            }
        }
        self.set(step, StepStatus::Done);
        Ok(format!("Started {app}; its window has not opened yet."))
    }
}

fn locked() -> String {
    "the session is locked".to_owned()
}

/// The settings as `get_settings` reads them: every line of the file but
/// the shortcuts, which are many and not the model's to rebind.
fn settings_text(path: Option<&Path>) -> String {
    let settings = path
        .and_then(|p| Settings::load(p).ok())
        .map(|(s, _)| s)
        .unwrap_or_default();
    settings
        .to_text()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("shortcut."))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Changes one setting in the settings file, which the session and the
/// Settings app both follow.
fn change_setting(path: Option<&Path>, key: &str, value: &str) -> Result<String, String> {
    // Where requests go is the person's to choose, in Settings: a model
    // that pointed them at another server would hear every later one.
    if key.starts_with("assistant.") || key.starts_with("shortcut.") {
        return Err(format!("{key} is only changed in the Settings app"));
    }
    let path = path.ok_or("there is no settings file")?;
    let (mut settings, _) = Settings::load(path).map_err(|e| e.to_string())?;
    if !settings.set(key, value) {
        return Err(format!(
            "{key} can't be set to \"{value}\"; get_settings lists the keys and their values"
        ));
    }
    settings.save(path).map_err(|e| e.to_string())?;
    Ok(format!("{key} is now {value}."))
}

impl Desktop for SessionDesktop {
    fn run(&mut self, tool: &Tool) -> Ran {
        let asking = tool.needs_approval();
        let (turn, label) = (self.turn, tool.label());
        let action = match tool {
            Tool::Act(action) => Some(action.clone()),
            _ => None,
        };
        let status = if asking {
            StepStatus::Asking
        } else {
            StepStatus::Waiting
        };
        // The turn is gone when the person cleared the conversation, which
        // ends the request too.
        let Some(Some(step)) = self.call(move |session| {
            session
                .shell
                .conversation
                .push_step(turn, label, action, status)
        }) else {
            return Ran::Stopped;
        };
        if asking {
            match self.approval() {
                None => {
                    self.set(step, StepStatus::Skipped);
                    return Ran::Stopped;
                }
                Some(false) => {
                    self.set(step, StepStatus::Failed("You didn't allow it".to_owned()));
                    return Ran::Refused;
                }
                Some(true) => self.set(step, StepStatus::Waiting),
            }
        }
        let settings = derisk_settings::default_path();
        let result = match tool {
            Tool::GetDesktop => self
                .call(|session| model::describe(&ipc::state(&session.shell), &session.shell.apps))
                .ok_or_else(locked),
            Tool::ListApps => self.call(|session| session.app_list()).ok_or_else(locked),
            // Its step is done when the window opens.
            Tool::OpenApp { app } => return ran(self.open(app.clone(), step)),
            // The shell marks the step itself: done, failed, or waiting for
            // a window.
            Tool::Act(action) => {
                let action = action.clone();
                return ran(self
                    .call(move |session| session.model_act(action, Some(step)))
                    .ok_or_else(locked)
                    .and_then(|r| r));
            }
            Tool::GetSettings => Ok(settings_text(settings.as_deref())),
            Tool::ChangeSetting { key, value } => change_setting(settings.as_deref(), key, value),
            Tool::FindElements { .. }
            | Tool::Click { .. }
            | Tool::TypeText { .. }
            | Tool::PressKeys { .. } => match tool.request() {
                Some(request) => self.agent(&request),
                None => Err("not a desktop request".to_owned()),
            },
        };
        self.set(
            step,
            match &result {
                Ok(_) => StepStatus::Done,
                Err(e) => StepStatus::Failed(e.clone()),
            },
        );
        ran(result)
    }

    fn working(&mut self, what: &str) {
        let (turn, what) = (self.turn, what.to_owned());
        self.call(move |session| session.shell.conversation.set_working(turn, &what));
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

fn ran(result: Result<String, String>) -> Ran {
    match result {
        Ok(text) => Ran::Done(text),
        Err(e) => Ran::Failed(e),
    }
}
