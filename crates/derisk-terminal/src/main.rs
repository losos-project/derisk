//! `derisk-terminal [--renderer auto|gpu|software] [-D DIR] [-a APP_ID] [-T TITLE] [[-e] COMMAND [ARG...]]`

use std::io::IsTerminal;

use derisk_terminal::{Options, Renderer};
use tracing_subscriber::EnvFilter;

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(
    "usage: derisk-terminal [--renderer auto|gpu|software] [-D DIR] [-a APP_ID] [-T TITLE] [[-e] COMMAND [ARG...]]"
)]
#[diagnostic(
    code(derisk_terminal::usage),
    help("DERISK_TERMINAL_RENDERER sets the renderer too; the option wins")
)]
struct Usage;

fn main() -> miette::Result<()> {
    logging();
    let mut options = Options {
        renderer: std::env::var("DERISK_TERMINAL_RENDERER")
            .ok()
            .and_then(|r| Renderer::parse(&r))
            .unwrap_or_default(),
        ..Options::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        // `--name=value` and `--name value` both, as in foot, so its
        // command lines (desktop entries, scripts) run here unchanged.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next()).ok_or(Usage);
        match flag {
            "-h" | "--help" => {
                println!("{Usage}");
                return Ok(());
            }
            // Everything after -e is the command, as in xterm and foot.
            "-e" | "--command" => {
                options.command = Some(args.by_ref().collect());
            }
            "-D" | "--working-directory" => options.directory = Some(value()?.into()),
            "-a" | "--app-id" => options.name.app_id = value()?,
            "-T" | "--title" => options.name.title = value()?,
            "--renderer" => {
                options.renderer = Renderer::parse(&value()?).ok_or(Usage)?;
            }
            _ if flag.starts_with('-') => return Err(Usage.into()),
            // The first word that is not an option starts the command, as
            // in foot.
            _ => {
                options.command = Some(std::iter::once(arg).chain(args.by_ref()).collect());
            }
        }
    }
    Ok(derisk_terminal::run(options)?)
}

/// Logs `tracing` events to stderr, which the session's unit hands to the
/// journal. `RUST_LOG` picks what is logged: `info` and up when unset.
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
