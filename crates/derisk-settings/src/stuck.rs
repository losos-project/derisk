//! Noticing an action of Settings that never finishes. A page draws work
//! in progress with [`working`]: a spinner and the work's label. Once the
//! same work has run for [`AFTER`], the line says so and, where this system
//! sends reports, offers to report it.
//!
//! Settings runs inside the session, so it asks the session over the agent
//! socket, the way any program would: `state` says whether reports can be
//! sent (`can_report`), and `report_stuck` shows the report. The session
//! writes it, with this process's journal, and nothing is sent unless the
//! person sends it.

use std::time::Duration;

use mcsapi_components::Tokens;
use mcsapi_ui::egui;

/// How long an action may take before it is called stuck. Bluetooth
/// pairing and a backup's first contact with its box both finish well
/// within it.
pub const AFTER: Duration = Duration::from_secs(30);

/// The app ID reports name.
#[cfg(not(target_family = "wasm"))]
const APP: &str = "org.derisk.settings";

/// What one piece of work's line remembers between frames.
#[derive(Clone, Copy, Default)]
struct Seen {
    /// When it was first drawn, in egui's seconds.
    since: f64,
    /// When it was last drawn: a gap means it finished and a new one began.
    last: f64,
    /// Whether the session was asked if it sends reports.
    asked: bool,
    /// Whether it was reported already.
    reported: bool,
}

/// A spinner with `label` beside it, for work in progress, and after
/// [`AFTER`] a line saying it is taking too long, with Report beside it
/// where the session can send reports. Returns whether it is that late.
pub fn working(ui: &mut egui::Ui, label: &str) -> bool {
    let id = egui::Id::new(("derisk-settings-working", label));
    let now = ui.input(|i| i.time);
    let mut seen = ui
        .ctx()
        .data_mut(|d| d.get_temp::<Seen>(id))
        .filter(|s| now - s.last < 1.0)
        .unwrap_or(Seen {
            since: now,
            ..Seen::default()
        });
    seen.last = now;
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(label);
    });
    let seconds = (now - seen.since).max(0.0);
    let late = seconds >= AFTER.as_secs_f64();
    // The session's answer, which a thread of its own stores: apart from
    // `seen`, which this frame writes back whole.
    let answer = id.with("can-report");
    if late {
        if !seen.asked {
            // Asked once, off the UI thread: the answer comes from the
            // session's own loop, which is drawing this frame.
            seen.asked = true;
            ask_can_report(ui.ctx().clone(), answer);
        }
        let can_report = ui.ctx().data(|d| d.get_temp::<bool>(answer)) == Some(true);
        let tokens = Tokens::current(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("This is taking much longer than it should.")
                    .color(tokens.muted_foreground),
            );
            if can_report {
                if seen.reported {
                    ui.label(egui::RichText::new("Reported.").color(tokens.muted_foreground));
                } else if ui.small_button("Report it").clicked() {
                    seen.reported = true;
                    report(label, seconds as u64);
                }
            }
        });
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, seen));
    // The moment it turns late has to be drawn without input.
    ui.ctx().request_repaint_after(Duration::from_secs(1));
    late
}

/// Asks the session whether it sends reports, and stores the answer at
/// `answer`.
#[cfg(not(target_family = "wasm"))]
fn ask_can_report(ctx: egui::Context, answer: egui::Id) {
    std::thread::spawn(move || {
        let can = agent::call(&serde_json::json!({"method": "state"}))
            .ok()
            .and_then(|state| state["can_report"].as_bool())
            .unwrap_or(false);
        ctx.data_mut(|d| d.insert_temp(answer, can));
        ctx.request_repaint();
    });
}

/// A browser page has no session to ask.
#[cfg(target_family = "wasm")]
fn ask_can_report(_ctx: egui::Context, _answer: egui::Id) {}

/// Asks the session to show the report of `label`, `seconds` in.
#[cfg(not(target_family = "wasm"))]
fn report(label: &str, seconds: u64) {
    let request = serde_json::json!({
        "method": "dispatch",
        "actions": [{
            "action": "report_stuck",
            "app": APP,
            "label": label,
            "seconds": seconds,
            "pid": std::process::id(),
        }],
    });
    // The session just said it sends reports, so this goes through but for
    // a session that is gone, which has nowhere to show the report anyway.
    std::thread::spawn(move || {
        let _ = agent::call(&request);
    });
}

#[cfg(target_family = "wasm")]
fn report(_label: &str, _seconds: u64) {}

/// The session's agent socket, from a thread.
#[cfg(not(target_family = "wasm"))]
mod agent {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        path::PathBuf,
        time::Duration,
    };

    use serde_json::Value;

    /// `$DERISK_AGENT_SOCKET`, else `$XDG_RUNTIME_DIR/derisk/agent.sock`,
    /// the lookup derisk makes.
    fn path() -> Option<PathBuf> {
        if let Some(path) = derisk_sys::env::var_os("DERISK_AGENT_SOCKET") {
            return Some(path.into());
        }
        let dir = derisk_sys::env::var_os("XDG_RUNTIME_DIR")?;
        Some(PathBuf::from(dir).join("derisk").join("agent.sock"))
    }

    /// Sends one request and returns its `result`.
    pub fn call(request: &Value) -> Result<Value, String> {
        let path = path().ok_or("XDG_RUNTIME_DIR is not set")?;
        let mut stream = UnixStream::connect(&path).map_err(|e| e.to_string())?;
        // The session answers between frames; one that does not within
        // this is not going to.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        writeln!(stream, "{request}").map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let mut reply: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if reply["ok"] == Value::Bool(true) {
            Ok(reply["result"].take())
        } else {
            Err(reply["error"].as_str().unwrap_or("refused").to_owned())
        }
    }
}
