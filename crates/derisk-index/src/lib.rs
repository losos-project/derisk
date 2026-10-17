//! derisk's file index: the names and the text of the files under home,
//! for the command palette to search.
//!
//! The index is kept by a user service of its own, `derisk-index.service`,
//! so the compositor never walks a disk or holds a search engine: the
//! service walks home once, then follows what changes through inotify, and
//! answers searches on `$XDG_RUNTIME_DIR/derisk/index.sock`. The session
//! asks it for the typed text and the palette's files plugin turns what
//! comes back into rows, each with the line that matched and the folder
//! the file is in.
//!
//! What is indexed is what ripgrep would search: hidden entries, symlinks
//! and whatever a `.gitignore`, `.ignore` or git's own excludes leave out
//! are skipped. Every other file and folder is found by its name; a text
//! file (no NUL in its first bytes) also by the words in its first
//! [`TEXT_BYTES`]. The index lives under `$XDG_CACHE_HOME/derisk-index`,
//! nowhere but the person's own home, and can be deleted at any time: the
//! service builds it again.
//!
//! This crate's library is the protocol and its client, all the session
//! links. The service, under the `service` feature, is the `derisk-index`
//! binary.
//!
//! The protocol is one JSON object a line each way:
//!
//! ```text
//! → {"search":"quarterly budget","limit":20}
//! ← {"matches":[{"path":"/home/me/notes/q3.md","folder":false,"line":12,"snippet":"Quarterly budget: …"}]}
//! ```

use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

#[cfg(feature = "service")]
pub mod service;

/// How much of a text file is read for its words. Past this a file is
/// found by its name and its first megabyte; logs and data dumps that
/// size are rarely what someone types a word from.
pub const TEXT_BYTES: usize = 1 << 20;

/// Most matches one search returns.
pub const MAX_MATCHES: usize = 100;

/// A search, as the session sends it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The typed text. Every word must be in a file's name, the names of
    /// the folders it is in under home, or its text, and the last may be
    /// the start of one, since it is still being typed.
    pub search: String,
    /// Most matches wanted, at most [`MAX_MATCHES`].
    pub limit: usize,
}

/// The service's answer to a [`Request`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Response {
    /// The best matches first.
    Matches(Vec<Match>),
    /// Why the search could not run.
    Error(String),
}

/// One file or folder a search found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    /// Absolute path.
    pub path: PathBuf,
    /// Whether it is a folder.
    pub folder: bool,
    /// The line of its text that matched, from 1; `None` when its name
    /// did.
    pub line: Option<u32>,
    /// That line, cut around the first matching word, with `…` where it
    /// was cut.
    pub snippet: Option<String>,
}

/// Where the service listens: `derisk/index.sock` under
/// `$XDG_RUNTIME_DIR`, as `derisk-index.socket` puts it.
pub fn socket() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|r| !r.is_empty())?;
    Some(PathBuf::from(runtime).join("derisk/index.sock"))
}

/// How long the client waits for an answer. A search over a whole home
/// takes milliseconds; past this the service is busy or stuck, and the
/// palette shows its other rows without file contents.
const TIMEOUT: Duration = Duration::from_secs(2);

/// A connection to the service, kept between searches.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    /// Connects to the service at `socket`.
    ///
    /// # Errors
    ///
    /// When nothing listens there: the service is not installed, or is
    /// not running in this session.
    pub fn connect(socket: &Path) -> io::Result<Self> {
        let writer = UnixStream::connect(socket)?;
        writer.set_read_timeout(Some(TIMEOUT))?;
        writer.set_write_timeout(Some(TIMEOUT))?;
        Ok(Self {
            reader: BufReader::new(writer.try_clone()?),
            writer,
        })
    }

    /// The files matching `text`, best first, at most `limit`.
    ///
    /// # Errors
    ///
    /// When the connection breaks or times out, or the service could not
    /// search; a broken connection is not used again.
    pub fn search(&mut self, text: &str, limit: usize) -> io::Result<Vec<Match>> {
        let request = Request {
            search: text.to_owned(),
            limit,
        };
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        let mut answer = String::new();
        if self.reader.read_line(&mut answer)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        match serde_json::from_str(&answer)? {
            Response::Matches(matches) => Ok(matches),
            Response::Error(e) => Err(io::Error::other(e)),
        }
    }
}

/// `path` as the palette shows it: `~/...` under `home`, otherwise whole.
pub fn shown(path: &Path, home: Option<&Path>) -> String {
    match home.map(|h| path.strip_prefix(h)) {
        Some(Ok(rel)) if rel.as_os_str().is_empty() => "~".to_owned(),
        Some(Ok(rel)) => format!("~/{}", rel.display()),
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_are_tagged_by_kind() {
        let found = Response::Matches(vec![Match {
            path: "/home/me/a.txt".into(),
            folder: false,
            line: Some(3),
            snippet: Some("hello".into()),
        }]);
        let text = serde_json::to_string(&found).unwrap();
        assert!(
            text.starts_with(r#"{"matches":[{"path":"/home/me/a.txt""#),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<Response>(&text).unwrap(), found);
        let failed = serde_json::to_string(&Response::Error("no".into())).unwrap();
        assert_eq!(failed, r#"{"error":"no"}"#);
    }

    #[test]
    fn home_is_shown_as_a_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(
            shown(Path::new("/home/me/notes/a.md"), Some(home)),
            "~/notes/a.md"
        );
        assert_eq!(shown(home, Some(home)), "~");
        assert_eq!(shown(Path::new("/srv/x"), Some(home)), "/srv/x");
        assert_eq!(shown(Path::new("/srv/x"), None), "/srv/x");
    }
}
