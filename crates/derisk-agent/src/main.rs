//! `derisk-agent` opens the window; `derisk-agent derisk-agent:ask?prompt=...`
//! (the palette's "Ask the agent") or `derisk-agent --ask TEXT` opens it on a
//! new chat with the request typed in, or hands the request to the window
//! already open. The other subcommands are what the window and its agents
//! start: `derisk-agent mcp` (the Agent's MCP server for one chat),
//! `derisk-agent routine run <id>` (a routine's systemd service) and
//! `derisk-agent preview-wasm <file>` (a wasm preview's sandbox).

use std::{
    io::IsTerminal,
    time::{Duration, Instant},
};

use derisk_agent::{AgentApp, routines, store::Store, tools::ToolContext};
use mcsapi_ui::{App as _, Theme, theme::Fonts};
use miette::{Result, bail, miette};
use serde_json::json;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: derisk-agent [--ask TEXT | derisk-agent:ask?prompt=TEXT]
       derisk-agent mcp --project <id> [--chat <id>]
       derisk-agent routine run <id>
       derisk-agent preview-wasm <component.wasm>
";

fn main() -> Result<()> {
    logging();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => window(None),
        Some("--ask") => ask(args[1..].join(" ")),
        Some(url) if url.starts_with("derisk-agent:") => match derisk_agent::ask_prompt(url) {
            Some(text) => ask(text),
            None => bail!("{url} is not a link the Agent opens"),
        },
        Some("mcp") => {
            let flag = |name: &str| {
                args.iter()
                    .position(|arg| arg == name)
                    .and_then(|index| args.get(index + 1))
                    .cloned()
            };
            derisk_agent::mcp::serve_stdio(&ToolContext {
                store: Store::open_default()?,
                project_id: flag("--project")
                    .ok_or_else(|| miette!("derisk-agent mcp needs --project <id>"))?,
                chat_id: flag("--chat"),
            })
        }
        Some("routine") => match (args.get(1).map(String::as_str), args.get(2)) {
            (Some("run"), Some(id)) => {
                let chat = routines::run(&Store::open_default()?, id)?;
                println!("{chat}");
                Ok(())
            }
            _ => bail!("usage: derisk-agent routine run <id>"),
        },
        Some("preview-wasm") => {
            let component = args
                .get(1)
                .ok_or_else(|| miette!("usage: derisk-agent preview-wasm <component.wasm>"))?;
            run_wasm(component.as_ref())
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command {other}\n{USAGE}"),
    }
}

/// Hands `text` to the open window, or opens one with it. One window per
/// person: it owns the control socket the agents' tools talk to.
fn ask(text: String) -> Result<()> {
    match derisk_agent::tools::control(&json!({"method": "ask", "text": text})) {
        Ok(reply) if reply["ok"] == true => Ok(()),
        _ => window(Some(text)),
    }
}

#[cfg(feature = "wasm")]
fn run_wasm(component: &std::path::Path) -> Result<()> {
    derisk_agent_preview::wasm::run(component)
}

#[cfg(not(feature = "wasm"))]
fn run_wasm(_component: &std::path::Path) -> Result<()> {
    bail!("this derisk-agent was built without wasm previews")
}

/// How often the window re-reads the published theme.
const THEME_POLL: Duration = Duration::from_secs(1);

struct Window {
    app: AgentApp,
    theme: Theme,
    fonts: Fonts,
    theme_read: Instant,
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The theme is re-read now and then, so a change in Settings reaches
        // a window already open, as it does derisk's own apps.
        if self.theme_read.elapsed() >= THEME_POLL {
            self.theme_read = Instant::now();
            if let Some(theme) = derisk_agent::desktop_theme() {
                self.theme = theme;
            }
            self.fonts = derisk_agent::desktop_fonts();
        }
        mcsapi_components::install_fonts(ui.ctx(), &self.fonts);
        egui::CentralPanel::no_frame().show(ui, |ui| self.app.ui(ui, &self.theme));
    }
}

fn window(ask: Option<String>) -> Result<()> {
    let store = Store::open_default()?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Agent")
            .with_app_id(derisk_agent::APP_ID)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Agent",
        options,
        Box::new(move |_| {
            let mut app = AgentApp::new(store);
            if let Some(text) = ask {
                app.ask(&text);
            }
            Ok(Box::new(Window {
                app,
                theme: derisk_agent::desktop_theme().unwrap_or_default(),
                fonts: derisk_agent::desktop_fonts(),
                theme_read: Instant::now(),
            }))
        }),
    )
    .map_err(|error| miette!("{error}"))
}

/// Logs `tracing` events to stderr, which the session hands to the journal.
/// `RUST_LOG` picks what is logged: warnings and up when unset. `mcp` speaks
/// JSON-RPC on stdout, so nothing else ever writes there.
fn logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}
