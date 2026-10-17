//! Reading a file's text, and cutting the line that matched down to what a
//! palette row has room for.

use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use crate::TEXT_BYTES;

/// How far into a file a NUL marks it as binary, as git and ripgrep look.
const SNIFF: usize = 8 << 10;

/// The text of the file at `path`: its first [`TEXT_BYTES`], with anything
/// that is not UTF-8 replaced, or `None` for a binary file, which has a NUL
/// in its first bytes.
pub fn read(path: &Path) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(TEXT_BYTES as u64)
        .read_to_end(&mut bytes)?;
    if bytes[..bytes.len().min(SNIFF)].contains(&0) {
        return Ok(None);
    }
    Ok(Some(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }))
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
