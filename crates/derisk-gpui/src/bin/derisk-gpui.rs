//! Opens one derisk core app in a GPUI window.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// `derisk-gpui` takes exactly one app ID, or `ask` and a question.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(
    "usage: derisk-gpui <app-id> | derisk-gpui ask --title <question> [--subtitle, --body, --grant, --deny, --app, --parent <value>] | derisk-gpui pick color|area --image <png> [--app <name>]"
)]
#[diagnostic(code(derisk_gpui::usage))]
struct Usage {
    #[help]
    apps: String,
}

/// `derisk-gpui ask` or `pick` was given options it cannot read.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("derisk-gpui: {0}")]
#[diagnostic(code(derisk_gpui::ask_usage))]
struct AskUsage(String);

fn main() -> miette::Result<()> {
    logging();
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("ask") {
        let question = derisk_gpui::ask::Question::from_args(args.skip(1)).map_err(AskUsage)?;
        // The answer is the exit status, which is all the portal reads:
        // 0 grants, 1 declines or closes.
        std::process::exit(if derisk_gpui::ask::ask(question) {
            0
        } else {
            1
        });
    }
    if args.peek().map(String::as_str) == Some("pick") {
        let pick = derisk_gpui::pick::Pick::from_args(args.skip(1)).map_err(AskUsage)?;
        // The portal reads the answer from stdout and whether there is one
        // from the exit status.
        match derisk_gpui::pick::pick(pick) {
            Some(answer) => {
                println!("{answer}");
                std::process::exit(0);
            }
            None => std::process::exit(1),
        }
    }
    let id = match (args.next(), args.next()) {
        (Some(id), None) if !id.starts_with('-') => id,
        _ => {
            return Err(Usage {
                apps: format!("apps: {}", derisk_gpui::APPS.join(", ")),
            }
            .into());
        }
    };
    // Accept the .desktop file name too, as `derisk launch` does.
    let id = id.strip_suffix(".desktop").unwrap_or(&id);
    derisk_gpui::run(id).map_err(|error| miette::miette!("{error}"))
}

/// Logs `tracing` events to stderr, which the session's unit or the
/// compositor that spawned this hands to the journal. `RUST_LOG` picks what
/// is logged: `info` and up when it is unset or does not parse.
fn logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        // No colour escapes when stderr is the journal or a pipe.
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}
