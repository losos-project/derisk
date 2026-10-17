//! `derisk-index`: indexes the files under home and answers the palette's
//! searches (the library's docs say how), or with `search`, asks the
//! running service from a terminal.

use std::{
    io::IsTerminal,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::{fs::DirBuilderExt, net::UnixListener},
    },
    path::{Path, PathBuf},
};

use miette::{IntoDiagnostic, WrapErr, miette};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: derisk-index [--home DIR] [--index DIR]
       derisk-index search WORDS...

Indexes the names and text of the files under home and answers searches on
$XDG_RUNTIME_DIR/derisk/index.sock, or on the socket systemd passes
(derisk-index.socket). `search` asks the running service.

  --home DIR    what to index (default: $HOME)
  --index DIR   where the index goes (default: $CACHE_DIRECTORY, which
                derisk-index.service sets, or $XDG_CACHE_HOME/derisk-index)";

fn main() -> miette::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("search") {
        return search(&args[1..].join(" "));
    }
    let mut home = std::env::var_os("HOME").map(PathBuf::from);
    let mut index = default_index();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--home" => home = it.next().map(PathBuf::from),
            "--index" => index = it.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(miette!("unknown option {other}\n\n{USAGE}")),
        }
    }
    let home = home.ok_or_else(|| miette!("no home to index: HOME is unset"))?;
    let index = index.ok_or_else(|| miette!("nowhere to keep the index: give --index"))?;
    logging();
    let listener = listener()?;
    derisk_index::service::run(&home, &index, &listener)
        .into_diagnostic()
        .wrap_err_with(|| format!("indexing {}", home.display()))
}

/// `$CACHE_DIRECTORY`, which `CacheDirectory=` sets, or `derisk-index`
/// under the XDG cache.
fn default_index() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CACHE_DIRECTORY").filter(|d| !d.is_empty()) {
        return Some(dir.into());
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
    Some(cache.join("derisk-index"))
}

/// The socket systemd passed, or one bound at [`derisk_index::socket`].
fn listener() -> miette::Result<UnixListener> {
    let passed = std::env::var("LISTEN_PID").is_ok_and(|pid| pid.parse() == Ok(std::process::id()))
        && std::env::var("LISTEN_FDS").is_ok_and(|n| n.parse::<u32>().is_ok_and(|n| n >= 1));
    if passed {
        // SAFETY: systemd passed this listening socket, as fd 3
        // (SD_LISTEN_FDS_START), to this very process (LISTEN_PID matched),
        // and nothing else in the process owns it.
        return Ok(UnixListener::from(unsafe { OwnedFd::from_raw_fd(3) }));
    }
    let path = derisk_index::socket().ok_or_else(|| miette!("XDG_RUNTIME_DIR is unset"))?;
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .into_diagnostic()?;
    }
    // A socket left by a service that was killed refuses the bind.
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path)
        .into_diagnostic()
        .wrap_err_with(|| format!("listening on {}", path.display()))
}

/// Prints what the running service finds for `text`.
fn search(text: &str) -> miette::Result<()> {
    let socket = derisk_index::socket().ok_or_else(|| miette!("XDG_RUNTIME_DIR is unset"))?;
    let mut client = derisk_index::Client::connect(&socket)
        .into_diagnostic()
        .wrap_err("is derisk-index.service running?")?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for found in client
        .search(text, derisk_index::MAX_MATCHES)
        .into_diagnostic()?
    {
        let shown = derisk_index::shown(&found.path, home.as_deref());
        match (found.line, found.snippet) {
            (Some(line), Some(snippet)) => println!("{shown}:{line}: {snippet}"),
            _ if found.folder => println!("{shown}/"),
            _ => println!("{shown}"),
        }
    }
    Ok(())
}

/// Logs `tracing` events to stderr, which systemd hands to the journal.
/// `RUST_LOG` picks what is logged: `info` and up when it is unset or does
/// not parse.
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
