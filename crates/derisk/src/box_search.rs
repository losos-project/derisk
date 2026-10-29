//! What is in the files of the person's LosOS boxes, for the command
//! palette: the typed text goes to `losos-boxes search` on a thread of its
//! own, and the rows it prints are published as the `boxes` source, which
//! the bundled `boxes` plugin turns into palette rows.
//!
//! `losos-boxes` and `losos`, the boxes' client with its tokens, are LosOS
//! Desktop's (`docs/losos-boxes.md` there, "In the command palette"). On a
//! PC they live in the LosOS boxes machine, the machine made from the
//! [`TEMPLATE`] template, and not on the host: the search runs there
//! through the guest agent, as [`IN_MACHINE`], and only while the machine
//! runs. A phone has no machines, and its session has `losos-boxes` on
//! `PATH`. Anywhere else there is no box to search, and nothing is asked.
//!
//! A search takes a second or two, a box's round trip and its own search,
//! so only one is under way at a time, and the text typed meanwhile is
//! asked next.

use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use derisk_machines::{Hypervisor, Program, State};
use serde_json::{Value, json};
use tracing::{debug, warn};

/// The palette source the bundled `boxes` plugin reads; only the shell
/// registers it.
pub const SOURCE: &str = "boxes";

/// The template of the LosOS boxes machine (losos-desktop's
/// `nixos/guest/boxes.nix`), which pm records on the machine.
pub const TEMPLATE: &str = "losos-boxes";

/// What runs the search in that machine: `losos-boxes search` as the
/// machine's user, on that user's bus, where `losos` finds its tokens. The
/// guest agent runs a program as root with an empty environment and needs
/// its full path.
pub const IN_MACHINE: &str = "/run/current-system/sw/bin/losos-boxes-search";

/// Fewer characters than this find too much to be worth a box's search,
/// as the box's own palette has it.
pub const MIN_CHARS: usize = 3;

/// How long one search may take before its answer is given up on.
const WAIT: Duration = Duration::from_secs(10);

/// The registration for `text`'s `hits`: the plugin shows the hits only
/// while the palette's text is still the one they answer.
pub fn palette(text: &str, hits: Vec<Value>) -> Value {
    json!({"text": text, "hits": hits})
}

/// The searching thread, and what was last asked of it.
#[derive(Debug)]
pub struct BoxSearch {
    queries: Sender<String>,
    found: Receiver<(String, Vec<Value>)>,
    asked: Option<String>,
}

impl BoxSearch {
    /// Starts the searching thread. It ends when this is dropped.
    pub fn start() -> Option<Self> {
        let (queries, queries_rx) = bounded(1);
        let (found_tx, found) = bounded(1);
        let spawned = std::thread::Builder::new()
            .name("derisk-box-search".into())
            .spawn(move || search(&queries_rx, &found_tx));
        if let Err(e) = spawned {
            warn!(error = %e, "the palette cannot search the LosOS boxes");
            return None;
        }
        Some(Self {
            queries,
            found,
            asked: None,
        })
    }

    /// Asks for `text` unless it was the last thing asked or is shorter
    /// than [`MIN_CHARS`], and returns what an earlier ask found, with the
    /// text it answers, once it has. `None` asks for nothing and forgets
    /// the last ask, for when the palette closes.
    pub fn poll(&mut self, text: Option<&str>) -> Option<(String, Vec<Value>)> {
        match text {
            Some(text)
                if self.asked.as_deref() != Some(text) && text.chars().count() >= MIN_CHARS =>
            {
                match self.queries.try_send(text.to_owned()) {
                    Ok(()) => self.asked = Some(text.to_owned()),
                    // Still searching for the last one: asked next frame.
                    Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => return None,
                }
            }
            Some(_) => {}
            None => self.asked = None,
        }
        self.found.try_recv().ok()
    }
}

/// Where a search runs.
enum Place {
    /// In this running machine, through its guest agent.
    Machine(String),
    /// Here, in the session: a phone's.
    Session,
}

/// The searching thread: one search at a time, wherever `losos-boxes` is
/// for that search.
fn search(queries: &Receiver<String>, found: &Sender<(String, Vec<Value>)>) {
    let mut hypervisor: Option<Hypervisor> = None;
    for text in queries {
        let hits = match (place(&mut hypervisor), &hypervisor) {
            (Some(Place::Machine(machine)), Some(hypervisor)) => {
                in_machine(hypervisor, &machine, &text)
            }
            (Some(Place::Session), _) => in_session(&text),
            _ => Ok(Vec::new()),
        }
        .unwrap_or_else(|e| {
            debug!(error = %e, "the LosOS boxes were not searched");
            Vec::new()
        });
        if found.send((text, hits)).is_err() {
            return;
        }
    }
}

/// Where `losos-boxes` is now: a running LosOS boxes machine, or else this
/// session's `PATH`. A machine that is off is not started for a search,
/// and a host with machines has no `losos-boxes` of its own.
fn place(hypervisor: &mut Option<Hypervisor>) -> Option<Place> {
    if hypervisor.is_none() {
        *hypervisor = Hypervisor::open().ok();
    }
    let listed = hypervisor.as_ref().map(|open| {
        open.machines().map(|machines| {
            machines
                .filter(|m| {
                    m.state == State::Running
                        && m.template.as_ref().is_some_and(|t| t.name == TEMPLATE)
                })
                .map(|m| m.name)
                .next()
        })
    });
    match listed {
        Some(Ok(running)) => return running.map(Place::Machine),
        // libvirtd went away; opened again for the next search.
        Some(Err(e)) => {
            debug!(error = %e, "libvirt did not list the machines");
            *hypervisor = None;
        }
        None => {}
    }
    on_path("losos-boxes").then_some(Place::Session)
}

/// Runs the search in the LosOS boxes machine.
fn in_machine(hypervisor: &Hypervisor, machine: &str, text: &str) -> Result<Vec<Value>, String> {
    let program = Program {
        path: IN_MACHINE.into(),
        args: vec![text.to_owned()],
        env: Vec::new(),
        capture_output: true,
        input: Vec::new(),
    };
    let exit = hypervisor
        .run(machine, &program, WAIT)
        .map_err(|e| e.to_string())?;
    if !exit.success() {
        return Err(String::from_utf8_lossy(&exit.stderr).trim().to_owned());
    }
    hits(&exit.stdout)
}

/// Runs the search in this session, given up on after [`WAIT`].
fn in_session(text: &str) -> Result<Vec<Value>, String> {
    let mut child = Command::new("losos-boxes")
        .args(["search", text])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let mut out = Vec::new();
            if let Some(mut stdout) = child.stdout.take() {
                stdout.read_to_end(&mut out).map_err(|e| e.to_string())?;
            }
            return if status.success() {
                hits(&out)
            } else {
                Err(format!("losos-boxes search exited with {status}"))
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("losos-boxes search did not finish".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The rows in `losos-boxes search`'s output, `{"hits": [...]}`.
fn hits(out: &[u8]) -> Result<Vec<Value>, String> {
    let printed: Value = serde_json::from_slice(out).map_err(|e| e.to_string())?;
    Ok(printed["hits"].as_array().cloned().unwrap_or_default())
}

/// Whether `program` is in a folder on `PATH`.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_rows_printed() {
        let out = br#"{"hits":[{"box":"mattbox","title":"budget.md"}]}"#;
        assert_eq!(
            hits(out).unwrap(),
            [json!({"box": "mattbox", "title": "budget.md"})]
        );
        assert_eq!(hits(b"{}").unwrap(), Vec::<Value>::new());
        assert!(hits(b"not json").is_err());
    }

    #[test]
    fn a_short_text_is_not_asked() {
        let mut search = BoxSearch::start().unwrap();
        assert_eq!(search.poll(Some("ab")), None);
        assert_eq!(search.asked, None);
    }
}
