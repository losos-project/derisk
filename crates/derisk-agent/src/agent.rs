//! Runs one chat turn through Claude Code (`claude -p`), with the Agent's MCP
//! server attached, and turns its `stream-json` output into transcript entries.

// A turn runs on a thread of its own, never on the UI thread.
#![allow(clippy::disallowed_methods)]

use std::{
    io::{BufRead as _, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
};

use chrono::Utc;
use miette::{Context as _, IntoDiagnostic as _, Result};
use serde_json::Value;

use crate::{
    mcp, skills,
    store::{Chat, Entry, Project, Role, Store},
};

/// How the Agent's chats are told to use the desktop. Appended to Claude Code's own
/// system prompt, after the project's instructions.
const DESKTOP_PROMPT: &str = "\
You are working inside derisk's Agent, a desktop app for building software on LosOS. \
The user sees this chat in the middle of the window and a live preview pane on \
the right.

When the user wants an app, build a real, fully-featured one and show it: \
call the desktop preview_new tool to create a Rust app that draws with egui, \
write the app in its src/main.rs, then call preview_build. Prefer the wasm \
target (wasm32-wasip2): it runs sandboxed and starts fast. Use native only when \
the app needs files, network or processes. Read cargo's errors from \
preview_build, fix them and build again until the preview runs, then check it \
with preview_status. Keep stdout free of prints in preview apps; it carries \
frames. Log to stderr.

LosOS is a NixOS system whose desktop is derisk and whose system manager is \
pm. Use nix_build and nix_eval for the LosOS flake and Nix packages, pm_explain \
and pm_build for pm build files, and derisk_state, derisk_tools and \
derisk_dispatch, or the derisk server's tools, to see and arrange the user's \
desktop. The derisk skills (customize-desktop, create-skill, \
reduce-online-fingerprint) say how to restyle the desktop, write new skills \
and delete unused online accounts; use them when the user asks for those. Use \
forge_pull_requests to see open pull or merge requests, and artifact_save to \
list documents and other files you produce in the sidebar.";

/// What a running turn reports.
#[derive(Debug)]
pub enum AgentEvent {
    /// Claude Code started: its session ID and each MCP server's status.
    Started {
        session_id: String,
        servers: Vec<(String, String)>,
    },
    Entry(Entry),
    /// The process exited; the turn is over.
    Finished {
        error: Option<String>,
    },
}

pub struct Turn {
    child: Child,
    pub events: mpsc::Receiver<AgentEvent>,
}

impl Turn {
    /// Stops the turn early. Claude Code saves the session up to here, so the
    /// next turn resumes it.
    pub fn stop(&mut self) {
        if let Err(error) = self.child.kill() {
            tracing::debug!("agent already exited: {error}");
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.stop();
        self.child.wait().ok();
    }
}

fn claude_program() -> PathBuf {
    std::env::var_os("DERISK_AGENT_CLAUDE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("claude"))
}

/// The `claude` command line for `prompt` in `chat`.
pub fn command(project: &Project, chat: &Chat, prompt: &str, store: &Store) -> Result<Command> {
    let mut command = Command::new(claude_program());
    command
        .arg("--print")
        .arg(prompt)
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--permission-mode", project.permission_mode.flag()])
        // The Agent's own tools and derisk's run without a prompt: there is
        // no terminal in `--print` mode for Claude Code to ask on. derisk
        // asks on screen itself before anything that logs out or loses work.
        .args(["--allowedTools", "mcp__desktop", "mcp__derisk"])
        // And the servers the person put in every chat (mcp::global_file).
        .args(
            mcp::global_servers()
                .iter()
                .map(|server| format!("mcp__{}", server.name)),
        )
        .arg("--mcp-config")
        .arg(mcp::claude_config(project, &chat.id)?.to_string())
        // The built-in skills, for this turn only.
        .arg("--plugin-dir")
        .arg(skills::install(store.root())?);
    let mut system = String::new();
    if !project.instructions.trim().is_empty() {
        system.push_str("Project instructions:\n");
        system.push_str(project.instructions.trim());
        system.push_str("\n\n");
    }
    system.push_str(DESKTOP_PROMPT);
    command.arg("--append-system-prompt").arg(system);
    match &chat.session_id {
        Some(session) => {
            command.arg("--resume").arg(session);
        }
        None => {
            // Chosen here so the chat knows its session before the first event.
            command
                .arg("--session-id")
                .arg(uuid::Uuid::new_v4().to_string());
        }
    }
    let mut folders = project.folders.iter();
    match folders.next() {
        Some(first) => {
            command.current_dir(first);
        }
        None => {
            command.current_dir(store.root());
        }
    }
    for folder in folders {
        command.arg("--add-dir").arg(folder);
    }
    Ok(command)
}

/// Starts a turn; events arrive on [`Turn::events`] and `notify` runs after
/// each, so a window can repaint.
pub fn start(
    project: &Project,
    chat: &Chat,
    prompt: &str,
    store: &Store,
    notify: impl Fn() + Send + 'static,
) -> Result<Turn> {
    let mut child = command(project, chat, prompt, store)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .into_diagnostic()
        .with_context(|| {
            format!(
                "starting {}; install Claude Code or set DERISK_AGENT_CLAUDE",
                claude_program().display()
            )
        })?;
    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = child.stderr.take().context("no stderr")?;
    let (sender, events) = mpsc::channel();
    let stderr_tail = std::thread::spawn(move || {
        let mut tail = Vec::new();
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            tail.push(line);
            if tail.len() > 20 {
                tail.remove(0);
            }
        }
        tail.join("\n")
    });
    std::thread::spawn(move || {
        let mut saw_result = false;
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            for event in parse_line(&line) {
                if matches!(&event, AgentEvent::Entry(entry) if entry.role == Role::Status) {
                    saw_result = true;
                }
                if sender.send(event).is_err() {
                    return;
                }
                notify();
            }
        }
        let stderr = stderr_tail.join().unwrap_or_default();
        let error = (!saw_result).then(|| {
            if stderr.trim().is_empty() {
                "Claude Code exited before finishing the turn".to_owned()
            } else {
                stderr
            }
        });
        sender.send(AgentEvent::Finished { error }).ok();
        notify();
    });
    Ok(Turn { child, events })
}

fn entry(role: Role, text: impl Into<String>) -> AgentEvent {
    AgentEvent::Entry(Entry {
        role,
        text: text.into(),
        at: Utc::now(),
    })
}

/// One `stream-json` line to zero or more events.
pub fn parse_line(line: &str) -> Vec<AgentEvent> {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    match message["type"].as_str() {
        Some("system") if message["subtype"] == "init" => vec![AgentEvent::Started {
            session_id: message["session_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            servers: message["mcp_servers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|server| {
                    (
                        server["name"].as_str().unwrap_or_default().to_owned(),
                        server["status"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect(),
        }],
        Some("assistant") => message["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| match block["type"].as_str()? {
                "text" => Some(entry(Role::Assistant, block["text"].as_str()?)),
                "tool_use" => Some(entry(
                    Role::Tool,
                    format!(
                        "{} {}",
                        block["name"].as_str().unwrap_or("tool"),
                        compact(&block["input"])
                    ),
                )),
                _ => None,
            })
            .collect(),
        Some("user") => message["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "tool_result")
            .map(|block| {
                let text = match &block["content"] {
                    Value::String(text) => text.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(|part| part["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    other => other.to_string(),
                };
                let role = if block["is_error"] == true {
                    Role::Error
                } else {
                    Role::ToolResult
                };
                entry(role, text)
            })
            .collect(),
        Some("result") => {
            let seconds = message["duration_ms"].as_f64().unwrap_or_default() / 1000.0;
            let cost = message["total_cost_usd"].as_f64().unwrap_or_default();
            let mut events = Vec::new();
            if message["is_error"] == true {
                events.push(entry(
                    Role::Error,
                    message["result"].as_str().unwrap_or("the turn failed"),
                ));
            }
            events.push(entry(
                Role::Status,
                format!("Finished in {seconds:.1}s, ${cost:.4}"),
            ));
            events
        }
        _ => Vec::new(),
    }
}

fn compact(input: &Value) -> String {
    let text = input.to_string();
    if text.chars().count() > 300 {
        let cut: String = text.chars().take(300).collect();
        format!("{cut}…")
    } else {
        text
    }
}

/// Runs a whole turn on the calling thread, saving the chat as it goes. Used by
/// routines, which have no window.
pub fn run_to_end(store: &Store, project: &Project, chat: &mut Chat, prompt: &str) -> Result<()> {
    chat.entries.push(Entry {
        role: Role::User,
        text: prompt.to_owned(),
        at: Utc::now(),
    });
    store.save_chat(chat)?;
    let turn = start(project, chat, prompt, store, || {})?;
    for event in turn.events.iter() {
        match event {
            AgentEvent::Started { session_id, .. } => chat.session_id = Some(session_id),
            AgentEvent::Entry(entry) => chat.entries.push(entry),
            AgentEvent::Finished { error } => {
                if let Some(error) = error {
                    chat.entries.push(Entry {
                        role: Role::Error,
                        text: error,
                        at: Utc::now(),
                    });
                }
                break;
            }
        }
        chat.updated = Utc::now();
        store.save_chat(chat)?;
    }
    chat.updated = Utc::now();
    store.save_chat(chat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_json_lines_become_entries() {
        let init = parse_line(
            r#"{"type":"system","subtype":"init","session_id":"abc","mcp_servers":[{"name":"desktop","status":"connected"}]}"#,
        );
        assert!(
            matches!(&init[..], [AgentEvent::Started { session_id, servers }] if session_id == "abc" && servers[0].1 == "connected")
        );

        let assistant = parse_line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Building it."},{"type":"tool_use","name":"mcp__desktop__preview_build","input":{"dir":"/tmp/app"}}]}}"#,
        );
        assert_eq!(assistant.len(), 2);
        assert!(
            matches!(&assistant[1], AgentEvent::Entry(entry) if entry.role == Role::Tool && entry.text.starts_with("mcp__desktop__preview_build"))
        );

        let result = parse_line(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":[{"type":"text","text":"cargo build failed"}]}]}}"#,
        );
        assert!(
            matches!(&result[..], [AgentEvent::Entry(entry)] if entry.role == Role::Error && entry.text == "cargo build failed")
        );

        let done = parse_line(
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":2500,"total_cost_usd":0.0123,"result":"ok"}"#,
        );
        assert!(
            matches!(&done[..], [AgentEvent::Entry(entry)] if entry.text == "Finished in 2.5s, $0.0123")
        );
        assert!(parse_line("not json").is_empty());
    }
}
