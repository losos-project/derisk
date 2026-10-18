//! The tools the Agent gives its agents over MCP: build an app and show it in the
//! preview pane, and reach the rest of the desktop — pm, derisk, Nix and the
//! forge — the way a person at this desktop would.

// `derisk-agent mcp` is its own process with no UI thread; blocking on a child here is
// what a tool call is.
#![allow(clippy::disallowed_methods)]

use std::{
    io::{BufRead as _, BufReader, Write as _},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use derisk_agent_preview::PreviewSource;
use miette::{Context as _, IntoDiagnostic as _, Result, bail};
use serde_json::{Value, json};

use crate::{
    forge,
    store::{ArtifactKind, Store},
};

/// Output lines a tool returns from a command; builds print far more than an
/// agent should read.
const OUTPUT_LINES: usize = 120;

/// Who is calling: the project and chat `derisk-agent mcp` was started for.
pub struct ToolContext {
    pub store: Store,
    pub project_id: String,
    pub chat_id: Option<String>,
}

pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
}

pub fn definitions() -> Value {
    let dir = json!({"type": "string", "description": "Absolute path; defaults to the project's first folder"});
    json!([
        {
            "name": "preview_new",
            "description": "Create a new Rust app that the Agent can show in its preview pane. It draws with egui through derisk_agent_preview::serve and builds for Linux and for wasm32-wasip2. Edit src/main.rs afterwards, then call preview_build.",
            "inputSchema": {"type": "object", "required": ["dir", "name"], "properties": {
                "dir": {"type": "string", "description": "Absolute path of the new app's folder; must be empty or not exist yet"},
                "name": {"type": "string", "description": "Cargo package name, such as todo-app"}
            }}
        },
        {
            "name": "preview_build",
            "description": "Build a preview app with cargo and show it live in the preview pane on the right of the user's Agent window. target 'wasm' builds wasm32-wasip2 and runs it sandboxed with no file or network access; 'native' builds a Linux binary. Returns cargo's errors on failure. The app is saved as an artifact of this project.",
            "inputSchema": {"type": "object", "required": ["dir"], "properties": {
                "dir": dir,
                "target": {"enum": ["wasm", "native"], "default": "wasm"},
                "package": {"type": "string", "description": "Cargo package to build in a workspace"},
                "title": {"type": "string", "description": "Name for the artifact; defaults to the package"}
            }}
        },
        {
            "name": "preview_status",
            "description": "What the preview pane is running, how many frames it has drawn, and the last lines it wrote to stderr. Use after preview_build to check the app started.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "artifact_save",
            "description": "List a file you produced (a document, an image, a release build) under this project's artifacts in the Agent's sidebar.",
            "inputSchema": {"type": "object", "required": ["path", "title"], "properties": {
                "path": {"type": "string"}, "title": {"type": "string"}
            }}
        },
        {
            "name": "pm_explain",
            "description": "Ask pm, LosOS's system manager, which grants and fingerprints a pm build file needs, without running it.",
            "inputSchema": {"type": "object", "required": ["build_file"], "properties": {"build_file": {"type": "string"}}}
        },
        {
            "name": "pm_build",
            "description": "Build a pm build file into a signed .cpkg in pm's sandbox. Network is granted per build file; a step that passes --offline gets none.",
            "inputSchema": {"type": "object", "required": ["build_file"], "properties": {"build_file": {"type": "string"}}}
        },
        {
            "name": "nix_build",
            "description": "Build a flake output, such as the LosOS image (.#image) or a package (.#derisk), and return its store paths.",
            "inputSchema": {"type": "object", "required": ["installable"], "properties": {"installable": {"type": "string"}, "dir": dir}}
        },
        {
            "name": "nix_eval",
            "description": "Evaluate a flake attribute without building it, for example a NixOS option.",
            "inputSchema": {"type": "object", "required": ["installable"], "properties": {"installable": {"type": "string"}, "dir": dir}}
        },
        {
            "name": "derisk_state",
            "description": "Describe the derisk desktop: workspaces, windows, focus, tray, failed systemd units, battery.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "derisk_dispatch",
            "description": "Apply derisk desktop actions in order, for example [{\"action\":\"snap\",\"zone\":\"right\"}]. Call derisk_tools for the full action schema.",
            "inputSchema": {"type": "object", "required": ["actions"], "properties": {"actions": {"type": "array", "items": {"type": "object"}}}}
        },
        {
            "name": "derisk_tools",
            "description": "derisk's own tool list, including every action derisk_dispatch accepts.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "forge_pull_requests",
            "description": "Open pull requests (GitHub, Forgejo) or merge requests (GitLab) of a folder's origin remote.",
            "inputSchema": {"type": "object", "properties": {"dir": dir}}
        }
    ])
}

pub fn call(context: &ToolContext, name: &str, arguments: &Value) -> ToolOutput {
    match run(context, name, arguments) {
        Ok(text) => ToolOutput {
            text,
            is_error: false,
        },
        Err(error) => ToolOutput {
            text: crate::describe(&error),
            is_error: true,
        },
    }
}

fn run(context: &ToolContext, name: &str, arguments: &Value) -> Result<String> {
    let text = |key: &str| arguments.get(key).and_then(Value::as_str);
    let required = |key: &str| text(key).with_context(|| format!("missing `{key}`"));
    let dir = || -> Result<PathBuf> {
        match text("dir") {
            Some(dir) => Ok(PathBuf::from(dir)),
            None => context
                .store
                .project(&context.project_id)?
                .folders
                .first()
                .cloned()
                .context("the project has no folder; pass `dir`"),
        }
    };
    match name {
        "preview_new" => scaffold(Path::new(required("dir")?), required("name")?),
        "preview_build" => preview_build(
            context,
            &dir()?,
            text("target").unwrap_or("wasm"),
            text("package"),
            text("title"),
        ),
        "preview_status" => Ok(control(&json!({"method": "preview_status"}))?.to_string()),
        "artifact_save" => {
            let path = PathBuf::from(required("path")?);
            if !path.exists() {
                bail!("{} does not exist", path.display());
            }
            let artifact = context.store.add_artifact(
                &context.project_id,
                context.chat_id.as_deref(),
                required("title")?,
                ArtifactKind::File { path },
            )?;
            control(&json!({"method": "store_changed"})).ok();
            Ok(format!("saved artifact {}", artifact.title))
        }
        "pm_explain" => command_output(
            Command::new("pm")
                .arg("explain")
                .arg(required("build_file")?),
            "pm",
        ),
        "pm_build" => command_output(
            Command::new("pm").arg("build").arg(required("build_file")?),
            "pm",
        ),
        "nix_build" => command_output(
            Command::new("nix")
                .args([
                    "build",
                    "--no-link",
                    "--print-out-paths",
                    "--print-build-logs",
                ])
                .arg(required("installable")?)
                .current_dir(dir()?),
            "nix",
        ),
        "nix_eval" => command_output(
            Command::new("nix")
                .args(["eval", "--json"])
                .arg(required("installable")?)
                .current_dir(dir()?),
            "nix",
        ),
        "derisk_state" => derisk(&json!({"method": "state"})).map(|value| value.to_string()),
        "derisk_tools" => derisk(&json!({"method": "tools"})).map(|value| value.to_string()),
        "derisk_dispatch" => derisk(&json!({
            "method": "dispatch",
            "actions": arguments.get("actions").cloned().unwrap_or(json!([])),
        }))
        .map(|value| value.to_string()),
        "forge_pull_requests" => {
            let repo = forge::repo_for_folder(&dir()?)?;
            let pulls = forge::open_pull_requests(&repo)?;
            Ok(serde_json::to_string(&json!({"repo": repo, "open": pulls})).into_diagnostic()?)
        }
        _ => bail!("unknown tool {name}"),
    }
}

fn command_output(command: &mut Command, program: &str) -> Result<String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .into_diagnostic()
        .with_context(|| format!("{program} is not installed or not on PATH"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let text = tail(&text, OUTPUT_LINES);
    if output.status.success() {
        Ok(text)
    } else {
        bail!("{program} failed ({}):\n{text}", output.status)
    }
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let skip = all.len().saturating_sub(lines);
    let mut kept = all[skip..].join("\n");
    if skip > 0 {
        kept.insert_str(0, &format!("[{skip} earlier lines cut]\n"));
    }
    kept
}

fn preview_build(
    context: &ToolContext,
    dir: &Path,
    target: &str,
    package: Option<&str>,
    title: Option<&str>,
) -> Result<String> {
    let mut command = Command::new("cargo");
    command
        .args([
            "build",
            "--release",
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(dir)
        .stdin(Stdio::null());
    let wasm = match target {
        "wasm" => {
            command.args(["--target", "wasm32-wasip2"]);
            true
        }
        "native" => false,
        other => bail!("unknown target {other}; use wasm or native"),
    };
    if let Some(package) = package {
        command.args(["--package", package]);
    }
    let output = command
        .output()
        .into_diagnostic()
        .context("cargo is not on PATH")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!("cargo build failed:\n{}", tail(&stderr, OUTPUT_LINES));
    }
    let (package_name, executable) = String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter_map(|message| {
            let executable = message["executable"].as_str()?.to_owned();
            let package = message["target"]["name"].as_str()?.to_owned();
            Some((package, executable))
        })
        .next()
        .context("cargo built no binary; a preview app needs a [[bin]] or src/main.rs")?;
    let source = if wasm {
        PreviewSource::Wasm {
            component: PathBuf::from(&executable),
            // Saved with the artifact, so it has to outlive a system update.
            runner: crate::stable_executable().into_diagnostic()?,
        }
    } else {
        PreviewSource::Native {
            program: PathBuf::from(&executable),
            args: Vec::new(),
            cwd: Some(dir.to_path_buf()),
        }
    };
    context.store.add_artifact(
        &context.project_id,
        context.chat_id.as_deref(),
        title.unwrap_or(&package_name),
        ArtifactKind::App {
            source: source.clone(),
        },
    )?;
    let shown = match control(&json!({"method": "preview_run", "source": source})) {
        Ok(_) => "It is running in the preview pane now; call preview_status to check it drew.",
        Err(_) => "The Agent's window is not open, so it was only saved as an artifact.",
    };
    Ok(format!("Built {executable}. {shown}"))
}

/// The guest half of `derisk_agent_preview`, vendored into each new app so it builds
/// without a checkout of this repository.
const GUEST_SOURCE: &str = include_str!("../../derisk-agent-preview/src/lib.rs");

fn guest_source() -> String {
    let mut lines = GUEST_SOURCE.lines().peekable();
    let mut kept = String::new();
    while let Some(line) = lines.next() {
        // Each host-only item is one `#[cfg(feature = ...)]` line and the item
        // under it.
        if line.contains("cfg(feature = \"host\")") || line.contains("cfg(feature = \"wasm\")") {
            lines.next();
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

fn scaffold(dir: &Path, name: &str) -> Result<String> {
    // An empty folder is fine: the setup wizard creates the app's folder
    // before the agent scaffolds into it.
    if dir.exists() && std::fs::read_dir(dir).into_diagnostic()?.next().is_some() {
        bail!("{} already exists and is not empty", dir.display());
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("`{name}` is not a valid cargo package name");
    }
    let guest = dir.join("derisk-agent-preview");
    std::fs::create_dir_all(dir.join("src")).into_diagnostic()?;
    std::fs::create_dir_all(guest.join("src")).into_diagnostic()?;
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
egui = {{ version = "0.36.2", default-features = false, features = ["default_fonts"] }}
derisk_agent_preview = {{ path = "derisk-agent-preview" }}

# A workspace of its own, so a parent workspace does not claim the app.
[workspace]
members = ["derisk-agent-preview"]

[profile.release]
debug = false
"#
        ),
    )
    .into_diagnostic()?;
    std::fs::write(
        guest.join("Cargo.toml"),
        r#"[package]
name = "derisk_agent_preview"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/derisk_agent_preview.rs"

[dependencies]
egui = { version = "0.36.2", default-features = false, features = ["serde"] }
postcard = { version = "1.1", default-features = false, features = ["use-std"] }
serde = { version = "1", features = ["derive"] }
"#,
    )
    .into_diagnostic()?;
    std::fs::write(guest.join("src/derisk_agent_preview.rs"), guest_source()).into_diagnostic()?;
    std::fs::write(
        dir.join("src/main.rs"),
        format!(
            r#"//! Drawn by the Agent's preview pane; stdout carries frames, so log to stderr.

fn main() -> std::io::Result<()> {{
    let mut clicks = 0u32;
    derisk_agent_preview::serve("{name}", move |ui, theme| {{
        ui.heading(egui::RichText::new("{name}").color(theme.foreground));
        if ui.button(format!("Clicked {{clicks}} times")).clicked() {{
            clicks += 1;
        }}
    }})
}}
"#
        ),
    )
    .into_diagnostic()?;
    std::fs::write(dir.join(".gitignore"), "/target\n").into_diagnostic()?;
    Ok(format!(
        "Created {name} in {}. Edit src/main.rs, then call preview_build with dir {}.",
        dir.display(),
        dir.display()
    ))
}

/// The socket the Agent's window listens on for preview and store messages.
pub fn control_socket() -> PathBuf {
    if let Some(path) = std::env::var_os("DERISK_AGENT_CONTROL_SOCKET") {
        return PathBuf::from(path);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    runtime.join("derisk-agent").join("control.sock")
}

/// Sends one JSON line to the Agent's window and returns its reply.
pub fn control(request: &Value) -> Result<Value> {
    exchange(&control_socket(), request, Duration::from_secs(10))
}

/// derisk's agent socket: `$DERISK_AGENT_SOCKET`, else
/// `$XDG_RUNTIME_DIR/derisk/agent.sock`, the same lookup `derisk ctl` uses.
fn derisk_socket() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DERISK_AGENT_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join("derisk").join("agent.sock"))
}

fn derisk(request: &Value) -> Result<Value> {
    let reply = exchange(&derisk_socket()?, request, Duration::from_secs(30))
        .context("derisk is not running in this session")?;
    if reply["ok"] == true {
        Ok(reply["result"].clone())
    } else {
        bail!("derisk refused: {}", reply["error"])
    }
}

fn exchange(socket: &Path, request: &Value, timeout: Duration) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .into_diagnostic()
        .with_context(|| format!("connecting to {}", socket.display()))?;
    stream.set_read_timeout(Some(timeout)).into_diagnostic()?;
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).into_diagnostic()?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .into_diagnostic()?;
    serde_json::from_str(&reply)
        .into_diagnostic()
        .with_context(|| format!("bad reply from {}", socket.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_guest_drops_the_host_and_wasm_modules() {
        let source = guest_source();
        assert!(source.contains("pub fn serve("));
        assert!(!source.contains("mod host"));
        assert!(!source.contains("pub mod wasm"));
        assert!(!source.contains("pub use host::"));
    }

    #[test]
    fn scaffold_refuses_folders_with_files_and_bad_names() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        std::fs::write(dir.path().join("notes.txt"), "").into_diagnostic()?;
        assert!(scaffold(dir.path(), "app").is_err());
        std::fs::create_dir(dir.path().join("empty")).into_diagnostic()?;
        scaffold(&dir.path().join("empty"), "empty")?;
        assert!(scaffold(&dir.path().join("x"), "bad name").is_err());
        scaffold(&dir.path().join("app"), "app")?;
        assert!(
            dir.path()
                .join("app/derisk-agent-preview/src/derisk_agent_preview.rs")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("a\nb\nc", 2), "[1 earlier lines cut]\nb\nc");
        assert_eq!(tail("a", 5), "a");
    }
}
