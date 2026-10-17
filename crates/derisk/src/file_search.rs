//! What is in the files, for the command palette: the typed text goes to
//! derisk's file index (`derisk-index.service`) on a thread of its own, and
//! what it finds comes back for the files plugin's rows.
//!
//! The service answers in milliseconds, but over a socket, which a frame
//! never waits on. Without the service (not installed, or a session not
//! started by systemd) every search finds nothing and the palette shows
//! files by name only, as it did before the index.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use derisk::palette::{self, FileMatch};
use derisk_index::Client;
use tracing::{debug, warn};

/// Most files one search lists: the palette shows nine rows at once, and
/// these share them with the files matched by name.
const LIMIT: usize = 20;

/// The searching thread, and what was last asked of it.
#[derive(Debug)]
pub struct FileSearch {
    queries: Sender<String>,
    found: Receiver<Vec<FileMatch>>,
    asked: Option<String>,
}

impl FileSearch {
    /// Starts the searching thread. It ends when this is dropped.
    pub fn start() -> Option<Self> {
        let (queries, queries_rx) = bounded(1);
        let (found_tx, found) = bounded(1);
        let spawned = std::thread::Builder::new()
            .name("derisk-file-search".into())
            .spawn(move || search(&queries_rx, &found_tx));
        if let Err(e) = spawned {
            warn!(error = %e, "the palette cannot search files' text");
            return None;
        }
        Some(Self {
            queries,
            found,
            asked: None,
        })
    }

    /// Asks for `text` unless it was the last thing asked, and returns
    /// what an earlier ask found, once it has. `None` asks for nothing and
    /// forgets the last ask, for when the palette closes.
    pub fn poll(&mut self, text: Option<&str>) -> Option<Vec<FileMatch>> {
        match text {
            Some(text) if self.asked.as_deref() != Some(text) => {
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

/// The searching thread: one search at a time over a connection kept open
/// between them, made again after it breaks.
fn search(queries: &Receiver<String>, found: &Sender<Vec<FileMatch>>) {
    let socket = derisk_index::socket();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut client: Option<Client> = None;
    for text in queries {
        let answer = match (&mut client, &socket) {
            (Some(client), _) => client.search(&text, LIMIT),
            (None, Some(socket)) => {
                Client::connect(socket).and_then(|c| client.insert(c).search(&text, LIMIT))
            }
            (None, None) => Ok(Vec::new()),
        };
        let matches = answer.unwrap_or_else(|e| {
            debug!(error = %e, "the file index did not answer");
            client = None;
            Vec::new()
        });
        let matches = matches
            .iter()
            .map(|m| palette::file_match(m, home.as_deref()))
            .collect();
        if found.send(matches).is_err() {
            return;
        }
    }
}
