//! Reading a file's text, and cutting the line that matched down to what a
//! palette row has room for.
//!
//! Plain text is read as it is. A PDF's text comes from poppler's
//! `pdftotext`, when it is installed, and an OpenDocument or Office Open
//! XML document's from the XML inside its ZIP ([`office`]), so the words
//! in a report or a spreadsheet are found as a note's are. Pictures have
//! no text to read without OCR, and are found by their names.

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use crate::TEXT_BYTES;

pub mod office;

/// How far into a file a NUL marks it as binary, as git and ripgrep look.
const SNIFF: usize = 8 << 10;

/// How long `pdftotext` may take over one PDF before it is stopped and the
/// PDF is found by its name: a scanned book can take minutes.
const PDF_TIME: Duration = Duration::from_secs(20);

/// The text of the file at `path`, at most [`TEXT_BYTES`] of it, with
/// anything that is not UTF-8 replaced, or `None` for a file with no text
/// to read: a binary file, which has a NUL in its first bytes, or a
/// document whose text could not be had.
pub fn read(path: &Path) -> io::Result<Option<String>> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("pdf") => Ok(pdf(path)),
        Some(kind) if office::Kind::of(kind).is_some() => {
            let kind = office::Kind::of(kind).unwrap_or(office::Kind::OpenDocument);
            Ok(office::text(&mut File::open(path)?, kind)?.map(|t| clip(t, TEXT_BYTES)))
        }
        _ => plain(path),
    }
}

/// A plain file's text, or `None` for a binary one.
fn plain(path: &Path) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(TEXT_BYTES as u64)
        .read_to_end(&mut bytes)?;
    if bytes[..bytes.len().min(SNIFF)].contains(&0) {
        return Ok(None);
    }
    Ok(Some(lossy(bytes)))
}

fn lossy(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// `text` cut to at most `bytes`, between characters.
fn clip(mut text: String, bytes: usize) -> String {
    text.truncate(text.floor_char_boundary(bytes));
    text
}

/// A PDF's text, from `pdftotext` on `PATH`, or `None` when it is not
/// installed, fails, or takes longer than [`PDF_TIME`]. Its output is read
/// up to [`TEXT_BYTES`]; a watcher beside the read kills it at the
/// deadline, which ends a read that would otherwise wait on the pipe.
fn pdf(path: &Path) -> Option<String> {
    let mut child = Command::new("pdftotext")
        .args(["-q", "-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let done = AtomicBool::new(false);
    let bytes = std::thread::scope(|s| {
        s.spawn(|| {
            let started = Instant::now();
            while !done.load(Ordering::Relaxed) {
                if started.elapsed() > PDF_TIME {
                    let _ = child.kill();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let mut bytes = Vec::new();
        let read = (&mut stdout)
            .take(TEXT_BYTES as u64)
            .read_to_end(&mut bytes);
        done.store(true, Ordering::Relaxed);
        read.ok().map(|_| bytes)
    })?;
    // Past TEXT_BYTES it is still writing: the closed pipe stops it.
    drop(stdout);
    let status = child.wait().ok()?;
    (status.success() || bytes.len() == TEXT_BYTES).then(|| lossy(bytes))
}

/// Characters of a line shown before the first matching word, so it reads
/// in context.
const LEAD: usize = 24;

/// `line` cut to at most `width` characters around byte offset `at`,
/// starting a little before it, with `…` where it was cut and runs of
/// whitespace, tabs included, as one space.
pub fn cut(line: &str, at: usize, width: usize) -> String {
    let at = line.floor_char_boundary(at.min(line.len()));
    let before: Vec<char> = line[..at].chars().collect();
    let keep = before.len().saturating_sub(LEAD);
    let start: usize = before[..keep].iter().map(|c| c.len_utf8()).sum();
    let mut out = String::new();
    if !line[..start].trim().is_empty() {
        out.push('…');
    }
    let mut count = 0;
    let mut space = false;
    let rest = line[start..].trim_start();
    for c in rest.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        let gap = usize::from(space && count > 0);
        space = false;
        if count + gap >= width {
            out.push('…');
            return out;
        }
        if gap == 1 {
            out.push(' ');
        }
        out.push(c);
        count += gap + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_line_is_whole() {
        assert_eq!(cut("  buy milk\tand eggs  ", 6, 80), "buy milk and eggs");
    }

    #[test]
    fn a_long_line_starts_near_the_match() {
        let line = format!("{}needle and the rest", "x ".repeat(100));
        let at = line.find("needle").unwrap();
        let shown = cut(&line, at, 30);
        assert!(shown.starts_with('…'), "{shown}");
        assert!(shown.contains("needle"), "{shown}");
        assert!(shown.chars().count() <= 32, "{shown}");
    }

    #[test]
    fn cuts_fall_between_characters() {
        let line = "ééééééééééééééééééééééééééééééé word";
        let shown = cut(line, line.find("word").unwrap(), 10);
        assert!(shown.starts_with('…'));
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn binary_files_have_no_text() {
        let dir = std::env::temp_dir().join(format!("derisk-index-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), b"PK\0\x03").unwrap();
        std::fs::write(dir.join("a.txt"), b"caf\xe9 au lait").unwrap();
        assert_eq!(read(&dir.join("a.bin")).unwrap(), None);
        assert_eq!(
            read(&dir.join("a.txt")).unwrap().unwrap(),
            "caf\u{fffd} au lait"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
