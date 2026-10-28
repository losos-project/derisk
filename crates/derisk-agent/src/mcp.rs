//! The Agent's MCP server (`derisk-agent mcp`), the config that hands it and the
//! project's other servers to Claude Code, and a small client that lists a
//! server's tools for the MCP tab.

// The server is its own process and the client runs on background threads.
#![allow(clippy::disallowed_methods)]

use std::{
    io::{BufRead as _, BufReader, Write as _},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use miette::{Context as _, IntoDiagnostic as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    store::{McpServer, McpTransport, Project},
    tools::{self, ToolContext},
};

const PROTOCOL_VERSION: &str = "2025-06-18";
const LIST_TIMEOUT: Duration = Duration::from_secs(20);

/// Serves the Agent's tools as MCP over stdin and stdout until stdin closes.
pub fn serve_stdio(context: &ToolContext) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.into_diagnostic()?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = handle(context, &line) else {
            continue;
        };
        writeln!(stdout, "{reply}").into_diagnostic()?;
        stdout.flush().into_diagnostic()?;
    }
    Ok(())
}

fn handle(context: &ToolContext, line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(error) => {
            return Some(json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": error.to_string()}}));
        }
    };
    // Notifications carry no id and get no answer.
    let id = message.get("id")?.clone();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "derisk-agent", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "The Agent's desktop tools: build apps into the live preview pane, and use pm, Nix, derisk and the forge.",
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools::definitions()}),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            let output = tools::call(context, name, &arguments);
            json!({"content": [{"type": "text", "text": output.text}], "isError": output.is_error})
        }
        method => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("no method {method}")}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Claude Code's `--mcp-config` for one chat: the Agent's own server,
/// derisk's when `derisk` is on `PATH`, the servers of [`global_file`], and
/// the project's enabled servers, which win over a global one of the same
/// name.
pub fn claude_config(project: &Project, chat_id: &str) -> Result<Value> {
    let executable = std::env::current_exe().into_diagnostic()?;
    let mut servers = serde_json::Map::new();
    servers.insert(
        "desktop".into(),
        json!({
            "type": "stdio",
            "command": executable,
            "args": ["mcp", "--project", project.id, "--chat", chat_id],
        }),
    );
    // `derisk mcp` serves the session's agent tools (windows, apps, the
    // accessibility tree, input), which the customize-desktop skill and the
    // palette's "Ask the agent" lean on. Sonne listed it as a context server
    // the same way.
    if let Some(derisk) = on_path("derisk") {
        servers.insert(
            "derisk".into(),
            json!({"type": "stdio", "command": derisk, "args": ["mcp"]}),
        );
    }
    for server in global_servers()
        .iter()
        .chain(project.mcp_servers.iter().filter(|server| server.enabled))
    {
        servers.insert(server.name.clone(), transport_config(&server.transport));
    }
    Ok(json!({"mcpServers": servers}))
}

/// The MCP servers every project's chats get, in Claude Code's
/// `{"mcpServers": {...}}` shape: `$XDG_CONFIG_HOME/derisk-agent/mcp.json`.
///
/// Other programs put their servers here on the person's say-so, as
/// `losos agents install derisk-agent` puts the LosOS companion, so that
/// the Agent knows them without a project's MCP tab. Their tools run
/// without a prompt, like the Agent's own: `--print` has no terminal to ask
/// on, and a server lands in this file only once the person agreed to it.
pub fn global_file() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("derisk-agent").join("mcp.json"))
}

/// The servers of [`global_file`], none when it is missing or not JSON.
pub fn global_servers() -> Vec<McpServer> {
    let Some(file) = global_file() else {
        return Vec::new();
    };
    read_servers(&file)
}

/// The first executable `name` in `$PATH`.
fn on_path(name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

fn transport_config(transport: &McpTransport) -> Value {
    match transport {
        McpTransport::Stdio { command, args } => {
            json!({"type": "stdio", "command": command, "args": args})
        }
        McpTransport::Http { url } => json!({"type": "http", "url": url}),
    }
}

/// The servers a folder's `.mcp.json` declares, which Claude Code loads by
/// itself when it runs there.
pub fn folder_servers(folder: &Path) -> Vec<McpServer> {
    read_servers(&folder.join(".mcp.json"))
}

/// The `mcpServers` of a Claude Code MCP config file.
fn read_servers(file: &Path) -> Vec<McpServer> {
    let Ok(bytes) = std::fs::read(file) else {
        return Vec::new();
    };
    let Ok(config) = serde_json::from_slice::<Value>(&bytes) else {
        tracing::warn!("{} is not JSON", file.display());
        return Vec::new();
    };
    config["mcpServers"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, server)| {
            let transport = if let Some(url) = server["url"].as_str() {
                McpTransport::Http { url: url.into() }
            } else {
                McpTransport::Stdio {
                    command: server["command"].as_str()?.into(),
                    args: server["args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|arg| arg.as_str().map(str::to_owned))
                        .collect(),
                }
            };
            Some(McpServer {
                name: name.clone(),
                transport,
                enabled: true,
            })
        })
        .collect()
}

/// The suggested servers the MCP tab offers to add: mcsapi's policy server,
/// which `bacon mcp` or `cargo run -p mcsapi-mcp` starts on this address.
pub fn suggested_servers() -> Vec<McpServer> {
    vec![McpServer {
        name: "mcsapi".into(),
        transport: McpTransport::Http {
            url: std::env::var("MCSAPI_MCP_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8787/mcp".into()),
        },
        enabled: true,
    }]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

/// Connects to `transport`, initializes, and lists its tools.
pub fn list_tools(transport: &McpTransport) -> Result<Vec<ToolInfo>> {
    let reply = match transport {
        McpTransport::Stdio { command, args } => list_stdio(command, args)?,
        McpTransport::Http { url } => list_http(url)?,
    };
    if let Some(error) = reply.get("error") {
        bail!("the server refused tools/list: {error}");
    }
    Ok(reply["result"]["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| ToolInfo {
            name: tool["name"].as_str().unwrap_or_default().to_owned(),
            description: tool["description"].as_str().unwrap_or_default().to_owned(),
        })
        .collect())
}

fn initialize_request() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "derisk-agent", "version": env!("CARGO_PKG_VERSION")},
    }})
}

fn list_stdio(command: &str, args: &[String]) -> Result<Value> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .into_diagnostic()
        .with_context(|| format!("starting {command}"))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let reply_to = |id: u64| -> Result<Value> {
        loop {
            let line = received
                .recv_timeout(LIST_TIMEOUT)
                .into_diagnostic()
                .context("the server did not answer")?;
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && message["id"] == id
            {
                return Ok(message);
            }
        }
    };
    let result = (|| {
        writeln!(stdin, "{}", initialize_request()).into_diagnostic()?;
        reply_to(1)?;
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .into_diagnostic()?;
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        )
        .into_diagnostic()?;
        reply_to(2)
    })();
    drop(stdin);
    if let Err(error) = child.kill() {
        tracing::debug!("MCP server already exited: {error}");
    }
    child.wait().ok();
    result
}

fn list_http(url: &str) -> Result<Value> {
    let agent = crate::forge::http_agent(LIST_TIMEOUT);
    let post = |body: &Value, session: Option<&str>| -> Result<(Option<String>, String)> {
        let mut request = agent
            .post(url)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        let mut response = request
            .send_json(body)
            .into_diagnostic()
            .with_context(|| format!("POST {url}"))?;
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok((
            session,
            response.body_mut().read_to_string().into_diagnostic()?,
        ))
    };
    let (session, _) = post(&initialize_request(), None)?;
    post(
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        session.as_deref(),
    )?;
    let (_, body) = post(
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        session.as_deref(),
    )?;
    parse_http_reply(&body, 2)
}

/// A streamable-HTTP reply is either one JSON object or server-sent events
/// whose `data:` lines hold JSON-RPC messages.
fn parse_http_reply(body: &str, id: u64) -> Result<Value> {
    if let Ok(message) = serde_json::from_str::<Value>(body) {
        return Ok(message);
    }
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .find(|message| message["id"] == id)
        .context("no reply in the server's event stream")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn context() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().to_path_buf()).expect("store");
        (
            dir,
            ToolContext {
                store,
                project_id: "p".into(),
                chat_id: None,
            },
        )
    }

    #[test]
    fn server_initializes_lists_and_ignores_notifications() {
        let (_dir, context) = context();
        let init = handle(&context, &initialize_request().to_string()).expect("reply");
        assert_eq!(init["result"]["serverInfo"]["name"], "derisk-agent");
        assert!(
            handle(
                &context,
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
            )
            .is_none()
        );
        let list = handle(
            &context,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .expect("reply");
        let names: Vec<_> = list["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect();
        for expected in ["preview_build", "pm_build", "derisk_dispatch", "nix_build"] {
            assert!(
                names.iter().any(|name| name == expected),
                "{expected} listed"
            );
        }
        let unknown =
            handle(&context, r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#).expect("reply");
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn tool_errors_come_back_as_tool_results() {
        let (_dir, context) = context();
        let reply = handle(
            &context,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"artifact_save","arguments":{"path":"/nonexistent","title":"x"}}}"#,
        )
        .expect("reply");
        assert_eq!(reply["result"]["isError"], true);
    }

    #[test]
    fn event_stream_replies_parse() -> Result<()> {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
        assert_eq!(parse_http_reply(body, 2)?["result"]["tools"], json!([]));
        Ok(())
    }

    #[test]
    fn folder_servers_read_dot_mcp_json() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"mcsapi":{"type":"http","url":"http://127.0.0.1:8787/mcp"},"fs":{"command":"mcp-fs","args":["/"]}}}"#,
        ).into_diagnostic()?;
        let servers = folder_servers(dir.path());
        assert_eq!(servers.len(), 2);
        Ok(())
    }

    #[test]
    fn global_servers_reach_every_chat_and_a_project_overrides_them() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        let file = dir.path().join("mcp.json");
        // As `losos agents install derisk-agent` writes it.
        std::fs::write(
            &file,
            r#"{"mcpServers":{"losos":{"command":"/run/current-system/sw/bin/losos","args":["mcp"]}}}"#,
        )
        .into_diagnostic()?;
        let servers = read_servers(&file);
        assert_eq!(
            servers,
            vec![McpServer {
                name: "losos".into(),
                transport: McpTransport::Stdio {
                    command: "/run/current-system/sw/bin/losos".into(),
                    args: vec!["mcp".into()],
                },
                enabled: true,
            }]
        );
        assert!(read_servers(&dir.path().join("missing.json")).is_empty());
        Ok(())
    }
}
