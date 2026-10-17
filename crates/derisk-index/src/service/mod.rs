//! The service: keeps the index ([`store`]) in step with home ([`watch`])
//! and answers searches on its socket.
//!
//! Three kinds of thread, all scoped to [`run`]: one reads inotify, one
//! writes the index (the first walk, then what changes), and one per
//! connection answers searches from the index as last committed.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
};

use crossbeam_channel::bounded;
use inotify::Inotify;
use tracing::{debug, error, warn};

use crate::{Request, Response};

pub mod store;
pub mod text;
pub mod watch;

pub use store::{Error, Store};

/// Changes waiting for the writer. The reading thread blocks past this, and
/// the kernel queues what follows until its own queue overflows, which
/// walks home again.
const CHANGES: usize = 4096;

/// Indexes `home` into `index` and answers searches on `listener` until
/// the process is stopped. Once running, a failure to write the index
/// ends the process, since the threads answering searches would keep it
/// open, and systemd starts it again.
///
/// # Errors
///
/// When the index cannot be opened or inotify is missing.
pub fn run(home: &Path, index: &Path, listener: &UnixListener) -> Result<(), Error> {
    let store = Store::open(index, home)?;
    let inotify = Inotify::init()?;
    let mut indexer = watch::Indexer::new(&store, inotify.watches())?;
    let (changes_tx, changes) = bounded(CHANGES);
    std::thread::scope(|s| {
        s.spawn(move || watch::read(inotify, &changes_tx));
        s.spawn(|| {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let store = &store;
                        s.spawn(move || answer(store, stream));
                    }
                    Err(e) => warn!(error = %e, "cannot accept a search"),
                }
            }
        });
        // Searches are answered from what is indexed so far while the
        // first walk runs.
        match indexer.walk().and_then(|()| indexer.follow(&changes)) {
            Err(e) => error!(error = %e, "stopped indexing"),
            Ok(()) => error!("stopped following changes under home"),
        }
        std::process::exit(1)
    })
}

/// Answers one connection's searches, a line each, until it closes.
fn answer(store: &Store, stream: UnixStream) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            return;
        };
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => match store.search(&request.search, request.limit) {
                Ok(matches) => Response::Matches(matches),
                Err(e) => Response::Error(e.to_string()),
            },
            Err(e) => Response::Error(format!("not a search: {e}")),
        };
        let mut out = serde_json::to_string(&response).unwrap_or_default();
        out.push('\n');
        if let Err(e) = writer.write_all(out.as_bytes()) {
            debug!(error = %e, "search answer not sent");
            return;
        }
    }
}
